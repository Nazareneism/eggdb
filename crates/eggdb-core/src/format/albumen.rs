use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::encoding::Encoding;
use crate::format::{bincode_config, FormatError};
use crate::value::{SqliteType, Value};

/// Metadata for a single column within a table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    pub sqlite_type: SqliteType,
    pub encoding: Encoding,
    pub cardinality: u64,
    pub null_count: u64,
    /// Byte offset of this column's compressed block, **relative to the yolk section start**.
    pub yolk_offset: u64,
    /// Length in bytes of this column's compressed block in the yolk.
    pub yolk_length: u64,
    /// Size in bytes of the uncompressed column block (null bitmap + encoded values).
    pub uncompressed_size: u64,
    pub dictionary: Option<Vec<Value>>,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
}

/// Metadata for a single table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableMeta {
    pub table_name: String,
    pub row_count: u64,
    pub original_ddl: String,
    pub columns: Vec<ColumnMeta>,
    pub index_definitions: Vec<String>,
}

/// Serialize the albumen (metadata section) using the format's bincode configuration.
///
/// Returns the serialized bytes. The caller is responsible for recording the
/// offset and computing the blake3 hash.
pub fn serialize_albumen(tables: &[TableMeta]) -> Result<Vec<u8>, FormatError> {
    bincode_config()
        .serialize(tables)
        .map_err(FormatError::Bincode)
}

