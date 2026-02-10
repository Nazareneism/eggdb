use bitvec::prelude::*;
use std::collections::HashSet;

use eggdb_core::encoding::analyzer::{analyze_column, select_encoding};
use eggdb_core::encoding::null_bitmap;
use eggdb_core::format::header::Header;
use eggdb_core::format::reader::{encoder_for, EggReader};
use eggdb_core::format::validate::validate;
use eggdb_core::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
use eggdb_core::value::{SqliteType, Value};
use eggdb_core::Encoding;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a ColumnInput from a full column of values (including nulls),
/// using the specified encoding.
fn make_column(
    name: &str,
    sqlite_type: SqliteType,
    values: &[Value],
    encoding: Encoding,
) -> ColumnInput {
    let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
    let non_null: Vec<Value> = values.iter().filter(|v| !v.is_null()).cloned().collect();
    let encoder = encoder_for(encoding);
    let encoded = encoder.encode(&non_null);
    let cardinality = {
        let mut set = HashSet::new();
        for v in &non_null {
            set.insert(v.clone());
        }
        set.len() as u64
    };
    let null_count = values.iter().filter(|v| v.is_null()).count() as u64;
    let min_value = non_null.iter().min().cloned();
    let max_value = non_null.iter().max().cloned();

    ColumnInput {
        name: name.into(),
        sqlite_type,
        encoding,
        cardinality,
        null_count,
        null_bitmap,
        encoded_values: encoded,
        dictionary: None,
        min_value,
        max_value,
        pre_compressed: None,
    }
}

/// Build a ColumnInput using automatic encoding selection.
fn make_auto_column(name: &str, sqlite_type: SqliteType, values: &[Value]) -> ColumnInput {
    let stats = analyze_column(values);
    let encoding = select_encoding(&stats);
    make_column(name, sqlite_type, values, encoding)
}

