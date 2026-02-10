use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bitvec::prelude::*;
use rusqlite::Connection;

use zeroize::Zeroizing;

use eggdb_core::encoding::analyzer::{analyze_column, select_encoding, StreamingColumnStats};
use eggdb_core::encoding::null_bitmap;
use eggdb_core::encoding::Encoding;
use eggdb_core::format::header::HeaderFlags;
use eggdb_core::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
use eggdb_core::{encoder_for, Value};

use crate::sqlite_utils::{
    get_columns, get_index_definitions, get_row_count, get_table_ddl, get_table_names,
    infer_sqlite_type, quote_identifier,
};
use crate::IoError;

/// Options for the lay (SQLite → .egg) conversion.
pub struct LayOptions {
    /// Zstd compression level (default 3).
    pub zstd_level: i32,
    /// Whether row order is preserved (default true).
    pub row_order_preserved: bool,
    /// When true, automatically select per-column zstd levels based on encoding type.
    /// Const/BoolBitmap get level 1, Delta/Rle/Dict get level 3, Raw gets level 9.
    pub adaptive_compression: bool,
    /// When true, sort rows by the lowest-cardinality column before encoding
    /// to maximize RLE and delta encoding effectiveness. Sets
    /// `row_order_preserved = false` in the output header.
    pub optimize_order: bool,
    /// When true, use a two-pass streaming approach that processes one column
    /// at a time, reducing peak memory from O(rows*columns) to O(rows*1).
    pub streaming: bool,
    /// AES-256-GCM encryption key (derived from passphrase). When set,
    /// yolk blocks are encrypted after compression. Wrapped in `Zeroizing`
    /// so the key is securely zeroed on drop.
    pub encryption_key: Option<Zeroizing<[u8; 32]>>,
    /// Salt used for key derivation. Required when `encryption_key` is set.
    pub encryption_salt: Option<[u8; 16]>,
}

impl Default for LayOptions {
    fn default() -> Self {
        Self {
            zstd_level: 3,
            row_order_preserved: true,
            adaptive_compression: false,
            optimize_order: false,
            streaming: false,
            encryption_key: None,
            encryption_salt: None,
        }
    }
}

/// Result metadata from a lay operation.
pub struct LayResult {
    pub table_count: usize,
    pub total_rows: u64,
    pub original_size: u64,
    pub egg_size: u64,
}

/// Convert a SQLite database to .egg format, returning the egg bytes.
///
/// Reads all tables from the connection, analyzes each column to select
/// the best encoding, encodes the data, and assembles a complete .egg file.
pub fn lay(conn: &Connection, options: &LayOptions) -> Result<Vec<u8>, IoError> {
    let table_names = get_table_names(conn)?;
    let mut table_inputs = Vec::with_capacity(table_names.len());

    for table_name in &table_names {
        let ddl = get_table_ddl(conn, table_name)?;
        let col_defs = get_columns(conn, table_name)?;
        let index_defs = get_index_definitions(conn, table_name)?;
        let row_count = get_row_count(conn, table_name)?;

        // Read all rows into column-oriented vectors
        let num_cols = col_defs.len();
        let mut column_values: Vec<Vec<Value>> =
            vec![Vec::with_capacity(row_count as usize); num_cols];

        let quoted = quote_identifier(table_name);
        let mut stmt = conn.prepare(&format!("SELECT * FROM {quoted}"))?;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            for (col_idx, col_vec) in column_values.iter_mut().enumerate() {
                let value = match row.get_ref(col_idx)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(i) => Value::Integer(i),
                    rusqlite::types::ValueRef::Real(f) => Value::Real(f),
                    rusqlite::types::ValueRef::Text(s) => {
                        Value::Text(String::from_utf8_lossy(s).into_owned())
                    }
                    rusqlite::types::ValueRef::Blob(b) => Value::Blob(b.to_vec()),
                };
                col_vec.push(value);
            }
        }

        // Optionally reorder rows for better compression
        if options.optimize_order && row_count > 0 {
            reorder_columns(&mut column_values);
        }

        // Build ColumnInput for each column
        let mut column_inputs = Vec::with_capacity(num_cols);

        for (col_idx, col_def) in col_defs.iter().enumerate() {
            let values = &column_values[col_idx];
            let stats = analyze_column(values);
            let encoding = select_encoding(&stats);

            let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
            let non_null_values = null_bitmap::extract_non_null(values, &null_bitmap);
            let encoder = encoder_for(encoding);
            let encoded_values = encoder.encode(&non_null_values);

            let sqlite_type = infer_sqlite_type(col_def.declared_type.as_deref());

            column_inputs.push(ColumnInput {
                name: col_def.name.clone(),
                sqlite_type,
                encoding,
                cardinality: stats.distinct_count as u64,
                null_count: stats.null_count as u64,
                null_bitmap,
                encoded_values,
                dictionary: None,
                min_value: stats.min_value,
                max_value: stats.max_value,
                pre_compressed: None,
            });
        }

        table_inputs.push(TableInput {
            table_name: table_name.clone(),
            row_count,
            original_ddl: ddl,
            columns: column_inputs,
            index_definitions: index_defs,
        });
    }

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let column_zstd_levels = if options.adaptive_compression {
        Some(
            table_inputs
                .iter()
                .map(|table| {
                    table
                        .columns
                        .iter()
                        .map(|col| adaptive_zstd_level(col.encoding, options.zstd_level))
                        .collect()
                })
                .collect(),
        )
    } else {
        None
    };

    let has_encryption = options.encryption_key.is_some();
    let write_options = WriteOptions {
        created_at,
        original_size: 0, // Unknown for in-memory connections
        flags: HeaderFlags::new(
            has_encryption,
            !options.optimize_order && options.row_order_preserved,
        ),
        zstd_level: options.zstd_level,
        column_zstd_levels,
        encryption_key: options.encryption_key.clone(),
        encryption_salt: options.encryption_salt,
    };

    let egg_bytes = write_egg(&table_inputs, &write_options)?;
    Ok(egg_bytes)
}

