# Changelog

All notable changes to EggDB are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Parallel column decoding** (M9): decode multiple columns concurrently using rayon.
  All hatch, crack, query, relay, and web viewer paths now decompress columns in
  parallel, providing multi-core speedup on tables with many columns.
  - `decode_columns_parallel()` standalone function in eggdb-core
  - `EggReader::decode_all_columns_parallel()` convenience method
  - Feature-gated behind `parallel` (on by default); `--no-default-features` compiles
    without rayon for single-threaded environments
  - Virtual table decodes only needed columns in parallel (column pruning preserved)

## [0.1.0] - 2026-02-10

Initial release. All milestones (M1-M8) complete.

### Added

#### Format & Encodings (M1-M2)

- `.egg` binary format with 64-byte fixed header, bincode metadata (albumen),
  zstd-compressed column data (yolk), and integrity checksums (membrane)
- 6 columnar encodings with automatic per-column selection:
  Const, BoolBitmap, Delta (zigzag varint), RLE, Dictionary, Raw
- Null bitmap with 1=null polarity; encoded values exclude nulls
- CRC32C file checksum (hardware-accelerated) with per-block checksums
- blake3 hash of serialized metadata for tamper detection
- Format version byte (v1) with strict rejection of unknown versions

#### SQLite Integration (M3)

- `egg lay`: SQLite to .egg compression with column analysis and encoding selection
- `egg hatch`: .egg to SQLite reconstruction with row-by-row fidelity
- Round-trip verification: row counts, values, types, indexes all preserved
- Schema introspection, bulk insert with transaction batching, index recreation

#### CLI (M4)

- 12 commands: lay, hatch, candle, info, validate, schema, crack, query, shell,
  diff, relay, web
- `egg candle`: quick metadata summary from 64-byte header
- `egg info`: per-table overview with `--table` flag for per-column detail
- `egg validate`: verify all CRC32C checksums, report pass/fail per block
- `egg schema`: print stored DDL (pipe-friendly, no color)
- `egg crack`: export to CSV or JSON without SQLite rebuild
- Colored output, human-readable sizes, progress spinners, timing

#### Virtual Table (M5)

- SQLite loadable extension (`libeggdb_vtable.dylib/.so`)
- Column pruning: only decompresses columns referenced in the query
- Filter push-down: encoding-aware filtering (Const, Dict, Delta, RLE)
  with `omit=false` for SQLite double-checking
- Joins between egg virtual tables and regular SQLite tables
- `col_used` bitmask for first 64 columns; beyond 64 always decoded

#### YQL & Query (M6)

- YQL (Yolk Query Language): single-table query syntax transpiled to SQL
- Commands: show, find, list, count, sum, avg, min, max
- Functions: month, year, day, date, lower, upper, length
- `between ... and ...` parsed as ternary operator
- `egg query`: auto-detects YQL vs SQL, transpiles if needed, runs via vtable
- `egg shell`: interactive REPL with rustyline history and dot-commands
  (`.tables`, `.schema`, `.help`, `.quit`)

#### Diff & Relay (M7)

- `egg diff`: compare two .egg files by schema, row counts, encodings, sizes;
  exit code 0=identical, 1=differences
- `egg relay`: re-analyze and re-encode .egg to .egg (pure egg-to-egg, no SQLite)

#### Advanced Features (M8)

- **Encryption**: AES-256-GCM on yolk blocks with Argon2id key derivation
  (m=19456, t=2, p=1). 16-byte random salt per file, 12-byte random nonce per block.
  Metadata stays readable for candle/info/schema without passphrase.
- **Streaming lay**: two-pass column-at-a-time processing, peak memory O(rows x 1)
- **Incremental lay**: compare column-by-column against existing .egg, copy unchanged
  blocks verbatim as PreCompressedBlock
- **Adaptive compression**: per-column zstd levels based on encoding type
  (Const/BoolBitmap=1, Delta/RLE/Dict=3, Raw=max(9, base_level))
- **Row reordering**: `--optimize-order` stable-sorts by lowest-cardinality column
  to improve RLE/Delta effectiveness
- **Partial hatch**: `--tables users,orders` to restore only specific tables
- **Web viewer**: `egg web` starts a local HTTP server with embedded frontend
  showing file summary, compression bar chart, per-column detail, sample data

#### Decoder Robustness (Post-M8)

- `ColumnEncoder::decode()` returns `Result<Vec<Value>, FormatError>` instead of
  panicking on malformed input
- `filter_indices()` returns `Result<BitVec, FormatError>`
- All 6 encoders have bounds checks, bincode error handling, varint overflow guards
- 30 malformed-input decode tests

#### Testing & Quality

- 470 tests: unit, property (proptest), integration (round-trip), virtual table, YQL
- Fuzz targets for .egg reader and YQL parser (cargo-fuzz, ~6M inputs, 0 crashes)
- Criterion benchmarks for all 6 encodings
- Zero clippy warnings across workspace
