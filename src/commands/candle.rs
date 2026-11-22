use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use crate::commands::pending::{describe, open_snapshot, print_pending};
use crate::format::{format_ratio, format_size, format_timestamp};

pub fn run(egg: &Path) -> Result<()> {
    let snapshot = open_snapshot(egg, None)?;
    let header = snapshot.header();
    let tables = snapshot.tables();
    let egg_size = snapshot.data().len() as u64;

    println!("{}", "Egg Summary".bold().underline());
    println!();
    println!("  {} {}", "File:".bold(), egg.display());
    println!("  {} {}", "Version:".bold(), header.version);
    println!("  {} {}", "Tables:".bold(), header.table_count);
    println!(
        "  {} {}",
        "Created:".bold(),
        format_timestamp(header.created_at)
    );
    println!(
        "  {} {}",
        "Original size:".bold(),
        format_size(header.original_size)
    );
    println!("  {} {}", "Egg size:".bold(), format_size(egg_size));
    println!(
        "  {} {}",
        "Compression:".bold(),
        format_ratio(header.original_size, egg_size)
    );
    println!(
        "  {} {}",
        "Encrypted:".bold(),
        if header.flags.has_encryption() {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "  {} {}",
        "Row order:".bold(),
        if header.flags.row_order_preserved() {
            "preserved"
        } else {
            "optimized"
        }
    );

    if !tables.is_empty() {
        println!();
        println!("{}", "Tables".bold().underline());
        println!();
        for (t, table) in tables.iter().enumerate() {
            let pending = describe(&snapshot.overlays()[t].pending_counts())
                .map(|d| format!(", pending: {d}").yellow().to_string())
                .unwrap_or_default();
            println!(
                "  {} {} ({} rows, {} columns{})",
                "•".green(),
                table.table_name.bold(),
                snapshot.live_row_count(t),
                table.columns.len(),
                pending
            );
        }
    }
    print_pending(&snapshot);

    Ok(())
}
