//! Per-table overlay: replays write-log operations on top of a table's
//! immutable main-file rows. Pure, no IO.
//!
//! # Physical row space
//!
//! A table's rows are numbered `[0, base_rows + appended)`. Rows below
//! `base_rows` come from the main file; rows above come from log inserts, in
//! commit order. **A rowid is its physical index**, so rowids are stable until
//! compaction: updates replace values in place, deletes leave a tombstone, and
//! deleted rowids are never reused.

use std::collections::BTreeMap;

use bitvec::prelude::*;

use crate::format::albumen::TableMeta;
use crate::format::wal::{WalCommit, WalOp};
use crate::format::FormatError;
use crate::value::Value;

/// How many changes an overlay holds relative to the main file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingCounts {
    /// Rows added through the log that are still live.
    pub inserted: u64,
    /// Main-file rows whose values were replaced and are still live.
    pub updated: u64,
    /// Main-file rows that were deleted.
    pub deleted: u64,
}

impl PendingCounts {
    /// True when the overlay changes nothing visible.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inserted == 0 && self.updated == 0 && self.deleted == 0
    }
}

/// Changes to one table, layered over its main-file rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableOverlay {
    base_rows: u64,
    column_count: usize,
    /// Tombstones over main-file rows. Empty until the first delete.
    deleted_base: BitVec,
    deleted_base_count: u64,
    /// Replacement values for live main-file rows.
    updated_base: BTreeMap<u64, Vec<Value>>,
    /// Rows inserted through the log. `None` = inserted, then deleted.
    appended: Vec<Option<Vec<Value>>>,
    appended_live: u64,
}

impl TableOverlay {
    /// An empty overlay over `base_rows` main-file rows with `column_count` columns.
    #[must_use]
    pub fn new(base_rows: u64, column_count: usize) -> Self {
        Self {
            base_rows,
            column_count,
            deleted_base: BitVec::new(),
            deleted_base_count: 0,
            updated_base: BTreeMap::new(),
            appended: Vec::new(),
            appended_live: 0,
        }
    }

    /// Number of rows in the main file.
    #[must_use]
    pub fn base_rows(&self) -> u64 {
        self.base_rows
    }

    /// Number of physical rows (main-file rows plus every logged insert).
    #[must_use]
    pub fn physical_rows(&self) -> u64 {
        self.base_rows + self.appended.len() as u64
    }

    /// Number of rows a query sees.
    #[must_use]
    pub fn live_rows(&self) -> u64 {
        self.base_rows - self.deleted_base_count + self.appended_live
    }

