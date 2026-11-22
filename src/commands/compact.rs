use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use colored::Colorize;
use comfy_table::{Cell, ContentArrangement, Table};

use eggdb_store::{compact, CompactOptions, CompactOutcome, CompactReport};

use crate::commands::pending::describe;
use crate::format::{format_duration, format_size};

/// Fold pending changes from the write log into a new main file.
pub fn run(egg: &Path, zstd_level: i32) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());
    let start = Instant::now();
    let outcome = compact(egg, &CompactOptions { zstd_level })
        .with_context(|| format!("failed to compact {}", egg.display()))?;
    match outcome {
        CompactOutcome::NothingPending => println!(
            "  {} {} has no pending changes; nothing to compact",
            "Done:".green().bold(),
            egg.display()
        ),
        CompactOutcome::Compacted(report) => print_report(egg, &report, start),
    }
    Ok(())
}

fn print_report(egg: &Path, report: &CompactReport, start: Instant) {
    let mut tbl = Table::new();
    tbl.set_content_arrangement(ContentArrangement::Dynamic);
    tbl.set_header(vec![
        "Table",
        "Rows before",
        "Rows after",
        "Changes folded in",
        "Blocks",
    ]);
    for t in &report.tables {
        tbl.add_row(vec![
            Cell::new(&t.table),
            Cell::new(t.rows_before),
            Cell::new(t.rows_after),
            Cell::new(describe(&t.changes).unwrap_or_else(|| "none".into())),
            Cell::new(if t.rewritten { "re-encoded" } else { "copied" }),
        ]);
    }
    println!("{tbl}");
    println!();
    println!("  {} {}", "Output:".bold(), egg.display());
    println!(
        "  {} {} -> {}",
        "Size:".bold(),
        format_size(report.size_before),
        format_size(report.size_after)
    );
    println!("  {} {}", "Time:".bold(), format_duration(start.elapsed()));
    println!(
        "  {} the write log was folded in and removed",
        "Done:".green().bold()
    );
}
