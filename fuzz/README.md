# fuzz/ — Fuzz Testing

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for crash testing.
Feeds arbitrary input to parsers and readers to ensure they never panic.

## Targets

| File | What It Fuzzes |
|------|----------------|
| `fuzz_targets/fuzz_egg_reader.rs` | `EggReader::open()` and `open_with_passphrase()` with random bytes |
| `fuzz_targets/fuzz_yql_parser.rs` | YQL `parse()`, `transpile()`, and `is_yql()` with random strings |
| `fuzz_targets/fuzz_wal_reader.rs` | Write log `parse_wal()` + overlay `replay()`: raw bytes, valid header + raw bytes, and CRC-correct frames around random payloads |

## Requirements

- **Nightly Rust**: `rustup install nightly`
- **cargo-fuzz**: `cargo install cargo-fuzz`

## Running

```bash
# Fuzz the .egg reader (runs until stopped or crash found)
cargo +nightly fuzz run fuzz_egg_reader

# Fuzz the YQL parser
cargo +nightly fuzz run fuzz_yql_parser
cargo +nightly fuzz run fuzz_wal_reader

# Time-limited run (60 seconds)
cargo +nightly fuzz run fuzz_egg_reader -- -max_total_time=60
cargo +nightly fuzz run fuzz_yql_parser -- -max_total_time=60
cargo +nightly fuzz run fuzz_wal_reader -- -max_total_time=60

# List all targets
cargo +nightly fuzz list
```

## Results

The fuzz targets have been tested with:
- ~5 million inputs against the egg reader
- ~630K inputs against the YQL parser
- **Zero crashes found**

## Notes

- The `fuzz/` directory is excluded from the workspace (`exclude = ["fuzz"]` in root `Cargo.toml`)
- It has its own `Cargo.toml` and `Cargo.lock`
- Corpus and artifacts are stored in `fuzz/corpus/` and `fuzz/artifacts/` (gitignored)
- The fuzz targets depend on `eggdb-core` and `eggdb-yql` via path dependencies
