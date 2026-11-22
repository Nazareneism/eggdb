# EggDB

A lifecycle-aware compressed database engine. Converts SQLite databases into highly
compressed `.egg` files using columnar encoding, and provides a SQLite virtual table
extension to query them directly.

```
SQLite (1.5 GB) ──egg lay──> .egg (293 MB, 5.1x)    # default
                ──egg lay──> .egg (177 MB, 8.5x)    # --adaptive-compression --zstd-level 19
                 <──egg hatch──
```

## When to Use EggDB

**Archiving databases you still need to query.** You have a 10 GB SQLite database
of historical records. You need it once a month for reports. EggDB compresses it
to 1-2 GB while keeping it queryable — no decompression step, no ETL pipeline.

**Shipping data with your application.** Bundle a compressed `.egg` file instead of
a raw SQLite database. Your app loads the virtual table extension and queries
directly. Smaller downloads, same SQL interface.

**Sharing datasets.** Distribute a `.egg` file instead of a CSV dump or database
backup. Recipients query it with `egg query`, `egg shell`, Python, or any SQLite
client. Schema, types, and indexes are all preserved.

**Cold storage with instant access.** Move infrequently-accessed tables to `.egg`
format. When you need them, query in place or hatch back to SQLite. No waiting
for a full restore.

**Working with large datasets on constrained machines.** The `--streaming` mode
processes one column at a time, keeping peak memory at O(rows x 1) instead of
O(rows x columns). Column pruning means queries only decompress what they touch.

### Examples

| Scenario | Before | After |
|----------|--------|-------|
| Analytics database (historical orders, 3 years) | 8.2 GB SQLite | 1.4 GB .egg |
| Bible translation corpus (11 tables, 1.5M rows) | 1.5 GB SQLite | 177 MB .egg |
| User activity logs (append-only, sorted timestamps) | 5.1 GB SQLite | 620 MB .egg |
| Reference data (country codes, currencies, configs) | 45 MB SQLite | 3.8 MB .egg |

Compression ratios depend on data characteristics. Low-cardinality and sorted columns
compress best. High-cardinality text (UUIDs, emails, JSON blobs) relies on zstd alone.

## Quick Start

```bash
# Build
cargo build --release

# Compress a SQLite database
egg lay mydata.sqlite -o mydata.egg

# Query it directly (no decompression needed)
egg query mydata.egg "SELECT count(*) FROM users WHERE status = 'active'"

# Or use YQL, a friendlier query syntax
egg query mydata.egg "count users where status = 'active'"

# Edit it directly (no decompression needed)
egg query mydata.egg "INSERT INTO users (email, name) VALUES ('new@example.com', 'New')"
egg query mydata.egg "UPDATE users SET status = 'inactive' WHERE id = 42"
egg query mydata.egg "DELETE FROM users WHERE id = 7"

# Fold the edits into the compressed file whenever convenient
egg compact mydata.egg

# Restore back to SQLite
egg hatch mydata.egg -o restored.sqlite
```

## How It Works

EggDB analyzes each column in a SQLite database and selects the best encoding strategy:

| Encoding | When Used | Example |
|----------|-----------|---------|
| **Const** | Entire column is one value | `status` = always "active" |
| **BoolBitmap** | Boolean / 2 distinct values | `is_admin` (true/false) |
| **Delta** | Sorted numeric sequences | Auto-increment `id` columns |
| **RLE** | Many consecutive duplicates | Sorted `country` column |
| **Dict** | Low cardinality (< 1% distinct) | `status` with 5 possible values |
| **Raw** | High cardinality (fallback) | `email`, `uuid` |

After encoding, each column block is zstd-compressed and stored in a compact binary
format with per-column offsets for selective decompression.

## CLI Commands

