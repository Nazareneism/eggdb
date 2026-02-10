# EggDB — A Lifecycle-Aware Compressed Database (Rust)

## Concept

A database engine inspired by biological eggs. Data is stored in a highly compressed
columnar format (the `.egg` file) that "hatches" into a fast, queryable runtime and
"lays" back down to a compact dormant state.

Two interfaces, one language:
1. **CLI tool** — `egg lay`, `egg hatch`, `egg query`, etc.
2. **SQLite virtual table** — `CREATE VIRTUAL TABLE t USING egg('file.egg', 'table')`
   and query with standard SQL. SQLite is the query engine. No custom executor needed.

A lightweight query syntax called **YQL** (Yolk Query Language) exists as CLI sugar.
It transpiles to SQL and runs through the virtual table. Not a query engine — just
a friendlier way to type queries in a terminal.

This is a side project. No deadline. Build it right.

---

## File Format: `.egg`

```
┌──────────────────────────────────────┐
│ SHELL (header, fixed 64 bytes)       │
│  Byte offsets (all little-endian):    │
│  [0..3]   magic: 0x45 0x47 0x47     │
│  [3]      version: u8               │
│  [4..8]   table_count: u32          │
│  [8..10]  flags: u16                │
│  [10..18] created_at: u64           │
│  [18..26] original_size: u64        │
│  [26..34] albumen_offset: u64       │
│  [34..42] yolk_offset: u64          │
│  [42..50] membrane_offset: u64      │
│  [50..54] checksum: u32 (CRC32C)    │
│  [54..64] reserved: zero-padded     │
│                                      │
│  version: 1 = bincode LE + varint   │
│    Unknown versions → reject file    │
│  flags (runtime options only):       │
│    bit 0: has_encryption             │
│    bit 1: row_order_preserved        │
│    bits 2-15: reserved (must be 0)   │
├──────────────────────────────────────┤
│ ALBUMEN (metadata)                   │
│  Serialized with bincode (compact,   │
│  zero-copy deserializable in Rust)   │
│                                      │
│  Vec<TableMeta>:                     │
│    table_name: String                │
│    row_count: u64                    │
│    original_ddl: String              │
│    columns: Vec<ColumnMeta>          │
│    index_definitions: Vec<String>    │
│                                      │
│  ColumnMeta:                         │
│    name: String                      │
│    sqlite_type: SqliteType enum      │
│    encoding: Encoding enum           │
│    cardinality: u64                  │
│    null_count: u64                   │
│    yolk_offset: u64                  │
│    yolk_length: u64                  │
│    uncompressed_size: u64            │
│    dictionary: Option<Vec<Value>>    │
│    min_value: Option<Value>          │
│    max_value: Option<Value>          │
├──────────────────────────────────────┤
│ YOLK (column data)                   │
│  For each table, for each column:    │
│    Block layout:                     │
│      null_bitmap: bitpacked bytes    │
│      encoded_values: encoding-       │
│        specific byte stream          │
│    Entire block zstd compressed      │
│    Seekable via albumen offsets      │
├──────────────────────────────────────┤
│ MEMBRANE (integrity)                 │
│  Vec<BlockChecksum>:                 │
│    table_index: u32                  │
│    column_index: u32                 │
│    crc32c: u32                       │
│  total_row_counts: Vec<u64>          │
│  albumen_hash: [u8; 32] (blake3)    │
└──────────────────────────────────────┘
```

### Format Version

**Version 1** defines:
- All multi-byte integers are **little-endian**
- Albumen is serialized with **bincode v1**, configured with **little-endian byte order**
  and **varint integer encoding** (not fixed-width)
- Readers encountering `version > 1` **must reject** the file with a clear error

The version byte is the serialization contract. Flags are reserved for runtime
options (encryption, row ordering) — not format/encoding details.

### Checksum Coverage

The header checksum (CRC32C at bytes [50..54]) covers the **entire file** with the
4 checksum bytes treated as `[0x00, 0x00, 0x00, 0x00]` during computation:

```
CRC32C(
  bytes[0..50]           // header before checksum field
  ++ [0x00, 0x00, 0x00, 0x00]  // checksum field zeroed
  ++ bytes[54..64]       // header after checksum field (reserved)
  ++ bytes[64..EOF]      // albumen + yolk + membrane
)
```

The **membrane block checksums** each cover the raw compressed bytes of a single
column block in the yolk (post-zstd, pre-decompression). These are redundant with
the file-level CRC but allow pinpointing which specific block is corrupt.

The **albumen hash** is blake3 of the serialized albumen bytes (the full bincode
output). This covers schema, encoding choices, dictionaries, and column stats.
Named `albumen_hash` (not "schema hash") because it covers all metadata, not just
the schema. Used to detect albumen corruption or tampering independently of the
file-level CRC.

