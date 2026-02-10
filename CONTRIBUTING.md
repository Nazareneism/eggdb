# Contributing to EggDB

Thank you for considering contributing to EggDB. This guide will help you get started.

## Getting Started

### Prerequisites

- **Rust** (2021 edition) via [rustup](https://rustup.rs/)
- **Nightly toolchain** (for fuzz testing only): `rustup install nightly`
- No system SQLite required — rusqlite bundles its own

### Build

```bash
cargo build                    # Debug build
cargo build --release          # Release build (much faster runtime)
```

### Test

```bash
cargo test --workspace         # All 440+ tests
cargo test -p eggdb-core       # One crate only
```

### Lint

```bash
cargo fmt --check              # Format check
cargo clippy -- -D warnings    # Lint (warnings are errors)
```

Always run `cargo fmt` and `cargo clippy -- -D warnings` before submitting a PR.

## Project Structure

```
crates/
  eggdb-core/     # Format, encodings, value types. Zero IO dependencies.
  eggdb-io/       # SQLite <-> .egg conversion (lay, hatch).
  eggdb-vtable/   # SQLite virtual table extension (loadable .so/.dylib).
  eggdb-yql/      # YQL -> SQL transpiler. No eggdb dependencies.
src/              # CLI binary (12 commands).
python/           # Python package (PyO3 + maturin).
tests/            # Integration tests + test fixtures.
benches/          # Criterion micro-benchmarks.
fuzz/             # cargo-fuzz targets (requires nightly).
```

Each crate has its own README with details. See the `crates/*/README.md` files.

## Crate Boundaries (Strict)

These boundaries are enforced in CI and must never be violated:

| Crate | May depend on |
|-------|---------------|
| `eggdb-core` | No IO, no `rusqlite`, no `std::fs` |
| `eggdb-yql` | No other eggdb crates |
| `eggdb-vtable` | `eggdb-core` only (not `eggdb-io`) |
| `eggdb-io` | `eggdb-core` + `rusqlite` |
| CLI (`src/`) | All four crates |

If your change adds a dependency to a crate, verify it doesn't break these boundaries.

## Coding Standards

### Rust Conventions

- **Error handling**: `thiserror` in library crates, `anyhow` in the CLI binary only
- **No `.unwrap()` or `.expect()`** in library code (test code is fine)
- **`#[must_use]`** on functions returning values that shouldn't be silently dropped
- **All public functions** must return `Result`
- **No `unsafe`** unless required by FFI (e.g., SQLite extension init). Comment why.
- **Functions under 50 lines** — split larger functions into helpers

### Naming

- Encoding types: `Const`, `BoolBitmap`, `Delta`, `Rle`, `Dict`, `Raw`
- Format sections: shell (header), albumen (metadata), yolk (data), membrane (integrity)
- Use the egg metaphor in user-facing output, not in internal code. Internal code uses descriptive names: `header`, `metadata`, `column_block`, `checksum`.

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(core): add delta encoder
fix(vtable): column pruning off-by-one
refactor(io): simplify hatch pipeline
test(yql): add between...and edge cases
docs: update README with encryption examples
chore: bump rusqlite to 0.31
```

Scope by crate when the change is crate-specific. One logical change per commit.

## Testing Requirements

Every PR should include tests for new functionality:

| What | How |
|------|-----|
| New encoder | Round-trip tests: empty, single, many, all-null, all-same. Property tests with `proptest`. |
| Format changes | Write .egg in-memory, read back, verify exact values |
| IO changes | `lay` -> `hatch` -> row-by-row comparison |
| Virtual table | Query results must match direct SQLite results |
| YQL changes | Parse + transpile tests with expected SQL output |
| CLI commands | At minimum, verify options are passed through correctly |

### Running the Full Suite

```bash
# Quick check (< 30 seconds)
cargo fmt --check && cargo clippy -- -D warnings && cargo test --workspace

# With fuzz testing (requires nightly)
cargo +nightly fuzz run fuzz_egg_reader -- -max_total_time=60
cargo +nightly fuzz run fuzz_yql_parser -- -max_total_time=60

# Benchmarks
cargo bench -p eggdb-core
```

### Test Fixtures

Test databases go in `tests/fixtures/`. The main fixture is `bible.eng.db` (1.5 GB, tracked via git-lfs). For smaller tests, create SQLite databases in-memory within the test.

## Common Tasks

### Adding a New Encoding

1. Create `crates/eggdb-core/src/encoding/your_encoding.rs`
2. Implement `ColumnEncoder` trait (`encode`, `decode`, `filter_indices`)
3. Add variant to `Encoding` enum in `crates/eggdb-core/src/encoding/mod.rs`
4. Add to `encoder_for()` factory in `crates/eggdb-core/src/format/reader.rs`
5. Update `select_encoding()` in `crates/eggdb-core/src/encoding/analyzer.rs`
6. Add round-trip tests + property tests + benchmark

### Adding a New CLI Command

1. Create `src/commands/your_command.rs`
2. Add to `src/commands/mod.rs`
3. Add variant to `Commands` enum in `src/main.rs`
4. Use `anyhow::Result` for error handling
5. Use `colored` for terminal colors, `indicatif` for progress, `comfy-table` for tables

### Modifying the .egg Format

The format is versioned. Any breaking change requires incrementing the version byte in the header. Readers must reject unknown versions. Before making format changes, open an issue to discuss the approach.

## Null Bitmap Semantics

This is critical to understand:

- **Polarity**: 1 = null, 0 = present
- **Encoders receive only non-null values** — nulls are stripped before encoding
- **Decoders return only non-null values** — the caller interleaves nulls using the bitmap
- The `expand_non_null_indices()` utility maps non-null filter results to full row-space

## Pull Requests

1. Fork the repo and create a feature branch
2. Make your changes with tests
3. Run `cargo fmt && cargo clippy -- -D warnings && cargo test --workspace`
4. Submit a PR with a clear description of what and why

## License

By contributing, you agree that your contributions will be licensed under the
MIT OR Apache-2.0 dual license (same as the project).