    /// True when the overlay holds no changes at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deleted_base_count == 0 && self.updated_base.is_empty() && self.appended.is_empty()
    }

    /// Change counts relative to the main file.
    #[must_use]
    pub fn pending_counts(&self) -> PendingCounts {
        PendingCounts {
            inserted: self.appended_live,
            updated: self.updated_base.len() as u64,
            deleted: self.deleted_base_count,
        }
    }

    /// Whether `rowid` names a live row.
    #[must_use]
    pub fn is_live(&self, rowid: u64) -> bool {
        if rowid < self.base_rows {
            !self.is_base_deleted(rowid)
        } else {
            self.appended_slot(rowid)
                .is_some_and(|slot| self.appended[slot].is_some())
        }
    }

    /// Append a row. Returns its rowid.
    pub fn insert(&mut self, row: Vec<Value>) -> Result<u64, FormatError> {
        self.check_width(&row)?;
        let rowid = self.physical_rows();
        self.appended.push(Some(row));
        self.appended_live += 1;
        Ok(rowid)
    }

    /// Replace all values of a live row.
    pub fn update(&mut self, rowid: u64, row: Vec<Value>) -> Result<(), FormatError> {
        self.check_width(&row)?;
        self.check_live(rowid, "update")?;
        if rowid < self.base_rows {
            self.updated_base.insert(rowid, row);
        } else if let Some(slot) = self.appended_slot(rowid) {
            self.appended[slot] = Some(row);
        }
        Ok(())
    }

    /// Delete a live row.
    pub fn delete(&mut self, rowid: u64) -> Result<(), FormatError> {
        self.check_live(rowid, "delete")?;
        if rowid < self.base_rows {
            if self.deleted_base.is_empty() {
                self.deleted_base = bitvec![0; to_index(self.base_rows)?];
            }
            self.deleted_base.set(to_index(rowid)?, true);
            self.deleted_base_count += 1;
            self.updated_base.remove(&rowid);
        } else if let Some(slot) = self.appended_slot(rowid) {
            self.appended[slot] = None;
            self.appended_live -= 1;
        }
        Ok(())
    }

    /// One column over the physical row space, given that column's main-file
    /// values. Deleted slots hold `Null`; use [`live_mask`](Self::live_mask)
    /// to skip them.
    pub fn physical_column(&self, col: usize, base: Vec<Value>) -> Result<Vec<Value>, FormatError> {
        let mut out = self.patched_base(col, base)?;
        out.reserve(self.appended.len());
        out.extend(
            self.appended
                .iter()
                .map(|slot| slot.as_ref().map_or(Value::Null, |row| row[col].clone())),
        );
        Ok(out)
    }

    /// One column containing only live rows, in rowid order.
    pub fn visible_column(&self, col: usize, base: Vec<Value>) -> Result<Vec<Value>, FormatError> {
        let mut out = self.patched_base(col, base)?;
        if self.deleted_base_count > 0 {
            let mut idx = 0usize;
            out.retain(|_| {
                let keep = !self.deleted_base[idx];
                idx += 1;
                keep
            });
        }
        out.extend(self.appended.iter().flatten().map(|row| row[col].clone()));
        Ok(out)
    }

    /// One bit per physical row: 1 = live.
    #[must_use]
    pub fn live_mask(&self) -> BitVec {
        let mut mask = if self.deleted_base.is_empty() {
            bitvec![1; self.base_rows as usize]
        } else {
            !self.deleted_base.clone()
        };
        mask.extend(self.appended.iter().map(Option::is_some));
        mask
    }

    /// Turn a filter mask computed on main-file rows into a physical-space mask
    /// with no false negatives: updated rows are included (their new value may
    /// match), deleted rows are excluded, and every live appended row is
    /// included. The caller must re-check the filter on returned rows.
    pub fn widen_base_mask(&self, base_mask: BitVec) -> Result<BitVec, FormatError> {
        if base_mask.len() as u64 != self.base_rows {
            return Err(overlay_err(format!(
                "filter mask has {} bits, table has {} main-file rows",
                base_mask.len(),
                self.base_rows
            )));
        }
        let mut mask = base_mask;
        for &rowid in self.updated_base.keys() {
            mask.set(to_index(rowid)?, true);
        }
        if !self.deleted_base.is_empty() {
            mask &= !self.deleted_base.clone();
        }
        mask.extend(self.appended.iter().map(Option::is_some));
        Ok(mask)
    }

    fn patched_base(&self, col: usize, mut base: Vec<Value>) -> Result<Vec<Value>, FormatError> {
        if col >= self.column_count {
            return Err(overlay_err(format!(
                "column {col} out of range ({} columns)",
                self.column_count
            )));
        }
        if base.len() as u64 != self.base_rows {
            return Err(overlay_err(format!(
                "column {col} has {} main-file values, expected {}",
                base.len(),
                self.base_rows
            )));
        }
        for (&rowid, row) in &self.updated_base {
            base[to_index(rowid)?] = row[col].clone();
        }
        for idx in self.deleted_base.iter_ones() {
            base[idx] = Value::Null;
        }
        Ok(base)
    }

    fn is_base_deleted(&self, rowid: u64) -> bool {
        !self.deleted_base.is_empty() && self.deleted_base[rowid as usize]
    }

    fn appended_slot(&self, rowid: u64) -> Option<usize> {
        let slot = usize::try_from(rowid.checked_sub(self.base_rows)?).ok()?;
        (slot < self.appended.len()).then_some(slot)
    }

    fn check_width(&self, row: &[Value]) -> Result<(), FormatError> {
        if row.len() == self.column_count {
            Ok(())
        } else {
            Err(overlay_err(format!(
                "row has {} values, table has {} columns",
                row.len(),
                self.column_count
            )))
        }
    }

    fn check_live(&self, rowid: u64, action: &str) -> Result<(), FormatError> {
        if self.is_live(rowid) {
            Ok(())
        } else {
            Err(overlay_err(format!(
                "cannot {action} rowid {rowid}: no such live row"
            )))
        }
    }
}