### Null Bitmap Semantics

- Polarity: **1 = null, 0 = present**. Set bits are nulls.
- `encoded_values` **excludes nulls**. Non-null values are packed contiguously.
  The bitmap is required during decode to reconstruct row positions.
- All-null columns: `encoded_values` is empty (zero bytes), bitmap is all-ones,
  encoding is `Const` with value `Null`.

### Design Decisions

- **bincode for metadata** — Rust-native, fast deserialization with `#[derive(Serialize, Deserialize)]`.
  No schema negotiation, no type ambiguity. Faster than msgpack/JSON, smaller than JSON.
  Configuration pinned to little-endian + varint encoding for cross-platform portability.
  Tradeoff: not readable by other languages without a bincode parser. Acceptable because
  the CLI tools handle all human-facing output.
- **Per-column offsets** — every column block is independently seekable. The virtual table
  and `egg query` decompress only the columns they need.
- **CRC32C** — hardware-accelerated on modern CPUs via SSE 4.2. Faster than CRC32.
- **blake3 for albumen hash** — fast, collision-resistant. Computed over the serialized
  albumen bytes. Covers all metadata (schema + encodings + dicts + stats).
- **Fixed 64-byte header** — validate any .egg file by reading 64 bytes.
- **Flags field** — future-proofs the format for encryption, row ordering options, etc.

---

## Column Encodings

Six encoding strategies, selected automatically per column during `lay`:

### Encoding Enum

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Encoding {
    Const,       // Entire column is one value
    BoolBitmap,  // Boolean/2-value, bitpacked
    Delta,       // Monotonic numeric, store deltas as zigzag varints
    Rle,         // Consecutive duplicates, (value, run_length) pairs
    Dict,        // Low cardinality, value → index mapping
    Raw,         // High cardinality, rely on zstd only
}
```

### Selection Algorithm

```rust
fn select_encoding(stats: &ColumnStats) -> Encoding {
    if stats.distinct_count == 0 {
        return Encoding::Const; // all null
    }
    if stats.distinct_count == 1 {
        return Encoding::Const;
    }
    if stats.distinct_count == 2 && stats.is_boolean_like() {
        return Encoding::BoolBitmap;
    }
    if stats.is_numeric && stats.is_sorted {
        return Encoding::Delta;
    }
    if stats.consecutive_dup_ratio > 0.5 {
        return Encoding::Rle;
    }
    if stats.distinct_count as f64 / stats.row_count as f64 <= 0.01
        && stats.distinct_count <= 65_536 {
        return Encoding::Dict;  // cap dict at 65K entries to bound memory
    }
    Encoding::Raw
}
```

### Encoder Traits

`values` passed to `encode()` contain **only non-null values**, in order. The null
bitmap is provided separately. `decode()` returns non-null values; the caller
interleaves nulls using the bitmap.

```rust
trait ColumnEncoder {
    /// Encode non-null values. `values` excludes nulls (bitmap handled separately).
    fn encode(&self, values: &[Value], null_bitmap: &BitVec) -> Vec<u8>;
    /// Decode to non-null values. Caller uses null_bitmap to reconstruct full column.
    fn decode(&self, data: &[u8], null_bitmap: &BitVec, row_count: usize) -> Vec<Value>;

    /// Optional: filter without full decode. Returns matching row indices.
    /// Default implementation decodes then filters. Specialized encodings
    /// override for performance (e.g., Dict resolves filter to index first).
    fn filter_indices(
        &self,
        data: &[u8],
        null_bitmap: &BitVec,
        row_count: usize,
        op: FilterOp,
        value: &Value,
    ) -> BitVec {
        // default: decode all, then filter
    }
}
```

### Per-Encoding Details

**Const**: Store single `Value` + row count. Encode = 1 value. Decode = repeat it.
Filter = one comparison, return all or nothing. Essentially free.

**BoolBitmap**: Bitpack into bytes, 8 values per byte. Decode = shift and mask.
Filter = bitwise AND/OR on the bitmap directly.

**Delta**: Store base value + zigzag-varint-encoded deltas. Decode = prefix sum.
Filter on range = binary search on accumulated values (since monotonic).

**Rle**: Store Vec<(Value, u64)> pairs. Decode = expand runs. Filter = check
each run's value, skip non-matching runs entirely. Very fast for sorted columns
with large run lengths.

**Dict**: Build a `Vec<Value>` dictionary sorted by frequency. Encode values as
varint indices into the dictionary. Decode = index lookup. Filter = resolve the
filter value to its dict index first, then scan encoded indices (integer comparison
instead of string comparison).

**Raw**: Store values sequentially with length-prefixed encoding. Rely entirely
on zstd for compression. Used for high-cardinality text (emails, names, UUIDs).

---

## CLI

```bash
egg lay <source.sqlite> [-o output.egg]       # SQLite → .egg
egg hatch <file.egg> [-o output.sqlite]       # .egg → SQLite
egg candle <file.egg>                         # Quick metadata summary
egg crack <file.egg> [--format csv|json]      # Dump without SQLite rebuild
egg query <file.egg> "<yql or sql>"           # Query via virtual table
egg info <file.egg> [--table name]            # Per-column compression stats
egg validate <file.egg>                       # Verify all checksums
egg schema <file.egg>                         # Print original DDL
egg diff <a.egg> <b.egg>                      # Compare two eggs (schema + row counts)
egg relay <file.egg> [-o output.egg]          # Re-analyze and re-encode (optimize)
egg shell <file.egg>                          # Interactive YQL/SQL REPL
```

### How `egg query` Works

```
egg query data.egg "count orders by status"

