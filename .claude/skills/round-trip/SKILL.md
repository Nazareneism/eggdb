---
name: round-trip
description: Test the full lay → hatch round-trip. Converts a SQLite DB to .egg and back, then compares row-by-row.
disable-model-invocation: true
allowed-tools: Bash(cargo run *), Bash(sqlite3 *), Bash(diff *), Bash(ls *), Bash(rm *), Read, Grep, Glob
argument-hint: <sqlite-db-path>
---

Test the EggDB round-trip: SQLite → .egg → SQLite, then verify data integrity.

## Arguments

- `$ARGUMENTS` must be a path to a SQLite database file.
- If no argument is given, create a small test database with diverse data types.

## Steps

### 1. Setup

If no SQLite DB was provided, create one:
```
sqlite3 /tmp/eggdb_roundtrip_test.db <<'SQL'
CREATE TABLE test_data (
  id INTEGER PRIMARY KEY,
  name TEXT,
  score REAL,
  active INTEGER,
  data BLOB,
  notes TEXT
);
INSERT INTO test_data VALUES (1, 'Alice', 95.5, 1, X'DEADBEEF', 'first');
INSERT INTO test_data VALUES (2, 'Bob', NULL, 0, NULL, NULL);
INSERT INTO test_data VALUES (3, 'Charlie', 87.3, 1, X'CAFE', 'third');
INSERT INTO test_data VALUES (4, NULL, 0.0, NULL, X'', 'fourth');
INSERT INTO test_data VALUES (5, 'Eve', 100.0, 1, NULL, 'fifth');
SQL
```

### 2. Lay (compress)

Run: `cargo run -- lay <input.db> /tmp/eggdb_roundtrip_test.egg`

Report: file sizes of input and output, compression ratio.

### 3. Hatch (decompress)

Run: `cargo run -- hatch /tmp/eggdb_roundtrip_test.egg /tmp/eggdb_roundtrip_hatched.db`

### 4. Compare

For each table in the original database:
1. Dump rows from original: `sqlite3 <original> "SELECT * FROM <table> ORDER BY rowid;"`
2. Dump rows from hatched: `sqlite3 /tmp/eggdb_roundtrip_hatched.db "SELECT * FROM <table> ORDER BY rowid;"`
3. Compare the outputs.

### 5. Report

- If all rows match: "Round-trip verified: N tables, M total rows, all identical."
- If any mismatch: show the table name, row number, and differing values.
- Report compression ratio: `original_size / egg_size`.

### 6. Cleanup

Remove temporary files: `/tmp/eggdb_roundtrip_test.*`, `/tmp/eggdb_roundtrip_hatched.db`.
