pub mod column_cache;
pub mod error;
pub mod vtab;

pub use vtab::{register_egg_module, EggTab};

#[cfg(feature = "loadable_extension")]
mod extension {
    use std::os::raw::{c_char, c_int};

    use rusqlite::ffi;

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
        if p_api.is_null() {
            return ffi::SQLITE_ERROR;
        }

        match extension_init(db, p_api) {
            Ok(()) => ffi::SQLITE_OK,
            Err(e) => unsafe { rusqlite::to_sqlite_error(&e, pz_err_msg) },
        }
    }

    fn extension_init(
        db: *mut ffi::sqlite3,
        p_api: *mut ffi::sqlite3_api_routines,
    ) -> rusqlite::Result<()> {
        // Safety: p_api is checked for null above, db is provided by SQLite.
        let conn = unsafe { rusqlite::Connection::extension_init2(db, p_api)? };
        register_egg_module(&conn)?;
        Ok(())
    }
}
