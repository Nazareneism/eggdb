use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::format::{format_ratio, format_size, format_timestamp};

pub fn run(egg: &Path) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let data = std::fs::read(egg).with_context(|| format!("failed to read {}", egg.display()))?;

    let reader = eggdb_core::EggReader::open(&data)
        .with_context(|| format!("failed to parse {}", egg.display()))?;

    let header = reader.header();
    let tables = reader.tables();
    let egg_size = data.len() as u64;

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
        for table in tables {
            println!(
                "  {} {} ({} rows, {} columns)",
                "•".green(),
                table.table_name.bold(),
                table.row_count,
                table.columns.len()
            );
        }
    }

    Ok(())
}