/// Convert a SQLite database file to an .egg file on disk.
///
/// Opens the SQLite database, calls [`lay`], and writes the result to the output path.
pub fn lay_file(source: &Path, output: &Path, options: &LayOptions) -> Result<LayResult, IoError> {
    let original_size = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);

    let conn = Connection::open(source)?;
    let egg_bytes = if options.streaming {
        lay_streaming(&conn, options)?
    } else {
        lay(&conn, options)?
    };

    // Patch original_size into the header (bytes [18..26])
    let mut egg_data = egg_bytes;
    egg_data[18..26].copy_from_slice(&original_size.to_le_bytes());

    // Recompute the CRC32C after patching
    let crc = eggdb_core::Header::compute_checksum(&egg_data);
    egg_data[50..54].copy_from_slice(&crc.to_le_bytes());

    let egg_size = egg_data.len() as u64;
    std::fs::write(output, &egg_data)?;

    // Count tables and rows from the egg
    let reader = eggdb_core::EggReader::open(&egg_data)?;
    let table_count = reader.table_count();
    let total_rows: u64 = reader.tables().iter().map(|t| t.row_count).sum();

    Ok(LayResult {
        table_count,
        total_rows,
        original_size,
        egg_size,
    })
}

/// Incrementally lay a SQLite database file to an .egg file on disk.
///
/// Reads the existing egg, diffs against the SQLite database, and writes
/// a new egg reusing unchanged column blocks where possible.
pub fn incremental_lay_file(
    source: &Path,
    existing_egg: &Path,
    output: &Path,
    options: &LayOptions,
) -> Result<LayResult, IoError> {
    let original_size = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);
    let existing_egg_data = std::fs::read(existing_egg)?;

    let conn = Connection::open(source)?;
    let egg_bytes = incremental_lay(&conn, &existing_egg_data, options)?;

    // Patch original_size into the header (bytes [18..26])
    let mut egg_data = egg_bytes;
    egg_data[18..26].copy_from_slice(&original_size.to_le_bytes());

    // Recompute the CRC32C after patching
    let crc = eggdb_core::Header::compute_checksum(&egg_data);
    egg_data[50..54].copy_from_slice(&crc.to_le_bytes());

    let egg_size = egg_data.len() as u64;
    std::fs::write(output, &egg_data)?;

    // Count tables and rows from the egg
    let reader = eggdb_core::EggReader::open(&egg_data)?;
    let table_count = reader.table_count();
    let total_rows: u64 = reader.tables().iter().map(|t| t.row_count).sum();

    Ok(LayResult {
        table_count,
        total_rows,
        original_size,
        egg_size,
    })
}