```bash
egg lay <source.sqlite> [-o output.egg]     # Compress SQLite to .egg
egg hatch <file.egg> [-o output.sqlite]     # Restore .egg to SQLite
egg candle <file.egg>                       # Quick metadata summary
egg info <file.egg> [--table name]          # Per-column compression stats
egg validate <file.egg>                     # Verify all checksums
egg schema <file.egg>                       # Print original DDL
egg crack <file.egg> [--format csv|json]    # Export without SQLite rebuild
egg query <file.egg> "<yql or sql>"         # Query or edit (INSERT/UPDATE/DELETE)
egg shell <file.egg>                        # Interactive YQL/SQL REPL (can edit too)
egg compact <file.egg>                      # Fold pending edits into the .egg
egg diff <a.egg> <b.egg>                    # Compare two .egg files
egg relay <file.egg> [-o output.egg]        # Re-analyze and re-encode
egg web <file.egg> [--port 8080]            # Browse in a local web viewer
```

## Editing an Egg

You can insert, update, and delete rows without restoring the file to SQLite.

- **Edits are saved immediately** to a small change journal next to your file
  (`data.egg` gets a `data.egg.wal`). The compressed `data.egg` itself is never
  modified while you edit, so a crash or power cut cannot damage it.
- **Every command sees your edits**: `query`, `crack`, `hatch`, `info`, `candle`,
  `diff`, `relay`, and the web viewer. `egg candle` shows how many changes are
  pending.
- **`egg compact` folds the journal into a fresh `.egg`** and removes it,
  restoring full compression. Tables you did not edit are copied as-is.
- **The table's rules are enforced** exactly like SQLite: `NOT NULL`, `UNIQUE`,
  `PRIMARY KEY`, and `CHECK` violations are rejected, new rows get the next
  `INTEGER PRIMARY KEY`, and omitted columns get their `DEFAULT`.
- **Transactions work**: `BEGIN ... ROLLBACK` undoes edits, and one transaction
  is saved all-or-nothing, even across several tables.
- **To discard all pending edits**, move the `.wal` file aside (only before
  running `egg compact`).

Current limits: encrypted eggs are read-only; one writer at a time (a second
writer gets a "being written by another process" error); `INSERT OR REPLACE`
and `ON CONFLICT` act like a plain failing insert; an explicit `NULL` inserted
into a column that has a `DEFAULT` gets the default (SQLite does not tell
virtual tables which columns an `INSERT` named). Design and crash-safety
details: [`docs/v0.3_plan.md`](docs/v0.3_plan.md).

### Advanced Options

```bash
# Encrypt with AES-256-GCM (prompts for passphrase)
egg lay mydata.sqlite --encrypt -o encrypted.egg
egg hatch encrypted.egg --passphrase "secret" -o restored.sqlite

# Optimize compression with row reordering
egg lay mydata.sqlite --optimize-order

# Adaptive per-column zstd levels
egg lay mydata.sqlite --adaptive-compression

# Low-memory streaming mode (column-at-a-time)
egg lay mydata.sqlite --streaming

# Re-encode only changed columns
egg lay modified.sqlite --incremental existing.egg -o updated.egg

# Restore only specific tables
egg hatch mydata.egg --tables users,orders -o partial.sqlite
```

## Python

### Install

```bash
uv pip install eggdb
```

### Opening and querying

```python
import eggdb

egg = eggdb.open("data.egg")

# List tables
print(egg.tables)
# ['users', 'orders', 'products']

# Query with SQL — returns a list of dicts
rows = egg.query("SELECT name, email FROM users WHERE status = 'active' LIMIT 10")
for row in rows:
    print(row)
# {'name': 'Alice', 'email': 'alice@example.com'}
# {'name': 'Bob', 'email': 'bob@example.com'}

# Query with YQL (a friendlier syntax)
rows = egg.query("count orders by status")
for row in rows:
    print(row)
# {'status': 'shipped', 'count': 4200}
# {'status': 'pending', 'count': 318}
```

### File summary and metadata

