//! State shared by every egg virtual table on the same connection that points
//! at the same file: the snapshot, the open transaction, and validators.
//!
//! One SQL transaction that writes several tables of one egg therefore becomes
//! one write-log frame, which is atomic.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use rusqlite::Error;
use zeroize::Zeroizing;

use eggdb_core::{FormatError, TableOverlay, Value, WalOp};
use eggdb_store::{EggSnapshot, LogState, StoreError, WalWriter};

use crate::constraints::RowValidator;

/// Module aux data. `register_egg_module` creates one per connection, so
/// sharing never crosses connections (each has its own transactions).
#[derive(Default)]
pub struct ModuleState {
    eggs: Mutex<Vec<Registered>>,
}

struct Registered {
    path: PathBuf,
    passphrase: Option<Zeroizing<String>>,
    egg: Weak<Mutex<SharedEgg>>,
}

/// Manual impl: never print passphrases.
impl std::fmt::Debug for ModuleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open = self.eggs.lock().map(|e| e.len()).unwrap_or(0);
        f.debug_struct("ModuleState")
            .field("open_eggs", &open)
            .finish()
    }
}

impl ModuleState {
    /// The shared state for `path` on this connection, opening it if needed.
    /// Different passphrases get separate state, so a wrong passphrase still
    /// fails instead of reusing a key someone else supplied.
    pub fn open(
        &self,
        path: &Path,
        passphrase: Option<&Zeroizing<String>>,
    ) -> Result<Arc<Mutex<SharedEgg>>, Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::ModuleError(format!("failed to read egg file '{}': {e}", path.display()))
        })?;
        let mut eggs = self.eggs.lock().map_err(|_| lock_poisoned())?;
        eggs.retain(|r| r.egg.strong_count() > 0);
        let existing = eggs
            .iter()
            .find(|r| r.path == canonical && r.passphrase.as_deref() == passphrase.map(|p| &**p))
            .and_then(|r| r.egg.upgrade());
        if let Some(found) = existing {
            return Ok(found);
        }
        let snapshot =
            EggSnapshot::open(&canonical, passphrase.map(|p| p.as_str())).map_err(store_err)?;
        let shared = Arc::new(Mutex::new(SharedEgg::new(snapshot)));
        eggs.push(Registered {
            path: canonical,
            passphrase: passphrase.cloned(),
            egg: Arc::downgrade(&shared),
        });
        Ok(shared)
    }
}

/// An open egg on one connection.
#[derive(Debug)]
pub struct SharedEgg {
    snapshot: EggSnapshot,
    txn: Option<Txn>,
    validators: HashMap<usize, RowValidator>,
    /// Set when the log may no longer match what SQLite believes; every
    /// operation fails until the connection reopens the file.
    poisoned: Option<String>,
}

#[derive(Debug)]
struct Txn {
    writer: WalWriter,
    /// Committed overlays plus this transaction's changes.
    working: Vec<TableOverlay>,
    pending: Vec<WalOp>,
    /// Sequence number of the frame written at sync time.
    synced: Option<u64>,
}

impl SharedEgg {
    fn new(snapshot: EggSnapshot) -> Self {
        Self {
            snapshot,
            txn: None,
            validators: HashMap::new(),
            poisoned: None,
        }
    }

    /// The main file and committed changes.
    #[must_use]
    pub fn snapshot(&self) -> &EggSnapshot {
        &self.snapshot
    }

    /// Changes visible to this connection for table `t`, including those of
    /// its own open transaction.
    #[must_use]
    pub fn overlay(&self, t: usize) -> &TableOverlay {
        match &self.txn {
            Some(txn) => &txn.working[t],
            None => &self.snapshot.overlays()[t],
        }
    }

    /// Fail if an earlier failure left the log in an uncertain state.
    pub fn check_usable(&self) -> Result<(), Error> {
        match &self.poisoned {
            Some(why) => Err(Error::ModuleError(why.clone())),
            None => Ok(()),
        }
    }