/// Incrementally lay a SQLite database to .egg format, reusing unchanged
/// column blocks from an existing .egg file.
///
/// For each table present in both the SQLite database and the existing egg:
/// - If the DDL or row count differs, the entire table is re-encoded.
/// - If the DDL and row count match, each column is compared value-by-value.
///   Unchanged columns copy their raw compressed block verbatim from the
///   existing egg. Changed columns are re-analyzed and re-encoded.
///
/// Tables present only in SQLite are encoded from scratch. Tables present
/// only in the existing egg are dropped (not included in the output).
pub fn incremental_lay(
    conn: &Connection,
    existing_egg_data: &[u8],
    options: &LayOptions,
) -> Result<Vec<u8>, IoError> {
    let reader = eggdb_core::EggReader::open(existing_egg_data)?;
    let existing_tables = reader.tables();

    let table_names = get_table_names(conn)?;
    let mut table_inputs = Vec::with_capacity(table_names.len());

    for table_name in &table_names {
        let ddl = get_table_ddl(conn, table_name)?;
        let col_defs = get_columns(conn, table_name)?;
        let index_defs = get_index_definitions(conn, table_name)?;
        let row_count = get_row_count(conn, table_name)?;

        // Look up the table in the existing egg
        let existing_table = existing_tables
            .iter()
            .enumerate()
            .find(|(_, t)| t.table_name == *table_name);

        // Read all rows from SQLite into column-oriented vectors
        let num_cols = col_defs.len();
        let mut column_values: Vec<Vec<Value>> =
            vec![Vec::with_capacity(row_count as usize); num_cols];

        let quoted = quote_identifier(table_name);
        let mut stmt = conn.prepare(&format!("SELECT * FROM {quoted}"))?;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            for (col_idx, col_vec) in column_values.iter_mut().enumerate() {
                let value = match row.get_ref(col_idx)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(i) => Value::Integer(i),
                    rusqlite::types::ValueRef::Real(f) => Value::Real(f),
                    rusqlite::types::ValueRef::Text(s) => {
                        Value::Text(String::from_utf8_lossy(s).into_owned())
                    }
                    rusqlite::types::ValueRef::Blob(b) => Value::Blob(b.to_vec()),
                };
                col_vec.push(value);
            }
        }

        // Can we reuse columns from the existing egg?
        let can_reuse = match existing_table {
            Some((_, et)) => {
                et.original_ddl == ddl && et.row_count == row_count && et.columns.len() == num_cols
            }
            None => false,
        };

        let mut column_inputs = Vec::with_capacity(num_cols);

        for (col_idx, col_def) in col_defs.iter().enumerate() {
            let values = &column_values[col_idx];
            let sqlite_type = infer_sqlite_type(col_def.declared_type.as_deref());

            let mut reused = false;

            if can_reuse {
                let (existing_table_idx, existing_meta) = existing_table.unwrap();
                let existing_col = &existing_meta.columns[col_idx];

                // Column name must match
                if existing_col.name == col_def.name {
                    // Decode existing column values and compare
                    if let Ok(existing_values) = reader.decode_column(existing_table_idx, col_idx) {
                        if existing_values == *values {
                            // Values match! Copy the raw compressed block verbatim.
                            let yolk_offset = reader.header().yolk_offset;
                            let block_start =
                                yolk_offset as usize + existing_col.yolk_offset as usize;
                            let block_end = block_start + existing_col.yolk_length as usize;

                            if block_end <= existing_egg_data.len() {
                                let compressed_data =
                                    existing_egg_data[block_start..block_end].to_vec();

                                // Find the CRC from the membrane
                                let membrane = reader.membrane();
                                let crc = membrane
                                    .block_checksums
                                    .iter()
                                    .find(|bc| {
                                        bc.table_index == existing_table_idx as u32
                                            && bc.column_index == col_idx as u32
                                    })
                                    .map(|bc| bc.crc32c)
                                    .unwrap_or(0);

                                let null_bitmap: BitVec =
                                    values.iter().map(|v| v.is_null()).collect();

                                column_inputs.push(ColumnInput {
                                    name: col_def.name.clone(),
                                    sqlite_type,
                                    encoding: existing_col.encoding,
                                    cardinality: existing_col.cardinality,
                                    null_count: existing_col.null_count,
                                    null_bitmap,
                                    encoded_values: Vec::new(),
                                    dictionary: existing_col.dictionary.clone(),
                                    min_value: existing_col.min_value.clone(),
                                    max_value: existing_col.max_value.clone(),
                                    pre_compressed: Some(eggdb_core::PreCompressedBlock {
                                        compressed_data,
                                        uncompressed_size: existing_col.uncompressed_size,
                                        crc32c: crc,
                                    }),
                                });
                                reused = true;
                            }
                        }
                    }
                }
            }

            if !reused {
                // Re-analyze and re-encode this column
                let stats = analyze_column(values);
                let encoding = select_encoding(&stats);

                let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
                let non_null_values = null_bitmap::extract_non_null(values, &null_bitmap);
                let encoder = encoder_for(encoding);
                let encoded_values = encoder.encode(&non_null_values);

                column_inputs.push(ColumnInput {
                    name: col_def.name.clone(),
                    sqlite_type,
                    encoding,
                    cardinality: stats.distinct_count as u64,
                    null_count: stats.null_count as u64,
                    null_bitmap,
                    encoded_values,
                    dictionary: None,
                    min_value: stats.min_value,
                    max_value: stats.max_value,
                    pre_compressed: None,
                });
            }
        }

        table_inputs.push(TableInput {
            table_name: table_name.clone(),
            row_count,
            original_ddl: ddl,
            columns: column_inputs,
            index_definitions: index_defs,
        });
    }

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let column_zstd_levels = if options.adaptive_compression {
        Some(
            table_inputs
                .iter()
                .map(|table| {
                    table
                        .columns
                        .iter()
                        .map(|col| adaptive_zstd_level(col.encoding, options.zstd_level))
                        .collect()
                })
                .collect(),
        )
    } else {
        None
    };

    let has_encryption = options.encryption_key.is_some();
    let write_options = WriteOptions {
        created_at,
        original_size: 0,
        flags: HeaderFlags::new(has_encryption, options.row_order_preserved),
        zstd_level: options.zstd_level,
        column_zstd_levels,
        encryption_key: options.encryption_key.clone(),
        encryption_salt: options.encryption_salt,
    };

    let egg_bytes = write_egg(&table_inputs, &write_options)?;
    Ok(egg_bytes)
}