```python
# File-level summary
info = egg.summary()
print(info)
# {
#     'file_size': 185338265,
#     'original_size': 1580000000,
#     'compression_ratio': '8.5x',
#     'table_count': 11,
#     'total_rows': 1497017,
#     'version': 1,
#     'encrypted': False,
#     'row_order_preserved': True
# }

# Per-column detail for a table
cols = egg.columns("users")
for col in cols:
    print(f"{col['name']:20s} {col['encoding']:12s} {col['cardinality']} distinct")
# id                   Delta        50000 distinct
# name                 Dict         12847 distinct
# email                Raw          49993 distinct
# status               Dict         3 distinct
# created_at           Delta        48219 distinct
```

### Encrypted files

```python
egg = eggdb.open("encrypted.egg", passphrase="secret")
rows = egg.query("SELECT * FROM users LIMIT 5")
```

### Joins and complex queries

Since queries run through SQLite, everything SQLite supports works:

```python
rows = egg.query("""
    SELECT u.name, sum(o.total) as revenue
    FROM users u
    JOIN orders o ON u.id = o.user_id
    WHERE o.created_at > '2024-01-01'
    GROUP BY u.name
    ORDER BY revenue DESC
    LIMIT 10
""")
```

### How it works under the hood

When you call `egg.query()`, eggdb:

1. Opens an in-memory SQLite connection
2. Registers the egg virtual table module (compiled from Rust)
3. Creates virtual tables for each table in the `.egg` file
4. Executes your SQL through SQLite's query optimizer
5. Returns results as Python dicts

Only the columns referenced in your query are decompressed (column pruning).
A query touching 2 columns of a 20-column table decompresses ~10% of the data.

## SQLite Virtual Table

Query `.egg` files directly from any SQLite client without decompressing:

```bash
# Build the loadable extension
cargo build --release -p eggdb-vtable --features loadable_extension
```

```sql
.load target/release/libeggdb_vtable

CREATE VIRTUAL TABLE users USING egg('data.egg', 'users');
CREATE VIRTUAL TABLE orders USING egg('data.egg', 'orders');

-- Tables are read-only unless created with mode=rw
CREATE VIRTUAL TABLE notes USING egg('data.egg', 'notes', 'mode=rw');
INSERT INTO notes (body) VALUES ('edited from any SQLite client');

-- Encrypted eggs: egg('data.egg', 'users', 'passphrase=...')

-- Only the 'status' column is decompressed (column pruning)
SELECT count(*) FROM users WHERE status = 'active';

-- Joins, aggregations, window functions -- all handled by SQLite
SELECT u.name, sum(o.total)
FROM users u JOIN orders o ON u.id = o.user_id
GROUP BY u.name
ORDER BY sum(o.total) DESC
LIMIT 10;
```

The virtual table supports column pruning (only decompresses columns you query)
and filter push-down (encoding-aware filtering before full decode).

Works from any language with SQLite bindings:

```python
# Python (without the eggdb package — just sqlite3 + the extension)
import sqlite3
conn = sqlite3.connect(":memory:")
conn.enable_load_extension(True)
conn.load_extension("libeggdb_vtable")
conn.execute("CREATE VIRTUAL TABLE users USING egg('data.egg', 'users')")
rows = conn.execute("SELECT * FROM users LIMIT 5").fetchall()
```

```javascript
// Node.js (better-sqlite3)
const db = require('better-sqlite3')(':memory:');
db.loadExtension('libeggdb_vtable');
db.exec("CREATE VIRTUAL TABLE users USING egg('data.egg', 'users')");
const rows = db.prepare('SELECT * FROM users LIMIT 5').all();
```

## YQL (Yolk Query Language)

A friendlier query syntax for the terminal. Single-table only -- joins require SQL.

```
show users where status = "active" sort name limit 10
  -> SELECT * FROM users WHERE status = 'active' ORDER BY name ASC LIMIT 10

count orders by status
  -> SELECT status, count(*) AS count FROM orders GROUP BY status

avg orders.total where status = "done" by month(created_at)
  -> SELECT strftime('%m', created_at) AS month, avg(total) AS avg_total
     FROM orders WHERE status = 'done' GROUP BY strftime('%m', created_at)

find products where price between 10 and 50
  -> SELECT * FROM products WHERE price BETWEEN 10 AND 50
```

## .egg File Format

