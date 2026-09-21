//! `pinyin_match`, the one SQL function over `PINYIN`.

use super::pinyin::{self, Pattern};
use duckdb::{ffi, ffi::duckdb_string_t, types::DuckString};
use std::{
    error::Error,
    ffi::{CStr, CString},
    ptr, slice,
};

/// The SQL name of the function.
const MATCH_FUNCTION_NAME: &str = "pinyin_match";

/// Register the two `pinyin_match` overloads:
///
/// ```text
/// pinyin_match(PINYIN,   VARCHAR) -> BOOLEAN   one syllable
/// pinyin_match(PINYIN[], VARCHAR) -> BOOLEAN   a sequence of them
/// ```
///
/// A bare `USMALLINT[]` reaches the second overload through the implicit
/// `USMALLINT[] -> PINYIN[]` cast registered in `lib.rs`, so there is no third
/// overload for it — and there must not be one. Overload resolution asks each
/// candidate for the cost of casting the argument to its parameter, and that
/// cast is registered at cost 0, the same as the exact match `USMALLINT[] ->
/// USMALLINT[]` would be. Two candidates at equal cost is an ambiguity error, so
/// spelling the same overload out a second time would not widen what binds — it
/// would make the call stop binding at all.
///
/// These go through the C API rather than the `duckdb` crate's `vscalar`
/// helper, because a parameter has to be the `PINYIN` alias itself. The crate's
/// `LogicalTypeHandle::new` is `pub(crate)`, so a `ScalarFunctionSignature` can
/// only name a built-in type — declaring `USMALLINT` there would make DuckDB
/// bind a string literal by parsing it as a *number*, and reject a `VARCHAR`
/// column outright. Spelling `PINYIN` out here means a literal goes through the
/// type's own cast and a bad one reports `Invalid pinyin syllable` instead of a
/// numeric conversion error. `PINYIN[]` is `LIST(PINYIN)`, which the C API can
/// build from the same handle, so the array overload costs no new type.
///
/// # Safety
///
/// `con` must be a live `duckdb_connection`.
pub unsafe fn register(con: ffi::duckdb_connection, alias: &CStr) -> Result<(), Box<dyn Error>> {
    let mut pinyin =
        unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT) };
    let mut varchar = unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR) };
    let mut boolean = unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN) };
    if pinyin.is_null() || varchar.is_null() || boolean.is_null() {
        unsafe {
            ffi::duckdb_destroy_logical_type(&mut pinyin);
            ffi::duckdb_destroy_logical_type(&mut varchar);
            ffi::duckdb_destroy_logical_type(&mut boolean);
        }
        return Err("could not create the pinyin_match logical types".into());
    }
    // A bare USMALLINT here would be a different type from the registered one.
    unsafe { ffi::duckdb_logical_type_set_alias(pinyin, alias.as_ptr()) };

    // `PINYIN[]`, built from the aliased handle so the element type is the
    // registered one rather than a bare USMALLINT.
    let mut list = unsafe { ffi::duckdb_create_list_type(pinyin) };
    if list.is_null() {
        unsafe {
            ffi::duckdb_destroy_logical_type(&mut pinyin);
            ffi::duckdb_destroy_logical_type(&mut varchar);
            ffi::duckdb_destroy_logical_type(&mut boolean);
        }
        return Err("could not create the PINYIN[] logical type".into());
    }

    let result = unsafe {
        register_one(
            con,
            &[pinyin, varchar],
            boolean,
            Some(pinyin_match_function),
        )
        .and_then(|()| {
            register_one(
                con,
                &[list, varchar],
                boolean,
                Some(pinyin_match_array_function),
            )
        })
    };

    unsafe {
        ffi::duckdb_destroy_logical_type(&mut pinyin);
        ffi::duckdb_destroy_logical_type(&mut varchar);
        ffi::duckdb_destroy_logical_type(&mut boolean);
        ffi::duckdb_destroy_logical_type(&mut list);
    }

    result
}