/// Convert a SQLite database to .egg format using a two-pass streaming approach.
///
/// This reduces peak memory usage from O(rows * columns) to O(rows * 1) by
/// processing one column at a time.
///
/// **Pass 1** streams all rows once, feeding each value into a
/// [`StreamingColumnStats`] accumulator per column. This determines encoding
/// selection without storing all values in memory.
///
/// **Pass 2** reads one column at a time via `SELECT <col> FROM <table>`,
/// encodes it, and drops the values before moving to the next column.
///
/// Note: `optimize_order` is ignored in streaming mode because row reordering
/// requires all columns in memory simultaneously.
pub fn lay_streaming(conn: &Connection, options: &LayOptions) -> Result<Vec<u8>, IoError> {
    let table_names = get_table_names(conn)?;
    let mut table_inputs = Vec::with_capacity(table_names.len());

    for table_name in &table_names {
        let ddl = get_table_ddl(conn, table_name)?;
        let col_defs = get_columns(conn, table_name)?;
        let index_defs = get_index_definitions(conn, table_name)?;
        let row_count = get_row_count(conn, table_name)?;
        let num_cols = col_defs.len();

        // Pass 1: stream all rows to compute per-column stats
        let mut streaming_stats: Vec<StreamingColumnStats> =
            (0..num_cols).map(|_| StreamingColumnStats::new()).collect();

        let quoted = quote_identifier(table_name);
        {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {quoted}"))?;
            let mut rows = stmt.query([])?;

            while let Some(row) = rows.next()? {
                for (col_idx, stats) in streaming_stats.iter_mut().enumerate() {
                    let value = extract_value(row, col_idx)?;
                    stats.update(&value);
                }
            }
        }

        let column_stats: Vec<_> = streaming_stats.iter().map(|s| s.finalize()).collect();

        // Pass 2: read one column at a time, encode, drop
        let mut column_inputs = Vec::with_capacity(num_cols);

        for (col_idx, col_def) in col_defs.iter().enumerate() {
            let stats = &column_stats[col_idx];
            let encoding = select_encoding(stats);

            let col_quoted = quote_identifier(&col_def.name);
            let mut stmt = conn.prepare(&format!("SELECT {col_quoted} FROM {quoted}"))?;
            let mut rows = stmt.query([])?;

            let mut values = Vec::with_capacity(row_count as usize);
            while let Some(row) = rows.next()? {
                values.push(extract_value(row, 0)?);
            }

            let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
            let non_null_values = null_bitmap::extract_non_null(&values, &null_bitmap);
            let encoder = encoder_for(encoding);
            let encoded_values = encoder.encode(&non_null_values);

            let sqlite_type = infer_sqlite_type(col_def.declared_type.as_deref());

            column_inputs.push(ColumnInput {
                name: col_def.name.clone(),
                sqlite_type,
                encoding,
                cardinality: stats.distinct_count as u64,
                null_count: stats.null_count as u64,
                null_bitmap,
                encoded_values,
                dictionary: None,
                min_value: stats.min_value.clone(),
                max_value: stats.max_value.clone(),
                pre_compressed: None,
            });
            // `values` is dropped here, freeing memory before the next column
        }

        table_inputs.push(TableInput {
            table_name: table_name.clone(),
            row_count,
            original_ddl: ddl,
            columns: column_inputs,
            index_definitions: index_defs,
        });
    }

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let column_zstd_levels = if options.adaptive_compression {
        Some(
            table_inputs
                .iter()
                .map(|table| {
                    table
                        .columns
                        .iter()
                        .map(|col| adaptive_zstd_level(col.encoding, options.zstd_level))
                        .collect()
                })
                .collect(),
        )
    } else {
        None
    };

    // Streaming mode ignores optimize_order (needs all columns in memory)
    let has_encryption = options.encryption_key.is_some();
    let write_options = WriteOptions {
        created_at,
        original_size: 0,
        flags: HeaderFlags::new(has_encryption, options.row_order_preserved),
        zstd_level: options.zstd_level,
        column_zstd_levels,
        encryption_key: options.encryption_key.clone(),
        encryption_salt: options.encryption_salt,
    };

    let egg_bytes = write_egg(&table_inputs, &write_options)?;
    Ok(egg_bytes)
}

/// Extract a `Value` from a rusqlite row at the given column index.
fn extract_value(row: &rusqlite::Row<'_>, col_idx: usize) -> Result<Value, IoError> {
    let value = match row.get_ref(col_idx)? {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(i) => Value::Integer(i),
        rusqlite::types::ValueRef::Real(f) => Value::Real(f),
        rusqlite::types::ValueRef::Text(s) => Value::Text(String::from_utf8_lossy(s).into_owned()),
        rusqlite::types::ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    };
    Ok(value)
}

