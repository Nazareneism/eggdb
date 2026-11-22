use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use eggdb_core::format::writer::{write_egg, WriteOptions};
use eggdb_core::{encode_table, Header};

use crate::commands::pending::open_snapshot;

use crate::format::{format_duration, format_ratio, format_size};

pub fn run(
    egg_path: &Path,
    output: Option<&Path>,
    zstd_level: i32,
    passphrase: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        egg_path.exists(),
        "egg file not found: {}",
        egg_path.display()
    );

    // Determine output path — never overwrite source
    let default_output = {
        let stem = egg_path.file_stem().unwrap_or_default().to_string_lossy();
        let parent = egg_path.parent().unwrap_or_else(|| Path::new("."));
        parent.join(format!("{stem}_relayed.egg"))
    };
    let output = output.unwrap_or(&default_output);

    // Never orphan pending changes of an existing egg at the output path
    eggdb_store::ensure_no_pending_log(output)?;

    // Safety: never overwrite the source file
    if same_file(egg_path, output) {
        anyhow::bail!(
            "output path would overwrite the source file: {}",
            output.display()
        );
    }

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
    );
    spinner.set_message(format!(
        "Relaying {} ...",
        egg_path.file_name().unwrap_or_default().to_string_lossy()
    ));
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let start = Instant::now();

    // Relay output includes pending write-log changes (like a compaction
    // into a new file); the source and its log are left untouched.
    let reader = open_snapshot(egg_path, passphrase)?;
    let data = reader.data();

    let source_header = reader.header().clone();
    let tables = reader.tables();

    // Track encoding changes for summary: (table, column, old, new)
    let mut encoding_changes: Vec<(String, String, String, String)> = Vec::new();

    // Decode each table (columns in parallel), then re-analyze and re-encode
    let mut table_inputs = Vec::with_capacity(tables.len());
    for (table_idx, table_meta) in tables.iter().enumerate() {
        spinner.set_message(format!(
            "Relaying {} — {} ...",
            egg_path.file_name().unwrap_or_default().to_string_lossy(),
            table_meta.table_name
        ));

        let all_values = reader
            .visible_table(table_idx)
            .with_context(|| format!("failed to decode table '{}'", table_meta.table_name))?;
        let table_input = encode_table(table_meta, reader.live_row_count(table_idx), all_values)
            .with_context(|| format!("failed to re-encode table '{}'", table_meta.table_name))?;

        for (old, new) in table_meta.columns.iter().zip(&table_input.columns) {
            if old.encoding != new.encoding {
                encoding_changes.push((
                    table_meta.table_name.clone(),
                    old.name.clone(),
                    old.encoding.to_string(),
                    new.encoding.to_string(),
                ));
            }
        }
        table_inputs.push(table_input);
    }

    // Write the new egg
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Preserve encryption: if source was encrypted, re-encrypt with same key and a new salt
    let (enc_key, enc_salt) = if let Some(key) = reader.encryption_key() {
        let salt = eggdb_core::generate_salt();
        (Some(eggdb_core::Zeroizing::new(*key)), Some(salt))
    } else {
        (None, None)
    };

    let write_options = WriteOptions {
        created_at,
        original_size: source_header.original_size,
        flags: source_header.flags,
        zstd_level,
        column_zstd_levels: None,
        encryption_key: enc_key,
        encryption_salt: enc_salt,
    };

    let mut egg_bytes = write_egg(&table_inputs, &write_options)
        .with_context(|| "failed to assemble relayed egg")?;

    // Recompute CRC (original_size is already set via WriteOptions)
    let crc = Header::compute_checksum(&egg_bytes);
    egg_bytes[50..54].copy_from_slice(&crc.to_le_bytes());

    std::fs::write(output, &egg_bytes)
        .with_context(|| format!("failed to write {}", output.display()))?;

    let elapsed = start.elapsed();
    spinner.finish_with_message("Done");

    // --- Summary ---
    let source_size = data.len() as u64;
    let relayed_size = egg_bytes.len() as u64;

    println!();
    println!("  {} {}", "Output:".bold(), output.display());
    println!(
        "  {} {} → {}",
        "Size:".bold(),
        format_size(source_size),
        format_size(relayed_size)
    );

    if source_size > 0 {
        let delta = relayed_size as f64 - source_size as f64;
        let pct = (delta / source_size as f64) * 100.0;
        let pct_str = if pct >= 0.0 {
            format!("+{pct:.1}%")
        } else {
            format!("{pct:.1}%")
        };
        println!("  {} {}", "Change:".bold(), pct_str);
    }

    if source_header.original_size > 0 {
        println!(
            "  {} {} → {}",
            "Compression:".bold(),
            format_ratio(source_header.original_size, source_size),
            format_ratio(source_header.original_size, relayed_size)
        );
    }

    println!("  {} {}", "Time:".bold(), format_duration(elapsed));

    if !encoding_changes.is_empty() {
        println!();
        println!("  {}", "Encoding changes:".bold());
        for (table, col, old, new) in &encoding_changes {
            println!("    {} {}.{}: {} → {}", "~".yellow(), table, col, old, new);
        }
    } else {
        println!();
        println!("  {} all encodings unchanged", "✓".green());
    }

    Ok(())
}