/// Build and register one `pinyin_match` overload. The logical types are
/// borrowed: the caller keeps ownership of every handle passed in, including on
/// failure.
///
/// # Safety
///
/// `con` must be live, and every handle in `parameters` and `returns` must be a
/// live `duckdb_logical_type`.
unsafe fn register_one(
    con: ffi::duckdb_connection,
    parameters: &[ffi::duckdb_logical_type],
    returns: ffi::duckdb_logical_type,
    callback: ffi::duckdb_scalar_function_t,
) -> Result<(), Box<dyn Error>> {
    let mut function = unsafe { ffi::duckdb_create_scalar_function() };
    if function.is_null() {
        return Err("could not create a pinyin_match scalar function".into());
    }

    unsafe {
        ffi::duckdb_scalar_function_set_name(function, c"pinyin_match".as_ptr());
        for parameter in parameters {
            ffi::duckdb_scalar_function_add_parameter(function, *parameter);
        }
        ffi::duckdb_scalar_function_set_return_type(function, returns);
        ffi::duckdb_scalar_function_set_function(function, callback);
    }

    let registered = unsafe { ffi::duckdb_register_scalar_function(con, function) };
    unsafe { ffi::duckdb_destroy_scalar_function(&mut function) };

    if registered != ffi::duckdb_state_DuckDBSuccess {
        return Err(format!("could not register {MATCH_FUNCTION_NAME}").into());
    }
    Ok(())
}

/// Read one validity bit out of a vector's null mask.
///
/// DuckDB's own header calls `duckdb_validity_row_is_valid` "the (slower)" way
/// to ask this, and as a loadable extension every call to it is an indirect one
/// through the API table on top of that. The bit is nothing more than
/// `mask[row / 64] & (1 << (row % 64))`, and a vector holding no nulls carries a
/// *null* mask meaning "all valid" — so the usual case is one pointer test, and
/// the rest inlines into the caller's loop instead of calling out of the
/// extension a few million times.
///
/// # Safety
///
/// `validity` must be null, or a mask covering at least `row + 1` rows.
#[inline(always)]
unsafe fn row_is_valid(validity: *const u64, row: usize) -> bool {
    validity.is_null() || unsafe { *validity.add(row >> 6) & (1u64 << (row & 63)) != 0 }
}

/// The width [`raw_string_bits`] reads, and the only thing making its comparison
/// whole-value rather than a prefix.
const _: () = assert!(std::mem::size_of::<duckdb_string_t>() == 16);

/// Read a `duckdb_string_t` as one integer, so that two of them can be compared
/// without decoding either.
///
/// The struct is self-describing, which is what makes the comparison mean what
/// it looks like it means: a string of 12 bytes or fewer keeps its characters
/// inline, and a longer one keeps its length and pointer in the same 16 bytes.
/// Either way equal bits imply the same string — a pointer that agrees has to
/// point at the same bytes. The converse does not hold, so a caller that sees
/// differing bits still has to compare the text itself before concluding
/// anything.
///
/// # Safety
///
/// `patterns` must point at at least `row + 1` initialised `duckdb_string_t`,
/// which is what a flattened `VARCHAR` vector is.
#[inline(always)]
unsafe fn raw_string_bits(patterns: *const duckdb_string_t, row: usize) -> u128 {
    unsafe { ptr::read_unaligned(patterns.add(row).cast::<u128>()) }
}