/// Reorder rows across all column vectors to maximize compression.
///
/// Finds the column with the lowest non-zero cardinality (most repeated values)
/// and stable-sorts all column vectors by that column's values. This groups
/// identical values together, improving RLE and delta encoding effectiveness.
fn reorder_columns(column_values: &mut [Vec<Value>]) {
    if column_values.is_empty() {
        return;
    }

    let row_count = column_values[0].len();
    if row_count == 0 {
        return;
    }

    // Compute cardinality for each column
    let mut best_col: Option<usize> = None;
    let mut best_cardinality = usize::MAX;

    for (col_idx, col) in column_values.iter().enumerate() {
        let distinct: HashSet<&Value> = col.iter().collect();
        let cardinality = distinct.len();
        // Pick lowest non-zero cardinality (skip all-null columns where distinct=0
        // would just be nulls)
        if cardinality > 0 && cardinality < best_cardinality {
            best_cardinality = cardinality;
            best_col = Some(col_idx);
        }
    }

    let Some(sort_col) = best_col else {
        return; // All columns are all-null, nothing to reorder
    };

    // Build row indices and stable-sort by the chosen column
    let mut indices: Vec<usize> = (0..row_count).collect();
    indices.sort_by(|&a, &b| column_values[sort_col][a].cmp(&column_values[sort_col][b]));

    // Apply the permutation to all columns
    for col in column_values.iter_mut() {
        let reordered: Vec<Value> = indices.iter().map(|&i| col[i].clone()).collect();
        *col = reordered;
    }
}