    /// Start a write transaction: take the log lock and catch up on changes
    /// other processes committed. Idempotent within one transaction.
    pub fn begin(&mut self) -> Result<(), Error> {
        self.check_usable()?;
        if self.txn.is_some() {
            return Ok(());
        }
        let (writer, log) =
            WalWriter::acquire(self.snapshot.egg_path(), Some(self.snapshot.base()))
                .map_err(store_err)?;
        let up_to_date = matches!(
            self.snapshot.log_state(),
            LogState::Active { next_seq, valid_length, .. }
                if *next_seq == log.next_seq && *valid_length == log.valid_length
        );
        if !up_to_date {
            self.snapshot.refresh_log(&log).map_err(store_err)?;
            self.validators.clear();
        }
        self.txn = Some(Txn {
            writer,
            working: self.snapshot.overlays().to_vec(),
            pending: Vec::new(),
            synced: None,
        });
        Ok(())
    }

    /// Insert a validated row. Returns its rowid.
    pub fn insert(&mut self, t: usize, row: Vec<Value>) -> Result<i64, Error> {
        self.check_usable()?;
        let Self {
            snapshot,
            txn,
            validators,
            ..
        } = self;
        let txn = txn.as_mut().ok_or_else(no_txn)?;
        check_width(snapshot, t, &row)?;
        let validator = validator_for(validators, snapshot, &txn.working[t], t)?;
        let row = validator.prepare_row(row, true)?;
        let rowid = txn.working[t].physical_rows();
        validator.insert_key(rowid, &row)?;
        if let Err(e) = txn.working[t].insert(row.clone()) {
            validators.remove(&t); // its keys no longer match the overlay
            return Err(format_err(e));
        }
        txn.pending.push(WalOp::Insert {
            table: table_u32(t)?,
            row,
        });
        i64::try_from(rowid).map_err(|_| Error::ModuleError(format!("rowid {rowid} out of range")))
    }

    /// Replace all values of a live row with a validated row.
    pub fn update(&mut self, t: usize, rowid: u64, row: Vec<Value>) -> Result<(), Error> {
        self.check_usable()?;
        let Self {
            snapshot,
            txn,
            validators,
            ..
        } = self;
        let txn = txn.as_mut().ok_or_else(no_txn)?;
        check_width(snapshot, t, &row)?;
        if !txn.working[t].is_live(rowid) {
            return Err(Error::ModuleError(format!(
                "cannot update rowid {rowid}: no such row"
            )));
        }
        let validator = validator_for(validators, snapshot, &txn.working[t], t)?;
        let row = validator.prepare_row(row, false)?;
        validator.update_key(rowid, &row)?;
        if let Err(e) = txn.working[t].update(rowid, row.clone()) {
            validators.remove(&t);
            return Err(format_err(e));
        }
        txn.pending.push(WalOp::Update {
            table: table_u32(t)?,
            rowid,
            row,
        });
        Ok(())
    }

    /// Delete a live row.
    pub fn delete(&mut self, t: usize, rowid: u64) -> Result<(), Error> {
        self.check_usable()?;
        let txn = self.txn.as_mut().ok_or_else(no_txn)?;
        txn.working[t].delete(rowid).map_err(format_err)?;
        txn.pending.push(WalOp::Delete {
            table: table_u32(t)?,
            rowid,
        });
        // A validator is only a cache of live keys; if updating it fails,
        // drop it and rebuild from the overlay when next needed.
        if let Some(v) = self.validators.get_mut(&t) {
            if v.delete_key(rowid).is_err() {
                self.validators.remove(&t);
            }
        }
        Ok(())
    }

