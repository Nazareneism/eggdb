//! Shared CLI helpers for eggs that may have pending changes in a write log.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use eggdb_core::PendingCounts;
use eggdb_store::{wal_path, EggSnapshot, LogState};

/// Open an egg together with its write log.
pub fn open_snapshot(egg: &Path, passphrase: Option<&str>) -> Result<EggSnapshot> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());
    EggSnapshot::open(egg, passphrase).with_context(|| format!("failed to open {}", egg.display()))
}

/// Pending changes summed over every table.
pub fn total_pending(snapshot: &EggSnapshot) -> PendingCounts {
    snapshot.overlays().iter().map(|o| o.pending_counts()).fold(
        PendingCounts::default(),
        |acc, c| PendingCounts {
            inserted: acc.inserted + c.inserted,
            updated: acc.updated + c.updated,
            deleted: acc.deleted + c.deleted,
        },
    )
}

/// "2 inserted, 1 updated, 3 deleted", or `None` when nothing is pending.
pub fn describe(counts: &PendingCounts) -> Option<String> {
    (!counts.is_empty()).then(|| {
        format!(
            "{} inserted, {} updated, {} deleted",
            counts.inserted, counts.updated, counts.deleted
        )
    })
}

/// Print the pending-changes block (nothing when there are none).
pub fn print_pending(snapshot: &EggSnapshot) {
    if let LogState::Active {
        torn_tail_bytes, ..
    } = snapshot.log_state()
    {
        if *torn_tail_bytes > 0 {
            println!(
                "  {} {} bytes from an interrupted write (e.g. a crash) will be discarded",
                "Note:".yellow(),
                torn_tail_bytes
            );
        }
    }
    let Some(summary) = describe(&total_pending(snapshot)) else {
        return;
    };
    let wal = wal_path(snapshot.egg_path());
    println!();
    println!(
        "  {} {} (stored in {})",
        "Pending changes:".yellow().bold(),
        summary,
        wal.file_name().unwrap_or_default().to_string_lossy()
    );
    println!(
        "  {}",
        format!(
            "Row counts include them. Run `egg compact {}` to fold them into the main file.",
            snapshot.egg_path().display()
        )
        .dimmed()
    );
}
