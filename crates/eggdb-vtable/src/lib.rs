pub mod args;
pub mod column_cache;
pub mod constraints;
pub mod error;
pub mod shared;
pub mod vtab;

pub use vtab::{register_egg_module, EggTab};

#[cfg(feature = "loadable_extension")]
mod extension {
    use std::os::raw::{c_char, c_int};

    use rusqlite::{ffi, Connection};

    use crate::register_egg_module;

    /// Entry point for loading as a SQLite extension.
    ///
    /// SQLite looks for `sqlite3_<filename>_init`. For `libeggvt.so`/`libeggvt.dylib`,
    /// that's `sqlite3_eggvt_init`.
    ///
    /// # Safety
    ///
    /// This function is called by SQLite's extension loading mechanism. The pointers
    /// are valid as long as SQLite is loaded and the extension is in use.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    #[no_mangle]
    pub unsafe extern "C" fn sqlite3_eggvt_init(
        db: *mut ffi::sqlite3,
        pz_err_msg: *mut *mut c_char,
        p_api: *mut ffi::sqlite3_api_routines,
    ) -> c_int {
        // Safety (FFI): all three pointers come straight from SQLite's
        // extension loader. rusqlite rejects a null `p_api`, wraps `db`
        // without taking ownership, and reports errors via `pz_err_msg`.
        unsafe { Connection::extension_init2(db, pz_err_msg, p_api, extension_init) }
    }

    /// Entry point under the name SQLite derives from the built file name
    /// (`libeggdb_vtable.dylib` / `.so` → `sqlite3_eggdbvtable_init`), so
    /// `.load target/release/libeggdb_vtable` works without naming an entry point.
    ///
    /// # Safety
    ///
    /// Same contract as [`sqlite3_eggvt_init`].
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    #[no_mangle]
    pub unsafe extern "C" fn sqlite3_eggdbvtable_init(
        db: *mut ffi::sqlite3,
        pz_err_msg: *mut *mut c_char,
        p_api: *mut ffi::sqlite3_api_routines,
    ) -> c_int {
        // Safety (FFI): forwards SQLite's own pointers unchanged.
        unsafe { sqlite3_eggvt_init(db, pz_err_msg, p_api) }
    }

    /// Register the module. `Ok(false)`: not a persistent extension, so it is
    /// released together with the connection that loaded it.
    fn extension_init(conn: Connection) -> rusqlite::Result<bool> {
        register_egg_module(&conn)?;
        Ok(false)
    }
}
