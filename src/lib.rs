use duckdb::{Result, ffi, ffi::duckdb_string_t, types::DuckString};
use std::{
    error::Error,
    ffi::{CStr, CString},
    ptr,
};

// Each `mod` carries a `#[path]`, and the submodules refer to each other with
// `super::` rather than `crate::`. Both are needed because this file is compiled
// under two different crate roots. Natively it *is* the root; for the wasm build
// `src/wasm_lib.rs` pulls it in as `mod lib`, so the wasm target can be a
// staticlib while the native one stays a cdylib. Under that second shape this
// file is `crate::lib`, so a plain `mod pinyin;` would look for
// `src/lib/pinyin.rs` and `crate::pinyin` would name something else entirely.
// `#[path]` is resolved against the directory this file sits in — `src/` either
// way — and `super::` is this module — this file — either way.
#[path = "functions.rs"]
mod functions;
#[path = "pinyin.rs"]
mod pinyin;
#[path = "pinyin_data.rs"]
mod pinyin_data;

// --- the PINYIN column type -------------------------------------------------

/// The name the type is registered and shown under.
const PINYIN_TYPE_NAME: &str = "PINYIN";

/// The oldest DuckDB this builds against. `make` injects the real value;
/// a bare `cargo build` falls back to the same default the entrypoint macro
/// uses, so that the crate still compiles outside the extension build.
const MIN_DUCKDB_VERSION: &str = match option_env!("DUCKDB_EXTENSION_MIN_DUCKDB_VERSION") {
    Some(version) => version,
    None => "v1.2.0",
};

/// Register `PINYIN` as an alias of `USMALLINT`, plus the casts in and out of
/// `VARCHAR`.
///
/// The `duckdb` crate wraps neither logical types nor casts, so this runs on a
/// raw connection borrowed from the database handle. Registering a type is not
/// scoped to the connection — DuckDB files C API-created types in the system
/// catalog — so the type is visible to every other connection too.
///
/// # Safety
///
/// `db` must be a live `duckdb_database`.
unsafe fn register_pinyin_type(db: ffi::duckdb_database) -> Result<(), Box<dyn Error>> {
    let mut raw: ffi::duckdb_connection = ptr::null_mut();
    if unsafe { ffi::duckdb_connect(db, &mut raw) } != ffi::duckdb_state_DuckDBSuccess {
        return Err("could not open a connection to register the PINYIN type".into());
    }

    let result = unsafe { register_on(raw) };

    unsafe { ffi::duckdb_disconnect(&mut raw) };
    result
}

/// # Safety
///
/// `con` must be a live `duckdb_connection`.
unsafe fn register_on(con: ffi::duckdb_connection) -> Result<(), Box<dyn Error>> {
    let name = CString::new(PINYIN_TYPE_NAME)?;

    // PINYIN is USMALLINT under a name. The alias is what makes DuckDB print
    // `PINYIN` in a schema and give the base type's storage for free.
    let mut base = unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT) };
    if base.is_null() {
        return Err("could not create the USMALLINT logical type".into());
    }
    unsafe { ffi::duckdb_logical_type_set_alias(base, name.as_ptr()) };

    // `info` carries the collation and the extension-owned bind/init callbacks,
    // none of which apply to a plain alias, so it stays null.
    let registered = unsafe { ffi::duckdb_register_logical_type(con, base, ptr::null_mut()) };

    // The registration copies the type, so this handle is ours to free either way.
    unsafe { ffi::duckdb_destroy_logical_type(&mut base) };

    if registered != ffi::duckdb_state_DuckDBSuccess {
        return Err(format!("could not register the {PINYIN_TYPE_NAME} type").into());
    }

    unsafe { register_casts(con, &name)? }
    // Registered here rather than through `Connection::register_scalar_function`
    // because its first parameter has to name the `PINYIN` alias, which only the
    // C API can spell. See `functions::register`.
    unsafe { functions::register(con, &name) }
}