Internally:
1. Parse input — is it YQL or SQL?
   - If it starts with a YQL command word (show, count, find, etc.) → YQL path
   - Otherwise → treat as raw SQL
2. If YQL: transpile to SQL
   "count orders by status"
   → "SELECT status, count(*) FROM orders GROUP BY status"
3. Open an in-memory SQLite connection
4. Load the egg virtual table extension
5. CREATE VIRTUAL TABLE orders USING egg('data.egg', 'orders')
6. Execute the SQL
7. Print results
8. Drop everything, done
```

No custom query executor. SQLite does the hard work. The virtual table just
streams decoded column values.

---

## SQLite Virtual Table

The core query interface. Everything flows through this.

### Usage

```sql
.load libeggvt

CREATE VIRTUAL TABLE users USING egg('/path/to/data.egg', 'users');
CREATE VIRTUAL TABLE orders USING egg('/path/to/data.egg', 'orders');

-- Column pruning: only decompresses 'status' column
SELECT count(*) FROM users WHERE status = 'active';

-- Joins work — SQLite handles the join logic
SELECT u.name, o.total
FROM users u JOIN orders o ON u.id = o.user_id
WHERE o.created_at > '2024-01-01'
ORDER BY o.total DESC
LIMIT 10;

-- Aggregations, subqueries, window functions — all SQLite
SELECT status, avg(total), count(*)
FROM orders
GROUP BY status
HAVING count(*) > 100;
```

### Implementation

Built in Rust using `rusqlite`'s virtual table API.

```rust
struct EggTab {
    egg_path: PathBuf,
    table_name: String,
    table_meta: TableMeta,
    // Column blocks are loaded lazily — only when queried
    column_cache: HashMap<usize, DecodedColumn>,
}

struct EggCursor {
    current_row: usize,
    row_count: usize,
    // Bitset of columns actually needed by this query
    required_columns: BitVec,
    // Only columns in required_columns are populated
    decoded_columns: Vec<Option<DecodedColumn>>,
    // Filter results — which rows passed WHERE conditions
    row_mask: Option<BitVec>,
}
```

Key methods:

- **xConnect** — open .egg, parse header + albumen, register column schema with SQLite.
  Does NOT decompress any yolk data yet.
- **xBestIndex** — report cost estimates to SQLite's planner:
  - CONST columns with equality filter: cost ≈ 0 (one comparison)
  - DICT columns with equality filter: cost ≈ dict_size (resolve + scan indices)
  - DELTA columns with range filter: cost ≈ log(row_count) (binary search)
  - RAW columns: cost ≈ row_count (full scan)
  - Also report which columns the vtable can handle via `filter_indices()`,
    so SQLite doesn't double-filter.
- **xFilter** — decompress only required columns (read offset from albumen, seek
  in file, decompress zstd block, decode encoding). If there's a usable constraint,
  call `filter_indices()` on the encoding to get a row mask.
- **xNext** — advance to next row (or next set bit in row_mask if filtered).
- **xColumn** — return decoded value for current row. Only called for columns
  SQLite actually needs (column pruning).
- **xEof** — true when current_row >= row_count (or past last bit in row_mask).

### NULL Handling in Filter Path

Since `encoded_values` excludes nulls, the filter path must map between two index
spaces: the **non-null index** (position in the encoded stream) and the **row index**
(position in the full table including nulls).

```
Full column (8 rows):   [A, NULL, B, A, NULL, NULL, C, A]
Null bitmap:            [0, 1,    0, 0, 1,    1,    0, 0]   (1=null)
Encoded values:         [A, B, A, C, A]                      (5 non-null values)
```

Filter flow for `WHERE col = 'A'`:
1. Run `filter_indices()` on the encoded stream → non-null match bitmap: `[1,0,1,0,1]`
2. Expand to full row space using null bitmap → row mask: `[1,0,0,1,0,0,0,1]`
3. Nulls never match equality/comparison filters (SQL semantics: `NULL != anything`)

Special cases:
- `IS NULL`: return the null bitmap directly (inverted: set bits = matching rows)
- `IS NOT NULL`: return the inverted null bitmap
- `WHERE col = 'A' OR col IS NULL`: union of filter result + null bitmap

The expansion from non-null indices to row indices is a shared utility in `eggdb-core`,
not reimplemented per encoding.

### Column Pruning

The vtable tracks which columns SQLite requests via xColumn calls during the first
row. On subsequent queries, it only decompresses those columns. For a query like:

```sql
SELECT count(*) FROM users WHERE status = 'active'
```

Only the `status` column is decompressed (~120KB DICT-encoded). The other 11
columns (potentially hundreds of MB) are never read from disk.

### Building as a Loadable Extension

The virtual table compiles to a `.so`/`.dylib` that any SQLite client can load:

```bash
cargo build --release --lib
# produces libeggvt.so / libeggvt.dylib