/// Check if two paths refer to the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use bitvec::prelude::*;
    use eggdb_core::encoding::analyzer::{analyze_column, select_encoding};
    use eggdb_core::encoding::null_bitmap;
    use eggdb_core::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
    use eggdb_core::value::SqliteType;
    use eggdb_core::{encoder_for, EggReader, Value};

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

    /// Helper: relay egg bytes in memory (same logic as run() but without file IO).
    fn relay_in_memory(source: &[u8], zstd_level: i32) -> Vec<u8> {
        let reader = EggReader::open(source).unwrap();
        let source_header = reader.header().clone();
        let tables = reader.tables();

        let mut table_inputs = Vec::with_capacity(tables.len());

        for (table_idx, table_meta) in tables.iter().enumerate() {
            let mut column_inputs = Vec::with_capacity(table_meta.columns.len());

            for (col_idx, col_meta) in table_meta.columns.iter().enumerate() {
                let values = reader.decode_column(table_idx, col_idx).unwrap();
                let stats = analyze_column(&values);
                let new_encoding = select_encoding(&stats);

                let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
                let non_null = null_bitmap::extract_non_null(&values, &null_bitmap);
                let encoder = encoder_for(new_encoding);
                let encoded = encoder.encode(&non_null);

                column_inputs.push(ColumnInput {
                    name: col_meta.name.clone(),
                    sqlite_type: col_meta.sqlite_type,
                    encoding: new_encoding,
                    cardinality: stats.distinct_count as u64,
                    null_count: stats.null_count as u64,
                    null_bitmap,
                    encoded_values: encoded,
                    dictionary: None,
                    min_value: stats.min_value,
                    max_value: stats.max_value,
                    pre_compressed: None,
                });
            }

            table_inputs.push(TableInput {
                table_name: table_meta.table_name.clone(),
                row_count: table_meta.row_count,
                original_ddl: table_meta.original_ddl.clone(),
                columns: column_inputs,
                index_definitions: table_meta.index_definitions.clone(),
            });
        }

        let write_options = WriteOptions {
            created_at: 0,
            original_size: source_header.original_size,
            flags: source_header.flags,
            zstd_level,
            column_zstd_levels: None,
            encryption_key: None,
            encryption_salt: None,
        };

        write_egg(&table_inputs, &write_options).unwrap()
    }

    /// Relayed egg should be valid (opens with EggReader).
    #[test]
    fn relay_produces_valid_egg() {
        let values: Vec<Value> = (0..100).map(Value::Integer).collect();
        let col = make_column("id", SqliteType::Integer, &values);
        let table = TableInput {
            table_name: "t".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let source = write_egg(&[table], &WriteOptions::default()).unwrap();
        let relayed = relay_in_memory(&source, 3);

        let reader = EggReader::open(&relayed).unwrap();
        assert_eq!(reader.table_count(), 1);
        assert_eq!(reader.tables()[0].table_name, "t");
        assert_eq!(reader.tables()[0].row_count, 100);
    }

    /// Row counts should be preserved after relay.
    #[test]
    fn relay_preserves_row_counts() {
        let values: Vec<Value> = (0..50).map(Value::Integer).collect();
        let col = make_column("x", SqliteType::Integer, &values);
        let table = TableInput {
            table_name: "data".into(),
            row_count: 50,
            original_ddl: "CREATE TABLE data (x INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let source = write_egg(&[table], &WriteOptions::default()).unwrap();
        let relayed = relay_in_memory(&source, 3);

        let reader = EggReader::open(&relayed).unwrap();
        assert_eq!(reader.tables()[0].row_count, 50);
    }

    /// Decoded data should be identical after relay (data fidelity).
    #[test]
    fn relay_preserves_data_fidelity() {
        let values: Vec<Value> = vec![
            Value::Integer(1),
            Value::Null,
            Value::Integer(3),
            Value::Text("hello".into()),
            Value::Null,
            Value::Real(3.14),
            Value::Blob(vec![0xDE, 0xAD]),
            Value::Integer(7),
        ];
        // Use Raw encoding to avoid any issues with mixed types
        let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
        let non_null = null_bitmap::extract_non_null(&values, &null_bitmap);
        let encoder = encoder_for(eggdb_core::Encoding::Raw);
        let encoded = encoder.encode(&non_null);

        let col = ColumnInput {
            name: "mixed".into(),
            sqlite_type: SqliteType::Text,
            encoding: eggdb_core::Encoding::Raw,
            cardinality: 6,
            null_count: 2,
            null_bitmap,
            encoded_values: encoded,
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        };

        let table = TableInput {
            table_name: "mixed".into(),
            row_count: 8,
            original_ddl: "CREATE TABLE mixed (mixed TEXT)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let source = write_egg(&[table], &WriteOptions::default()).unwrap();

        // Decode from source
        let source_reader = EggReader::open(&source).unwrap();
        let source_decoded = source_reader.decode_column(0, 0).unwrap();

        // Relay
        let relayed = relay_in_memory(&source, 3);
        let relayed_reader = EggReader::open(&relayed).unwrap();
        let relayed_decoded = relayed_reader.decode_column(0, 0).unwrap();

        assert_eq!(source_decoded, relayed_decoded);
    }

    /// Relay with multiple tables preserves all data.
    #[test]
    fn relay_multiple_tables() {
        let int_values: Vec<Value> = (0..20).map(Value::Integer).collect();
        let text_values: Vec<Value> = (0..10).map(|i| Value::Text(format!("row_{i}"))).collect();

        let tables = vec![
            TableInput {
                table_name: "ints".into(),
                row_count: 20,
                original_ddl: "CREATE TABLE ints (n INTEGER)".into(),
                columns: vec![make_column("n", SqliteType::Integer, &int_values)],
                index_definitions: Vec::new(),
            },
            TableInput {
                table_name: "texts".into(),
                row_count: 10,
                original_ddl: "CREATE TABLE texts (s TEXT)".into(),
                columns: vec![make_column("s", SqliteType::Text, &text_values)],
                index_definitions: Vec::new(),
            },
        ];

        let source = write_egg(&tables, &WriteOptions::default()).unwrap();
        let relayed = relay_in_memory(&source, 3);

        let src_reader = EggReader::open(&source).unwrap();
        let rel_reader = EggReader::open(&relayed).unwrap();

        assert_eq!(rel_reader.table_count(), 2);

        for t in 0..2 {
            for c in 0..src_reader.tables()[t].columns.len() {
                let src_col = src_reader.decode_column(t, c).unwrap();
                let rel_col = rel_reader.decode_column(t, c).unwrap();
                assert_eq!(src_col, rel_col);
            }
        }
    }

    /// Empty table should survive relay.
    #[test]
    fn relay_empty_table() {
        let col = ColumnInput {
            name: "id".into(),
            sqlite_type: SqliteType::Integer,
            encoding: eggdb_core::Encoding::Const,
            cardinality: 0,
            null_count: 0,
            null_bitmap: bitvec![],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        };
        let table = TableInput {
            table_name: "empty".into(),
            row_count: 0,
            original_ddl: "CREATE TABLE empty (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let source = write_egg(&[table], &WriteOptions::default()).unwrap();
        let relayed = relay_in_memory(&source, 3);

        let reader = EggReader::open(&relayed).unwrap();
        assert_eq!(reader.tables()[0].row_count, 0);
        let decoded = reader.decode_column(0, 0).unwrap();
        assert!(decoded.is_empty());
    }

    /// All-null column should survive relay.
    #[test]
    fn relay_all_null_column() {
        let values = vec![Value::Null; 10];
        let col = ColumnInput {
            name: "nul".into(),
            sqlite_type: SqliteType::Integer,
            encoding: eggdb_core::Encoding::Const,
            cardinality: 0,
            null_count: 10,
            null_bitmap: bitvec![1; 10],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        };
        let table = TableInput {
            table_name: "nulls".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE nulls (nul INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let source = write_egg(&[table], &WriteOptions::default()).unwrap();
        let src_reader = EggReader::open(&source).unwrap();
        let src_decoded = src_reader.decode_column(0, 0).unwrap();

        let relayed = relay_in_memory(&source, 3);
        let rel_reader = EggReader::open(&relayed).unwrap();
        let rel_decoded = rel_reader.decode_column(0, 0).unwrap();

        assert_eq!(src_decoded, rel_decoded);
        assert_eq!(rel_decoded, values);
    }
}