/// # Safety
///
/// `con` must be a live `duckdb_connection`.
unsafe fn register_casts(con: ffi::duckdb_connection, name: &CStr) -> Result<(), Box<dyn Error>> {
    let mut varchar = unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR) };
    let mut u16_ = unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT) };
    let mut pinyin = unsafe { ffi::duckdb_create_logical_type(ffi::DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT) };
    if varchar.is_null() || u16_.is_null() || pinyin.is_null() {
        return Err("could not create the cast logical types".into());
    }
    // Two handles onto USMALLINT: one bare, one carrying the alias. They are
    // different types to the cast registry.
    unsafe { ffi::duckdb_logical_type_set_alias(pinyin, name.as_ptr()) };

    // The same pair one level up, for the array overload.
    let mut list_u16 = unsafe { ffi::duckdb_create_list_type(u16_) };
    let mut list_pinyin = unsafe { ffi::duckdb_create_list_type(pinyin) };

    let mut to_pinyin = unsafe { ffi::duckdb_create_cast_function() };
    let mut from_pinyin = unsafe { ffi::duckdb_create_cast_function() };
    let mut to_u16 = unsafe { ffi::duckdb_create_cast_function() };
    let mut to_pinyin_list = unsafe { ffi::duckdb_create_cast_function() };
    if list_u16.is_null() || list_pinyin.is_null() {
        return Err("could not create the LIST cast logical types".into());
    }
    if to_pinyin.is_null() || from_pinyin.is_null() || to_u16.is_null() || to_pinyin_list.is_null()
    {
        return Err("could not create the cast functions".into());
    }

    // VARCHAR -> PINYIN parses; PINYIN -> VARCHAR renders; PINYIN -> USMALLINT
    // takes the packing apart.
    unsafe {
        ffi::duckdb_cast_function_set_source_type(to_pinyin, varchar);
        ffi::duckdb_cast_function_set_target_type(to_pinyin, pinyin);
        ffi::duckdb_cast_function_set_function(to_pinyin, Some(cast_to_pinyin));
        // Both directions are what a user means by `::`, so let them happen
        // without a TRY_CAST.
        ffi::duckdb_cast_function_set_implicit_cast_cost(to_pinyin, 0);

        ffi::duckdb_cast_function_set_source_type(from_pinyin, pinyin);
        ffi::duckdb_cast_function_set_target_type(from_pinyin, varchar);
        ffi::duckdb_cast_function_set_function(from_pinyin, Some(cast_from_pinyin));
        // Rendering is a convenience, not a coercion: DuckDB's own numeric ->
        // VARCHAR cast must keep winning where a plain `USMALLINT` is involved,
        // so this one is explicit-only.
        ffi::duckdb_cast_function_set_implicit_cast_cost(from_pinyin, -1);

        // The alias is a name, not a distinct type: DuckDB gives no cast at all
        // from an aliased type back to the type it aliases, so without this
        // `pinyin_match(s, ...)` cannot bind `s` to its `USMALLINT` parameter.
        // Registering it implicitly is also what makes a PINYIN usable as the
        // u16 it is — arithmetic, ordering, `min`/`max`, `USMALLINT` functions.
        ffi::duckdb_cast_function_set_source_type(to_u16, pinyin);
        ffi::duckdb_cast_function_set_target_type(to_u16, u16_);
        ffi::duckdb_cast_function_set_function(to_u16, Some(cast_untag_pinyin));
        ffi::duckdb_cast_function_set_implicit_cast_cost(to_u16, 0);

        // The same thing one level up. Parquet — and every other round trip —
        // keeps the storage but drops the alias, so a `PINYIN[]` column read
        // back is a `USMALLINT[]`, and without this the array overload of
        // `pinyin_match` could no longer bind it. The two layouts are
        // identical, so the cast moves nothing and only re-tags the elements.
        ffi::duckdb_cast_function_set_source_type(to_pinyin_list, list_u16);
        ffi::duckdb_cast_function_set_target_type(to_pinyin_list, list_pinyin);
        ffi::duckdb_cast_function_set_function(to_pinyin_list, Some(cast_retag_pinyin_list));
        ffi::duckdb_cast_function_set_implicit_cast_cost(to_pinyin_list, 0);
    }

    let results = [
        unsafe { ffi::duckdb_register_cast_function(con, to_pinyin) },
        unsafe { ffi::duckdb_register_cast_function(con, from_pinyin) },
        unsafe { ffi::duckdb_register_cast_function(con, to_u16) },
        unsafe { ffi::duckdb_register_cast_function(con, to_pinyin_list) },
    ];

    unsafe {
        ffi::duckdb_destroy_cast_function(&mut to_pinyin);
        ffi::duckdb_destroy_cast_function(&mut from_pinyin);
        ffi::duckdb_destroy_cast_function(&mut to_u16);
        ffi::duckdb_destroy_cast_function(&mut to_pinyin_list);
        ffi::duckdb_destroy_logical_type(&mut list_u16);
        ffi::duckdb_destroy_logical_type(&mut list_pinyin);
        ffi::duckdb_destroy_logical_type(&mut varchar);
        ffi::duckdb_destroy_logical_type(&mut u16_);
        ffi::duckdb_destroy_logical_type(&mut pinyin);
    }

    if results
        .iter()
        .any(|state| *state != ffi::duckdb_state_DuckDBSuccess)
    {
        return Err("could not register the PINYIN casts".into());
    }
    Ok(())
}

