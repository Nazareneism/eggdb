pub mod reader;
pub mod sqlite_utils;
pub mod writer;

/// Errors from SQLite ↔ .egg conversion operations.
#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("format error: {0}")]
    Format(#[from] eggdb_core::FormatError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
