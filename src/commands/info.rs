use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use comfy_table::{Cell, ContentArrangement, Table};

use crate::format::{format_ratio, format_size, truncate_display};

pub fn run(egg: &Path, table_name: Option<&str>) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let data = std::fs::read(egg).with_context(|| format!("failed to read {}", egg.display()))?;

    let reader = eggdb_core::EggReader::open(&data)
        .with_context(|| format!("failed to parse {}", egg.display()))?;

    let tables = reader.tables();

    match table_name {
        Some(name) => {
            let table = tables
                .iter()
                .find(|t| t.table_name == name)
                .ok_or_else(|| anyhow::anyhow!("table '{}' not found", name))?;

            print_table_detail(table);
        }
        None => {
            print_overview(tables);
        }
    }

    Ok(())
}

fn print_overview(tables: &[eggdb_core::TableMeta]) {
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
    ]);

    for table in tables {
        let compressed: u64 = table.columns.iter().map(|c| c.yolk_length).sum();
        let uncompressed: u64 = table.columns.iter().map(|c| c.uncompressed_size).sum();

        tbl.add_row(vec![
            Cell::new(&table.table_name),
            Cell::new(table.row_count),
            Cell::new(table.columns.len()),
            Cell::new(format_size(compressed)),
            Cell::new(format_size(uncompressed)),
            Cell::new(format_ratio(uncompressed, compressed)),
        ]);
    }

    println!("{tbl}");
}

fn print_table_detail(table: &eggdb_core::TableMeta) {
    println!(
        "{} {} ({} rows)",
        "Table:".bold(),
        table.table_name.bold(),
        table.row_count
    );
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
