use std::path::Path;

use rusqlite::Connection;

use eggdb_core::EggReader;
use eggdb_core::Value;

use crate::sqlite_utils::quote_identifier;
use crate::IoError;

/// Options for the hatch (.egg → SQLite) conversion.
#[derive(Default)]
pub struct HatchOptions {
    /// If Some, only materialize these tables. If None, materialize all.
    pub tables: Option<Vec<String>>,
    /// Passphrase for encrypted .egg files.
    pub passphrase: Option<String>,
}

/// Result metadata from a hatch operation.
pub struct HatchResult {
    pub table_count: usize,
    pub total_rows: u64,
}

/// Reconstruct a SQLite database from .egg data.
///
/// Reads the .egg format, creates tables from stored DDL, decodes all column
/// data, transposes to rows, and bulk-inserts into the provided connection.
/// Indexes are recreated after data insertion for performance.
pub fn hatch(
    egg_data: &[u8],
    conn: &Connection,
    options: &HatchOptions,
) -> Result<HatchResult, IoError> {
    let reader = if let Some(ref passphrase) = options.passphrase {
        EggReader::open_with_passphrase(egg_data, passphrase)?
    } else {
        EggReader::open(egg_data)?
    };
    let tables = reader.tables();

    let mut table_count = 0usize;
    let mut total_rows = 0u64;

    for (table_idx, table_meta) in tables.iter().enumerate() {
        // Skip tables not in the filter list
        if let Some(ref allowed) = options.tables {
            if !allowed.iter().any(|n| n == &table_meta.table_name) {
                continue;
            }
        }

        // Validate DDL before execution (defense against crafted .egg files)
        let ddl_trimmed = table_meta.original_ddl.trim();
        if !ddl_trimmed.to_ascii_uppercase().starts_with("CREATE TABLE") {
            return Err(IoError::Other(format!(
                "table '{}' has invalid DDL: must start with CREATE TABLE",
                table_meta.table_name
            )));
        }

        // Create the table from stored DDL (single statement only — no execute_batch)
        conn.execute(&table_meta.original_ddl, [])?;

        let row_count = table_meta.row_count as usize;
        let num_cols = table_meta.columns.len();

        if row_count > 0 && num_cols > 0 {
            // Decode all columns
            let mut columns: Vec<Vec<Value>> = Vec::with_capacity(num_cols);
            for col_idx in 0..num_cols {
                let col_values = reader.decode_column(table_idx, col_idx)?;
                columns.push(col_values);
            }

            // Build the INSERT statement
            let quoted_table = quote_identifier(&table_meta.table_name);
            let col_names: Vec<String> = table_meta
                .columns
                .iter()
                .map(|c| quote_identifier(&c.name))
                .collect();
            let placeholders: Vec<String> = (1..=num_cols).map(|i| format!("?{i}")).collect();
            let insert_sql = format!(
                "INSERT INTO {quoted_table} ({}) VALUES ({})",
                col_names.join(", "),
                placeholders.join(", ")
            );

            // Bulk insert inside a transaction
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(&insert_sql)?;

                for row_idx in 0..row_count {
                    let params: Vec<Box<dyn rusqlite::types::ToSql>> =
                        columns.iter().map(|col| value_to_sql_box(&col[row_idx])).collect();

                    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                        params.iter().map(|p| p.as_ref()).collect();

                    stmt.execute(param_refs.as_slice())?;
                }
            }
            tx.commit()?;
        }

        // Recreate indexes after data insertion (validate each is a CREATE INDEX)
        for index_ddl in &table_meta.index_definitions {
            let idx_trimmed = index_ddl.trim().to_ascii_uppercase();
            if !idx_trimmed.starts_with("CREATE INDEX")
                && !idx_trimmed.starts_with("CREATE UNIQUE INDEX")
            {
                return Err(IoError::Other(format!(
                    "table '{}' has invalid index DDL: must start with CREATE INDEX or CREATE UNIQUE INDEX",
                    table_meta.table_name
                )));
            }
            conn.execute(index_ddl, [])?;
        }

        // Verify row count
        let actual_count = crate::sqlite_utils::get_row_count(conn, &table_meta.table_name)?;
        if actual_count != table_meta.row_count {
            return Err(IoError::Other(format!(
                "row count mismatch for table '{}': expected {}, got {}",
                table_meta.table_name, table_meta.row_count, actual_count
            )));
        }

        table_count += 1;
        total_rows += table_meta.row_count;
    }

    Ok(HatchResult {
        table_count,
        total_rows,
    })
}

/// Convert an eggdb Value to a boxed ToSql for rusqlite parameter binding.
fn value_to_sql_box(value: &Value) -> Box<dyn rusqlite::types::ToSql> {
    match value {
        Value::Null => Box::new(rusqlite::types::Null),
        Value::Integer(i) => Box::new(*i),
        Value::Real(f) => Box::new(*f),
        Value::Text(s) => Box::new(s.clone()),
        Value::Blob(b) => Box::new(b.clone()),
    }
}

/// Reconstruct a SQLite database file from an .egg file on disk.
///
/// Reads the .egg file, creates a new SQLite database at the output path,
/// and calls [`hatch`] to populate it.
pub fn hatch_file(
    egg_path: &Path,
    output: &Path,
    options: &HatchOptions,
) -> Result<HatchResult, IoError> {
    let egg_data = std::fs::read(egg_path)?;
    let conn = Connection::open(output)?;
    hatch(&egg_data, &conn, options)
}