sqlite3
> .load ./libeggvt
> CREATE VIRTUAL TABLE t USING egg('data.egg', 'mytable');
> SELECT * FROM t LIMIT 5;
```

This requires exporting `sqlite3_extension_init` via `#[no_mangle]` and using
the SQLite loadable extension entry point pattern. `rusqlite` handles most of
this, but the loadable `.so` needs to be built separately from the CLI binary
(different crate target: cdylib vs bin).

---

## YQL (Yolk Query Language)

A thin transpiler. YQL → SQL string. No execution, no planning, no optimizer.

**YQL is single-table by design.** Multi-table queries (joins, subqueries) require
SQL via `egg query` or `egg shell`. Dotted identifiers like `users.name` are
stripped to `name` since the table is always unambiguous.

### Grammar

```
query      = command table [columns] [filter] [grouping] [ordering] [limit]
command    = "show" | "count" | "sum" | "avg" | "min" | "max" | "find" | "list"
table      = identifier
columns    = column_expr ("," column_expr)*
column_expr= [table "."] column | function "(" column ")"
function   = "month" | "year" | "day" | "date" | "lower" | "upper" | "length"
filter     = "where" condition ("and" condition)*
condition  = column_expr operator value
           | column_expr "between" value "and" value   // between is a keyword, not two ands
           | column_expr "in" "(" value_list ")"
operator   = "=" | "!=" | ">" | "<" | ">=" | "<=" | "like"
grouping   = "by" column_expr ("," column_expr)*
ordering   = "sort" column_expr ["asc" | "desc"]
limit      = "limit" number
value      = quoted_string | number | "null"
value_list = value ("," value)*
```

Note: `between ... and ...` is parsed as a single ternary operator. The `and` inside
`between` is consumed by the `between` rule, not by the condition-chaining `and`.

### Translation Rules

```
show users where x = 1 sort name limit 10
→ SELECT * FROM users WHERE x = 1 ORDER BY name ASC LIMIT 10

count orders by status
→ SELECT status, count(*) AS count FROM orders GROUP BY status

avg orders.total where status = "done" by month(created_at)
→ SELECT strftime('%m', created_at) AS month, avg(total) AS avg_total
  FROM orders WHERE status = 'done'
  GROUP BY strftime('%m', created_at)

find products where price between 10 and 50
→ SELECT * FROM products WHERE price BETWEEN 10 AND 50

list users.name, users.email where country in ("US", "UK")
→ SELECT name, email FROM users WHERE country IN ('US', 'UK')

sum orders.total
→ SELECT sum(total) AS sum_total FROM orders
```

Implementation: ~200-300 lines of Rust. A tokenizer, a small recursive descent parser
that produces an AST, and an AST-to-SQL formatter. No optimization, no planning.
SQLite's optimizer handles everything downstream.

---

## Project Structure

