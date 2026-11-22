use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use eggdb_store::EggSnapshot;

use crate::commands::pending::open_snapshot;
use crate::format::{format_ratio, format_size};

/// Live row count (main file plus pending changes) per table name.
fn live_rows_by_name(snapshot: &EggSnapshot) -> HashMap<&str, u64> {
    snapshot
        .tables()
        .iter()
        .enumerate()
        .map(|(i, t)| (t.table_name.as_str(), snapshot.live_row_count(i)))
        .collect()
}

pub fn run(a_path: &Path, b_path: &Path) -> Result<()> {
    anyhow::ensure!(a_path.exists(), "egg file not found: {}", a_path.display());
    anyhow::ensure!(b_path.exists(), "egg file not found: {}", b_path.display());

    // Row counts include pending write-log changes; encodings and sizes
    // describe each main file.
    let a_reader = open_snapshot(a_path, None)?;
    let b_reader = open_snapshot(b_path, None)?;
    let a_data = a_reader.data();
    let b_data = b_reader.data();
    let a_rows = live_rows_by_name(&a_reader);
    let b_rows = live_rows_by_name(&b_reader);

    let mut differences = 0u64;

    // --- Header comparison ---
    println!("{}", "Header".bold().underline());
    println!();

    let a_header = a_reader.header();
    let b_header = b_reader.header();

    if a_header.version != b_header.version {
        differences += 1;
        println!(
            "  {} version: {} → {}",
            "~".yellow(),
            a_header.version,
            b_header.version
        );
    }

    if a_header.table_count != b_header.table_count {
        differences += 1;
        println!(
            "  {} table count: {} → {}",
            "~".yellow(),
            a_header.table_count,
            b_header.table_count
        );
    }

    if a_header.flags.to_raw() != b_header.flags.to_raw() {
        differences += 1;
        println!(
            "  {} flags: 0x{:04x} → 0x{:04x}",
            "~".yellow(),
            a_header.flags.to_raw(),
            b_header.flags.to_raw()
        );
    }

    if a_header.original_size != b_header.original_size {
        differences += 1;
        println!(
            "  {} original size: {} → {}",
            "~".yellow(),
            format_size(a_header.original_size),
            format_size(b_header.original_size)
        );
    }

    if differences == 0 {
        println!("  {} headers match", "✓".green());
    }

    // --- Table matching ---
    let a_tables: HashMap<&str, _> = a_reader
        .tables()
        .iter()
        .map(|t| (t.table_name.as_str(), t))
        .collect();
    let b_tables: HashMap<&str, _> = b_reader
        .tables()
        .iter()
        .map(|t| (t.table_name.as_str(), t))
        .collect();

    // Tables only in A
    let mut only_a: Vec<&str> = a_tables
        .keys()
        .filter(|k| !b_tables.contains_key(*k))
        .copied()
        .collect();
    only_a.sort();

    // Tables only in B
    let mut only_b: Vec<&str> = b_tables
        .keys()
        .filter(|k| !a_tables.contains_key(*k))
        .copied()
        .collect();
    only_b.sort();

    // Tables in both
    let mut common: Vec<&str> = a_tables
        .keys()
        .filter(|k| b_tables.contains_key(*k))
        .copied()
        .collect();
    common.sort();

    if !only_a.is_empty() || !only_b.is_empty() || !common.is_empty() {
        println!();
        println!("{}", "Tables".bold().underline());
        println!();
    }

    for name in &only_a {
        differences += 1;
        let t = a_tables[name];
        println!(
            "  {} {} (only in A, {} rows, {} columns)",
            "-".red(),
            name.bold(),
            a_rows[name],
            t.columns.len()
        );
    }

    for name in &only_b {
        differences += 1;
        let t = b_tables[name];
        println!(
            "  {} {} (only in B, {} rows, {} columns)",
            "+".green(),
            name.bold(),
            b_rows[name],
            t.columns.len()
        );
    }

    // --- Per-table comparison for common tables ---
    for name in &common {
        let at = a_tables[name];
        let bt = b_tables[name];
        let mut table_diffs = 0u64;
        let mut table_lines: Vec<String> = Vec::new();

        // Row count
        if a_rows[name] != b_rows[name] {
            table_diffs += 1;
            table_lines.push(format!(
                "    {} row count: {} → {}",
                "~".yellow(),
                a_rows[name],
                b_rows[name]
            ));
        }

        // DDL
        if at.original_ddl != bt.original_ddl {
            table_diffs += 1;
            table_lines.push(format!("    {} DDL changed", "~".yellow()));
        }

        // Index definitions
        if at.index_definitions != bt.index_definitions {
            table_diffs += 1;
            table_lines.push(format!("    {} index definitions changed", "~".yellow()));
        }

        // Column matching
        let a_cols: HashMap<&str, _> = at.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        let b_cols: HashMap<&str, _> = bt.columns.iter().map(|c| (c.name.as_str(), c)).collect();

        let mut cols_only_a: Vec<&str> = a_cols
            .keys()
            .filter(|k| !b_cols.contains_key(*k))
            .copied()
            .collect();
        cols_only_a.sort();

        let mut cols_only_b: Vec<&str> = b_cols
            .keys()
            .filter(|k| !a_cols.contains_key(*k))
            .copied()
            .collect();
        cols_only_b.sort();

        let mut cols_common: Vec<&str> = a_cols
            .keys()
            .filter(|k| b_cols.contains_key(*k))
            .copied()
            .collect();
        cols_common.sort();

        for col_name in &cols_only_a {
            table_diffs += 1;
            table_lines.push(format!(
                "    {} column {} (only in A)",
                "-".red(),
                col_name.bold()
            ));
        }

        for col_name in &cols_only_b {
            table_diffs += 1;
            table_lines.push(format!(
                "    {} column {} (only in B)",
                "+".green(),
                col_name.bold()
            ));
        }

        for col_name in &cols_common {
            let ac = a_cols[col_name];
            let bc = b_cols[col_name];

            if ac.encoding != bc.encoding {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.encoding: {} → {}",
                    "~".yellow(),
                    col_name,
                    ac.encoding,
                    bc.encoding
                ));
            }

            if ac.sqlite_type != bc.sqlite_type {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.type: {:?} → {:?}",
                    "~".yellow(),
                    col_name,
                    ac.sqlite_type,
                    bc.sqlite_type
                ));
            }

            if ac.cardinality != bc.cardinality {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.cardinality: {} → {}",
                    "~".yellow(),
                    col_name,
                    ac.cardinality,
                    bc.cardinality
                ));
            }

            if ac.null_count != bc.null_count {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.null_count: {} → {}",
                    "~".yellow(),
                    col_name,
                    ac.null_count,
                    bc.null_count
                ));
            }

            if ac.yolk_length != bc.yolk_length {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.compressed_size: {} → {}",
                    "~".yellow(),
                    col_name,
                    format_size(ac.yolk_length),
                    format_size(bc.yolk_length)
                ));
            }

            if ac.uncompressed_size != bc.uncompressed_size {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.uncompressed_size: {} → {}",
                    "~".yellow(),
                    col_name,
                    format_size(ac.uncompressed_size),
                    format_size(bc.uncompressed_size)
                ));
            }

            if ac.min_value != bc.min_value {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.min_value changed",
                    "~".yellow(),
                    col_name
                ));
            }

            if ac.max_value != bc.max_value {
                table_diffs += 1;
                table_lines.push(format!(
                    "    {} {}.max_value changed",
                    "~".yellow(),
                    col_name
                ));
            }
        }

        if table_diffs == 0 {
            println!("  {} {} (identical)", "✓".green(), name.bold());
        } else {
            differences += table_diffs;
            println!(
                "  {} {} ({} differences)",
                "~".yellow(),
                name.bold(),
                table_diffs
            );
            for line in &table_lines {
                println!("{line}");
            }
        }
    }

    // --- Size comparison ---
    let a_size = a_data.len() as u64;
    let b_size = b_data.len() as u64;

    println!();
    println!("{}", "Size".bold().underline());
    println!();
    println!("  {} A: {}", "•".dimmed(), format_size(a_size));
    println!("  {} B: {}", "•".dimmed(), format_size(b_size));

    if a_size != b_size {
        let pct = if a_size > 0 {
            let delta = b_size as f64 - a_size as f64;
            let pct_val = (delta / a_size as f64) * 100.0;
            if pct_val >= 0.0 {
                format!("+{pct_val:.1}%")
            } else {
                format!("{pct_val:.1}%")
            }
        } else {
            "N/A".into()
        };
        println!("  {} change: {}", "~".yellow(), pct);
    } else {
        println!("  {} same size", "✓".green());
    }

    if a_header.original_size > 0 {
        println!(
            "  {} A compression: {}",
            "•".dimmed(),
            format_ratio(a_header.original_size, a_size)
        );
    }
    if b_header.original_size > 0 {
        println!(
            "  {} B compression: {}",
            "•".dimmed(),
            format_ratio(b_header.original_size, b_size)
        );
    }

    // --- Summary ---
    println!();
    if differences == 0 {
        println!("{}", "No differences found.".green().bold());
    } else {
        println!(
            "{} found.",
            format!(
                "{differences} difference{}",
                if differences == 1 { "" } else { "s" }
            )
            .yellow()
            .bold()
        );
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use bitvec::prelude::*;
    use eggdb_core::encoding::analyzer::{analyze_column, select_encoding};
    use eggdb_core::encoding::null_bitmap;
    use eggdb_core::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
    use eggdb_core::value::SqliteType;
    use eggdb_core::{encoder_for, Encoding, Value};

    /// Helper: create a ColumnInput from values using the full encode pipeline.
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

    fn make_test_egg(tables: &[TableInput]) -> Vec<u8> {
        write_egg(tables, &WriteOptions::default()).unwrap()
    }

    fn make_simple_egg() -> Vec<u8> {
        let values: Vec<Value> = (0..10).map(Value::Integer).collect();
        let col = make_column("id", SqliteType::Integer, &values);
        let table = TableInput {
            table_name: "users".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE users (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };
        make_test_egg(&[table])
    }

    /// Self-diff should find no differences.
    #[test]
    fn self_diff_no_differences() {
        let egg = make_simple_egg();
        let a = eggdb_core::EggReader::open(&egg).unwrap();
        let b = eggdb_core::EggReader::open(&egg).unwrap();

        // Verify headers match
        assert_eq!(a.header().version, b.header().version);
        assert_eq!(a.header().table_count, b.header().table_count);
        assert_eq!(a.tables().len(), b.tables().len());

        for (at, bt) in a.tables().iter().zip(b.tables().iter()) {
            assert_eq!(at.table_name, bt.table_name);
            assert_eq!(at.row_count, bt.row_count);
            assert_eq!(at.columns.len(), bt.columns.len());
            for (ac, bc) in at.columns.iter().zip(bt.columns.iter()) {
                assert_eq!(ac.name, bc.name);
                assert_eq!(ac.encoding, bc.encoding);
                assert_eq!(ac.cardinality, bc.cardinality);
            }
        }
    }

    /// Different row counts should be detected.
    #[test]
    fn different_row_counts() {
        let vals_a: Vec<Value> = (0..10).map(Value::Integer).collect();
        let vals_b: Vec<Value> = (0..20).map(Value::Integer).collect();

        let egg_a = make_test_egg(&[TableInput {
            table_name: "t".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![make_column("id", SqliteType::Integer, &vals_a)],
            index_definitions: Vec::new(),
        }]);
        let egg_b = make_test_egg(&[TableInput {
            table_name: "t".into(),
            row_count: 20,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![make_column("id", SqliteType::Integer, &vals_b)],
            index_definitions: Vec::new(),
        }]);

        let a = eggdb_core::EggReader::open(&egg_a).unwrap();
        let b = eggdb_core::EggReader::open(&egg_b).unwrap();
        assert_ne!(a.tables()[0].row_count, b.tables()[0].row_count);
    }

    /// Different encodings should be detected.
    #[test]
    fn different_encodings() {
        // Manually force different encodings for the same data
        let values = vec![Value::Integer(42); 10];
        let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
        let non_null = null_bitmap::extract_non_null(&values, &null_bitmap);

        // Egg A: Const encoding
        let encoder_a = encoder_for(Encoding::Const);
        let encoded_a = encoder_a.encode(&non_null);
        let col_a = ColumnInput {
            name: "v".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Const,
            cardinality: 1,
            null_count: 0,
            null_bitmap: null_bitmap.clone(),
            encoded_values: encoded_a,
            dictionary: None,
            min_value: Some(Value::Integer(42)),
            max_value: Some(Value::Integer(42)),
            pre_compressed: None,
        };

        // Egg B: Raw encoding (same data, different encoding)
        let encoder_b = encoder_for(Encoding::Raw);
        let encoded_b = encoder_b.encode(&non_null);
        let col_b = ColumnInput {
            name: "v".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Raw,
            cardinality: 1,
            null_count: 0,
            null_bitmap,
            encoded_values: encoded_b,
            dictionary: None,
            min_value: Some(Value::Integer(42)),
            max_value: Some(Value::Integer(42)),
            pre_compressed: None,
        };

        let egg_a = make_test_egg(&[TableInput {
            table_name: "t".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE t (v INTEGER)".into(),
            columns: vec![col_a],
            index_definitions: Vec::new(),
        }]);
        let egg_b = make_test_egg(&[TableInput {
            table_name: "t".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE t (v INTEGER)".into(),
            columns: vec![col_b],
            index_definitions: Vec::new(),
        }]);

        let a = eggdb_core::EggReader::open(&egg_a).unwrap();
        let b = eggdb_core::EggReader::open(&egg_b).unwrap();
        assert_ne!(
            a.tables()[0].columns[0].encoding,
            b.tables()[0].columns[0].encoding
        );
    }

    /// Added/removed tables should be detected.
    #[test]
    fn added_removed_tables() {
        let values: Vec<Value> = (0..5).map(Value::Integer).collect();

        let egg_a = make_test_egg(&[TableInput {
            table_name: "alpha".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE alpha (id INTEGER)".into(),
            columns: vec![make_column("id", SqliteType::Integer, &values)],
            index_definitions: Vec::new(),
        }]);

        let egg_b = make_test_egg(&[
            TableInput {
                table_name: "alpha".into(),
                row_count: 5,
                original_ddl: "CREATE TABLE alpha (id INTEGER)".into(),
                columns: vec![make_column("id", SqliteType::Integer, &values)],
                index_definitions: Vec::new(),
            },
            TableInput {
                table_name: "beta".into(),
                row_count: 5,
                original_ddl: "CREATE TABLE beta (id INTEGER)".into(),
                columns: vec![make_column("id", SqliteType::Integer, &values)],
                index_definitions: Vec::new(),
            },
        ]);

        let a = eggdb_core::EggReader::open(&egg_a).unwrap();
        let b = eggdb_core::EggReader::open(&egg_b).unwrap();
        assert_eq!(a.table_count(), 1);
        assert_eq!(b.table_count(), 2);
    }

    /// Added/removed columns should be detected.
    #[test]
    fn added_removed_columns() {
        let values: Vec<Value> = (0..5).map(Value::Integer).collect();
        let text_values: Vec<Value> = (0..5).map(|i| Value::Text(format!("v{i}"))).collect();

        let egg_a = make_test_egg(&[TableInput {
            table_name: "t".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![make_column("id", SqliteType::Integer, &values)],
            index_definitions: Vec::new(),
        }]);

        let egg_b = make_test_egg(&[TableInput {
            table_name: "t".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE t (id INTEGER, name TEXT)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &values),
                make_column("name", SqliteType::Text, &text_values),
            ],
            index_definitions: Vec::new(),
        }]);

        let a = eggdb_core::EggReader::open(&egg_a).unwrap();
        let b = eggdb_core::EggReader::open(&egg_b).unwrap();
        assert_eq!(a.tables()[0].columns.len(), 1);
        assert_eq!(b.tables()[0].columns.len(), 2);
    }
}
