# tests/ — Integration Tests

End-to-end tests that exercise the full pipeline across crates.

## Test Files

| File | What It Tests |
|------|---------------|
| `yql.rs` | YQL parser and transpiler: valid inputs, invalid inputs, edge cases, expected SQL output |

Most tests live within each crate (in `crates/*/tests/` or `#[cfg(test)]` modules).
The workspace-level `tests/` directory is for cross-crate integration tests.

## Fixtures

Test databases live in `tests/fixtures/`:

| File | Size | Description |
|------|------|-------------|
| `bible.eng.db` | 1.5 GB | 11 tables, 1,497,017 rows. Primary benchmark and integration test database. |
| `.gitkeep` | — | Ensures the directory exists in git |

The `.egg` files in fixtures are generated during testing and listed in `.gitignore`.

## Running Tests

```bash
# All workspace tests
cargo test --workspace

# Just integration tests
cargo test --test yql

# Specific crate tests
cargo test -p eggdb-core
cargo test -p eggdb-io
cargo test -p eggdb-vtable
cargo test -p eggdb-yql
```

## Writing Integration Tests

- For round-trip tests (lay -> hatch -> compare), use the helpers in `eggdb-io`
- Create test SQLite databases in-memory rather than adding large fixtures
- Sort query results before comparison — vtable row order may differ from direct SQLite
- Use `tempfile` for any tests that write to disk