    /// Make the transaction durable: one frame for all its changes. SQLite
    /// calls this on every participating table; only the first call writes.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.check_usable()?;
        let Some(txn) = self.txn.as_mut() else {
            return Ok(());
        };
        if txn.synced.is_none() && !txn.pending.is_empty() {
            let ops = std::mem::take(&mut txn.pending);
            txn.synced = Some(txn.writer.append_commit(ops).map_err(store_err)?);
        }
        Ok(())
    }

    /// Publish the transaction's changes to this connection and release the lock.
    pub fn commit(&mut self) -> Result<(), Error> {
        let Some(mut txn) = self.txn.take() else {
            return Ok(());
        };
        if txn.synced.is_none() && !txn.pending.is_empty() {
            // SQLite always syncs before committing; never drop writes if not.
            let ops = std::mem::take(&mut txn.pending);
            if let Err(e) = txn.writer.append_commit(ops) {
                self.poison(format!("changes could not be saved at commit: {e}"));
                return Err(store_err(e));
            }
        }
        let state = position(&txn.writer);
        self.snapshot
            .replace_overlays(txn.working, state)
            .map_err(store_err)
    }

    /// Discard the transaction. If its frame was already written, append a
    /// revert so the frame never applies.
    pub fn rollback(&mut self) -> Result<(), Error> {
        let Some(mut txn) = self.txn.take() else {
            return Ok(());
        };
        self.validators.clear();
        if let Some(seq) = txn.synced {
            if let Err(e) = txn.writer.append_revert(seq) {
                self.poison(format!(
                    "a rolled-back transaction could not be cancelled in the write log and may \
                     reappear when the file is reopened ({e}); check it with `egg validate`"
                ));
                return Err(store_err(e));
            }
        }
        if txn.writer.is_uncertain() {
            self.poison(
                "a failed write could not be removed from the write log and may reappear \
                 when the file is reopened; check it with `egg validate`"
                    .into(),
            );
        }
        self.snapshot.set_log_state(position(&txn.writer));
        Ok(())
    }

    fn poison(&mut self, why: String) {
        self.poisoned = Some(why);
    }
}

/// Lock shared state, turning a poisoned mutex (a panic elsewhere) into an error.
pub fn lock(shared: &Mutex<SharedEgg>) -> Result<MutexGuard<'_, SharedEgg>, Error> {
    shared.lock().map_err(|_| lock_poisoned())
}

fn validator_for<'a>(
    validators: &'a mut HashMap<usize, RowValidator>,
    snapshot: &EggSnapshot,
    overlay: &TableOverlay,
    t: usize,
) -> Result<&'a mut RowValidator, Error> {
    match validators.entry(t) {
        Entry::Occupied(e) => Ok(e.into_mut()),
        Entry::Vacant(e) => {
            let meta = &snapshot.tables()[t];
            let built =
                RowValidator::build(meta, |cols| live_key_rows(snapshot, overlay, t, cols))?;
            Ok(e.insert(built))
        }
    }
}

/// `(rowid, values)` for every live row, for the given columns.
fn live_key_rows(
    snapshot: &EggSnapshot,
    overlay: &TableOverlay,
    t: usize,
    cols: &[usize],
) -> Result<Vec<(u64, Vec<Value>)>, Error> {
    let physical = snapshot
        .decode_base_columns(t, cols)
        .map_err(store_err)?
        .into_iter()
        .zip(cols)
        .map(|(base, &c)| overlay.physical_column(c, base).map_err(format_err))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(overlay
        .live_mask()
        .iter_ones()
        .map(|p| {
            (
                p as u64,
                physical.iter().map(|col| col[p].clone()).collect(),
            )
        })
        .collect())
}

fn check_width(snapshot: &EggSnapshot, t: usize, row: &[Value]) -> Result<(), Error> {
    let expected = snapshot.tables()[t].columns.len();
    if row.len() == expected {
        Ok(())
    } else {
        Err(Error::ModuleError(format!(
            "row has {} values, table has {expected} columns",
            row.len()
        )))
    }
}

fn position(writer: &WalWriter) -> LogState {
    LogState::Active {
        next_seq: writer.next_seq(),
        valid_length: writer.valid_length(),
        torn_tail_bytes: 0,
    }
}

fn table_u32(t: usize) -> Result<u32, Error> {
    u32::try_from(t).map_err(|_| Error::ModuleError(format!("table index {t} out of range")))
}

fn no_txn() -> Error {
    Error::ModuleError("internal error: write outside a transaction".into())
}

fn lock_poisoned() -> Error {
    Error::ModuleError("internal error: egg state lock poisoned by an earlier panic".into())
}

/// Convert a store error for SQLite, keeping its full message.
pub fn store_err(e: StoreError) -> Error {
    Error::ModuleError(e.to_string())
}

/// Convert a format error for SQLite.
pub fn format_err(e: FormatError) -> Error {
    Error::ModuleError(e.to_string())
}