```
+--------------------------------------+
| SHELL   (64-byte header)             |  Magic, version, offsets, CRC32C
+--------------------------------------+
| ALBUMEN (metadata, bincode)          |  Table schemas, column encodings,
|                                      |  dictionaries, min/max stats
+--------------------------------------+
| YOLK    (column data, zstd)          |  Null bitmaps + encoded values,
|                                      |  independently seekable per column
+--------------------------------------+
| MEMBRANE (integrity)                 |  Per-block CRC32C, blake3 hash
+--------------------------------------+
```

- Fixed 64-byte header -- validate any .egg by reading 64 bytes
- Per-column offsets enable selective decompression
- CRC32C checksums (hardware-accelerated) + blake3 metadata hash
- Optional AES-256-GCM encryption on yolk blocks (metadata stays readable)

## Architecture

```
crates/
  eggdb-core/     Zero IO. Format, encodings, value types, encryption.
  eggdb-io/       SQLite <-> .egg conversion (lay, hatch). Depends on rusqlite.
  eggdb-store/    Write log (.egg.wal): snapshots, crash-safe locked writes, compaction.
  eggdb-vtable/   SQLite virtual table extension (cdylib). Column pruning, filter push-down.
  eggdb-yql/      YQL -> SQL transpiler. Pure string transform, no external deps.
src/              CLI binary (clap). Thin wrappers around crate APIs.
python/           Python package (PyO3 + maturin). uv pip install eggdb.
```

Crate boundaries are strict:
- `eggdb-core` has no IO dependencies (no rusqlite, no std::fs)
- `eggdb-yql` has no dependencies on other eggdb crates
- `eggdb-store` depends on `eggdb-core` only (no rusqlite)
- `eggdb-vtable` depends on `eggdb-core` and `eggdb-store` (not eggdb-io)

## Benchmarks

Measured on `bible.eng.db` (1.5 GB SQLite, 11 tables, 1,497,017 rows). Apple Silicon, release build.

| Configuration | .egg Size | Ratio | Lay Time |
|---------------|-----------|-------|----------|
| Default (zstd 3) | 293 MB | **5.1x** | 12.2s |
| `--adaptive-compression` | 254 MB | **5.9x** | 21.7s |
| `--zstd-level 19` | 177 MB | **8.5x** | 7m 10s |
| `--zstd-level 19 --adaptive-compression` | 177 MB | **8.5x** | 5m 40s |

| Operation | Time |
|-----------|------|
| Hatch (.egg -> SQLite) | 6.3s |
| Validate (176 checks) | 0.09s |
| Test suite (unit + property + integration) | 530 tests |
| Fuzz (egg reader + YQL parser + write log) | ~9M inputs, 0 crashes |

Write support on the same dataset. Each edit is one `egg query` command; times
include starting the program and reading the 293 MB `.egg`.

| Operation | Result |
|-----------|--------|
| INSERT 1 row, UPDATE 66 rows, or DELETE 3,612 rows | 0.1-0.2s each |
| UPDATE 1 row in a 1.2M-row table | 1.2s |
| Bulk `INSERT ... SELECT` of 31,086 rows | 1.5s (15 MB write log) |
| `egg validate` with those changes pending (177 checks) | 0.17s |
| Hatch with changes pending | 8.3s |
| `egg compact` (5 changed tables re-encoded, 6 copied as-is) | 5.2s, 293 MB -> 295 MB |
| Hatch after compaction | 7.0s |

The same edits were applied to a plain SQLite copy of the database. Hatched
before and after compaction, all 11 tables (1,524,492 rows) were identical to it
row by row, including value types.

Encoding micro-benchmarks (100K values, Criterion):

| Encoding | Encode | Decode |
|----------|--------|--------|
| Const | 17 ns | 251 us |
| BoolBitmap | 384 us | 268 us |
| Delta | 112 us | 150 us |
| Rle | 189 us | 261 us |
| Dict | 1.60 ms | 2.18 ms |
| Raw | 3.53 ms | 3.44 ms |

## Security

