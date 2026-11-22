//! Re-encode fully decoded columns into writer input.
//!
//! Shared by `egg relay` (re-optimize an egg) and compaction (fold the write
//! log into a new main file). Every column is re-analyzed, so encodings adapt
//! to the current data.

use bitvec::prelude::*;

use crate::encoding::analyzer::{analyze_column, select_encoding};
use crate::encoding::null_bitmap;
use crate::format::albumen::TableMeta;
use crate::format::reader::encoder_for;
use crate::format::writer::{ColumnInput, TableInput};
use crate::format::FormatError;
use crate::value::{SqliteType, Value};

/// Analyze a full column (nulls inline), select its encoding, and encode it.
#[must_use]
pub fn encode_column(name: &str, sqlite_type: SqliteType, values: &[Value]) -> ColumnInput {
    let stats = analyze_column(values);
    let encoding = select_encoding(&stats);
    let null_bitmap: BitVec = values.iter().map(Value::is_null).collect();
    let non_null_values = null_bitmap::extract_non_null(values, &null_bitmap);
    let encoded_values = encoder_for(encoding).encode(&non_null_values);

    ColumnInput {
        name: name.to_string(),
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
    }
}

/// Build writer input for one table from its full decoded columns.
///
/// `columns` must hold one entry per column in `meta`, each with exactly
/// `row_count` values. Name, DDL, and indexes are carried over from `meta`.
pub fn encode_table(
    meta: &TableMeta,
    row_count: u64,
    columns: Vec<Vec<Value>>,
) -> Result<TableInput, FormatError> {
    if columns.len() != meta.columns.len() {
        return Err(FormatError::ColumnData(format!(
            "table '{}': got {} columns, expected {}",
            meta.table_name,
            columns.len(),
            meta.columns.len()
        )));
    }
    if let Some((col, values)) = meta
        .columns
        .iter()
        .zip(&columns)
        .find(|(_, values)| values.len() as u64 != row_count)
    {
        return Err(FormatError::ColumnData(format!(
            "table '{}' column '{}': got {} values, expected {row_count}",
            meta.table_name,
            col.name,
            values.len()
        )));
    }

    let column_inputs = meta
        .columns
        .iter()
        .zip(columns)
        .map(|(col, values)| encode_column(&col.name, col.sqlite_type, &values))
        .collect();

    Ok(TableInput {
        table_name: meta.table_name.clone(),
        row_count,
        original_ddl: meta.original_ddl.clone(),
        columns: column_inputs,
        index_definitions: meta.index_definitions.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Encoding;
    use crate::format::albumen::ColumnMeta;

    fn meta(cols: &[&str]) -> TableMeta {
        TableMeta {
            table_name: "t".into(),
            row_count: 0,
            original_ddl: "CREATE TABLE t (a, b)".into(),
            columns: cols
                .iter()
                .map(|name| ColumnMeta {
                    name: (*name).into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Raw,
                    cardinality: 0,
                    null_count: 0,
                    yolk_offset: 0,
                    yolk_length: 0,
                    uncompressed_size: 0,
                    dictionary: None,
                    min_value: None,
                    max_value: None,
                })
                .collect(),
            index_definitions: vec!["CREATE INDEX i ON t (a)".into()],
        }
    }

    #[test]
    fn encode_column_round_trips_with_nulls() {
        let values = vec![
            Value::Integer(1),
            Value::Null,
            Value::Integer(2),
            Value::Integer(3),
        ];
        let col = encode_column("a", SqliteType::Integer, &values);
        assert_eq!(col.null_count, 1);
        assert_eq!(col.encoding, Encoding::Delta);
        let decoded = encoder_for(col.encoding)
            .decode(&col.encoded_values, 3)
            .unwrap();
        let full = null_bitmap::interleave_nulls(&decoded, &col.null_bitmap);
        assert_eq!(full.unwrap(), values);
    }

    #[test]
    fn encode_table_carries_metadata_and_checks_shape() {
        let m = meta(&["a", "b"]);
        let cols = vec![
            vec![Value::Integer(1), Value::Integer(2)],
            vec![Value::Null, Value::Text("x".into())],
        ];
        let table = encode_table(&m, 2, cols).unwrap();
        assert_eq!(table.row_count, 2);
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.index_definitions, m.index_definitions);

        assert!(encode_table(&m, 2, vec![vec![Value::Integer(1); 2]]).is_err());
        assert!(encode_table(&m, 3, vec![vec![Value::Integer(1); 2]; 2]).is_err());
    }
}
