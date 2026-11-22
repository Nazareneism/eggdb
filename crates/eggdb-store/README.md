# eggdb-store

Write support for `.egg` files: the write log (`data.egg.wal`), consistent
snapshots that merge it with the immutable main file, an exclusive crash-safe
log writer, and compaction.

Depends on `eggdb-core` and `std::fs` only (no SQLite). Used by `eggdb-vtable`,
`eggdb-io`, the CLI, and the Python bindings. Design: `docs/v0.3_plan.md`.

## Usage

```rust
use eggdb_store::{compact, CompactOptions, EggSnapshot, WalWriter};
use eggdb_core::WalOp;

// Read: main file + pending changes, as one consistent snapshot.
let snap = EggSnapshot::open(path, None)?;
let rows = snap.live_row_count(0);
let columns = snap.visible_table(0)?; // live rows, changes applied

// Write: exclusive lock, one durable frame per commit.
let (mut writer, _log) = WalWriter::acquire(path, Some(snap.base()))?;
writer.append_commit(vec![WalOp::Delete { table: 0, rowid: 4 }])?;
drop(writer); // releases the lock

// Fold the log into a new main file.
compact(path, &CompactOptions::default())?;
```

## Modules

| File | Purpose |
|------|---------|
| `snapshot.rs` | `EggSnapshot`: reads main file + log consistently (retries if the main file is replaced mid-read), classifies the log, replays it into overlays |
| `writer.rs` | `WalWriter`: OS file lock, catch-up on other writers' frames, append + fsync, revert, compaction marker, recovery of interrupted/absorbed logs |
| `compact.rs` | `compact()`: validate, re-encode changed tables (copy unchanged blocks), verify, marker, atomic rename, remove log |
| `fsutil.rs` | Sidecar paths, directory fsync, file identity, `ensure_no_pending_log()` |
| `error.rs` | `StoreError`, with messages that say what to do next |

## Guarantees

- `Ok` from `append_commit` means the frame survives a crash.
- A torn final frame (crash mid-append) is ignored by readers and truncated by the next writer.
- A log that does not belong to the main file is an error, never silently applied or dropped.
- A crash at any step of compaction leaves either the old main file with its log, or the new
  main file with a log that is recognized as already folded in.
- Compaction refuses to run on a main file that fails validation, and refuses to install an
  output that fails validation or has unexpected row counts.
