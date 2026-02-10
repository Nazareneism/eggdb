# eggdb-io

SQLite-to-.egg conversion library. Handles the `lay` (compress) and `hatch` (restore)
pipelines, including schema introspection, row iteration, and bulk insert.

## Modules

| File | Purpose |
|------|---------|
| `writer.rs` | `lay()` / `lay_file()` — SQLite -> .egg pipeline |
| `reader.rs` | `hatch()` / `hatch_file()` — .egg -> SQLite pipeline |
| `sqlite_utils.rs` | Schema introspection, type inference, identifier quoting |

## Lay Pipeline

`lay()` reads a SQLite database and produces an .egg file:

1. Introspect schema (tables, columns, types, DDL, indexes)
2. Read all rows, extract typed values
3. Analyze each column (compute stats, select encoding)
4. Encode, compress (zstd), assemble .egg format via `eggdb-core`

```rust
use eggdb_io::{lay, lay_file, LayOptions};

// In-memory
let egg_bytes = lay(&conn, &LayOptions::default())?;

// To disk (patches original_size in header)
lay_file(&conn, "output.egg", &LayOptions::default())?;
```

### LayOptions

| Option | Default | Effect |
|--------|---------|--------|
| `zstd_level` | 3 | Compression level for all columns |
| `adaptive_compression` | false | Per-column zstd levels based on encoding type |
| `optimize_order` | false | Sort rows by lowest-cardinality column |
| `streaming` | false | Two-pass column-at-a-time (lower memory) |
| `encryption_passphrase` | None | Encrypt yolk blocks with AES-256-GCM |

### Streaming Mode

`lay_streaming()` processes one column at a time instead of loading all data into memory:

- **Pass 1**: Stream rows to accumulate `StreamingColumnStats` per column
- **Pass 2**: Read one column at a time, encode, compress, write, then free before next column
- Peak memory: O(rows x 1) instead of O(rows x columns)

### Incremental Lay

`incremental_lay()` compares each column against an existing .egg file:

- Unchanged columns are copied verbatim as `PreCompressedBlock` (no re-encoding)
- Changed columns are re-analyzed and re-encoded from scratch
- DDL or row-count mismatch triggers full table re-encode
- New tables are encoded from scratch; removed tables are skipped

## Hatch Pipeline

`hatch()` restores an .egg file back to SQLite:

1. Parse .egg (via `eggdb-core` reader)
2. Create tables from stored DDL
3. Bulk insert decoded rows (inside a transaction)
4. Recreate indexes
5. Verify row counts

```rust
use eggdb_io::{hatch, hatch_file, HatchOptions};

// In-memory
hatch(&egg_bytes, &conn, &HatchOptions::default())?;

// Partial hatch (specific tables only)
let opts = HatchOptions { tables: Some(vec!["users".into(), "orders".into()]) };
hatch(&egg_bytes, &conn, &opts)?;
```

## SQLite Utilities

`sqlite_utils.rs` provides helpers used by both lay and hatch:

- `get_table_names()` — list user tables (excludes sqlite_ internal tables)
- `get_columns()` — column names and declared types
- `get_table_ddl()` — original CREATE TABLE statement
- `get_index_definitions()` — CREATE INDEX statements
- `get_row_count()` — row count for a table
- `infer_sqlite_type()` — map SQLite value affinity to `SqliteType` enum
- `quote_identifier()` — escape identifiers for SQL injection safety

## Dependencies

```
eggdb-core   — Format, encodings, value types
rusqlite     — SQLite access (bundled, no system dependency)
bitvec       — Null bitmaps
thiserror    — Error types
```

## Testing

```bash
cargo test -p eggdb-io
```

Tests include:
- Round-trip: lay -> hatch -> row-by-row comparison for 11 different table types
- Empty tables, single-row tables, all-null columns, unicode text, blobs
- Streaming mode produces valid output
- Incremental lay copies unchanged blocks, re-encodes changed columns
- Row reordering sets header flag correctly