```
eggdb/
├── Cargo.toml
├── crates/
│   ├── eggdb-core/              # Format, encoding, analysis — no IO dependencies
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── format/
│   │       │   ├── mod.rs
│   │       │   ├── header.rs    # 64-byte shell: read/write
│   │       │   ├── albumen.rs   # Metadata: serialize/deserialize (bincode)
│   │       │   ├── membrane.rs  # Checksums: generate/verify
│   │       │   ├── writer.rs    # .egg file assembly
│   │       │   ├── reader.rs    # .egg file parsing + column decoding
│   │       │   ├── validate.rs  # Checksum + integrity verification
│   │       │   └── crypto.rs    # AES-256-GCM encryption/decryption
│   │       ├── encoding/
│   │       │   ├── mod.rs       # Encoding enum, ColumnEncoder trait
│   │       │   ├── analyzer.rs  # Column profiling, encoding selection
│   │       │   ├── constant.rs
│   │       │   ├── bool_bitmap.rs
│   │       │   ├── delta.rs
│   │       │   ├── rle.rs
│   │       │   ├── dictionary.rs
│   │       │   ├── raw.rs
│   │       │   └── null_bitmap.rs
│   │       └── value.rs         # Value enum (Integer, Real, Text, Blob, Null)
│   │
│   ├── eggdb-io/                # File reading/writing, SQLite interaction
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── writer.rs        # SQLite → .egg pipeline (lay)
│   │       ├── reader.rs        # .egg → column data / SQLite (hatch)
│   │       └── sqlite_utils.rs  # Schema introspection, bulk insert helpers
│   │
│   ├── eggdb-vtable/            # SQLite virtual table extension (cdylib)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs           # sqlite3_extension_init, module registration
│   │       ├── vtab.rs          # EggTab, EggCursor, xConnect/xBestIndex/etc.
│   │       ├── column_cache.rs  # Lazy column decompression, LRU eviction
│   │       └── error.rs         # VTable-specific error types
│   │
│   └── eggdb-yql/               # YQL transpiler
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── tokenizer.rs
│           ├── parser.rs        # Recursive descent → AST
│           ├── ast.rs           # Query AST types
│           └── sql_gen.rs       # AST → SQL string
│
├── src/
│   ├── main.rs                  # CLI entry point
│   └── commands/
│       ├── mod.rs
│       ├── lay.rs
│       ├── hatch.rs
│       ├── candle.rs
│       ├── crack.rs
│       ├── query.rs
│       ├── info.rs
│       ├── validate.rs
│       ├── schema.rs
│       ├── diff.rs
│       ├── relay.rs
│       ├── shell.rs
│       └── web.rs
│
├── tests/
│   ├── round_trip.rs            # lay → hatch → compare
│   ├── encoders.rs              # Per-encoding round-trip + edge cases
│   ├── vtable.rs                # Virtual table queries vs SQLite baseline
│   ├── yql.rs                   # YQL parse + transpile tests
│   └── fixtures/                # Small test SQLite databases
│
└── benches/
    ├── encoding.rs              # Encode/decode throughput per encoding
    ├── lay_hatch.rs             # End-to-end lay/hatch timing
    └── vtable_query.rs          # Virtual table query performance
```

### Crate Separation Rationale

- **eggdb-core** has zero IO, zero SQLite dependency. Pure data structures, encoding
  algorithms, format parsing. Easy to test, easy to fuzz, compiles fast.
- **eggdb-io** depends on `rusqlite` and `std::fs`. Handles the messy real-world IO.
- **eggdb-vtable** builds as a `cdylib` for `.load` in any SQLite client. Depends on
  eggdb-core for decoding. Separate crate because it has a different build target.
- **eggdb-yql** is a pure string-to-string transformer. No dependencies on the rest.
  Could theoretically be used standalone.

### Dependencies

```toml
# eggdb-core
serde = { version = "1", features = ["derive"] }
bincode = "1"
bitvec = "1"           # Bit vectors for null bitmaps, bool bitmaps, row masks
zstd = "0.13"          # Compression
crc32c = "0.6"         # Hardware-accelerated checksums
blake3 = "1"           # Albumen hashing
aes-gcm = "0.10"       # AES-256-GCM encryption
argon2 = "0.5"         # Passphrase key derivation (Argon2id)
rand = "0.9"           # Salt/nonce generation

# eggdb-io
rusqlite = { version = "0.31", features = ["bundled"] }  # Bundled = no system sqlite dep

# eggdb-vtable
rusqlite = { version = "0.31", features = ["bundled", "vtab"] }

# eggdb-yql
# No external deps — just thiserror

# CLI (root crate)
clap = { version = "4", features = ["derive"] }
indicatif = "0.17"     # Progress bars
comfy-table = "7"      # Table formatting for info/query output
colored = "2"          # Terminal colors
rustyline = "14"       # REPL for `egg shell`
rpassword = "7"        # Secure passphrase input
axum = "0.7"           # Web viewer HTTP server
tokio = "1"            # Async runtime for web viewer
tower-http = "0.6"     # CORS middleware
```

---

## Milestones

No deadlines. Each milestone is a working, testable state.

### M1: Format + Encoders ✅

Goal: encode and decode column data with all 6 strategies. No file IO yet.

- [x] `Value` enum (Integer, Real, Text, Blob, Null) with comparison, display
- [x] `ColumnStats` struct and `analyze_column()` function
  - For M1: analyze full column in memory
  - Note: sample size is tunable (default `min(100_000, row_count)` for large tables in M3+).
    Sampling is only for encoding selection, not correctness — all values are still encoded.
