use std::path::PathBuf;

use eggdb_core::FormatError;
use thiserror::Error;

/// Errors from opening, writing, or compacting an `.egg` file and its write log.
#[derive(Debug, Error)]
pub enum StoreError {
    /// A filesystem operation failed.
    #[error("{action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The main file or the write log is malformed.
    #[error("{}: {source}", path.display())]
    Format {
        path: PathBuf,
        #[source]
        source: FormatError,
    },

    /// The write log was made for a different version of the main file.
    #[error(
        "{} does not belong to {} (the .egg was replaced after these changes were written). \
         Pending changes were NOT applied. Restore the original .egg, or move the .wal file \
         aside to discard its changes.",
        wal.display(),
        egg.display()
    )]
    ForeignLog { egg: PathBuf, wal: PathBuf },

    /// Another writer holds the write lock.
    #[error("{} is being written by another process; try again when it finishes", egg.display())]
    Busy { egg: PathBuf },

    /// The main file kept changing while it was being read.
    #[error("{} changed repeatedly while being read; try again", egg.display())]
    Unstable { egg: PathBuf },

    /// The main file on disk is not the one this handle was opened on.
    #[error(
        "{} was replaced (for example by `egg compact`) since it was opened; reopen it and retry",
        egg.display()
    )]
    Stale { egg: PathBuf },

    /// Writing to encrypted eggs is not supported yet.
    #[error(
        "{} is encrypted; writing to encrypted eggs is not supported yet \
         (hatch, edit the SQLite file, and lay again with --encrypt)",
        egg.display()
    )]
    EncryptedWrite { egg: PathBuf },

    /// An integrity check failed, so the operation refused to continue.
    #[error("{} failed integrity check '{check}': {detail}", egg.display())]
    Integrity {
        egg: PathBuf,
        check: String,
        detail: String,
    },

    /// An output path already has a write log beside it.
    #[error(
        "{} has pending changes in {}; run `egg compact` first, or move the .wal file aside",
        egg.display(),
        wal.display()
    )]
    PendingLog { egg: PathBuf, wal: PathBuf },
}

impl StoreError {
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn format(path: impl Into<PathBuf>, source: FormatError) -> Self {
        Self::Format {
            path: path.into(),
            source,
        }
    }
}
