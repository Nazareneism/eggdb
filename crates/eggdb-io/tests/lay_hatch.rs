use rusqlite::Connection;

use eggdb_io::reader::{hatch, HatchOptions};
use eggdb_io::writer::{lay, LayOptions};

/// Helper: compare two SQLite connections table-by-table, row-by-row.
fn assert_databases_equal(original: &Connection, hatched: &Connection) {
    let orig_tables = eggdb_io::sqlite_utils::get_table_names(original).unwrap();
    let hatch_tables = eggdb_io::sqlite_utils::get_table_names(hatched).unwrap();
    assert_eq!(orig_tables, hatch_tables, "table names differ");

    for table in &orig_tables {
        let orig_count = eggdb_io::sqlite_utils::get_row_count(original, table).unwrap();
        let hatch_count = eggdb_io::sqlite_utils::get_row_count(hatched, table).unwrap();
        assert_eq!(
            orig_count, hatch_count,
            "row count differs for table {table}"
        );

        let quoted = eggdb_io::sqlite_utils::quote_identifier(table);
        let sql = format!("SELECT * FROM {quoted}");

        let mut orig_stmt = original.prepare(&sql).unwrap();
        let mut hatch_stmt = hatched.prepare(&sql).unwrap();

        let orig_col_count = orig_stmt.column_count();
        let hatch_col_count = hatch_stmt.column_count();
        assert_eq!(
            orig_col_count, hatch_col_count,
            "column count differs for table {table}"
        );

        let mut orig_rows = orig_stmt.query([]).unwrap();
        let mut hatch_rows = hatch_stmt.query([]).unwrap();

        let mut row_idx = 0u64;
        loop {
            let orig_row = orig_rows.next().unwrap();
            let hatch_row = hatch_rows.next().unwrap();

            match (orig_row, hatch_row) {
                (Some(o), Some(h)) => {
                    for col in 0..orig_col_count {
                        let ov = o.get_ref(col).unwrap();
                        let hv = h.get_ref(col).unwrap();
                        assert_values_equal(
                            &ov,
                            &hv,
                            &format!("table={table} row={row_idx} col={col}"),
                        );
                    }
                }
                (None, None) => break,
                _ => panic!("row count mismatch at row {row_idx} in table {table}"),
            }
            row_idx += 1;
        }
    }
}

fn assert_values_equal(
    a: &rusqlite::types::ValueRef,
    b: &rusqlite::types::ValueRef,
    context: &str,
) {
    use rusqlite::types::ValueRef;
    match (a, b) {
        (ValueRef::Null, ValueRef::Null) => {}
        (ValueRef::Integer(ai), ValueRef::Integer(bi)) => {
            assert_eq!(ai, bi, "integer mismatch at {context}");
        }
        (ValueRef::Real(af), ValueRef::Real(bf)) => {
            assert!(
                (af - bf).abs() < f64::EPSILON || (af.is_nan() && bf.is_nan()),
                "real mismatch at {context}: {af} vs {bf}"
            );
        }
        (ValueRef::Text(at), ValueRef::Text(bt)) => {
            assert_eq!(at, bt, "text mismatch at {context}");
        }
        (ValueRef::Blob(ab), ValueRef::Blob(bb)) => {
            assert_eq!(ab, bb, "blob mismatch at {context}");
        }
        _ => panic!("type mismatch at {context}: {a:?} vs {b:?}"),
    }
}

/// Helper: lay → hatch round-trip via in-memory connections.
fn round_trip(original: &Connection) -> Connection {
    let egg_data = lay(original, &LayOptions::default()).unwrap();
    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg_data, &hatched, &HatchOptions::default()).unwrap();
    hatched
}

// ============================================================================
// Test cases
// ============================================================================