- [x] Encoding selection algorithm (with dict cap at 65K entries)
- [x] Null bitmap encode/decode (polarity: 1 = null, 0 = present)
- [x] Const encoder/decoder
- [x] BoolBitmap encoder/decoder
- [x] Delta encoder/decoder (zigzag varint)
- [x] RLE encoder/decoder
- [x] Dictionary encoder/decoder
- [x] Raw encoder/decoder
- [x] Round-trip property tests for every encoder (use `proptest` or `quickcheck`)
- [x] Benchmark: encode/decode throughput per encoding on synthetic data

### M2: File Format ✅

Goal: write and read `.egg` files. No SQLite interaction yet — test with in-memory data.

- [x] Header struct with read/write (64 bytes, fixed layout)
- [x] TableMeta / ColumnMeta structs with bincode serialization
- [x] Albumen writer: serialize metadata, record offset
- [x] Yolk writer: for each column, encode → zstd compress → write block, record offsets
- [x] Membrane writer: compute per-block CRC32C, write checksums
- [x] Assemble: header + albumen + yolk + membrane → `.egg` file
- [x] Reader: parse header → albumen → seek to specific column → decompress → decode
- [x] Validate: recompute all CRC32Cs, verify albumen hash
- [x] Test: write .egg from synthetic data, read back, verify exact values

### M3: SQLite Integration (Lay + Hatch) ✅

Goal: convert SQLite → .egg and back. Round-trip fidelity.

- [x] SQLite schema introspection: list tables, columns, types, indexes, DDL
- [x] Row iteration: stream rows from SQLite with type-aware value extraction
- [x] `egg lay`: analyze columns → select encodings → encode → write .egg
  - Progress bar per table
  - Print encoding summary per table
  - Print compression ratio
- [x] `egg hatch`: read .egg → create tables → bulk insert → rebuild indexes
  - Progress bar per table
  - Verify row counts
- [x] Round-trip test suite: lay → hatch → compare row-by-row
  - Integer columns, real columns, text, blobs, nulls
  - Empty tables, single-row tables, million-row tables
  - Tables with all-null columns
  - Tables with no nullable columns
  - Unicode text, empty strings, very long strings

### M4: CLI (Core Commands) ✅

Goal: usable CLI for lay, hatch, candle, info, validate, schema, crack.

- [x] `clap` derive-based CLI with subcommands
- [x] `egg lay` — full pipeline with progress, summary, error handling
- [x] `egg hatch` — full pipeline with progress, verification
- [x] `egg candle` — read 64-byte header + albumen, print summary
- [x] `egg info` — per-table overview, `--table` flag for per-column detail
- [x] `egg validate` — check every CRC32C, report pass/fail per block
- [x] `egg schema` — print stored DDL
- [x] `egg crack` — decode + stream to CSV or JSON without building SQLite
- [x] Colored output, human-readable sizes (1.9 GB, 187 MB), timing
- [x] Error handling: clear messages for corrupt files, missing tables, bad paths

### M5: Virtual Table ✅

Goal: query .egg files from any SQLite client via `.load`.

- [x] `EggTab` struct implementing `rusqlite::vtab::VTab`
- [x] `xConnect`: open .egg, parse header + albumen, declare schema to SQLite
- [x] `xBestIndex`: report cost estimates based on encoding + filter type
- [x] `xFilter`: decompress required columns, apply encoding-level filters
- [x] `xNext` / `xColumn` / `xEof`: iterate decoded rows
- [x] Column pruning: track which columns SQLite requests, skip the rest
- [x] `xDisconnect`: clean up file handles, free buffers
- [x] Build as cdylib: `cargo build --release -p eggdb-vtable`
- [x] Test: load extension, create vtable, run queries, compare against SQLite baseline
- [x] Test: multiple vtables from same .egg file
- [x] Test: joins between egg vtables and regular SQLite tables

### M6: YQL + Query Command ✅

Goal: `egg query` works for common patterns.

- [x] YQL tokenizer
- [x] YQL parser (recursive descent → AST)
- [x] SQL generator (AST → SQL string)
- [x] `egg query` command: detect YQL vs SQL, transpile if needed, run via vtable
- [x] `egg shell` command: interactive REPL (rustyline) with history
- [x] Auto-detect: input starting with SELECT/INSERT/etc. = SQL, else = YQL
- [x] Test: suite of YQL inputs → expected SQL outputs
- [x] Test: YQL queries produce same results as equivalent SQL on test databases

### M7: Diff + Relay ✅

Goal: compare eggs and re-optimize.

- [x] `egg diff`: compare two .egg files
  - Schema differences (added/removed/changed columns)
  - Row count differences per table
  - Encoding differences per column
  - Size comparison