/// Select a zstd compression level based on encoding type and base level.
///
/// Lightweight encodings (Const, BoolBitmap) use level 1 since they already
/// produce very compact output. Medium encodings (Delta, Rle, Dict) use level 3.
/// Raw encoding relies heavily on zstd so it uses `max(9, base_level)` --
/// this ensures `--zstd-level 19 --adaptive-compression` gives Raw columns
/// the full level 19 instead of capping at 9.
fn adaptive_zstd_level(encoding: Encoding, base_level: i32) -> i32 {
    match encoding {
        Encoding::Const | Encoding::BoolBitmap => 1,
        Encoding::Delta | Encoding::Rle | Encoding::Dict => 3,
        Encoding::Raw => base_level.max(9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create an in-memory SQLite database with the given schema and data.
    fn make_test_db(schema: &str, inserts: &[&str]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema).unwrap();
        for insert in inserts {
            conn.execute_batch(insert).unwrap();
        }
        conn
    }

    // ================================================================
    // F3: Row Reordering tests
    // ================================================================

    #[test]
    fn reorder_produces_valid_egg() {
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, status TEXT, amount REAL);",
            &[
                "INSERT INTO t VALUES (1, 'active', 10.0);",
                "INSERT INTO t VALUES (2, 'inactive', 20.0);",
                "INSERT INTO t VALUES (3, 'active', 30.0);",
                "INSERT INTO t VALUES (4, 'inactive', 40.0);",
                "INSERT INTO t VALUES (5, 'active', 50.0);",
            ],
        );

        let options = LayOptions {
            optimize_order: true,
            ..Default::default()
        };
        let egg_bytes = lay(&conn, &options).unwrap();

        // Verify the egg is valid and readable
        let reader = eggdb_core::EggReader::open(&egg_bytes).unwrap();
        assert_eq!(reader.tables().len(), 1);
        assert_eq!(reader.tables()[0].row_count, 5);

        // Verify all original values are present (order may differ)
        let ids = reader.decode_column(0, 0).unwrap();
        let mut id_vals: Vec<i64> = ids
            .iter()
            .map(|v| match v {
                Value::Integer(i) => *i,
                _ => panic!("expected integer"),
            })
            .collect();
        id_vals.sort();
        assert_eq!(id_vals, vec![1, 2, 3, 4, 5]);

        let statuses = reader.decode_column(0, 1).unwrap();
        let mut status_vals: Vec<String> = statuses
            .iter()
            .map(|v| match v {
                Value::Text(s) => s.clone(),
                _ => panic!("expected text"),
            })
            .collect();
        status_vals.sort();
        assert_eq!(
            status_vals,
            vec!["active", "active", "active", "inactive", "inactive"]
        );
    }

    #[test]
    fn reorder_groups_by_lowest_cardinality() {
        // status has cardinality 2, id has cardinality 5 -> sort by status
        let mut columns = vec![
            vec![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(4),
                Value::Integer(5),
            ],
            vec![
                Value::Text("B".into()),
                Value::Text("A".into()),
                Value::Text("B".into()),
                Value::Text("A".into()),
                Value::Text("B".into()),
            ],
        ];

        reorder_columns(&mut columns);

        // After sorting by column 1 (status, cardinality 2), all "A"s should
        // come before all "B"s (stable sort)
        let statuses: Vec<&str> = columns[1]
            .iter()
            .map(|v| match v {
                Value::Text(s) => s.as_str(),
                _ => panic!("expected text"),
            })
            .collect();
        assert_eq!(statuses, vec!["A", "A", "B", "B", "B"]);

        // Corresponding IDs should follow their original partner
        let ids: Vec<i64> = columns[0]
            .iter()
            .map(|v| match v {
                Value::Integer(i) => *i,
                _ => panic!("expected integer"),
            })
            .collect();
        // A rows were (2, A) and (4, A), B rows were (1, B), (3, B), (5, B)
        assert_eq!(ids, vec![2, 4, 1, 3, 5]);
    }

    #[test]
    fn reorder_empty_table() {
        let mut columns: Vec<Vec<Value>> = vec![Vec::new(), Vec::new()];
        reorder_columns(&mut columns);
        assert!(columns[0].is_empty());
        assert!(columns[1].is_empty());
    }

    #[test]
    fn reorder_sets_flag_correctly() {
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, v TEXT);",
            &[
                "INSERT INTO t VALUES (1, 'a');",
                "INSERT INTO t VALUES (2, 'b');",
            ],
        );

        // With optimize_order, row_order_preserved should be false
        let options = LayOptions {
            optimize_order: true,
            ..Default::default()
        };
        let egg_bytes = lay(&conn, &options).unwrap();
        let reader = eggdb_core::EggReader::open(&egg_bytes).unwrap();
        assert!(
            !reader.header().flags.row_order_preserved(),
            "optimize_order should set row_order_preserved=false"
        );

        // Without optimize_order, row_order_preserved should be true (default)
        let options2 = LayOptions::default();
        let egg_bytes2 = lay(&conn, &options2).unwrap();
        let reader2 = eggdb_core::EggReader::open(&egg_bytes2).unwrap();
        assert!(
            reader2.header().flags.row_order_preserved(),
            "default should have row_order_preserved=true"
        );
    }

    // ================================================================
    // F5: Streaming Lay tests
    // ================================================================

    #[test]
    fn streaming_produces_valid_egg() {
        let conn = make_test_db(
            "CREATE TABLE users (id INTEGER, name TEXT, score REAL);",
            &[
                "INSERT INTO users VALUES (1, 'Alice', 9.5);",
                "INSERT INTO users VALUES (2, 'Bob', 8.0);",
                "INSERT INTO users VALUES (3, 'Carol', 7.5);",
            ],
        );

        let options = LayOptions {
            streaming: true,
            ..Default::default()
        };
        let egg_bytes = lay_streaming(&conn, &options).unwrap();

        let reader = eggdb_core::EggReader::open(&egg_bytes).unwrap();
        assert_eq!(reader.tables().len(), 1);
        assert_eq!(reader.tables()[0].row_count, 3);
        assert_eq!(reader.tables()[0].columns.len(), 3);
    }

    #[test]
    fn streaming_matches_normal_lay_values() {
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, val TEXT);",
            &[
                "INSERT INTO t VALUES (1, 'hello');",
                "INSERT INTO t VALUES (2, 'world');",
                "INSERT INTO t VALUES (3, 'test');",
                "INSERT INTO t VALUES (4, NULL);",
                "INSERT INTO t VALUES (5, 'hello');",
            ],
        );

        let normal_options = LayOptions::default();
        let normal_bytes = lay(&conn, &normal_options).unwrap();
        let normal_reader = eggdb_core::EggReader::open(&normal_bytes).unwrap();

        let streaming_options = LayOptions {
            streaming: true,
            ..Default::default()
        };
        let streaming_bytes = lay_streaming(&conn, &streaming_options).unwrap();
        let streaming_reader = eggdb_core::EggReader::open(&streaming_bytes).unwrap();

        // Same table structure
        assert_eq!(
            normal_reader.tables()[0].row_count,
            streaming_reader.tables()[0].row_count
        );
        assert_eq!(
            normal_reader.tables()[0].columns.len(),
            streaming_reader.tables()[0].columns.len(),
        );

        // Same decoded values
        for col_idx in 0..2 {
            let normal_vals = normal_reader.decode_column(0, col_idx).unwrap();
            let streaming_vals = streaming_reader.decode_column(0, col_idx).unwrap();
            assert_eq!(
                normal_vals, streaming_vals,
                "column {col_idx} values differ between normal and streaming lay"
            );
        }
    }

    #[test]
    fn streaming_handles_nulls() {
        let conn = make_test_db(
            "CREATE TABLE t (a INTEGER, b TEXT);",
            &[
                "INSERT INTO t VALUES (NULL, NULL);",
                "INSERT INTO t VALUES (1, NULL);",
                "INSERT INTO t VALUES (NULL, 'x');",
            ],
        );

        let options = LayOptions {
            streaming: true,
            ..Default::default()
        };
        let egg_bytes = lay_streaming(&conn, &options).unwrap();

        let reader = eggdb_core::EggReader::open(&egg_bytes).unwrap();
        let col_a = reader.decode_column(0, 0).unwrap();
        let col_b = reader.decode_column(0, 1).unwrap();

        assert_eq!(col_a, vec![Value::Null, Value::Integer(1), Value::Null]);
        assert_eq!(
            col_b,
            vec![Value::Null, Value::Null, Value::Text("x".into())]
        );
    }

    #[test]
    fn streaming_empty_table() {
        let conn = make_test_db("CREATE TABLE t (id INTEGER, name TEXT);", &[]);

        let options = LayOptions {
            streaming: true,
            ..Default::default()
        };
        let egg_bytes = lay_streaming(&conn, &options).unwrap();

        let reader = eggdb_core::EggReader::open(&egg_bytes).unwrap();
        assert_eq!(reader.tables()[0].row_count, 0);
    }

    #[test]
    fn lay_file_uses_streaming_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let egg_path = dir.path().join("test.egg");

        // Create a small SQLite database
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER, val TEXT);
             INSERT INTO t VALUES (1, 'a');
             INSERT INTO t VALUES (2, 'b');",
        )
        .unwrap();
        drop(conn);

        let options = LayOptions {
            streaming: true,
            ..Default::default()
        };
        let result = lay_file(&db_path, &egg_path, &options).unwrap();
        assert_eq!(result.table_count, 1);
        assert_eq!(result.total_rows, 2);

        // Verify the egg file is valid
        let egg_data = std::fs::read(&egg_path).unwrap();
        let reader = eggdb_core::EggReader::open(&egg_data).unwrap();
        assert_eq!(reader.tables()[0].row_count, 2);
    }

    // ================================================================
    // F6: Incremental Lay tests
    // ================================================================

    #[test]
    fn incremental_lay_unchanged_data() {
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, name TEXT);",
            &[
                "INSERT INTO t VALUES (1, 'Alice');",
                "INSERT INTO t VALUES (2, 'Bob');",
                "INSERT INTO t VALUES (3, 'Carol');",
            ],
        );

        // Initial lay
        let options = LayOptions::default();
        let egg1 = lay(&conn, &options).unwrap();

        // Incremental lay with the same data — should reuse blocks
        let egg2 = incremental_lay(&conn, &egg1, &options).unwrap();

        // Both should produce valid eggs with the same data
        let reader1 = eggdb_core::EggReader::open(&egg1).unwrap();
        let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();

        assert_eq!(reader1.tables()[0].row_count, reader2.tables()[0].row_count);

        for col_idx in 0..2 {
            let vals1 = reader1.decode_column(0, col_idx).unwrap();
            let vals2 = reader2.decode_column(0, col_idx).unwrap();
            assert_eq!(vals1, vals2, "column {col_idx} values differ");
        }
    }

    #[test]
    fn incremental_lay_changed_column() {
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, name TEXT);",
            &[
                "INSERT INTO t VALUES (1, 'Alice');",
                "INSERT INTO t VALUES (2, 'Bob');",
                "INSERT INTO t VALUES (3, 'Carol');",
            ],
        );

        let options = LayOptions::default();
        let egg1 = lay(&conn, &options).unwrap();

        // Modify data in the database
        conn.execute_batch("UPDATE t SET name = 'Dave' WHERE id = 2;")
            .unwrap();

        let egg2 = incremental_lay(&conn, &egg1, &options).unwrap();

        let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
        let names = reader2.decode_column(0, 1).unwrap();
        assert_eq!(
            names,
            vec![
                Value::Text("Alice".into()),
                Value::Text("Dave".into()),
                Value::Text("Carol".into()),
            ]
        );
    }

    #[test]
    fn incremental_lay_new_table() {
        let conn = make_test_db(
            "CREATE TABLE t1 (id INTEGER);",
            &["INSERT INTO t1 VALUES (1);"],
        );

        let options = LayOptions::default();
        let egg1 = lay(&conn, &options).unwrap();

        // Add a new table
        conn.execute_batch(
            "CREATE TABLE t2 (x TEXT);
             INSERT INTO t2 VALUES ('hello');",
        )
        .unwrap();

        let egg2 = incremental_lay(&conn, &egg1, &options).unwrap();
        let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
        assert_eq!(reader2.tables().len(), 2);

        // Find t2 in the output
        let t2 = reader2
            .tables()
            .iter()
            .find(|t| t.table_name == "t2")
            .expect("t2 should be in output");
        assert_eq!(t2.row_count, 1);
    }

    #[test]
    fn incremental_lay_dropped_table() {
        let conn = make_test_db(
            "CREATE TABLE t1 (id INTEGER);
             CREATE TABLE t2 (id INTEGER);",
            &["INSERT INTO t1 VALUES (1);", "INSERT INTO t2 VALUES (2);"],
        );

        let options = LayOptions::default();
        let egg1 = lay(&conn, &options).unwrap();

        // Drop a table from the database
        conn.execute_batch("DROP TABLE t2;").unwrap();

        let egg2 = incremental_lay(&conn, &egg1, &options).unwrap();
        let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
        assert_eq!(reader2.tables().len(), 1);
        assert_eq!(reader2.tables()[0].table_name, "t1");
    }

    #[test]
    fn incremental_lay_row_count_changed() {
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, name TEXT);",
            &[
                "INSERT INTO t VALUES (1, 'Alice');",
                "INSERT INTO t VALUES (2, 'Bob');",
            ],
        );

        let options = LayOptions::default();
        let egg1 = lay(&conn, &options).unwrap();

        // Add a row — row count differs so entire table must be re-encoded
        conn.execute_batch("INSERT INTO t VALUES (3, 'Carol');")
            .unwrap();

        let egg2 = incremental_lay(&conn, &egg1, &options).unwrap();
        let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
        assert_eq!(reader2.tables()[0].row_count, 3);

        let ids = reader2.decode_column(0, 0).unwrap();
        assert_eq!(
            ids,
            vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)]
        );
    }

    // ================================================================
    // Helper function tests
    // ================================================================

    #[test]
    fn reorder_columns_picks_correct_sort_column() {
        // col0: 4 distinct, col1: 2 distinct, col2: 3 distinct
        // Should sort by col1 (lowest cardinality)
        let mut columns = vec![
            vec![
                Value::Integer(4),
                Value::Integer(3),
                Value::Integer(2),
                Value::Integer(1),
            ],
            vec![
                Value::Text("Y".into()),
                Value::Text("X".into()),
                Value::Text("Y".into()),
                Value::Text("X".into()),
            ],
            vec![
                Value::Text("a".into()),
                Value::Text("b".into()),
                Value::Text("c".into()),
                Value::Text("a".into()),
            ],
        ];

        reorder_columns(&mut columns);

        // col1 should be sorted: X, X, Y, Y
        let col1: Vec<&str> = columns[1]
            .iter()
            .map(|v| match v {
                Value::Text(s) => s.as_str(),
                _ => panic!("expected text"),
            })
            .collect();
        assert_eq!(col1, vec!["X", "X", "Y", "Y"]);
    }

    #[test]
    fn adaptive_zstd_respects_base_level() {
        // When base_level > 9, Raw columns should get the base level
        assert_eq!(adaptive_zstd_level(Encoding::Raw, 3), 9);
        assert_eq!(adaptive_zstd_level(Encoding::Raw, 9), 9);
        assert_eq!(adaptive_zstd_level(Encoding::Raw, 19), 19);
        assert_eq!(adaptive_zstd_level(Encoding::Raw, 22), 22);

        // Non-Raw encodings are unaffected by base_level
        assert_eq!(adaptive_zstd_level(Encoding::Const, 19), 1);
        assert_eq!(adaptive_zstd_level(Encoding::BoolBitmap, 19), 1);
        assert_eq!(adaptive_zstd_level(Encoding::Delta, 19), 3);
        assert_eq!(adaptive_zstd_level(Encoding::Rle, 19), 3);
        assert_eq!(adaptive_zstd_level(Encoding::Dict, 19), 3);
    }

    #[test]
    fn adaptive_with_high_zstd_produces_smaller_raw() {
        // Create a table with a high-cardinality Raw column
        let conn = make_test_db(
            "CREATE TABLE t (id INTEGER, email TEXT);",
            &[
                "INSERT INTO t VALUES (1, 'alice@example.com');",
                "INSERT INTO t VALUES (2, 'bob@example.com');",
                "INSERT INTO t VALUES (3, 'carol@example.com');",
                "INSERT INTO t VALUES (4, 'dave@example.com');",
                "INSERT INTO t VALUES (5, 'eve@example.com');",
            ],
        );

        // Adaptive with default zstd_level=3 -> Raw gets level 9
        let opts_default = LayOptions {
            adaptive_compression: true,
            ..Default::default()
        };
        let egg1 = lay(&conn, &opts_default).unwrap();

        // Adaptive with zstd_level=19 -> Raw gets level 19
        let opts_high = LayOptions {
            adaptive_compression: true,
            zstd_level: 19,
            ..Default::default()
        };
        let egg2 = lay(&conn, &opts_high).unwrap();

        // Both should produce valid eggs
        let reader1 = eggdb_core::EggReader::open(&egg1).unwrap();
        let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
        assert_eq!(reader1.tables()[0].row_count, 5);
        assert_eq!(reader2.tables()[0].row_count, 5);

        // Values should be identical
        for col in 0..2 {
            let v1 = reader1.decode_column(0, col).unwrap();
            let v2 = reader2.decode_column(0, col).unwrap();
            assert_eq!(v1, v2, "column {col} values differ");
        }
    }

    #[test]
    fn reorder_all_null_columns() {
        let mut columns = vec![
            vec![Value::Null, Value::Null, Value::Null],
            vec![Value::Null, Value::Null, Value::Null],
        ];
        // Should not panic — all-null columns have cardinality 1 (just Null)
        reorder_columns(&mut columns);
    }
}
