type SqliteExtensionEntry = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut std::ffi::c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> std::ffi::c_int;

/// Registers the sqlite-vec extension once per process.
pub(super) fn register_sqlite_vec() -> rusqlite::Result<()> {
    static RESULT: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    let code = *RESULT.get_or_init(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            SqliteExtensionEntry,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )))
    });
    if code == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            Some("failed to register sqlite-vec".to_string()),
        ))
    }
}