- [x] `egg relay`: read .egg → re-analyze → re-encode with potentially better encodings
  - Useful after the encoding algorithm improves
  - Or after data patterns change (e.g., a column that was DICT-worthy is now RAW)
  - Operates without touching SQLite — pure .egg → .egg

### M8: Advanced Features ✅

Goal: production-grade features for real-world use.

- [x] **Streaming lay**: two-pass column-at-a-time processing. Pass 1 streams
  for stats via `StreamingColumnStats`. Pass 2 reads/encodes one column at a time,
  reducing peak memory from O(rows × columns) to O(rows × 1).
  CLI: `egg lay source.db --streaming`

- [x] **Partial hatch**: `egg hatch data.egg -o out.sqlite --tables users,orders`
  Only materialize specified tables. Skip the rest. Warns on unknown table names.

- [x] **Encryption**: AES-256-GCM on yolk blocks. Key derived from passphrase via Argon2id.
  - `egg lay data.sqlite -o data.egg --encrypt`  (prompts for passphrase)
  - `egg hatch data.egg -o data.sqlite --passphrase <pass>`
  - Header flag indicates encryption. Albumen is NOT encrypted (schema is visible
    for candle/info). Only yolk data blocks are encrypted.
  - 16-byte salt stored at start of yolk section. Per-block random 12-byte nonce.
  - Virtual table accepts passphrase: `USING egg('file.egg', 'table', 'passphrase')`
  - All commands support `--passphrase`: hatch, validate, crack, query, shell, relay

- [x] **Row reordering for compression**: `egg lay --optimize-order`
  Stable-sorts all columns by the lowest-cardinality column to maximize RLE
  and delta encoding effectiveness. Tradeoff: original rowid order is lost.
  Flag in header records whether order was preserved.

- [x] **Incremental lay**: `egg lay source.db --incremental existing.egg`
  Compares each column value-by-value against the existing egg. Unchanged columns
  are copied verbatim as `PreCompressedBlock`. Changed columns are re-analyzed and
  re-encoded. DDL/row-count mismatch triggers full table re-encode.

- [x] **Column-level compression tuning**: `egg lay --adaptive-compression`
  Automatic per-column zstd levels based on encoding type:
  Const/BoolBitmap → level 1, Delta/Rle/Dict → level 3, Raw → max(9, base_level).
  Combines with `--zstd-level`: e.g. `--zstd-level 19 --adaptive-compression`
  gives Raw columns level 19 while keeping lightweight encodings at 1/3.

- [x] **Web viewer**: `egg web data.egg [--port 8080]` starts a local HTTP server
  on 127.0.0.1 with an embedded HTML/JS/CSS frontend showing file summary,
  per-table compression bar chart, per-column encoding detail, and sample data viewer.
  API endpoints: `/api/summary`, `/api/tables`, `/api/table/{name}`, `/api/sample/{name}`.

---

## Benchmarks to Track

Run on every release. Track regressions.

### Compression

Measured on bible.eng.db (1.5 GB, 11 tables, 1,497,017 rows). Release build, Apple Silicon.

| Configuration | .egg Size | Ratio | Lay Time |
|---------------|-----------|-------|----------|
| Default (zstd 3) | 292.91 MB | **5.1x** | 12.4s |
| `--adaptive-compression` | 254.29 MB | **5.9x** | 21.7s |
| `--zstd-level 19` | 176.78 MB | **8.5x** | 7m 10s |
| `--zstd-level 19 --adaptive-compression` | 176.92 MB | **8.5x** | 6m 22s |
| `--optimize-order` | 292.91 MB | **5.1x** | 12.0s |

Note: `--optimize-order` shows no improvement on this dataset because the dominant
columns are high-cardinality text (JSON blobs, verse text). Row reordering benefits
datasets with low-cardinality columns where grouping improves RLE/Delta selection.

### Throughput

| Operation | Time |
|-----------|------|
| Hatch (.egg → SQLite) | 7.10s |
| Validate (176 checks) | < 1s |

### Encoding Micro-benchmarks

100,000 values per run. Criterion benchmarks on Apple Silicon.

| Encoding | Encode (100K) | Decode (100K) |
|----------|---------------|---------------|
| Const | 17 ns | 251 µs |
| BoolBitmap | 384 µs | 268 µs |
| Delta | 112 µs | 150 µs |
| Rle | 189 µs | 261 µs |
| Dict | 1.60 ms | 2.18 ms |
| Raw | 3.53 ms | 3.44 ms |

---

## Testing Strategy

### Unit Tests (per crate)
- Every encoder: round-trip with empty, single, many, all-null, all-same, random
- Header: write + read at boundary values (max table count, max size, etc.)
- Albumen: serialize + deserialize with varied schemas
- YQL parser: valid inputs, invalid inputs, edge cases

