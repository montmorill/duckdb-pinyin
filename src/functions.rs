//! `pinyin_match`, the one SQL function over `PINYIN`.

use crate::pinyin::{self, Pattern};
use duckdb::{ffi, ffi::duckdb_string_t, types::DuckString};
use std::{
    error::Error,
    ffi::{CStr, CString},
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
    let mut cached: Option<(Vec<u8>, Pattern)> = None;

    for i in 0..count {
        let valid = unsafe {
            ffi::duckdb_validity_row_is_valid(value_validity, i as ffi::idx_t)
                && ffi::duckdb_validity_row_is_valid(pattern_validity, i as ffi::idx_t)
        };
        if !valid {
            unsafe {
                ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false);
                *answers.add(i) = false;
            }
            continue;
        }

        let mut raw = unsafe { *raw_patterns.add(i) };
        let text = DuckString::new(&mut raw).as_str();

        let pattern = match &cached {
            Some((key, pattern)) if key.as_slice() == text.as_bytes() => *pattern,
            _ => {
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
                cached = Some((text.as_bytes().to_vec(), pattern));
                pattern
            }
        };

        unsafe { *answers.add(i) = pattern.matches(*values.add(i)) };
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
    // not recompile it per row. Held as two pieces rather than an `Option` so
    // that the comparison below borrows nothing that the update in the `None`
    // arm would have to fight with.
    let mut cached_key: Vec<u8> = Vec::new();
    let mut cached_elems: Vec<pinyin::Elem> = Vec::new();
    let mut have_cached = false;

    // Reused across rows so that a long list does not allocate per row.
    let mut syllables: Vec<u16> = Vec::new();

    for i in 0..count {
        let valid = unsafe {
            ffi::duckdb_validity_row_is_valid(list_validity, i as ffi::idx_t)
                && ffi::duckdb_validity_row_is_valid(pattern_validity, i as ffi::idx_t)
        };
        if !valid {
            unsafe {
                ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false);
                *answers.add(i) = false;
            }
            continue;
        }

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
                    // Same reasoning as the one-syllable overload: a pattern that
                    // names nothing legal is a typo, so say so rather than
                    // quietly returning false for every row.
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

        // A null anywhere in the list makes the whole answer null: there is no
        // syllable there to have matched or not matched.
        let entry = unsafe { *entries.add(i) };
        let start = entry.offset as usize;
        let end = start + entry.length as usize;

        syllables.clear();
        let mut null_element = false;
        for j in start..end {
            if !unsafe { ffi::duckdb_validity_row_is_valid(child_validity, j as ffi::idx_t) } {
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

        unsafe { *answers.add(i) = pinyin::match_sequence(&cached_elems, &syllables) };
    }
}