/// Match every row of the chunk and write the answers out.
///
/// DuckDB flattens the input vectors before calling a plain scalar function, so
/// both are read as flat arrays. The `'p?'` in a query is one flat vector of
/// the same bytes repeated, which is what makes the pattern cache below pay off.
///
/// # Safety
///
/// Called by DuckDB with the vectors registered above.
unsafe extern "C" fn pinyin_match_function(
    info: ffi::duckdb_function_info,
    input: ffi::duckdb_data_chunk,
    output: ffi::duckdb_vector,
) {
    let count = unsafe { ffi::duckdb_data_chunk_get_size(input) } as usize;
    let syllables = unsafe { ffi::duckdb_data_chunk_get_vector(input, 0) };
    let patterns = unsafe { ffi::duckdb_data_chunk_get_vector(input, 1) };

    let values = unsafe { ffi::duckdb_vector_get_data(syllables) }.cast::<u16>();
    let raw_patterns = unsafe { ffi::duckdb_vector_get_data(patterns) }.cast::<duckdb_string_t>();
    let value_validity = unsafe { ffi::duckdb_vector_get_validity(syllables) };
    let pattern_validity = unsafe { ffi::duckdb_vector_get_validity(patterns) };

    // The output is all-valid until a row is failed, and a vector with no nulls
    // starts with a null mask, so the mask must be made writable before it is
    // read.
    unsafe { ffi::duckdb_vector_ensure_validity_writable(output) };
    let answers = unsafe { ffi::duckdb_vector_get_data(output) }.cast::<bool>();
    let out_validity = unsafe { ffi::duckdb_vector_get_validity(output) };

    // A query repeats one literal pattern on every row, and compiling it is the
    // expensive half of the work, so remember the last one. Where the rows
    // disagree this is one recompile per distinct pattern, which is what a fresh
    // compile would have cost anyway.
    //
    // Whether the pattern is the one already in hand is decided on the raw bits
    // of the `duckdb_string_t`, not on the text: `DuckString::as_str` is a
    // `from_utf8_lossy` over `duckdb_string_t_length` and `duckdb_string_t_data`,
    // two calls out through the C API table, and paying that on every row to
    // rediscover that a literal has not changed was most of what this function
    // cost. Equal bits mean equal strings; differing bits mean only "look
    // closer", which is what the text comparison below is for.
    let mut cached_bits = 0u128;
    let mut cached_key: Vec<u8> = Vec::new();
    let mut cached = Pattern::Never;
    let mut have_cached = false;

    for i in 0..count {
        let valid = unsafe { row_is_valid(value_validity, i) && row_is_valid(pattern_validity, i) };
        if !valid {
            unsafe {
                ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false);
                *answers.add(i) = false;
            }
            continue;
        }

        let bits = unsafe { raw_string_bits(raw_patterns, i) };
        if !have_cached || bits != cached_bits {
            let mut raw = unsafe { *raw_patterns.add(i) };
            let text = DuckString::new(&mut raw).as_str();

            // A row holding the same pattern stored somewhere else — a VARCHAR
            // column rather than a folded literal — still reuses the compiled
            // form, and only a genuinely new pattern is worth compiling.
            if !have_cached || cached_key.as_slice() != text.as_bytes() {
                let pattern = pinyin::compile_pattern(&text);
                if pattern == Pattern::Never {
                    // A pattern that names nothing legal is a typo far more
                    // often than it is an intent to match nothing, so say so
                    // rather than quietly returning false for every row.
                    match CString::new(format!("Invalid pinyin pattern: '{text}'")) {
                        Ok(message) => unsafe {
                            ffi::duckdb_scalar_function_set_error(info, message.as_ptr())
                        },
                        Err(_) => unsafe {
                            ffi::duckdb_scalar_function_set_error(
                                info,
                                c"invalid pinyin pattern".as_ptr(),
                            )
                        },
                    }
                    return;
                }
                cached_key.clear();
                cached_key.extend_from_slice(text.as_bytes());
                cached = pattern;
                have_cached = true;
            }
            cached_bits = bits;
        }

        unsafe { *answers.add(i) = cached.matches(*values.add(i)) };
    }
}