### Property Tests (proptest)
- For any `Vec<Value>`, encoding then decoding produces the original
- For any `Vec<Value>` with a filter, `filter_indices()` matches brute-force filter
- For any valid .egg file, `validate()` passes

### Integration Tests
- Round-trip: lay → hatch → row-by-row comparison on 10+ different SQLite schemas
- Virtual table: query results match direct SQLite results on same data
- YQL: transpiled SQL produces same results as hand-written SQL

### Fuzz Tests (cargo-fuzz)
- Feed random bytes to .egg reader — should error gracefully, never panic/crash
- Feed random strings to YQL parser — should error or parse, never panic

---

## Resolved Decisions

- **bincode config**: Pinned to v1 with little-endian + varint. Tied to format
  version byte. Version 1 = this config. Unknown versions rejected.
- **Cross-platform portability**: All integers little-endian. bincode config forced
  to LE. Test on x86 and ARM.
- **Null bitmap polarity**: 1 = null, 0 = present. encoded_values excludes nulls.
- **Albumen hash**: blake3 of serialized albumen bytes (covers schema + encodings + dicts).
  Named `albumen_hash`, not "schema hash", because it covers all metadata.
- **Checksum**: File-level CRC32C with checksum field zeroed during computation.
- **YQL scope**: Single-table only. Joins require SQL.
- **Row ordering**: Default preserved (flag bit 1 = 1). `--optimize-order` is opt-in.
- **Multi-database eggs**: One .egg = one SQLite database. Keep it simple.
- **Dict cap**: Max 65,536 dictionary entries. Above that, fall back to RAW.

## Future Roadmap

### v0.2 — Query Performance + Distribution

- **Zone maps**: Store min/max per chunk of N rows in the albumen. During queries,
  skip entire chunks that can't match the predicate. Cheap metadata overhead,
  significant speedup for range queries on large files. Requires format version 2
  (new albumen field) or a backwards-compatible extension in reserved space.

- **Predicate push-down improvements**: Range scans on Delta columns via binary
  search, dict-aware filtering (resolve value → index before scanning), leverage
  zone maps for early chunk elimination.

- **Parallel column decoding**: Decompress multiple columns concurrently using rayon.
  Column blocks are independent, so this is embarrassingly parallel.

- **Distribution**: npm package (SQLite extension for Node.js/Bun/Deno),
  Homebrew formula (`brew install eggdb`).

### v0.3 — Write Support

Write support follows an append + compaction model (standard for columnar formats):

- **Append mode**: New rows go into a sidecar buffer (small .egg or WAL-like file).
  `egg relay` merges the buffer into the main file (compaction). The virtual table
  reads from both the main file and the buffer, presenting a unified view.

- **Delete bitmaps**: A bitset marking rows as logically deleted without rewriting
  column data. Stored in the membrane section or a sidecar. Queries skip deleted
  rows. Compaction reclaims space by rewriting without deleted rows.

- **Schema evolution**: Add a nullable column without rewriting existing data. The
  new column is represented as all-null until the next compaction/relay adds real
  values. Drop column = mark as deprecated in albumen, skip during decode.

- **Write-through virtual table**: `INSERT`/`UPDATE`/`DELETE` via xUpdate. Built on
  append mode + delete bitmaps. UPDATE = delete old row + append new row. Requires
  all three primitives above.

### Exploring

- **Parquet export**: `egg crack --format parquet` for Arrow/DuckDB/Spark interop.
- **WASM build**: Read .egg files in the browser with client-side decoding.
- **Multi-file queries**: Query a directory of .egg files as one logical table.
- **Column-level encryption**: Encrypt only sensitive columns (PII, financials).
- **Remote storage**: HTTP range requests to read .egg from S3/GCS/Azure Blob.
- **Change tracking**: Structured changesets from `egg diff` (added/modified/deleted rows).

## Open Questions

- **Blob columns**: Always RAW, or detect repeated blobs (e.g., default avatar
  thumbnails) and DICT-encode them? Start with always RAW, optimize later.

- **Zone map granularity**: How many rows per chunk? Fixed (e.g., 10K rows) or
  adaptive based on column cardinality? Fixed is simpler; adaptive compresses
  better but complicates the format.

- **Append buffer format**: Separate .egg file, or a custom lightweight format?
  Separate .egg is simpler (reuse existing reader) but means two files on disk.
  A WAL-style single file is cleaner UX but more implementation work.

- **bincode v1 → v2 migration**: If bincode v2 becomes necessary in the future,
  that's a new format version (version byte = 2). No in-place migration — old
  readers reject v2 files, new readers handle both.
