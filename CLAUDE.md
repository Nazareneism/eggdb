# EggDB

A lifecycle-aware compressed database engine in Rust. Converts SQLite databases into
highly compressed `.egg` files using columnar encoding, and provides a SQLite virtual
table extension to query them directly.

## Project Context

- **Plan**: @docs/v0_plan.md (the authoritative spec — format, encodings, milestones, architecture)
- **Write support**: @docs/v0.3_plan.md (write log, compaction, writable vtable; supersedes the v0 roadmap sketch)
- **Language**: Rust (2021 edition), workspace with 5 crates
- **No deadline**: Side project. Correctness over speed. No shortcuts.

## Claude Code Configuration

- **Rules**: `.claude/rules/` — path-scoped coding rules
  - `rust.md` — Rust conventions (applies to `crates/**/*.rs`, `src/**/*.rs`)
  - `encodings.md` — ColumnEncoder contract (applies to `crates/eggdb-core/src/encoding/**`)
- **Agents**: `.claude/agents/` — specialized subagents
  - `test-runner` (haiku) — runs fmt check → clippy → test after code changes
  - `architecture-guard` (haiku) — validates crate dependency boundaries
  - `encoding-reviewer` (sonnet) — reviews encoders against the ColumnEncoder contract
- **Skills**: `.claude/skills/` — user-invoked workflows
  - `/validate [--fix]` — full pre-commit pipeline (fmt, clippy, test)
  - `/bench [crate] [--compare main]` — run benchmarks, optionally compare branches
  - `/round-trip <db-path>` — lay → hatch → row-by-row comparison

## Architecture

```
crates/
  eggdb-core/     # Format, encodings, value types. Zero IO, zero SQLite dependency.
  eggdb-io/       # SQLite ↔ .egg conversion (lay, hatch). Depends on rusqlite.
  eggdb-store/    # Write log (.egg.wal) IO: snapshots, locked crash-safe appends, compaction.
  eggdb-vtable/   # SQLite virtual table extension (cdylib). Column pruning, filter push-down.
  eggdb-yql/      # YQL → SQL transpiler. Pure string transform, no external deps.
src/              # CLI binary (clap). Thin wrappers around crate APIs.
```

Crate boundaries are strict:
- `eggdb-core` must never depend on `rusqlite`, `std::fs`, or any IO
- `eggdb-yql` must never depend on any other eggdb crate
- `eggdb-store` depends on `eggdb-core` only (`std::fs` yes, `rusqlite` never)
- `eggdb-vtable` depends on `eggdb-core` and `eggdb-store` (not `eggdb-io`)
- `eggdb-io` depends on `eggdb-core`, `eggdb-store`, and `rusqlite`
- The CLI root crate depends on all five

## .egg Format Quick Reference

- Fixed 64-byte header (see plan for byte offsets)
- Version byte: 1 = bincode LE + varint metadata
- Null bitmap polarity: **1 = null, 0 = present**
- `encoded_values` **excludes nulls** — bitmap required for row reconstruction
- Per-column byte offsets in metadata enable selective decompression
- CRC32C with checksum field zeroed during computation
- `albumen_hash` = blake3 of serialized albumen bytes (not DDL text)

## Coding Standards

### Rust

- Use `thiserror` for library error types, `anyhow` in the CLI binary only
- All public functions must return `Result` — no unwrap/expect in library code
- `#[must_use]` on functions returning values that should not be silently dropped
- Prefer `&[u8]` and `&str` over owned types in function signatures where possible
- Use `impl Into<T>` for generic constructors, not trait objects
- No `unsafe` unless required by FFI (sqlite3 extension init). Comment why.

### Naming

- Encoding types: `Const`, `BoolBitmap`, `Delta`, `Rle`, `Dict`, `Raw` (match the enum)
- File format sections: shell (header), albumen (metadata), yolk (data), membrane (integrity)
- CLI commands: `lay`, `hatch`, `candle`, `crack`, `query`, `info`, `validate`, `schema`, `diff`, `relay`, `shell`, `web`, `compact`
- Use the egg metaphor in user-facing output and docs, not in internal code.
  Internal code uses descriptive names: `header`, `metadata`, `column_block`, `checksum`.

### Testing

- Every encoder: round-trip tests with edge cases (empty, single value, all null, all same, 1M rows)
- Property tests with `proptest` for encoders: encode(decode(x)) == x for any valid input
- Integration tests: lay → hatch → row-by-row comparison against original SQLite
- Virtual table tests: query results must match direct SQLite query results on same data
- Fuzz tests (`cargo-fuzz`) for .egg reader and YQL parser — must never panic on arbitrary input
- Run `cargo test` after every change. Run `cargo clippy` before committing.

### Commits

- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
- Scope by crate: `feat(core): add delta encoder`, `fix(vtable): column pruning off-by-one`
- One logical change per commit

## Key Dependencies

```
eggdb-core:  serde, bincode (v1, pinned LE+varint), bitvec, zstd, crc32c, blake3,
             aes-gcm, argon2, rand, zeroize, thiserror
eggdb-store: eggdb-core, thiserror, zeroize
eggdb-io:    rusqlite =0.40.2 (bundled), eggdb-store, thiserror, bitvec, zeroize
eggdb-vtable: rusqlite =0.40.2 (bundled, vtab), eggdb-store, bitvec, thiserror, zeroize
eggdb-yql:   thiserror
CLI:         clap (derive), indicatif, comfy-table, colored, rustyline, rpassword,
             axum, tokio, tower-http, anyhow
```

## Common Commands

```bash
cargo build                          # Build all crates
cargo test                           # Run all tests
cargo test -p eggdb-core             # Test one crate
cargo clippy -- -D warnings          # Lint
cargo build --release -p eggdb-vtable --features loadable_extension  # Loadable .dylib/.so
cargo bench                          # Run benchmarks
cargo +nightly fuzz run fuzz_egg_reader   # Fuzz the .egg reader
cargo +nightly fuzz run fuzz_wal_reader   # Fuzz the write log parser + replay
```

## Things to Remember

- The virtual table is the query engine. YQL transpiles to SQL. No custom executor.
- `encoded_values` excludes nulls. The null bitmap is always needed for decode.
- Dict encoding caps at 65,536 entries. Above that, fall back to Raw.
- Column analysis uses sampling for large tables: `min(100_000, row_count)` rows.
  Sampling is for encoding selection only — all values are still encoded.
- bincode config is tied to format version, not header flags. Flags are runtime-only.
- `between ... and ...` in YQL is a single ternary keyword, not two `and` conditions.
- Row order is preserved by default (flag bit 1 = 1). `--optimize-order` is opt-in.
- Writes never modify the main `.egg`. They append to `data.egg.wal` (one fsynced frame
  per transaction); every reader merges it via `EggSnapshot`. Never read row data with
  `EggReader` directly in a command: it ignores pending changes.
- Rowid = physical position. Main rows are `[0, n)`, logged inserts follow; updates keep
  their rowid; deleted rowids are never reused. Only `compact` renumbers.
- The log header binds to the main file (header CRC + length). A log that does not match
  is an error, never silently applied or dropped.
- SQLite discards DEFAULTs declared in a vtab schema and never enforces vtab constraints;
  `eggdb-vtable/src/constraints.rs` enforces them through a scratch in-memory SQLite.
- `register_egg_module` gives each connection its own state: one SQL transaction on one
  egg is one log frame, even across tables.
