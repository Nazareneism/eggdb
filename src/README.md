# src/ — CLI Binary

The `egg` command-line tool. Thin wrappers around the library crate APIs with
user-friendly output, progress indicators, and error messages.

## Commands

| Command | File | Description |
|---------|------|-------------|
| `egg lay` | `commands/lay.rs` | Compress SQLite -> .egg |
| `egg hatch` | `commands/hatch.rs` | Restore .egg -> SQLite |
| `egg candle` | `commands/candle.rs` | Quick metadata summary |
| `egg info` | `commands/info.rs` | Per-column compression stats |
| `egg validate` | `commands/validate.rs` | Verify all checksums |
| `egg schema` | `commands/schema.rs` | Print original DDL |
| `egg crack` | `commands/crack.rs` | Export as CSV or JSON (no SQLite rebuild) |
| `egg query` | `commands/query.rs` | Run YQL or SQL via virtual table |
| `egg shell` | `commands/shell.rs` | Interactive REPL |
| `egg diff` | `commands/diff.rs` | Compare two .egg files |
| `egg relay` | `commands/relay.rs` | Re-analyze and re-encode |
| `egg web` | `commands/web.rs` | Local web viewer (HTTP server) |

## Other Files

| File | Purpose |
|------|---------|
| `main.rs` | Entry point, clap argument parsing, command dispatch |
| `format.rs` | Shared formatting helpers (`format_size`, `format_duration`, `truncate_display`) |
| `commands/mod.rs` | Module declarations |

## Conventions

- Uses `anyhow::Result` for all error handling (not `thiserror`)
- `colored` for terminal colors
- `indicatif` for spinners and progress bars
- `comfy-table` for tabular output
- `rustyline` for the REPL in `egg shell`
- `rpassword` for secure passphrase input on `--encrypt` / `--passphrase`
- `axum` + `tokio` for the web viewer HTTP server
- Commands that support encrypted files accept `--passphrase`
- `egg schema` outputs plain text (no color) for pipe-friendliness
- `egg validate` exits with code 1 on any failed check
- `egg diff` exits with code 0 (identical) or 1 (differences)

## Adding a New Command

1. Create `commands/your_command.rs` with a `pub fn run(...) -> anyhow::Result<()>`
2. Add `pub mod your_command;` to `commands/mod.rs`
3. Add a variant to the `Commands` enum in `main.rs` with clap derive attributes
4. Call `your_command::run()` from the match in `main()`