/// `PINYIN -> USMALLINT`.
///
/// The two types are the same USMALLINT storage, so this only has to move the
/// values and carry the null mask across.
///
/// # Safety
///
/// Called by DuckDB with vectors of the types registered above.
unsafe extern "C" fn cast_untag_pinyin(
    _info: ffi::duckdb_function_info,
    count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
) -> bool {
    let count = count as usize;
    unsafe {
        let source = ffi::duckdb_vector_get_data(input).cast::<u16>();
        let target = ffi::duckdb_vector_get_data(output).cast::<u16>();
        ptr::copy_nonoverlapping(source, target, count);

        // Both sides are all-valid until proven otherwise, and a null mask that
        // starts out null is exactly that, so touching the output mask is only
        // necessary for the rows that are actually null.
        let validity = ffi::duckdb_vector_get_validity(input);
        if !validity.is_null() {
            ffi::duckdb_vector_ensure_validity_writable(output);
            let out_validity = ffi::duckdb_vector_get_validity(output);
            for i in 0..count {
                if !ffi::duckdb_validity_row_is_valid(validity, i as ffi::idx_t) {
                    ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false);
                }
            }
        }
    }
    true
}

/// `USMALLINT[] -> PINYIN[]`.
///
/// Both sides are the same `LIST(USMALLINT)` layout, so no element moves: the
/// output is pointed straight at the input's data, and only the element type's
/// alias differs.
///
/// # Safety
///
/// Called by DuckDB with vectors of the types registered above.
unsafe extern "C" fn cast_retag_pinyin_list(
    _info: ffi::duckdb_function_info,
    _count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
) -> bool {
    unsafe { ffi::duckdb_vector_reference_vector(output, input) };
    true
}

/// `VARCHAR -> PINYIN`.
///
/// # Safety
///
/// Called by DuckDB with vectors of the types registered above.
unsafe extern "C" fn cast_to_pinyin(
    info: ffi::duckdb_function_info,
    count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
) -> bool {
    let count = count as usize;
    let data = unsafe { ffi::duckdb_vector_get_data(input) }.cast::<duckdb_string_t>();
    let validity = unsafe { ffi::duckdb_vector_get_validity(input) };
    let out = unsafe { ffi::duckdb_vector_get_data(output) }.cast::<u16>();

    // A TRY_CAST turns a bad syllable into NULL; a plain cast must raise.
    let try_mode =
        unsafe { ffi::duckdb_cast_function_get_cast_mode(info) } == ffi::duckdb_cast_mode_DUCKDB_CAST_TRY;

    // The output starts out all-valid, and a row we fail is marked invalid
    // individually, so make the validity mask writable up front.
    unsafe { ffi::duckdb_vector_ensure_validity_writable(output) };
    let out_validity = unsafe { ffi::duckdb_vector_get_validity(output) };

    for i in 0..count {
        if !unsafe { ffi::duckdb_validity_row_is_valid(validity, i as ffi::idx_t) } {
            unsafe { ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false) };
            continue;
        }

        let mut raw = unsafe { *data.add(i) };
        let text = DuckString::new(&mut raw).as_str();

        match pinyin::parse(&text) {
            Ok(value) => unsafe { *out.add(i) = value },
            Err(_) if try_mode => {
                unsafe { ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false) }
            }
            Err(err) => {
                let message = CString::new(err.to_string())
                    .unwrap_or_else(|_| CString::new("invalid pinyin syllable").unwrap());
                unsafe { ffi::duckdb_cast_function_set_error(info, message.as_ptr()) };
                return false;
            }
        }
    }
    true
}

