use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use comfy_table::{Cell, ContentArrangement, Table};

use eggdb_store::EggSnapshot;

use crate::commands::pending::{describe, open_snapshot, print_pending};
use crate::format::{format_ratio, format_size, truncate_display};

pub fn run(egg: &Path, table_name: Option<&str>) -> Result<()> {
    let snapshot = open_snapshot(egg, None)?;

    match table_name {
        Some(name) => {
            let t = snapshot
                .table_index(name)
                .ok_or_else(|| anyhow::anyhow!("table '{}' not found", name))?;
            print_table_detail(&snapshot, t);
        }
        None => {
            print_overview(&snapshot);
        }
    }
    print_pending(&snapshot);

    Ok(())
}

fn print_overview(snapshot: &EggSnapshot) {
    println!("{}", "Table Overview".bold().underline());
    println!();

    let mut tbl = Table::new();
    tbl.set_content_arrangement(ContentArrangement::Dynamic);
    tbl.set_header(vec![
        Cell::new("Table"),
        Cell::new("Rows"),
        Cell::new("Columns"),
        Cell::new("Compressed"),
        Cell::new("Uncompressed"),
        Cell::new("Ratio"),
        Cell::new("Pending changes"),
    ]);

    for (t, table) in snapshot.tables().iter().enumerate() {
        let compressed: u64 = table.columns.iter().map(|c| c.yolk_length).sum();
        let uncompressed: u64 = table.columns.iter().map(|c| c.uncompressed_size).sum();
        let pending = describe(&snapshot.overlays()[t].pending_counts());

        tbl.add_row(vec![
            Cell::new(&table.table_name),
            Cell::new(snapshot.live_row_count(t)),
            Cell::new(table.columns.len()),
            Cell::new(format_size(compressed)),
            Cell::new(format_size(uncompressed)),
            Cell::new(format_ratio(uncompressed, compressed)),
            Cell::new(pending.unwrap_or_else(|| "-".into())),
        ]);
    }

    println!("{tbl}");
}

fn print_table_detail(snapshot: &EggSnapshot, t: usize) {
    let table = &snapshot.tables()[t];
    println!(
        "{} {} ({} rows)",
        "Table:".bold(),
        table.table_name.bold(),
        snapshot.live_row_count(t)
    );
    if !snapshot.overlays()[t].is_empty() {
        println!(
            "{}",
            "Column statistics below describe the main file; pending changes are not included."
                .dimmed()
        );
    }
    println!();

    let mut tbl = Table::new();
    tbl.set_content_arrangement(ContentArrangement::Dynamic);
    tbl.set_header(vec![
        Cell::new("Column"),
        Cell::new("Type"),
        Cell::new("Encoding"),
        Cell::new("Cardinality"),
        Cell::new("Nulls"),
        Cell::new("Compressed"),
        Cell::new("Uncompressed"),
        Cell::new("Ratio"),
        Cell::new("Min"),
        Cell::new("Max"),
    ]);

    for col in &table.columns {
        let null_pct = if table.row_count > 0 {
            format!(
                "{} ({:.1}%)",
                col.null_count,
                col.null_count as f64 / table.row_count as f64 * 100.0
            )
        } else {
            "0".into()
        };

        let min_str = col
            .min_value
            .as_ref()
            .map(|v| truncate_display(&v.to_string(), 20))
            .unwrap_or_else(|| "N/A".into());
        let max_str = col
            .max_value
            .as_ref()
            .map(|v| truncate_display(&v.to_string(), 20))
            .unwrap_or_else(|| "N/A".into());

        tbl.add_row(vec![
            Cell::new(&col.name),
            Cell::new(col.sqlite_type),
            Cell::new(col.encoding),
            Cell::new(col.cardinality),
            Cell::new(null_pct),
            Cell::new(format_size(col.yolk_length)),
            Cell::new(format_size(col.uncompressed_size)),
            Cell::new(format_ratio(col.uncompressed_size, col.yolk_length)),
            Cell::new(min_str),
            Cell::new(max_str),
        ]);
    }

    println!("{tbl}");
}
