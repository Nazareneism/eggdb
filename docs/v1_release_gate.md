# EggDB v1 Release Criteria

This document defines what "v1" means for EggDB and what must be true to ship it.
Milestones M1-M8 are fully implemented and tested.

## V1 Scope (Must Have)

- CLI: `lay`, `hatch`, `query`, `shell`, `candle`, `info`, `validate`, `schema`, `crack`, `diff`, `relay`, `web`
- SQLite virtual table extension: loadable `.so/.dylib`, column pruning, filter push-down
- YQL: single-table query sugar with documented grammar and translation rules
- `.egg` format v1: fixed header, albumen/yolk/membrane layout, CRC32C + albumen hash
- Encryption: AES-256-GCM for yolk blocks with Argon2id passphrase derivation
- Streaming lay: column-at-a-time, two-pass stats + encode
- Partial hatch: table selection with warnings for unknown tables
- Incremental lay: copy unchanged columns, re-encode changed columns
- Adaptive compression: per-column zstd levels
- Web viewer: local read-only UI with summary, tables, column stats, samples

## Non-Goals (Explicitly Out of Scope)

- Online writes or mutable `.egg` files
- Multi-writer concurrency or transactional semantics
- YQL joins or multi-table YQL
- SQL execution engine (SQLite remains the query engine)
- Remote/server mode (local files only)

## Product Positioning (v1)

EggDB is a compressed, read-optimized snapshot format for SQLite databases with
fast query access via SQLite virtual tables. It is suitable for analytics,
sharing, and archival. It is not an OLTP database.

## Release Gates

### Correctness

- [x] All unit, property, and integration tests pass across the workspace (440 tests).
- [x] `egg validate` passes on all test fixtures (176/176 checks) and rejects corrupted files.
- [x] Round-trip fidelity: `lay -> hatch` is row-by-row identical on `bible.eng.db` (11/11 tables, 1,497,017 rows).
- [x] Virtual table results match baseline SQLite queries (31 vtable tests including joins, aggregations, subqueries).
- [x] YQL transpilation produces equivalent results to SQL (70 tests: 56 unit + 14 integration).
- [x] Fuzz testing: `fuzz_egg_reader` and `fuzz_yql_parser` run ~6M inputs with zero panics.

### Format Stability

- [x] Header byte offsets and checksum coverage are fixed and documented in `docs/v0_plan.md`.
- [x] Format version byte is enforced; unknown versions are rejected with clear error.
- [x] Albumen hash is deterministic (blake3 of serialized albumen bytes).
- [ ] Cross-platform compatibility: proven on ARM (Apple Silicon). x86 untested (needs CI).

### Security and Crypto

- [x] AES-256-GCM encryption implemented for yolk blocks.
- [x] Argon2id KDF parameters documented and fixed for v1:
  - Memory cost: m=19456 (19 MiB), Time cost: t=2, Parallelism: p=1
  - Salt: 16 bytes, random per file. Output: 256-bit key.
  - Documented in `crates/eggdb-core/src/format/crypto.rs`.
- [x] Encrypted files fail fast with wrong passphrase (tested: core + vtable).
- [x] Nonce: 12 bytes, random per block. Salt and nonce generation via `rand::rng()`.
- [x] 14 encryption tests across workspace (crypto unit, writer round-trip, vtable passphrase).

### Performance (Regression Gates)

Use `bible.eng.db` as the baseline dataset (1.5 GB, 11 tables, 1,497,017 rows).
Release build on Apple Silicon.

| Gate | Threshold | Actual | Status |
|------|-----------|--------|--------|
| Compression ratio | >= 5.0x | **5.1x** (293 MB) | PASS |
| Lay time | <= 13.0s | **12.21s** | PASS |
| Hatch time | <= 8.0s | **6.90s** | PASS |
| Validate time (176 checks) | <= 1.0s | **0.09s** | PASS |
| Encoding micro-benchmarks | no >20% regression | No regression | PASS |

### UX and Reliability

- [x] CLI help text lists all 12 commands; examples in `README.md` are accurate.
- [x] Errors are explicit: missing files ("egg file not found"), corrupt files (CRC mismatch), bad passphrase ("decryption failed"), unknown tables (lists available tables).
- [x] All commands accept `--passphrase` where required by encrypted files.
- [x] Web viewer binds to `127.0.0.1` only (verified in `src/commands/web.rs`).
- [x] Zero clippy warnings, zero unused imports.

### Documentation

- [x] `README.md` updated with current commands, benchmarks, and build steps.
- [x] `.egg` format section is accurate and consistent with code.
- [x] `docs/v0_plan.md` reflects completed milestones and resolved decisions.

## Manual Acceptance Tests

All scenarios verified on 2026-02-10:

- [x] Lay -> query (SQL + YQL) -> hatch -> diff on a medium SQLite database (60K rows, 2 tables).
- [x] VTable joins + aggregates via `egg query` (SQL JOIN between users and orders tables).
- [x] Encrypt -> validate -> query -> hatch with passphrase (14 tests across core + vtable).
- [x] Streaming lay produces valid output, validates 19/19 checks.
- [x] Incremental lay against modified SQLite (100 rows changed, unchanged columns reused, 0.03s vs 0.09s).
- [x] Web viewer serves `/api/summary`, `/api/tables`, `/api/table/:name` with valid JSON; 6 unit tests pass.

## Fuzz Testing

Two fuzz targets set up in `fuzz/`:
- `fuzz_egg_reader`: feeds random bytes to `EggReader::open()` and `open_with_passphrase()`.
- `fuzz_yql_parser`: feeds random strings to `parse()`, `transpile()`, `is_yql()`.

Requires nightly: `cargo +nightly fuzz run fuzz_egg_reader -- -max_total_time=60`

Initial run: ~5M inputs (reader) + ~630K inputs (parser) in 10 seconds each, zero crashes.

## Packaging and Distribution

For v1, source distribution is acceptable:

- `cargo build --release` produces the CLI binary.
- `cargo build --release -p eggdb-vtable` produces the loadable extension.
- No external SQLite dependency (rusqlite bundled).

## Open Items

- **x86 cross-platform testing**: ARM (Apple Silicon) verified. x86 requires CI setup.
- **CI/CD pipeline**: No automated pipeline exists. Recommended before wider distribution.

## Definition of Done

V1 is ready when all Release Gates pass and manual acceptance tests succeed.
If any gate fails, fix the issue or update this document with explicit rationale.

**Status: READY** — All gates pass except x86 cross-platform (needs CI). All manual acceptance tests verified.