/// Apply one log operation to the overlay of the table it targets.
/// Returns the rowid the operation affected.
pub fn apply_op(overlays: &mut [TableOverlay], op: &WalOp) -> Result<u64, FormatError> {
    let table = op.table();
    let overlay = overlays
        .get_mut(table as usize)
        .ok_or_else(|| overlay_err(format!("operation targets unknown table index {table}")))?;
    match op {
        WalOp::Insert { row, .. } => overlay.insert(row.clone()),
        WalOp::Update { rowid, row, .. } => overlay.update(*rowid, row.clone()).map(|()| *rowid),
        WalOp::Delete { rowid, .. } => overlay.delete(*rowid).map(|()| *rowid),
    }
}

/// Build one overlay per table by replaying committed transactions in order.
pub fn replay<'a>(
    tables: &[TableMeta],
    commits: impl IntoIterator<Item = &'a WalCommit>,
) -> Result<Vec<TableOverlay>, FormatError> {
    let mut overlays: Vec<TableOverlay> = tables
        .iter()
        .map(|t| TableOverlay::new(t.row_count, t.columns.len()))
        .collect();
    for commit in commits {
        for op in &commit.ops {
            apply_op(&mut overlays, op)
                .map_err(|e| overlay_err(format!("replaying commit {}: {e}", commit.seq)))?;
        }
    }
    Ok(overlays)
}

fn to_index(rowid: u64) -> Result<usize, FormatError> {
    usize::try_from(rowid).map_err(|_| overlay_err(format!("rowid {rowid} exceeds address space")))
}

