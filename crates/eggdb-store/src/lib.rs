//! Write support for `.egg` files: snapshots that merge the immutable main
//! file with its write log, an exclusive crash-safe log writer, and
//! compaction that folds the log into a new main file.
//!
//! Depends on `eggdb-core` and `std::fs` only. See `docs/v0.3_plan.md`.

pub mod compact;
pub mod error;
pub mod fsutil;
pub mod snapshot;
pub mod writer;

pub use compact::{compact, CompactOptions, CompactOutcome, CompactReport, TableCompaction};
pub use error::StoreError;
pub use fsutil::{compact_tmp_path, ensure_no_pending_log, wal_path};
pub use snapshot::{EggSnapshot, LogState};
pub use writer::WalWriter;
