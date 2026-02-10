# eggdb-core

Core library for the EggDB .egg file format. Provides encoding algorithms, format
reading/writing, validation, and encryption — with **zero IO dependencies**.

This crate never touches the filesystem or SQLite. It operates entirely on `&[u8]`
slices and `Vec<u8>` buffers, making it easy to test, fuzz, and embed.

## Modules

### `encoding/` — Column Encoders

Six encoding strategies, each implementing the `ColumnEncoder` trait:

| File | Encoding | When Used |
|------|----------|-----------|
| `constant.rs` | Const | Entire column is one value (or all null) |
| `bool_bitmap.rs` | BoolBitmap | Boolean / 2 distinct values, bitpacked |
| `delta.rs` | Delta | Sorted numeric sequences, zigzag varint deltas |
| `rle.rs` | Rle | Many consecutive duplicates, (value, run) pairs |
| `dictionary.rs` | Dict | Low cardinality (< 1% distinct, max 65K entries) |
| `raw.rs` | Raw | High cardinality fallback, relies on zstd |

Supporting files:

- **`mod.rs`** — `Encoding` enum, `ColumnEncoder` trait, `FilterOp` for predicate push-down
- **`analyzer.rs`** — `ColumnStats`, `analyze_column()`, `select_encoding()`, `StreamingColumnStats` for two-pass analysis
- **`null_bitmap.rs`** — Null bitmap encode/decode (polarity: 1=null, 0=present), `expand_non_null_indices()` utility

### `format/` — .egg File Format

| File | Purpose |
|------|---------|
| `header.rs` | 64-byte fixed header (magic, version, offsets, CRC32C) |
| `albumen.rs` | Metadata structs (`TableMeta`, `ColumnMeta`) with bincode serialization |
| `writer.rs` | `write_egg()` — assembles header + albumen + yolk + membrane |
| `reader.rs` | `EggReader` — parses .egg data, decodes individual columns |
| `membrane.rs` | Per-block CRC32C checksums, row counts, albumen blake3 hash |
| `validate.rs` | `validate()` — verifies all checksums and integrity (7 check categories) |
| `crypto.rs` | AES-256-GCM encryption/decryption, Argon2id key derivation |
| `mod.rs` | `FormatError` enum |

### `value.rs` — Value Type

`Value` enum with variants: `Integer(i64)`, `Real(f64)`, `Text(String)`, `Blob(Vec<u8>)`, `Null`. Implements `Ord`, `Eq`, `Display`, `Serialize`, `Deserialize`.

## Key Types

```rust
// Open and read an .egg file
let reader = EggReader::open(&egg_bytes)?;
let reader = EggReader::open_with_passphrase(&egg_bytes, "secret")?;

// Access metadata
let tables: &[TableMeta] = reader.tables();
let header: &Header = reader.header();

// Decode a specific column
let values: Vec<Value> = reader.decode_column(table_index, column_index)?;

// Validate integrity
let results: Vec<ValidationResult> = validate(&egg_bytes)?;

// Write an .egg file
let egg_bytes: Vec<u8> = write_egg(&tables, &options)?;
```

## ColumnEncoder Contract

Encoders work on **non-null values only**:

- `encode(values)` — `values` excludes nulls; bitmap handled separately
- `decode(data, non_null_count)` — returns `Result<Vec<Value>, FormatError>`; caller interleaves nulls
- `filter_indices(data, non_null_count, op, value)` — returns `Result<BitVec, FormatError>` of matching indices in non-null space

The null bitmap always uses polarity 1=null, 0=present.

## Dependencies

```
serde, bincode     — Metadata serialization (LE + varint config)
bitvec             — Null bitmaps, bool bitmaps, row masks
zstd               — Column block compression
crc32c             — Hardware-accelerated checksums
blake3             — Albumen metadata hashing
aes-gcm            — AES-256-GCM encryption
argon2             — Passphrase key derivation (Argon2id)
rand               — Salt/nonce generation
thiserror          — Error types
```

No `rusqlite`. No `std::fs`. No IO of any kind.

## Testing

```bash
cargo test -p eggdb-core            # Unit + integration tests
cargo bench -p eggdb-core           # Encoding micro-benchmarks (Criterion)
cargo +nightly fuzz run fuzz_egg_reader  # Fuzz the .egg reader
```

Tests include:
- Per-encoder round-trip with edge cases (empty, single, all-null, all-same, 1M rows)
- Property tests with `proptest` (encode/decode invariant for any valid input)
- Format round-trip (write .egg in-memory, read back, verify values)
- Malformed decode tests (crafted data returns `Err`, never panics)
- Validation tests (corrupt headers, bad checksums, version mismatches)
- Encryption tests (round-trip, wrong passphrase, encrypted + no key)
