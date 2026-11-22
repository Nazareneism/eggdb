use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};

use eggdb_core::Value;
use eggdb_store::EggSnapshot;

use crate::commands::pending::open_snapshot;

pub fn run(
    egg: &Path,
    format: &str,
    table_name: Option<&str>,
    passphrase: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());
    anyhow::ensure!(
        format == "csv" || format == "json",
        "unsupported format: '{}' (use 'csv' or 'json')",
        format
    );

    let snapshot = open_snapshot(egg, passphrase)?;

    match format {
        "csv" => crack_csv(&snapshot, table_name),
        "json" => crack_json(&snapshot, table_name),
        _ => unreachable!(),
    }
}

fn crack_csv(snapshot: &EggSnapshot, table_filter: Option<&str>) -> Result<()> {
    let tables = snapshot.tables();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for (table_idx, table) in tables.iter().enumerate() {
        if let Some(filter) = table_filter {
            if table.table_name != filter {
                continue;
            }
        }

        let row_count = snapshot.live_row_count(table_idx) as usize;
        let num_cols = table.columns.len();

        // Header row
        let col_names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
        writeln!(out, "{}", col_names.join(","))?;

        if row_count == 0 || num_cols == 0 {
            continue;
        }

        // Decode all columns in parallel
        let columns = snapshot
            .visible_table(table_idx)
            .with_context(|| format!("failed to decode table '{}'", table.table_name))?;

        // Transpose and write rows
        for row_idx in 0..row_count {
            for (col_idx, col) in columns.iter().enumerate() {
                if col_idx > 0 {
                    write!(out, ",")?;
                }
                write!(out, "{}", csv_escape(&col[row_idx]))?;
            }
            writeln!(out)?;
        }

        // Only print the matched table if filter was specified
        if table_filter.is_some() {
            return Ok(());
        }
    }

    if let Some(name) = table_filter {
        if !tables.iter().any(|t| t.table_name == name) {
            anyhow::bail!("table '{}' not found", name);
        }
    }

    Ok(())
}

fn crack_json(snapshot: &EggSnapshot, table_filter: Option<&str>) -> Result<()> {
    let tables = snapshot.tables();
    let mut all_tables = serde_json::Map::new();

    for (table_idx, table) in tables.iter().enumerate() {
        if let Some(filter) = table_filter {
            if table.table_name != filter {
                continue;
            }
        }

        let row_count = snapshot.live_row_count(table_idx) as usize;
        let num_cols = table.columns.len();

        let mut rows_json = Vec::with_capacity(row_count);

        if row_count > 0 && num_cols > 0 {
            // Decode all columns in parallel
            let columns = snapshot
                .visible_table(table_idx)
                .with_context(|| format!("failed to decode table '{}'", table.table_name))?;

            // Transpose to rows
            for row_idx in 0..row_count {
                let mut row_obj = serde_json::Map::new();
                for (col_idx, col) in columns.iter().enumerate() {
                    let col_name = &table.columns[col_idx].name;
                    row_obj.insert(col_name.clone(), value_to_json(&col[row_idx]));
                }
                rows_json.push(serde_json::Value::Object(row_obj));
            }
        }

        all_tables.insert(
            table.table_name.clone(),
            serde_json::Value::Array(rows_json),
        );
    }

    if let Some(name) = table_filter {
        if !tables.iter().any(|t| t.table_name == name) {
            anyhow::bail!("table '{}' not found", name);
        }
    }

    let output = serde_json::json!({ "tables": all_tables });
    let stdout = io::stdout();
    let out = stdout.lock();
    serde_json::to_writer_pretty(out, &output)?;
    println!();

    Ok(())
}

/// Escape a Value for CSV output.
fn csv_escape(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => {
            if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.clone()
            }
        }
        Value::Blob(b) => {
            // Hex-encode blobs for CSV
            let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("x'{hex}'")
        }
    }
}

/// Convert a Value to a serde_json::Value.
fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Real(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Blob(b) => serde_json::Value::String(base64_encode(b)),
    }
}

/// Simple base64 encoding without external dependency.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result
}