/// `PINYIN -> VARCHAR`.
///
/// # Safety
///
/// Called by DuckDB with vectors of the types registered above.
unsafe extern "C" fn cast_from_pinyin(
    info: ffi::duckdb_function_info,
    count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
) -> bool {
    let _ = info;
    let count = count as usize;
    let data = unsafe { ffi::duckdb_vector_get_data(input) }.cast::<u16>();
    let validity = unsafe { ffi::duckdb_vector_get_validity(input) };

    // Rendering allocates, so hand the strings to DuckDB rather than writing
    // into the output vector's own buffer.
    let mut strings: Vec<Option<CString>> = Vec::with_capacity(count);
    for i in 0..count {
        if !unsafe { ffi::duckdb_validity_row_is_valid(validity, i as ffi::idx_t) } {
            strings.push(None);
            continue;
        }
        let value = unsafe { *data.add(i) };
        strings.push(CString::new(pinyin::render(value)).ok());
    }

    // The validity pointer has to be taken *after* making it writable: a vector
    // whose rows are all valid starts with a null mask.
    unsafe { ffi::duckdb_vector_ensure_validity_writable(output) };
    let out_validity = unsafe { ffi::duckdb_vector_get_validity(output) };

    for (i, string) in strings.iter().enumerate() {
        match string {
            Some(string) => unsafe {
                ffi::duckdb_vector_assign_string_element(output, i as ffi::idx_t, string.as_ptr())
            },
            None => unsafe {
                ffi::duckdb_validity_set_row_validity(out_validity, i as ffi::idx_t, false)
            },
        }
    }
    true
}

// --- entrypoint -------------------------------------------------------------

/// # Safety
///
/// Called by DuckDB. `info` and `access` must be the handles DuckDB passes to
/// the extension's C entrypoint.
unsafe fn extension_entrypoint(
    info: ffi::duckdb_extension_info,
    access: *const ffi::duckdb_extension_access,
) -> std::result::Result<bool, Box<dyn Error>> {
    unsafe {
        let have_api_struct = ffi::duckdb_rs_extension_api_init(
            info,
            access,
            MIN_DUCKDB_VERSION,
        )?;
        if !have_api_struct {
            // The API version did not match, so there is nothing we can do.
            return Ok(false);
        }

        let get_database = (*access)
            .get_database
            .ok_or("get_database function pointer is null in duckdb_extension_access")?;
        let db_ptr = get_database(info);
        if db_ptr.is_null() {
            // DuckDB already has the real reason for returning a null database.
            return Ok(false);
        }
        let db: ffi::duckdb_database = *db_ptr;

        register_pinyin_type(db)?;

        Ok(true)
    }
}

/// # Safety
///
/// The entrypoint DuckDB calls when loading this extension.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pinyin_init_c_api(
    info: ffi::duckdb_extension_info,
    access: *const ffi::duckdb_extension_access,
) -> bool {
    unsafe {
        match extension_entrypoint(info, access) {
            Ok(v) => v,
            Err(x) => {
                if let Some(set_error_fn) = (*access).set_error {
                    match CString::new(x.to_string()) {
                        Ok(e) => set_error_fn(info, e.as_ptr()),
                        Err(_e) => set_error_fn(
                            info,
                            c"Extension initialization failed, but the error message could not be converted to a C string".as_ptr(),
                        ),
                    }
                }
                false
            }
        }
    }
}