fn overlay_err(msg: impl Into<String>) -> FormatError {
    FormatError::WriteLog(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Value {
        Value::Integer(n)
    }

    fn row(a: i64, b: &str) -> Vec<Value> {
        vec![int(a), Value::Text(b.into())]
    }

    /// Three main-file rows: (1,a) (2,b) (3,c).
    fn base_cols() -> (Vec<Value>, Vec<Value>) {
        (
            vec![int(1), int(2), int(3)],
            vec![
                Value::Text("a".into()),
                Value::Text("b".into()),
                Value::Text("c".into()),
            ],
        )
    }

    #[test]
    fn empty_overlay_passes_base_through() {
        let ov = TableOverlay::new(3, 2);
        let (ids, _) = base_cols();
        assert!(ov.is_empty());
        assert_eq!(ov.live_rows(), 3);
        assert_eq!(ov.visible_column(0, ids.clone()).unwrap(), ids);
        assert_eq!(ov.physical_column(0, ids.clone()).unwrap(), ids);
        assert_eq!(ov.live_mask(), bitvec![1, 1, 1]);
        assert!(ov.pending_counts().is_empty());
    }

    #[test]
    fn insert_appends_with_next_physical_rowid() {
        let mut ov = TableOverlay::new(3, 2);
        assert_eq!(ov.insert(row(4, "d")).unwrap(), 3);
        assert_eq!(ov.insert(row(5, "e")).unwrap(), 4);
        let (ids, _) = base_cols();
        assert_eq!(
            ov.visible_column(0, ids).unwrap(),
            vec![int(1), int(2), int(3), int(4), int(5)]
        );
        assert_eq!(ov.live_rows(), 5);
        assert_eq!(ov.pending_counts().inserted, 2);
    }

    #[test]
    fn update_keeps_rowid_and_order() {
        let mut ov = TableOverlay::new(3, 2);
        ov.update(1, row(20, "B")).unwrap();
        let (_, names) = base_cols();
        assert_eq!(
            ov.visible_column(1, names).unwrap(),
            vec![
                Value::Text("a".into()),
                Value::Text("B".into()),
                Value::Text("c".into())
            ]
        );
        assert_eq!(ov.pending_counts().updated, 1);
    }

    #[test]
    fn delete_tombstones_and_rowids_are_not_reused() {
        let mut ov = TableOverlay::new(3, 2);
        ov.delete(0).unwrap();
        let new_id = ov.insert(row(9, "z")).unwrap();
        ov.delete(new_id).unwrap();
        assert_eq!(
            ov.insert(row(10, "y")).unwrap(),
            4,
            "rowid 3 must not be reused"
        );

        let (ids, _) = base_cols();
        assert_eq!(
            ov.visible_column(0, ids.clone()).unwrap(),
            vec![int(2), int(3), int(10)]
        );
        assert_eq!(
            ov.physical_column(0, ids).unwrap(),
            vec![Value::Null, int(2), int(3), Value::Null, int(10)]
        );
        assert_eq!(ov.live_mask(), bitvec![0, 1, 1, 0, 1]);
        assert_eq!(ov.live_rows(), 3);
        assert_eq!(
            ov.pending_counts(),
            PendingCounts {
                inserted: 1,
                updated: 0,
                deleted: 1
            }
        );
    }

    #[test]
    fn delete_after_update_drops_the_update() {
        let mut ov = TableOverlay::new(3, 2);
        ov.update(2, row(30, "C")).unwrap();
        ov.delete(2).unwrap();
        assert_eq!(ov.pending_counts().updated, 0);
        assert_eq!(ov.pending_counts().deleted, 1);
    }

    #[test]
    fn invalid_operations_are_errors() {
        let mut ov = TableOverlay::new(3, 2);
        ov.delete(1).unwrap();
        assert!(ov.delete(1).is_err(), "double delete");
        assert!(ov.update(1, row(0, "x")).is_err(), "update deleted");
        assert!(ov.update(3, row(0, "x")).is_err(), "update past end");
        assert!(ov.delete(u64::MAX).is_err(), "delete far past end");
        assert!(ov.insert(vec![int(1)]).is_err(), "too few values");
        assert!(ov.update(0, vec![int(1); 3]).is_err(), "too many values");
        assert!(ov.visible_column(2, vec![]).is_err(), "bad column");
        assert!(ov.visible_column(0, vec![int(1)]).is_err(), "short base");
    }

    #[test]
    fn widen_mask_has_no_false_negatives() {
        let mut ov = TableOverlay::new(3, 2);
        ov.update(0, row(99, "match")).unwrap();
        ov.delete(1).unwrap();
        ov.insert(row(7, "new")).unwrap();
        // Encoding-level filter on old values matched only row 1 (now deleted).
        let widened = ov.widen_base_mask(bitvec![0, 1, 0]).unwrap();
        assert_eq!(widened, bitvec![1, 0, 0, 1]);
        assert!(ov.widen_base_mask(bitvec![0, 1]).is_err());
    }

    #[test]
    fn replay_applies_commits_in_order_and_reports_bad_ops() {
        let tables = vec![TableMeta {
            table_name: "t".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE t (a, b)".into(),
            columns: vec![],
            index_definitions: vec![],
        }];
        // Zero declared columns: any row width is wrong, so the error path is
        // exercised by the second commit.
        let ok = WalCommit {
            seq: 0,
            ops: vec![WalOp::Delete { table: 0, rowid: 2 }],
        };
        let overlays = replay(&tables, [&ok]).unwrap();
        assert_eq!(overlays[0].live_rows(), 2);

        let bad = WalCommit {
            seq: 1,
            ops: vec![WalOp::Insert {
                table: 0,
                row: vec![int(1)],
            }],
        };
        let err = replay(&tables, [&ok, &bad]).unwrap_err().to_string();
        assert!(err.contains("commit 1"), "{err}");

        let unknown = WalCommit {
            seq: 0,
            ops: vec![WalOp::Delete { table: 5, rowid: 0 }],
        };
        assert!(replay(&tables, [&unknown]).is_err());
    }
}