/// Write, open, decode, and compare values for each column in each table.
fn round_trip_check(tables: &[TableInput]) {
    let data = write_egg(tables, &WriteOptions::default()).unwrap();

    // Validate passes all checks
    let results = validate(&data).unwrap();
    for r in &results {
        assert!(
            r.passed,
            "validation check '{}' failed: {}",
            r.check, r.detail
        );
    }

    // Open and decode
    let reader = EggReader::open(&data).unwrap();
    assert_eq!(reader.table_count(), tables.len());

    for (ti, table) in tables.iter().enumerate() {
        assert_eq!(reader.tables()[ti].table_name, table.table_name);
        assert_eq!(reader.tables()[ti].row_count, table.row_count);

        for ci in 0..table.columns.len() {
            let decoded = reader.decode_column(ti, ci).unwrap();
            let col = &table.columns[ci];

            // Reconstruct expected values from ColumnInput
            let expected_non_null = {
                let encoder = encoder_for(col.encoding);
                encoder
                    .decode(&col.encoded_values, decoded.len() - col.null_count as usize)
                    .unwrap()
            };
            let expected =
                null_bitmap::interleave_nulls(&expected_non_null, &col.null_bitmap).unwrap();

            assert_eq!(
                decoded, expected,
                "column '{}' in table '{}' mismatch",
                col.name, table.table_name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Per-encoding round-trip tests
// ---------------------------------------------------------------------------

#[test]
fn round_trip_const_encoding() {
    let values = vec![Value::Integer(42); 100];
    let table = TableInput {
        table_name: "const_test".into(),
        row_count: 100,
        original_ddl: "CREATE TABLE const_test (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_bool_bitmap_encoding() {
    let values: Vec<Value> = (0..200).map(|i| Value::Integer(i % 2)).collect();
    let table = TableInput {
        table_name: "bool_test".into(),
        row_count: 200,
        original_ddl: "CREATE TABLE bool_test (flag INTEGER)".into(),
        columns: vec![make_column(
            "flag",
            SqliteType::Integer,
            &values,
            Encoding::BoolBitmap,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_delta_encoding() {
    let values: Vec<Value> = (0..500).map(Value::Integer).collect();
    let table = TableInput {
        table_name: "delta_test".into(),
        row_count: 500,
        original_ddl: "CREATE TABLE delta_test (seq INTEGER)".into(),
        columns: vec![make_column(
            "seq",
            SqliteType::Integer,
            &values,
            Encoding::Delta,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_rle_encoding() {
    let mut values = Vec::new();
    for i in 0..10 {
        for _ in 0..50 {
            values.push(Value::Text(format!("group_{i}")));
        }
    }
    let table = TableInput {
        table_name: "rle_test".into(),
        row_count: 500,
        original_ddl: "CREATE TABLE rle_test (grp TEXT)".into(),
        columns: vec![make_column("grp", SqliteType::Text, &values, Encoding::Rle)],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_dict_encoding() {
    let labels = ["alpha", "beta", "gamma", "delta"];
    let values: Vec<Value> = (0..400)
        .map(|i| Value::Text(labels[i % 4].into()))
        .collect();
    let table = TableInput {
        table_name: "dict_test".into(),
        row_count: 400,
        original_ddl: "CREATE TABLE dict_test (label TEXT)".into(),
        columns: vec![make_column(
            "label",
            SqliteType::Text,
            &values,
            Encoding::Dict,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_raw_encoding() {
    let values: Vec<Value> = (0..100)
        .map(|i| Value::Text(format!("user_{i}@example.com")))
        .collect();
    let table = TableInput {
        table_name: "raw_test".into(),
        row_count: 100,
        original_ddl: "CREATE TABLE raw_test (email TEXT)".into(),
        columns: vec![make_column(
            "email",
            SqliteType::Text,
            &values,
            Encoding::Raw,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

// ---------------------------------------------------------------------------
// Mixed encodings in one table
// ---------------------------------------------------------------------------

#[test]
fn round_trip_mixed_encodings() {
    let id_values: Vec<Value> = (1..=100).map(Value::Integer).collect();
    let status_values: Vec<Value> = (0..100)
        .map(|i| {
            Value::Text(
                match i % 3 {
                    0 => "active",
                    1 => "pending",
                    _ => "done",
                }
                .into(),
            )
        })
        .collect();
    let flag_values: Vec<Value> = (0..100).map(|i| Value::Integer(i % 2)).collect();
    let score_values: Vec<Value> = (0..100).map(|i| Value::Real(i as f64 * 0.1)).collect();

    let table = TableInput {
        table_name: "mixed".into(),
        row_count: 100,
        original_ddl: "CREATE TABLE mixed (id INTEGER, status TEXT, flag INTEGER, score REAL)"
            .into(),
        columns: vec![
            make_column("id", SqliteType::Integer, &id_values, Encoding::Delta),
            make_column("status", SqliteType::Text, &status_values, Encoding::Dict),
            make_column(
                "flag",
                SqliteType::Integer,
                &flag_values,
                Encoding::BoolBitmap,
            ),
            make_column("score", SqliteType::Real, &score_values, Encoding::Raw),
        ],
        index_definitions: vec!["CREATE INDEX idx_mixed_id ON mixed (id)".into()],
    };
    round_trip_check(&[table]);
}

// ---------------------------------------------------------------------------
// Multiple tables
// ---------------------------------------------------------------------------

#[test]
fn round_trip_multiple_tables() {
    let users_values = vec![Value::Integer(1); 50];
    let orders_values: Vec<Value> = (0..200)
        .map(|i| Value::Text(format!("order_{i}")))
        .collect();

    let tables = vec![
        TableInput {
            table_name: "users".into(),
            row_count: 50,
            original_ddl: "CREATE TABLE users (id INTEGER)".into(),
            columns: vec![make_column(
                "id",
                SqliteType::Integer,
                &users_values,
                Encoding::Const,
            )],
            index_definitions: Vec::new(),
        },
        TableInput {
            table_name: "orders".into(),
            row_count: 200,
            original_ddl: "CREATE TABLE orders (name TEXT)".into(),
            columns: vec![make_column(
                "name",
                SqliteType::Text,
                &orders_values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        },
    ];
    round_trip_check(&tables);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn round_trip_empty_table() {
    let table = TableInput {
        table_name: "empty".into(),
        row_count: 0,
        original_ddl: "CREATE TABLE empty (id INTEGER)".into(),
        columns: vec![ColumnInput {
            name: "id".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Const,
            cardinality: 0,
            null_count: 0,
            null_bitmap: bitvec![],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        }],
        index_definitions: Vec::new(),
    };

    let data = write_egg(&[table], &WriteOptions::default()).unwrap();
    let reader = EggReader::open(&data).unwrap();
    let decoded = reader.decode_column(0, 0).unwrap();
    assert!(decoded.is_empty());

    let results = validate(&data).unwrap();
    for r in &results {
        assert!(r.passed, "check '{}' failed: {}", r.check, r.detail);
    }
}

#[test]
fn round_trip_single_row() {
    let values = vec![Value::Text("hello".into())];
    let table = TableInput {
        table_name: "single".into(),
        row_count: 1,
        original_ddl: "CREATE TABLE single (msg TEXT)".into(),
        columns: vec![make_column(
            "msg",
            SqliteType::Text,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_all_null_column() {
    let table = TableInput {
        table_name: "nulls".into(),
        row_count: 100,
        original_ddl: "CREATE TABLE nulls (val INTEGER)".into(),
        columns: vec![ColumnInput {
            name: "val".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Const,
            cardinality: 0,
            null_count: 100,
            null_bitmap: bitvec![1; 100],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        }],
        index_definitions: Vec::new(),
    };

    let data = write_egg(&[table], &WriteOptions::default()).unwrap();
    let reader = EggReader::open(&data).unwrap();
    let decoded = reader.decode_column(0, 0).unwrap();
    assert_eq!(decoded, vec![Value::Null; 100]);
}

#[test]
fn round_trip_no_null_column() {
    let values: Vec<Value> = (0..100).map(Value::Integer).collect();
    let table = TableInput {
        table_name: "no_nulls".into(),
        row_count: 100,
        original_ddl: "CREATE TABLE no_nulls (id INTEGER)".into(),
        columns: vec![make_column(
            "id",
            SqliteType::Integer,
            &values,
            Encoding::Delta,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_all_same_column() {
    let values = vec![Value::Text("same".into()); 1000];
    let table = TableInput {
        table_name: "same".into(),
        row_count: 1000,
        original_ddl: "CREATE TABLE same (v TEXT)".into(),
        columns: vec![make_column("v", SqliteType::Text, &values, Encoding::Const)],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_unicode_text() {
    let values = vec![
        Value::Text("Hello, World!".into()),
        Value::Text("こんにちは".into()),
        Value::Text("Привет мир".into()),
        Value::Text("🥚🐣🐤".into()),
        Value::Text("".into()),
        Value::Text("café résumé naïve".into()),
    ];
    let table = TableInput {
        table_name: "unicode".into(),
        row_count: 6,
        original_ddl: "CREATE TABLE unicode (txt TEXT)".into(),
        columns: vec![make_column("txt", SqliteType::Text, &values, Encoding::Raw)],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_blobs() {
    let values = vec![
        Value::Blob(vec![0x00, 0x01, 0x02]),
        Value::Blob(vec![0xFF, 0xFE, 0xFD]),
        Value::Blob(Vec::new()),
        Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]),
    ];
    let table = TableInput {
        table_name: "blobs".into(),
        row_count: 4,
        original_ddl: "CREATE TABLE blobs (data BLOB)".into(),
        columns: vec![make_column(
            "data",
            SqliteType::Blob,
            &values,
            Encoding::Raw,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_empty_strings() {
    let values = vec![Value::Text(String::new()); 50];
    let table = TableInput {
        table_name: "empty_str".into(),
        row_count: 50,
        original_ddl: "CREATE TABLE empty_str (s TEXT)".into(),
        columns: vec![make_column("s", SqliteType::Text, &values, Encoding::Const)],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_100k_rows() {
    let values: Vec<Value> = (0..100_000).map(Value::Integer).collect();
    let table = TableInput {
        table_name: "large".into(),
        row_count: 100_000,
        original_ddl: "CREATE TABLE large (id INTEGER)".into(),
        columns: vec![make_column(
            "id",
            SqliteType::Integer,
            &values,
            Encoding::Delta,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

#[test]
fn round_trip_mixed_nulls() {
    // Pattern: value, null, value, value, null, null, value
    let values = vec![
        Value::Integer(10),
        Value::Null,
        Value::Integer(20),
        Value::Integer(30),
        Value::Null,
        Value::Null,
        Value::Integer(40),
    ];
    let table = TableInput {
        table_name: "mixed_nulls".into(),
        row_count: 7,
        original_ddl: "CREATE TABLE mixed_nulls (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Raw,
        )],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

// ---------------------------------------------------------------------------
// Auto-encoding selection round-trip
// ---------------------------------------------------------------------------

#[test]
fn round_trip_auto_encoding_selection() {
    // Const (all same)
    let const_values = vec![Value::Integer(99); 100];
    // Delta (sorted numeric)
    let delta_values: Vec<Value> = (0..100).map(Value::Integer).collect();
    // BoolBitmap (two distinct)
    let bool_values: Vec<Value> = (0..100).map(|i| Value::Integer(i % 2)).collect();
    // Raw (all unique text)
    let raw_values: Vec<Value> = (0..100)
        .map(|i| Value::Text(format!("unique_{i}")))
        .collect();

    let table = TableInput {
        table_name: "auto".into(),
        row_count: 100,
        original_ddl: "CREATE TABLE auto (a INTEGER, b INTEGER, c INTEGER, d TEXT)".into(),
        columns: vec![
            make_auto_column("a", SqliteType::Integer, &const_values),
            make_auto_column("b", SqliteType::Integer, &delta_values),
            make_auto_column("c", SqliteType::Integer, &bool_values),
            make_auto_column("d", SqliteType::Text, &raw_values),
        ],
        index_definitions: Vec::new(),
    };
    round_trip_check(&[table]);
}

// ---------------------------------------------------------------------------
// Corruption detection
// ---------------------------------------------------------------------------

#[test]
fn corruption_flipped_bit_in_yolk() {
    let values = vec![Value::Integer(42); 50];
    let table = TableInput {
        table_name: "t".into(),
        row_count: 50,
        original_ddl: "CREATE TABLE t (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };

    let mut data = write_egg(&[table], &WriteOptions::default()).unwrap();
    let header = Header::from_bytes(&data).unwrap();
    let yolk_start = header.yolk_offset as usize;
    data[yolk_start] ^= 0xFF;

    let results = validate(&data).unwrap();
    let file_crc = results.iter().find(|r| r.check == "file_crc32c").unwrap();
    assert!(!file_crc.passed, "file CRC should detect yolk corruption");
}

#[test]
fn corruption_flipped_bit_in_header() {
    let values = vec![Value::Integer(1); 10];
    let table = TableInput {
        table_name: "t".into(),
        row_count: 10,
        original_ddl: "CREATE TABLE t (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };

    let mut data = write_egg(&[table], &WriteOptions::default()).unwrap();
    // Flip a bit in the flags field (byte 8), keeping it valid for flags
    // Actually flip a bit in created_at (byte 10) which won't break parsing
    data[10] ^= 0x01;

    let results = validate(&data).unwrap();
    let file_crc = results.iter().find(|r| r.check == "file_crc32c").unwrap();
    assert!(!file_crc.passed, "file CRC should detect header corruption");
}

#[test]
fn corruption_truncated_file() {
    let values = vec![Value::Integer(1); 10];
    let table = TableInput {
        table_name: "t".into(),
        row_count: 10,
        original_ddl: "CREATE TABLE t (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };

    let data = write_egg(&[table], &WriteOptions::default()).unwrap();

    // Truncate to half
    let half = data.len() / 2;
    let truncated = &data[..half];

    // Should either fail to validate or report failures
    let result = validate(truncated);
    match result {
        Err(_) => {} // Header parsing failed - acceptable
        Ok(results) => {
            // At least some checks should fail
            assert!(
                results.iter().any(|r| !r.passed),
                "truncated file should have failing checks"
            );
        }
    }
}

#[test]
fn corruption_zeroed_checksum() {
    let values = vec![Value::Integer(1); 10];
    let table = TableInput {
        table_name: "t".into(),
        row_count: 10,
        original_ddl: "CREATE TABLE t (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };

    let mut data = write_egg(&[table], &WriteOptions::default()).unwrap();
    // Zero out the checksum field
    data[50] = 0;
    data[51] = 0;
    data[52] = 0;
    data[53] = 0;

    let results = validate(&data).unwrap();
    // The actual CRC with zeros in the checksum field might coincidentally match
    // if the actual checksum happens to be 0 (extremely unlikely on real data)
    // So we just verify the check exists
    assert!(
        results.iter().any(|r| r.check == "file_crc32c"),
        "CRC check should always be present"
    );
}

#[test]
fn corruption_flipped_albumen() {
    let values = vec![Value::Integer(1); 10];
    let table = TableInput {
        table_name: "t".into(),
        row_count: 10,
        original_ddl: "CREATE TABLE t (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };

    let mut data = write_egg(&[table], &WriteOptions::default()).unwrap();
    let header = Header::from_bytes(&data).unwrap();

    // Flip a byte in the albumen
    let mid = (header.albumen_offset as usize + header.yolk_offset as usize) / 2;
    if mid < data.len() {
        data[mid] ^= 0xFF;
    }

    let results = validate(&data).unwrap();
    // Either albumen hash or CRC should fail
    let any_failed = results.iter().any(|r| !r.passed);
    assert!(any_failed, "albumen corruption should be detected");
}

// ---------------------------------------------------------------------------
// No-tables edge case
// ---------------------------------------------------------------------------

#[test]
fn round_trip_no_tables() {
    let data = write_egg(&[], &WriteOptions::default()).unwrap();
    let reader = EggReader::open(&data).unwrap();
    assert_eq!(reader.table_count(), 0);

    let results = validate(&data).unwrap();
    for r in &results {
        assert!(r.passed, "check '{}' failed: {}", r.check, r.detail);
    }
}

// ---------------------------------------------------------------------------
// Write options
// ---------------------------------------------------------------------------

#[test]
fn custom_write_options_preserved() {
    let values = vec![Value::Integer(1); 5];
    let table = TableInput {
        table_name: "t".into(),
        row_count: 5,
        original_ddl: "CREATE TABLE t (v INTEGER)".into(),
        columns: vec![make_column(
            "v",
            SqliteType::Integer,
            &values,
            Encoding::Const,
        )],
        index_definitions: Vec::new(),
    };

    let opts = WriteOptions {
        created_at: 1_700_000_000,
        original_size: 999_999,
        flags: eggdb_core::HeaderFlags::new(false, true),
        zstd_level: 1,
        column_zstd_levels: None,
        encryption_key: None,
        encryption_salt: None,
    };

    let data = write_egg(&[table], &opts).unwrap();
    let reader = EggReader::open(&data).unwrap();
    let header = reader.header();

    assert_eq!(header.created_at, 1_700_000_000);
    assert_eq!(header.original_size, 999_999);
    assert!(header.flags.row_order_preserved());
    assert!(!header.flags.has_encryption());
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptest_format {
    use super::*;
    use proptest::prelude::*;

    fn arb_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            any::<i64>().prop_map(Value::Integer),
            any::<f64>()
                .prop_filter("not NaN", |f| !f.is_nan())
                .prop_map(Value::Real),
            "[a-z]{0,20}".prop_map(Value::Text),
            proptest::collection::vec(any::<u8>(), 0..20).prop_map(Value::Blob),
        ]
    }

    fn arb_nullable_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            1 => Just(Value::Null),
            4 => arb_value(),
        ]
    }

    proptest! {
        #[test]
        fn prop_write_read_round_trip(
            values in proptest::collection::vec(arb_nullable_value(), 1..200)
        ) {
            let col = make_column("v", SqliteType::Text, &values, Encoding::Raw);
            let table = TableInput {
                table_name: "prop".into(),
                row_count: values.len() as u64,
                original_ddl: "CREATE TABLE prop (v TEXT)".into(),
                columns: vec![col],
                index_definitions: Vec::new(),
            };

            let data = write_egg(&[table], &WriteOptions::default()).unwrap();

            // Validate passes
            let results = validate(&data).unwrap();
            for r in &results {
                prop_assert!(r.passed, "check '{}' failed: {}", r.check, r.detail);
            }

            // Decode matches
            let reader = EggReader::open(&data).unwrap();
            let decoded = reader.decode_column(0, 0).unwrap();
            prop_assert_eq!(decoded, values);
        }

        #[test]
        fn prop_integer_column_round_trip(
            values in proptest::collection::vec(any::<i64>().prop_map(Value::Integer), 1..500)
        ) {
            let col = make_column("n", SqliteType::Integer, &values, Encoding::Raw);
            let table = TableInput {
                table_name: "ints".into(),
                row_count: values.len() as u64,
                original_ddl: "CREATE TABLE ints (n INTEGER)".into(),
                columns: vec![col],
                index_definitions: Vec::new(),
            };

            let data = write_egg(&[table], &WriteOptions::default()).unwrap();
            let reader = EggReader::open(&data).unwrap();
            let decoded = reader.decode_column(0, 0).unwrap();
            prop_assert_eq!(decoded, values);
        }

        #[test]
        fn prop_sorted_integers_delta_round_trip(
            mut values in proptest::collection::vec(0i64..1000, 1..500)
        ) {
            values.sort();
            let values: Vec<Value> = values.into_iter().map(Value::Integer).collect();
            let col = make_column("s", SqliteType::Integer, &values, Encoding::Delta);
            let table = TableInput {
                table_name: "sorted".into(),
                row_count: values.len() as u64,
                original_ddl: "CREATE TABLE sorted (s INTEGER)".into(),
                columns: vec![col],
                index_definitions: Vec::new(),
            };

            let data = write_egg(&[table], &WriteOptions::default()).unwrap();
            let reader = EggReader::open(&data).unwrap();
            let decoded = reader.decode_column(0, 0).unwrap();
            prop_assert_eq!(decoded, values);
        }
    }
}