Encryption uses AES-256-GCM on yolk (data) blocks. Metadata (albumen) is not encrypted,
so `candle`, `info`, and `schema` work without a passphrase.

- Key derivation: Argon2id (m=19456/19 MiB, t=2, p=1)
- Salt: 16 bytes, random per file
- Nonce: 12 bytes, random per block
- Wrong passphrase fails fast with a clear error

## Building

```bash
cargo build --release          # CLI binary
cargo test --workspace         # Run all 470 tests
cargo clippy -- -D warnings    # Lint
cargo bench -p eggdb-core      # Encoding benchmarks
cargo +nightly fuzz run fuzz_egg_reader   # Fuzz the .egg reader
cargo +nightly fuzz run fuzz_yql_parser   # Fuzz the YQL parser
```

Requires Rust 2021 edition. SQLite is bundled via rusqlite -- no system dependencies.
Fuzz testing requires the nightly toolchain (`rustup install nightly`).

## Roadmap

### Done (v0.1)

- 6 columnar encodings with automatic selection
- Full SQLite round-trip (lay/hatch) with fidelity verification
- 12 CLI commands (lay, hatch, candle, info, validate, schema, crack, query, shell, diff, relay, web)
- SQLite virtual table extension with column pruning and filter push-down
- YQL query language transpiler
- AES-256-GCM encryption with Argon2id key derivation
- Streaming lay, incremental lay, adaptive compression, row reordering
- Parallel column decoding via rayon (multi-core speedup on hatch/query/crack)
- Web viewer for browsing .egg files in a browser
- Python bindings (`uv pip install eggdb`)
- 470 tests, fuzz testing, CI/CD

### Next (v0.2) — Query Performance + Distribution

- **Zone maps** — store min/max per chunk of N rows; skip entire chunks during range
  queries (`WHERE id > 50000` skips chunks whose max is below 50000). Cheap to store,
  big speedup for range scans on large files.
- **Predicate push-down improvements** — range scans on Delta columns, dict-aware
  filtering, leverage zone maps for early chunk elimination
- **npm package** — distribute the SQLite extension for Node.js/Bun/Deno
- **Homebrew formula** — `brew install eggdb`

### Done (v0.3) — Write Support

Built ahead of v0.2. Columnar formats are read-optimized, so writes use an
append + compaction model (design: [`docs/v0.3_plan.md`](docs/v0.3_plan.md)):

- **Write log** — INSERT/UPDATE/DELETE are appended to a crash-safe sidecar
  (`data.egg.wal`); the compressed `.egg` is never modified in place
- **Writable virtual table** — `egg query`, `egg shell`, or any SQLite client with
  `mode=rw`; transactions, and the source table's constraints are enforced
- **`egg compact`** — folds the log into a new `.egg`, copying unchanged tables as-is
- **Every command sees pending changes** — hatch, crack, info, candle, diff,
  validate, relay, web viewer, Python bindings

Still to come: schema evolution (add/drop/rename columns), writes to encrypted
eggs, automatic compaction, and writes from the Python package.

### Exploring

These are ideas under consideration, not committed to a timeline:

- **Parquet export** — `egg crack --format parquet` for interop with Arrow/DuckDB/Spark
- **WASM build** — read .egg files in the browser, power the web viewer with
  client-side decoding instead of a local server
- **Multi-file queries** — query a directory of .egg files as one logical table,
  useful for time-partitioned datasets (`2024-01.egg`, `2024-02.egg`, ...)
- **Column-level encryption** — encrypt only sensitive columns (PII, financials)
  while leaving others queryable without a passphrase
- **Remote storage** — read .egg files directly from S3/GCS/Azure Blob using
  HTTP range requests and per-column offsets (no full download needed)
- **Change tracking** — `egg diff` already compares two files; extend this to
  produce a structured changeset (added/modified/deleted rows)

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on:

- Project structure and crate boundaries
- Coding standards and naming conventions
- Testing requirements
- How to add new encodings, CLI commands, or format changes

```bash
# Quick pre-PR check
cargo fmt && cargo clippy -- -D warnings && cargo test --workspace
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT), at your option.
