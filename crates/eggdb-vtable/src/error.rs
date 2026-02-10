use thiserror::Error;

/// Errors specific to the egg virtual table.
#[derive(Debug, Error)]
pub enum VtableError {
    #[error("wrong number of arguments: expected 2 (file path and table name), got {0}")]
    WrongArgCount(usize),

    #[error("table '{0}' not found in egg file")]
    TableNotFound(String),

    #[error("failed to read egg file '{path}': {source}")]
    FileRead {
        path: String,
        source: std::io::Error,
    },

    #[error("egg format error: {0}")]
    Format(#[from] eggdb_core::FormatError),

    #[error("{0}")]
    Other(String),
}
