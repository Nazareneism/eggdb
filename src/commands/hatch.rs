use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use crate::format::{format_duration, format_size};

pub fn run(
    egg: &Path,
    output: Option<&Path>,
    force: bool,
    tables: Option<Vec<String>>,
    passphrase: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let default_output = egg.with_extension("sqlite");
    let output = output.unwrap_or(&default_output);

    if output.exists() && !force {
        anyhow::bail!(
            "output file already exists: {} (use --force to overwrite)",
            output.display()
        );
    }

    // Remove existing output if --force
    if output.exists() && force {
        std::fs::remove_file(output)
            .with_context(|| format!("failed to remove existing file: {}", output.display()))?;
    }

    // Warn about tables not found in the egg file
    if let Some(ref requested) = tables {
        let egg_data =
            std::fs::read(egg).with_context(|| format!("failed to read {}", egg.display()))?;
        let reader = eggdb_core::EggReader::open(&egg_data)
            .with_context(|| format!("failed to parse {}", egg.display()))?;
        let available: Vec<&str> = reader
            .tables()
            .iter()
            .map(|t| t.table_name.as_str())
            .collect();
        for name in requested {
            if !available.iter().any(|a| a == name) {
                eprintln!(
                    "{}: table '{}' not found in egg file (available: {})",
                    "Warning".yellow(),
                    name,
                    available.join(", ")
                );
            }
        }
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
    );
    spinner.set_message(format!(
        "Hatching {} ...",
        egg.file_name().unwrap_or_default().to_string_lossy()
    ));
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let options = eggdb_io::reader::HatchOptions {
        tables,
        passphrase: passphrase.map(String::from),
    };

    let start = Instant::now();
    let result = eggdb_io::reader::hatch_file(egg, output, &options)
        .with_context(|| format!("failed to hatch {}", egg.display()))?;
    let elapsed = start.elapsed();

    spinner.finish_with_message("Done");

    let output_size = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    println!();
    println!("  {} {}", "File:".bold(), output.display());
    println!("  {} {}", "Tables:".bold(), result.table_count);
    println!("  {} {}", "Rows:".bold(), result.total_rows);
    println!("  {} {}", "Size:".bold(), format_size(output_size));
    println!("  {} {}", "Time:".bold(), format_duration(elapsed));

    Ok(())
}

#[cfg(test)]
mod tests {
    use bitvec::prelude::*;
    use eggdb_core::encoding::analyzer::{analyze_column, select_encoding};
    use eggdb_core::encoding::null_bitmap;
    use eggdb_core::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
    use eggdb_core::value::SqliteType;
    use eggdb_core::{encoder_for, Value};

    fn make_column(name: &str, sqlite_type: SqliteType, values: &[Value]) -> ColumnInput {
        let stats = analyze_column(values);
        let encoding = select_encoding(&stats);
        let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
        let non_null = null_bitmap::extract_non_null(values, &null_bitmap);
        let encoder = encoder_for(encoding);
        let encoded = encoder.encode(&non_null);

        ColumnInput {
            name: name.into(),
            sqlite_type,
            encoding,
            cardinality: stats.distinct_count as u64,
            null_count: stats.null_count as u64,
            null_bitmap,
            encoded_values: encoded,
            dictionary: None,
            min_value: stats.min_value,
            max_value: stats.max_value,
            pre_compressed: None,
        }
    }

    fn make_two_table_egg() -> Vec<u8> {
        let ids: Vec<Value> = (0..5).map(Value::Integer).collect();
        let names: Vec<Value> = ["Alice", "Bob", "Carol", "Dave", "Eve"]
            .iter()
            .map(|s| Value::Text(s.to_string()))
            .collect();
        let amounts: Vec<Value> = [100, 200, 300, 400, 500]
            .iter()
            .map(|&v| Value::Integer(v))
            .collect();

        let users = TableInput {
            table_name: "users".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE users (id INTEGER, name TEXT)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &ids),
                make_column("name", SqliteType::Text, &names),
            ],
            index_definitions: Vec::new(),
        };

        let orders = TableInput {
            table_name: "orders".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE orders (id INTEGER, amount INTEGER)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &ids),
                make_column("amount", SqliteType::Integer, &amounts),
            ],
            index_definitions: Vec::new(),
        };

        write_egg(&[users, orders], &WriteOptions::default()).unwrap()
    }

    #[test]
    fn partial_hatch_filters_tables() {
        let egg_data = make_two_table_egg();
        let dir = tempfile::tempdir().unwrap();
        let egg_path = dir.path().join("test.egg");
        let out_path = dir.path().join("test.sqlite");
        std::fs::write(&egg_path, &egg_data).unwrap();

        // Hatch only the "users" table
        let options = eggdb_io::reader::HatchOptions {
            tables: Some(vec!["users".into()]),
            ..Default::default()
        };
        let result = eggdb_io::reader::hatch_file(&egg_path, &out_path, &options).unwrap();

        assert_eq!(result.table_count, 1);
        assert_eq!(result.total_rows, 5);

        // Verify only "users" table exists, not "orders"
        let conn = rusqlite::Connection::open(&out_path).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(tables, vec!["users"]);
    }

    #[test]
    fn partial_hatch_all_tables() {
        let egg_data = make_two_table_egg();
        let dir = tempfile::tempdir().unwrap();
        let egg_path = dir.path().join("test.egg");
        let out_path = dir.path().join("test.sqlite");
        std::fs::write(&egg_path, &egg_data).unwrap();

        // Hatch all tables (None = no filter)
        let options = eggdb_io::reader::HatchOptions {
            tables: None,
            ..Default::default()
        };
        let result = eggdb_io::reader::hatch_file(&egg_path, &out_path, &options).unwrap();

        assert_eq!(result.table_count, 2);
        assert_eq!(result.total_rows, 10);
    }

    #[test]
    fn partial_hatch_nonexistent_table() {
        let egg_data = make_two_table_egg();
        let dir = tempfile::tempdir().unwrap();
        let egg_path = dir.path().join("test.egg");
        let out_path = dir.path().join("test.sqlite");
        std::fs::write(&egg_path, &egg_data).unwrap();

        // Request a table that doesn't exist
        let options = eggdb_io::reader::HatchOptions {
            tables: Some(vec!["nonexistent".into()]),
            ..Default::default()
        };
        let result = eggdb_io::reader::hatch_file(&egg_path, &out_path, &options).unwrap();

        assert_eq!(result.table_count, 0);
        assert_eq!(result.total_rows, 0);
    }
}
