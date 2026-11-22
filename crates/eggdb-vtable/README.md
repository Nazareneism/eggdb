# eggdb-vtable

SQLite virtual table extension for querying `.egg` files directly. Compiles as a
loadable `.so`/`.dylib` that any SQLite client can load, or as an `rlib` for
embedding in Rust applications.

## Usage

### As a Loadable Extension

```bash
# Build the extension
cargo build --release -p eggdb-vtable --features loadable_extension
# Output: target/release/libeggdb_vtable.dylib (macOS) or .so (Linux)
```

```sql
-- From any SQLite client
.load target/release/libeggdb_vtable

CREATE VIRTUAL TABLE users USING egg('data.egg', 'users');
CREATE VIRTUAL TABLE orders USING egg('data.egg', 'orders');

-- Column pruning: only 'status' is decompressed
SELECT count(*) FROM users WHERE status = 'active';

-- Joins work normally — SQLite handles the join logic
SELECT u.name, sum(o.total)
FROM users u JOIN orders o ON u.id = o.user_id
GROUP BY u.name;
```

### Writing

Tables are read-only unless created with `mode=rw`:

```sql
CREATE VIRTUAL TABLE users USING egg('data.egg', 'users', 'mode=rw');

INSERT INTO users (email, name) VALUES ('new@example.com', 'New');
UPDATE users SET name = 'Renamed' WHERE id = 2;
DELETE FROM users WHERE id = 3;

BEGIN; ...; ROLLBACK;   -- undoes egg writes too
```

- Writes go to the write log (`data.egg.wal`); the `.egg` is never modified in place.
  Run `egg compact data.egg` to fold them in.
- The original table's rules are enforced exactly as SQLite does (NOT NULL, UNIQUE,
  PRIMARY KEY, CHECK, type affinity, INTEGER PRIMARY KEY assignment, DEFAULTs).
- One SQL transaction is one atomic log frame, even across several egg tables on the
  same connection. One writer at a time; others get a "busy" error.
- Rowids are physical positions: stable until `egg compact`. Explicit rowids in
  INSERT and rowid changes in UPDATE are rejected.
- Limits: no writes to encrypted eggs; conflict clauses (`OR REPLACE`, `ON CONFLICT`)
  act as ABORT; an explicit NULL inserted into a column with a DEFAULT gets the default;
  inside `BEGIN ... COMMIT`, a multi-row statement that fails partway keeps the rows it
  already wrote (no `xSavepoint` support in rusqlite).

### Encrypted Files

```sql
CREATE VIRTUAL TABLE users USING egg('encrypted.egg', 'users', 'passphrase=my_passphrase');
-- The legacy form with a bare third argument also works:
CREATE VIRTUAL TABLE users USING egg('encrypted.egg', 'users', 'my_passphrase');
```

### From Rust Code

```rust
use eggdb_vtable::register_egg_module;

let conn = rusqlite::Connection::open_in_memory()?;
register_egg_module(&conn)?;
conn.execute("CREATE VIRTUAL TABLE t USING egg('data.egg', 'mytable')", [])?;
```

## How It Works

### Column Pruning

The vtable tracks which columns SQLite actually requests. Only those columns are
decompressed. A query touching 2 columns of a 20-column table decompresses ~10%
of the data.

The `col_used` bitmask covers the first 64 columns. Columns beyond index 64 are
always decoded (SQLite API limitation).

### Filter Push-Down

When SQLite passes WHERE constraints, the vtable pushes them down to the encoding
layer via `filter_indices()`:

- **Const** columns: one comparison, return all or nothing
- **Dict** columns: resolve filter value to dict index, scan integers
- **Delta** columns: binary search on accumulated values (monotonic)
- **Raw** columns: full scan

All filters use `set_omit(false)` so SQLite double-checks results for safety.
Cross-type comparisons (e.g., Integer column vs Real value) skip push-down.

### Cost Estimation

`xBestIndex` reports cost estimates to SQLite's query planner based on encoding type:
- Const with equality: near-zero cost
- Dict with equality: proportional to dictionary size
- Delta with range: logarithmic (binary search)
- Raw: proportional to row count

## Modules

| File | Purpose |
|------|---------|
| `lib.rs` | Module exports, feature-gated `sqlite3_eggvt_init` / `sqlite3_eggdbvtable_init` entry points |
| `vtab.rs` | `EggTab` (VTab, UpdateVTab, TransactionVTab), `EggCursor`, xConnect/xBestIndex/xFilter/xUpdate |
| `args.rs` | `USING egg(...)` argument parsing (`mode=`, `passphrase=`, legacy positional passphrase) |
| `shared.rs` | Per-connection state shared by all tables of one egg: snapshot, transaction, validators |
| `constraints.rs` | Write validation through a scratch in-memory SQLite (affinity, NOT NULL, CHECK, UNIQUE, DEFAULT) |
| `column_cache.rs` | Lazy column decompression with caching |
| `error.rs` | `VtableError` enum mapping to SQLite error codes |

## Architecture

- Depends on `eggdb-core` and `eggdb-store` (not `eggdb-io`)
- `EggTab` holds `Arc<Mutex<SharedEgg>>`, shared by every egg table on the same connection
  and file (registered through the module's per-connection aux data)
- Scans merge logged changes over main-file rows; encoding-level filter masks are widened
  so changed rows are never missed (SQLite re-checks every row: `omit = false`)
- Uses standalone `read_column_block_from_parts()` and `decode_column_from_parts()` from eggdb-core
- `idx_str` format for passing filter info: `cols:<hex>|<col>:<op>;<col>:<op>...`

## Build Targets

```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

- **cdylib** — produces the loadable `.so`/`.dylib` for SQLite
- **rlib** — used by tests and the CLI binary

The `loadable_extension` feature gates the `sqlite3_eggvt_init` entry point. Without
it, use `register_egg_module()` to register the vtable from Rust code.

## Dependencies

```
eggdb-core   — Format reading, column decoding, encryption
rusqlite     — SQLite virtual table API (bundled, vtab feature)
bitvec       — Row masks for filter results
thiserror    — Error types
```

## Testing

```bash
cargo test -p eggdb-vtable
```

Tests include:
- Query results match direct SQLite on same data
- Column pruning (only requested columns decompressed)
- Filter push-down for each encoding type
- Multiple virtual tables from same .egg file
- Joins between egg vtables and regular SQLite tables
- Encrypted file access with correct/wrong passphrase