/// Deserialize the albumen from bytes.
pub fn deserialize_albumen(data: &[u8]) -> Result<Vec<TableMeta>, FormatError> {
    bincode_config()
        .deserialize(data)
        .map_err(FormatError::Bincode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let tables: Vec<TableMeta> = Vec::new();
        let bytes = serialize_albumen(&tables).unwrap();
        let result = deserialize_albumen(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn round_trip_single_table() {
        let tables = vec![TableMeta {
            table_name: "users".into(),
            row_count: 1000,
            original_ddl: "CREATE TABLE users (id INTEGER, name TEXT)".into(),
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Delta,
                    cardinality: 1000,
                    null_count: 0,
                    yolk_offset: 0,
                    yolk_length: 512,
                    uncompressed_size: 8000,
                    dictionary: None,
                    min_value: Some(Value::Integer(1)),
                    max_value: Some(Value::Integer(1000)),
                },
                ColumnMeta {
                    name: "name".into(),
                    sqlite_type: SqliteType::Text,
                    encoding: Encoding::Dict,
                    cardinality: 50,
                    null_count: 10,
                    yolk_offset: 512,
                    yolk_length: 256,
                    uncompressed_size: 4000,
                    dictionary: Some(vec![Value::Text("Alice".into()), Value::Text("Bob".into())]),
                    min_value: Some(Value::Text("Alice".into())),
                    max_value: Some(Value::Text("Zara".into())),
                },
            ],
            index_definitions: vec!["CREATE INDEX idx_users_name ON users (name)".into()],
        }];

        let bytes = serialize_albumen(&tables).unwrap();
        let result = deserialize_albumen(&bytes).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].table_name, "users");
        assert_eq!(result[0].row_count, 1000);
        assert_eq!(result[0].columns.len(), 2);
        assert_eq!(result[0].columns[0].name, "id");
        assert_eq!(result[0].columns[0].encoding, Encoding::Delta);
        assert_eq!(result[0].columns[1].name, "name");
        assert_eq!(result[0].columns[1].encoding, Encoding::Dict);
        assert_eq!(result[0].columns[1].null_count, 10);
        assert_eq!(result[0].index_definitions.len(), 1);
    }

    #[test]
    fn round_trip_multiple_tables() {
        let tables = vec![
            TableMeta {
                table_name: "orders".into(),
                row_count: 0,
                original_ddl: "CREATE TABLE orders (id INTEGER)".into(),
                columns: vec![ColumnMeta {
                    name: "id".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Const,
                    cardinality: 0,
                    null_count: 0,
                    yolk_offset: 0,
                    yolk_length: 0,
                    uncompressed_size: 0,
                    dictionary: None,
                    min_value: None,
                    max_value: None,
                }],
                index_definitions: Vec::new(),
            },
            TableMeta {
                table_name: "products".into(),
                row_count: 500,
                original_ddl: "CREATE TABLE products (sku TEXT)".into(),
                columns: vec![ColumnMeta {
                    name: "sku".into(),
                    sqlite_type: SqliteType::Text,
                    encoding: Encoding::Raw,
                    cardinality: 500,
                    null_count: 0,
                    yolk_offset: 0,
                    yolk_length: 1024,
                    uncompressed_size: 5000,
                    dictionary: None,
                    min_value: Some(Value::Text("AAA-001".into())),
                    max_value: Some(Value::Text("ZZZ-999".into())),
                }],
                index_definitions: Vec::new(),
            },
        ];

        let bytes = serialize_albumen(&tables).unwrap();
        let result = deserialize_albumen(&bytes).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].table_name, "orders");
        assert_eq!(result[1].table_name, "products");
    }

    #[test]
    fn all_encoding_types() {
        let tables = vec![TableMeta {
            table_name: "test".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE test (a INT, b INT, c INT, d INT, e INT, f INT)".into(),
            columns: vec![
                ColumnMeta {
                    name: "a".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Const,
                    cardinality: 1,
                    null_count: 0,
                    yolk_offset: 0,
                    yolk_length: 10,
                    uncompressed_size: 10,
                    dictionary: None,
                    min_value: Some(Value::Integer(42)),
                    max_value: Some(Value::Integer(42)),
                },
                ColumnMeta {
                    name: "b".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::BoolBitmap,
                    cardinality: 2,
                    null_count: 0,
                    yolk_offset: 10,
                    yolk_length: 20,
                    uncompressed_size: 30,
                    dictionary: None,
                    min_value: Some(Value::Integer(0)),
                    max_value: Some(Value::Integer(1)),
                },
                ColumnMeta {
                    name: "c".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Delta,
                    cardinality: 100,
                    null_count: 0,
                    yolk_offset: 30,
                    yolk_length: 50,
                    uncompressed_size: 800,
                    dictionary: None,
                    min_value: Some(Value::Integer(0)),
                    max_value: Some(Value::Integer(99)),
                },
                ColumnMeta {
                    name: "d".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Rle,
                    cardinality: 5,
                    null_count: 0,
                    yolk_offset: 80,
                    yolk_length: 40,
                    uncompressed_size: 200,
                    dictionary: None,
                    min_value: Some(Value::Integer(1)),
                    max_value: Some(Value::Integer(5)),
                },
                ColumnMeta {
                    name: "e".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Dict,
                    cardinality: 3,
                    null_count: 0,
                    yolk_offset: 120,
                    yolk_length: 30,
                    uncompressed_size: 100,
                    dictionary: Some(vec![
                        Value::Integer(1),
                        Value::Integer(2),
                        Value::Integer(3),
                    ]),
                    min_value: Some(Value::Integer(1)),
                    max_value: Some(Value::Integer(3)),
                },
                ColumnMeta {
                    name: "f".into(),
                    sqlite_type: SqliteType::Text,
                    encoding: Encoding::Raw,
                    cardinality: 100,
                    null_count: 0,
                    yolk_offset: 150,
                    yolk_length: 500,
                    uncompressed_size: 2000,
                    dictionary: None,
                    min_value: Some(Value::Text("aaa".into())),
                    max_value: Some(Value::Text("zzz".into())),
                },
            ],
            index_definitions: Vec::new(),
        }];

        let bytes = serialize_albumen(&tables).unwrap();
        let result = deserialize_albumen(&bytes).unwrap();

        assert_eq!(result[0].columns.len(), 6);
        assert_eq!(result[0].columns[0].encoding, Encoding::Const);
        assert_eq!(result[0].columns[1].encoding, Encoding::BoolBitmap);
        assert_eq!(result[0].columns[2].encoding, Encoding::Delta);
        assert_eq!(result[0].columns[3].encoding, Encoding::Rle);
        assert_eq!(result[0].columns[4].encoding, Encoding::Dict);
        assert_eq!(result[0].columns[5].encoding, Encoding::Raw);
    }

    #[test]
    fn blob_and_null_values() {
        let tables = vec![TableMeta {
            table_name: "blobs".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE blobs (data BLOB)".into(),
            columns: vec![ColumnMeta {
                name: "data".into(),
                sqlite_type: SqliteType::Blob,
                encoding: Encoding::Raw,
                cardinality: 10,
                null_count: 5,
                yolk_offset: 0,
                yolk_length: 100,
                uncompressed_size: 200,
                dictionary: None,
                min_value: Some(Value::Blob(vec![0x00, 0x01])),
                max_value: Some(Value::Blob(vec![0xff, 0xfe])),
            }],
            index_definitions: Vec::new(),
        }];

        let bytes = serialize_albumen(&tables).unwrap();
        let result = deserialize_albumen(&bytes).unwrap();

        assert_eq!(result[0].columns[0].null_count, 5);
        assert_eq!(
            result[0].columns[0].min_value,
            Some(Value::Blob(vec![0x00, 0x01]))
        );
    }
}