#[test]
fn basic_round_trip() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);
         INSERT INTO users VALUES (1, 'Alice', 30);
         INSERT INTO users VALUES (2, 'Bob', 25);
         INSERT INTO users VALUES (3, 'Charlie', 35);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn multiple_tables() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO users VALUES (1, 'Alice');
         INSERT INTO users VALUES (2, 'Bob');
         CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total REAL);
         INSERT INTO orders VALUES (1, 1, 99.99);
         INSERT INTO orders VALUES (2, 2, 49.50);
         INSERT INTO orders VALUES (3, 1, 75.00);
         CREATE TABLE products (id INTEGER PRIMARY KEY, sku TEXT, price REAL);
         INSERT INTO products VALUES (1, 'ABC-123', 29.99);
         INSERT INTO products VALUES (2, 'DEF-456', 19.99);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn empty_table() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE empty (id INTEGER, name TEXT, value REAL);")
        .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn single_row_table() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE single (id INTEGER, name TEXT);
         INSERT INTO single VALUES (42, 'only row');",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn all_null_column() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER, val TEXT);
         INSERT INTO t VALUES (1, NULL);
         INSERT INTO t VALUES (2, NULL);
         INSERT INTO t VALUES (3, NULL);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn mixed_nulls() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (a INTEGER, b TEXT, c REAL);
         INSERT INTO t VALUES (1, 'hello', 1.5);
         INSERT INTO t VALUES (NULL, 'world', NULL);
         INSERT INTO t VALUES (3, NULL, 3.5);
         INSERT INTO t VALUES (NULL, NULL, NULL);
         INSERT INTO t VALUES (5, 'end', 5.5);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn integer_column() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val INTEGER);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..100 {
        stmt.execute([i as i64]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn real_column() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val REAL);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..50 {
        stmt.execute([i as f64 * 0.1]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn text_column() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val TEXT);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..100 {
        stmt.execute([format!("item_{i}")]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn blob_column() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val BLOB);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..20 {
        let blob: Vec<u8> = (0..=255u8).cycle().skip(i).take(64).collect();
        stmt.execute([blob]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn blob_with_null_bytes() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val BLOB);").unwrap();

    let blob_with_nulls = [0u8, 1, 0, 0, 2, 0, 3, 0, 0, 0];
    orig.execute("INSERT INTO t VALUES (?1)", [&blob_with_nulls[..]])
        .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn empty_strings() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (val TEXT);
         INSERT INTO t VALUES ('');
         INSERT INTO t VALUES ('');
         INSERT INTO t VALUES ('notempty');
         INSERT INTO t VALUES ('');",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn unicode_text() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val TEXT);").unwrap();

    let unicode_strings = vec![
        "Hello, World!",
        "\u{1F600}\u{1F601}\u{1F602}",              // emoji
        "\u{4F60}\u{597D}\u{4E16}\u{754C}",         // Chinese: 你好世界
        "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}", // Arabic: مرحبا
        "\u{00E9}\u{00E0}\u{00FC}\u{00F1}",         // Latin accented
        "\u{0410}\u{0411}\u{0412}\u{0413}",         // Cyrillic
        "",                                         // empty
        "normal ascii",
    ];

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for s in &unicode_strings {
        stmt.execute([*s]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn very_long_text() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val TEXT);").unwrap();

    let long_text = "x".repeat(10_000);
    orig.execute("INSERT INTO t VALUES (?1)", [&long_text])
        .unwrap();
    orig.execute("INSERT INTO t VALUES (?1)", ["short"])
        .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn large_table() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (id INTEGER, status TEXT, amount REAL);")
        .unwrap();

    let statuses = ["active", "inactive", "pending", "deleted", "archived"];
    let tx = orig.unchecked_transaction().unwrap();
    {
        let mut stmt = tx.prepare("INSERT INTO t VALUES (?1, ?2, ?3)").unwrap();
        for i in 0..10_000 {
            stmt.execute(rusqlite::params![
                i as i64,
                statuses[i % 5],
                (i as f64) * 0.01
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn encoding_variety_const() {
    // All same value → Const encoding
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val INTEGER);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for _ in 0..100 {
        stmt.execute([42i64]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn encoding_variety_bool_bitmap() {
    // 2 distinct values → BoolBitmap encoding
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (flag INTEGER);")
        .unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..100 {
        stmt.execute([i % 2]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn encoding_variety_delta() {
    // Sorted numeric → Delta encoding
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val INTEGER);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..200 {
        stmt.execute([i as i64]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn encoding_variety_rle() {
    // High consecutive duplicates → RLE encoding
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val TEXT);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for group in 0..10 {
        for _ in 0..50 {
            stmt.execute([format!("group_{group}")]).unwrap();
        }
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn encoding_variety_dict() {
    // Low cardinality → Dict encoding (need <=1% ratio, so 5 values in 10K+ rows)
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val TEXT);").unwrap();

    let categories = ["cat_a", "cat_b", "cat_c", "cat_d", "cat_e"];
    let tx = orig.unchecked_transaction().unwrap();
    {
        let mut stmt = tx.prepare("INSERT INTO t VALUES (?1)").unwrap();
        for i in 0..10_000 {
            // Shuffle to avoid consecutive dups triggering RLE
            stmt.execute([categories[(i * 7 + 3) % 5]]).unwrap();
        }
    }
    tx.commit().unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn encoding_variety_raw() {
    // All unique text → Raw encoding
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val TEXT);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..100 {
        stmt.execute([format!("unique_value_{i}")]).unwrap();
    }
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn index_preservation() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT);
         CREATE INDEX idx_name ON users(name);
         CREATE INDEX idx_email ON users(email);
         INSERT INTO users VALUES (1, 'Alice', 'alice@example.com');
         INSERT INTO users VALUES (2, 'Bob', 'bob@example.com');",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);

    // Verify indexes were recreated
    let indexes = eggdb_io::sqlite_utils::get_index_definitions(&hatched, "users").unwrap();
    assert_eq!(indexes.len(), 2);
    let index_text: String = indexes.join(" ");
    assert!(index_text.contains("idx_name"));
    assert!(index_text.contains("idx_email"));
}

#[test]
fn partial_hatch() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE users (id INTEGER, name TEXT);
         INSERT INTO users VALUES (1, 'Alice');
         CREATE TABLE orders (id INTEGER, total REAL);
         INSERT INTO orders VALUES (1, 99.99);
         CREATE TABLE products (id INTEGER, name TEXT);
         INSERT INTO products VALUES (1, 'Widget');",
    )
    .unwrap();

    let egg_data = lay(&orig, &LayOptions::default()).unwrap();

    // Only hatch the 'users' table
    let hatched = Connection::open_in_memory().unwrap();
    let options = HatchOptions {
        tables: Some(vec!["users".to_string()]),
        ..Default::default()
    };
    let result = hatch(&egg_data, &hatched, &options).unwrap();

    assert_eq!(result.table_count, 1);
    assert_eq!(result.total_rows, 1);

    // users table should exist and have data
    let count = eggdb_io::sqlite_utils::get_row_count(&hatched, "users").unwrap();
    assert_eq!(count, 1);

    // orders and products should NOT exist
    let tables = eggdb_io::sqlite_utils::get_table_names(&hatched).unwrap();
    assert_eq!(tables, vec!["users"]);
}

#[test]
fn column_ordering_preserved() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (z_last TEXT, a_first INTEGER, m_middle REAL);
         INSERT INTO t VALUES ('hello', 42, 3.14);",
    )
    .unwrap();

    let hatched = round_trip(&orig);

    // Check column order via PRAGMA table_info
    let orig_cols = eggdb_io::sqlite_utils::get_columns(&orig, "t").unwrap();
    let hatch_cols = eggdb_io::sqlite_utils::get_columns(&hatched, "t").unwrap();

    assert_eq!(orig_cols.len(), hatch_cols.len());
    for (o, h) in orig_cols.iter().zip(hatch_cols.iter()) {
        assert_eq!(o.name, h.name, "column name order mismatch");
    }

    assert_databases_equal(&orig, &hatched);
}

#[test]
fn mixed_type_table() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (
            id INTEGER,
            name TEXT,
            score REAL,
            data BLOB,
            optional INTEGER
         );
         INSERT INTO t VALUES (1, 'Alice', 95.5, X'DEADBEEF', 100);
         INSERT INTO t VALUES (2, 'Bob', NULL, NULL, NULL);
         INSERT INTO t VALUES (3, NULL, 88.0, X'00FF', 200);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn no_nullable_columns() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER NOT NULL, name TEXT NOT NULL);
         INSERT INTO t VALUES (1, 'Alice');
         INSERT INTO t VALUES (2, 'Bob');
         INSERT INTO t VALUES (3, 'Charlie');",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn multiple_empty_tables() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE a (id INTEGER);
         CREATE TABLE b (name TEXT);
         CREATE TABLE c (val REAL);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn table_with_many_columns() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE wide (
            c1 INTEGER, c2 TEXT, c3 REAL, c4 BLOB,
            c5 INTEGER, c6 TEXT, c7 REAL, c8 BLOB,
            c9 INTEGER, c10 TEXT
         );
         INSERT INTO wide VALUES (1, 'a', 1.0, X'01', 2, 'b', 2.0, X'02', 3, 'c');
         INSERT INTO wide VALUES (4, 'd', 4.0, X'04', 5, 'e', 5.0, X'05', 6, 'f');",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn negative_integers() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (val INTEGER);
         INSERT INTO t VALUES (-1);
         INSERT INTO t VALUES (-100);
         INSERT INTO t VALUES (0);
         INSERT INTO t VALUES (100);
         INSERT INTO t VALUES (-9223372036854775808);
         INSERT INTO t VALUES (9223372036854775807);",
    )
    .unwrap();

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn special_float_values() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (val REAL);").unwrap();

    let mut stmt = orig.prepare("INSERT INTO t VALUES (?1)").unwrap();
    stmt.execute([0.0f64]).unwrap();
    stmt.execute([-0.0f64]).unwrap();
    stmt.execute([f64::MIN]).unwrap();
    stmt.execute([f64::MAX]).unwrap();
    stmt.execute([f64::MIN_POSITIVE]).unwrap();
    stmt.execute([1.0e-308f64]).unwrap();
    stmt.execute([1.0e308f64]).unwrap();
    drop(stmt);

    let hatched = round_trip(&orig);
    assert_databases_equal(&orig, &hatched);
}

// ============================================================================
// Adaptive compression tests
// ============================================================================

#[test]
fn adaptive_compression_round_trip() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (
            id INTEGER,
            status TEXT,
            description TEXT
         );",
    )
    .unwrap();

    let statuses = ["active", "inactive", "pending"];
    let tx = orig.unchecked_transaction().unwrap();
    {
        let mut stmt = tx.prepare("INSERT INTO t VALUES (?1, ?2, ?3)").unwrap();
        for i in 0..1000 {
            stmt.execute(rusqlite::params![
                i as i64,
                statuses[i % 3],
                format!("Description text for item number {i}")
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();

    let options = LayOptions {
        adaptive_compression: true,
        ..LayOptions::default()
    };
    let egg_data = lay(&orig, &options).unwrap();

    // Egg should be valid
    let reader = eggdb_core::EggReader::open(&egg_data).unwrap();
    assert_eq!(reader.table_count(), 1);

    // Hatch it back and verify data integrity
    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg_data, &hatched, &HatchOptions::default()).unwrap();
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn adaptive_compression_produces_valid_egg() {
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);
         INSERT INTO users VALUES (1, 'Alice', 30);
         INSERT INTO users VALUES (2, 'Bob', 25);
         INSERT INTO users VALUES (3, 'Charlie', 35);",
    )
    .unwrap();

    let options = LayOptions {
        adaptive_compression: true,
        ..LayOptions::default()
    };
    let egg_data = lay(&orig, &options).unwrap();

    // Validate the produced egg
    let results = eggdb_core::validate(&egg_data).unwrap();
    for r in &results {
        assert!(
            r.passed,
            "validation check '{}' failed: {}",
            r.check, r.detail
        );
    }
}

#[test]
fn adaptive_vs_default_both_valid() {
    // Verify that both adaptive and non-adaptive produce valid, readable eggs
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch("CREATE TABLE t (const_col INTEGER, seq_col INTEGER, text_col TEXT);")
        .unwrap();

    let tx = orig.unchecked_transaction().unwrap();
    {
        let mut stmt = tx.prepare("INSERT INTO t VALUES (?1, ?2, ?3)").unwrap();
        for i in 0..500 {
            stmt.execute(rusqlite::params![42i64, i as i64, format!("unique_{i}")])
                .unwrap();
        }
    }
    tx.commit().unwrap();

    let default_egg = lay(&orig, &LayOptions::default()).unwrap();
    let adaptive_egg = lay(
        &orig,
        &LayOptions {
            adaptive_compression: true,
            ..LayOptions::default()
        },
    )
    .unwrap();

    // Both should be valid
    let default_results = eggdb_core::validate(&default_egg).unwrap();
    for r in &default_results {
        assert!(r.passed, "default check '{}' failed: {}", r.check, r.detail);
    }

    let adaptive_results = eggdb_core::validate(&adaptive_egg).unwrap();
    for r in &adaptive_results {
        assert!(
            r.passed,
            "adaptive check '{}' failed: {}",
            r.check, r.detail
        );
    }

    // Both should hatch to the same data
    let hatched_default = Connection::open_in_memory().unwrap();
    hatch(&default_egg, &hatched_default, &HatchOptions::default()).unwrap();

    let hatched_adaptive = Connection::open_in_memory().unwrap();
    hatch(&adaptive_egg, &hatched_adaptive, &HatchOptions::default()).unwrap();

    assert_databases_equal(&orig, &hatched_default);
    assert_databases_equal(&orig, &hatched_adaptive);
}

// --- Incremental Lay Tests ---

#[test]
fn incremental_lay_unchanged_data() {
    // When data hasn't changed, incremental lay should produce a valid egg
    // with the same data.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER, name TEXT);
         INSERT INTO t VALUES (1, 'alice');
         INSERT INTO t VALUES (2, 'bob');
         INSERT INTO t VALUES (3, 'carol');",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();
    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();

    // The incremental egg should produce the same data when hatched
    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg2, &hatched, &HatchOptions::default()).unwrap();
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn incremental_lay_unchanged_copies_blocks() {
    // When data is unchanged, the raw compressed block should be identical
    // (same CRC) in the incremental egg.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT);
         INSERT INTO t VALUES (1, 'hello');
         INSERT INTO t VALUES (2, 'world');",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();
    let reader1 = eggdb_core::EggReader::open(&egg1).unwrap();
    let crc1: Vec<u32> = reader1
        .membrane()
        .block_checksums
        .iter()
        .map(|bc| bc.crc32c)
        .collect();

    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();
    let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
    let crc2: Vec<u32> = reader2
        .membrane()
        .block_checksums
        .iter()
        .map(|bc| bc.crc32c)
        .collect();

    // CRCs should match because the blocks were copied verbatim
    assert_eq!(crc1, crc2);
}

#[test]
fn incremental_lay_changed_column() {
    // When one column changes, only that column should be re-encoded.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER, val TEXT);
         INSERT INTO t VALUES (1, 'hello');
         INSERT INTO t VALUES (2, 'world');",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();
    let reader1 = eggdb_core::EggReader::open(&egg1).unwrap();
    let id_crc1 = reader1.membrane().block_checksums[0].crc32c;

    // Modify the val column but keep id unchanged
    orig.execute_batch("UPDATE t SET val = 'changed' WHERE id = 1;")
        .unwrap();

    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();
    let reader2 = eggdb_core::EggReader::open(&egg2).unwrap();
    let id_crc2 = reader2.membrane().block_checksums[0].crc32c;

    // id column should be identical (copied verbatim)
    assert_eq!(id_crc1, id_crc2);

    // Verify the new data is correct
    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg2, &hatched, &HatchOptions::default()).unwrap();

    let val: String = hatched
        .query_row("SELECT val FROM t WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(val, "changed");
}

#[test]
fn incremental_lay_row_count_mismatch() {
    // When row count changes, all columns should be re-encoded.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER, val TEXT);
         INSERT INTO t VALUES (1, 'a');
         INSERT INTO t VALUES (2, 'b');",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();

    // Add a row
    orig.execute_batch("INSERT INTO t VALUES (3, 'c');")
        .unwrap();

    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();

    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg2, &hatched, &HatchOptions::default()).unwrap();
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn incremental_lay_new_table() {
    // Tables only in SQLite should be encoded from scratch.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t1 (id INTEGER);
         INSERT INTO t1 VALUES (1);",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();

    // Add a new table
    orig.execute_batch(
        "CREATE TABLE t2 (name TEXT);
         INSERT INTO t2 VALUES ('hello');",
    )
    .unwrap();

    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();

    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg2, &hatched, &HatchOptions::default()).unwrap();
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn incremental_lay_dropped_table() {
    // Tables only in the egg (not in SQLite) should be dropped.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t1 (id INTEGER);
         INSERT INTO t1 VALUES (1);
         CREATE TABLE t2 (name TEXT);
         INSERT INTO t2 VALUES ('hello');",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();

    // Drop t2 from SQLite
    orig.execute_batch("DROP TABLE t2;").unwrap();

    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();

    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg2, &hatched, &HatchOptions::default()).unwrap();
    assert_databases_equal(&orig, &hatched);

    // t2 should not exist in the hatched DB
    let table_names = eggdb_io::sqlite_utils::get_table_names(&hatched).unwrap();
    assert!(!table_names.contains(&"t2".to_string()));
}

#[test]
fn incremental_lay_ddl_change() {
    // When DDL changes (e.g. column added), entire table is re-encoded.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER);
         INSERT INTO t VALUES (1);
         INSERT INTO t VALUES (2);",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();

    // Recreate with different DDL
    orig.execute_batch(
        "DROP TABLE t;
         CREATE TABLE t (id INTEGER, extra TEXT);
         INSERT INTO t VALUES (1, 'x');
         INSERT INTO t VALUES (2, 'y');",
    )
    .unwrap();

    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();

    let hatched = Connection::open_in_memory().unwrap();
    hatch(&egg2, &hatched, &HatchOptions::default()).unwrap();
    assert_databases_equal(&orig, &hatched);
}

#[test]
fn incremental_lay_validates() {
    // The incremental egg should pass all validation checks.
    let orig = Connection::open_in_memory().unwrap();
    orig.execute_batch(
        "CREATE TABLE t (id INTEGER, val TEXT, score REAL);
         INSERT INTO t VALUES (1, 'a', 1.5);
         INSERT INTO t VALUES (2, 'b', 2.5);
         INSERT INTO t VALUES (3, NULL, NULL);",
    )
    .unwrap();

    let egg1 = lay(&orig, &LayOptions::default()).unwrap();
    let egg2 = eggdb_io::writer::incremental_lay(&orig, &egg1, &LayOptions::default()).unwrap();

    let results = eggdb_core::validate(&egg2).unwrap();
    for r in &results {
        assert!(r.passed, "validation failed: {} - {}", r.check, r.detail);
    }
}