/// The `PINYIN[]` overload: match a whole list of syllables against a
/// whitespace-separated pattern.
///
/// A list vector is a vector of `(offset, length)` entries pointing into a
/// separate child vector of the element type, so the syllables of one row are a
/// slice of the child rather than a run of contiguous rows — each row's slice
/// starts wherever the previous one ended, which is why the offsets have to be
/// read rather than assumed.
///
/// # Safety
///
/// Called by DuckDB with the vectors registered above.
unsafe extern "C" fn pinyin_match_array_function(
    info: ffi::duckdb_function_info,
    input: ffi::duckdb_data_chunk,
    output: ffi::duckdb_vector,
) {
    let count = unsafe { ffi::duckdb_data_chunk_get_size(input) } as usize;
    let lists = unsafe { ffi::duckdb_data_chunk_get_vector(input, 0) };
    let patterns = unsafe { ffi::duckdb_data_chunk_get_vector(input, 1) };

    let entries = unsafe { ffi::duckdb_vector_get_data(lists) }.cast::<ffi::duckdb_list_entry>();
    let children = unsafe { ffi::duckdb_list_vector_get_child(lists) };
    let child_values = unsafe { ffi::duckdb_vector_get_data(children) }.cast::<u16>();
    let child_validity = unsafe { ffi::duckdb_vector_get_validity(children) };

    let raw_patterns = unsafe { ffi::duckdb_vector_get_data(patterns) }.cast::<duckdb_string_t>();
    let list_validity = unsafe { ffi::duckdb_vector_get_validity(lists) };
    let pattern_validity = unsafe { ffi::duckdb_vector_get_validity(patterns) };

    unsafe { ffi::duckdb_vector_ensure_validity_writable(output) };
    let answers = unsafe { ffi::duckdb_vector_get_data(output) }.cast::<bool>();
    let out_validity = unsafe { ffi::duckdb_vector_get_validity(output) };

    // The last compiled pattern, kept so that a query repeating one literal does
    // not recompile it per row. Held as pieces rather than an `Option` so that
    // the comparison below borrows nothing that the update in the `None` arm
    // would have to fight with.
    //
    // As in the one-syllable overload, the cheap "is this still the same
    // pattern" test is the raw bits of the `duckdb_string_t` — decoding costs
    // two calls out through the C API table plus a UTF-8 scan, none of which is
    // worth paying per row to rediscover an unchanged literal.
    let mut cached_bits = 0u128;
    let mut cached_key: Vec<u8> = Vec::new();
    let mut cached_elems: Vec<pinyin::Elem> = Vec::new();
    let mut have_cached = false;

    // Only the null-bearing path below stages a row's syllables here; a row
    // with none needs no copy at all. Reused across rows either way.
    let mut syllables: Vec<u16> = Vec::new();

    for i in 0..count {
        let valid = unsafe { row_is_valid(list_validity, i) && row_is_valid(pattern_validity, i) };
        if !valid {
            unsafe {
                ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false);
                *answers.add(i) = false;
            }
            continue;
        }

        let bits = unsafe { raw_string_bits(raw_patterns, i) };
        if !have_cached || bits != cached_bits {
            let mut raw = unsafe { *raw_patterns.add(i) };
            let text = DuckString::new(&mut raw).as_str();

            if !have_cached || cached_key.as_slice() != text.as_bytes() {
                match pinyin::compile_sequence(&text) {
                    Some(elems) => {
                        cached_key.clear();
                        cached_key.extend_from_slice(text.as_bytes());
                        cached_elems = elems;
                        have_cached = true;
                    }
                    None => {
                        // Same reasoning as the one-syllable overload: a pattern
                        // that names nothing legal is a typo, so say so rather
                        // than quietly returning false for every row.
                        match CString::new(format!("Invalid pinyin pattern: '{text}'")) {
                            Ok(message) => unsafe {
                                ffi::duckdb_scalar_function_set_error(info, message.as_ptr())
                            },
                            Err(_) => unsafe {
                                ffi::duckdb_scalar_function_set_error(
                                    info,
                                    c"invalid pinyin pattern".as_ptr(),
                                )
                            },
                        }
                        return;
                    }
                }
            }
            cached_bits = bits;
        }

        let entry = unsafe { *entries.add(i) };
        let start = entry.offset as usize;
        let length = entry.length as usize;

        // A null anywhere in the list makes the whole answer null: there is no
        // syllable there to have matched or not matched. A child vector holding
        // no nulls carries a null mask, and then the row's syllables are already
        // contiguous in the child — so the common case matches straight off that
        // slice, with no staging copy and no per-element validity test.
        let matched = if child_validity.is_null() {
            let row = unsafe { slice::from_raw_parts(child_values.add(start), length) };
            pinyin::match_sequence(&cached_elems, row)
        } else {
            syllables.clear();
            let mut null_element = false;
            for j in start..start + length {
                if !unsafe { row_is_valid(child_validity, j) } {
                    null_element = true;
                    break;
                }
                syllables.push(unsafe { *child_values.add(j) });
            }
            if null_element {
                unsafe {
                    ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false);
                    *answers.add(i) = false;
                }
                continue;
            }
            pinyin::match_sequence(&cached_elems, &syllables)
        };

        unsafe { *answers.add(i) = matched };
    }
}
