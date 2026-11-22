//! Model-based property tests for `TableOverlay` and the write log.
//!
//! A naive model (`Vec<Option<Row>>` over physical rows) runs the same random
//! operation sequence as the real overlay. They must agree on every read, on
//! which operations are rejected, and on replay from serialized log bytes.

use std::collections::BTreeSet;

use bitvec::prelude::*;
use eggdb_core::{
    encode_frame, parse_wal, replay, ColumnMeta, Encoding, SqliteType, TableMeta, TableOverlay,
    Value, WalCommit, WalFrame, WalHeader, WalOp,
};
use proptest::prelude::*;

const COLS: usize = 2;

fn arb_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<i64>().prop_map(Value::Integer),
        prop::num::f64::NORMAL.prop_map(Value::Real),
        "[a-z]{0,8}".prop_map(Value::Text),
        prop::collection::vec(any::<u8>(), 0..8).prop_map(Value::Blob),
    ]
}

fn arb_row() -> impl Strategy<Value = Vec<Value>> {
    prop::collection::vec(arb_value(), COLS)
}

/// Abstract operations. `pick` selects a target rowid; most picks land on
/// existing rows, some deliberately miss to test error parity.
#[derive(Clone, Debug)]
enum Action {
    Insert(Vec<Value>),
    Update(u64, Vec<Value>),
    Delete(u64),
}

fn arb_action() -> impl Strategy<Value = Action> {
    prop_oneof![
        3 => arb_row().prop_map(Action::Insert),
        3 => (0u64..40, arb_row()).prop_map(|(p, r)| Action::Update(p, r)),
        3 => (0u64..40).prop_map(Action::Delete),
    ]
}

/// The reference model: physical rows, `None` = deleted.
struct Model {
    rows: Vec<Option<Vec<Value>>>,
    base_rows: u64,
    updated_base: BTreeSet<u64>,
}

impl Model {
    fn live(&self, rowid: u64) -> bool {
        self.rows.get(rowid as usize).is_some_and(Option::is_some)
    }

    /// Apply to the model; returns false when the action must be rejected.
    fn apply(&mut self, action: &Action) -> bool {
        match action {
            Action::Insert(row) => {
                self.rows.push(Some(row.clone()));
                true
            }
            Action::Update(rowid, row) if self.live(*rowid) => {
                self.rows[*rowid as usize] = Some(row.clone());
                if *rowid < self.base_rows {
                    self.updated_base.insert(*rowid);
                }
                true
            }
            Action::Delete(rowid) if self.live(*rowid) => {
                self.rows[*rowid as usize] = None;
                true
            }
            _ => false,
        }
    }
}

fn to_op(action: &Action) -> WalOp {
    match action.clone() {
        Action::Insert(row) => WalOp::Insert { table: 0, row },
        Action::Update(rowid, row) => WalOp::Update {
            table: 0,
            rowid,
            row,
        },
        Action::Delete(rowid) => WalOp::Delete { table: 0, rowid },
    }
}

fn apply_to_overlay(ov: &mut TableOverlay, action: &Action) -> bool {
    match action.clone() {
        Action::Insert(row) => ov.insert(row).is_ok(),
        Action::Update(rowid, row) => ov.update(rowid, row).is_ok(),
        Action::Delete(rowid) => ov.delete(rowid).is_ok(),
    }
}

fn table_meta(base_rows: u64) -> TableMeta {
    let column = |name: &str| ColumnMeta {
        name: name.into(),
        sqlite_type: SqliteType::Text,
        encoding: Encoding::Raw,
        cardinality: 0,
        null_count: 0,
        yolk_offset: 0,
        yolk_length: 0,
        uncompressed_size: 0,
        dictionary: None,
        min_value: None,
        max_value: None,
    };
    TableMeta {
        table_name: "t".into(),
        row_count: base_rows,
        original_ddl: "CREATE TABLE t (a, b)".into(),
        columns: vec![column("a"), column("b")],
        index_definitions: vec![],
    }
}

fn check_reads(ov: &TableOverlay, model: &Model, base: &[Vec<Value>]) {
    for (c, base_col) in base.iter().enumerate() {
        let expected_physical: Vec<Value> = model
            .rows
            .iter()
            .map(|r| r.as_ref().map_or(Value::Null, |row| row[c].clone()))
            .collect();
        let expected_visible: Vec<Value> = model
            .rows
            .iter()
            .flatten()
            .map(|row| row[c].clone())
            .collect();
        assert_eq!(
            ov.physical_column(c, base_col.clone()).unwrap(),
            expected_physical
        );
        assert_eq!(
            ov.visible_column(c, base_col.clone()).unwrap(),
            expected_visible
        );
    }
    let expected_mask: BitVec = model.rows.iter().map(Option::is_some).collect();
    assert_eq!(ov.live_mask(), expected_mask);
    assert_eq!(ov.live_rows(), expected_mask.count_ones() as u64);
    assert_eq!(ov.physical_rows(), model.rows.len() as u64);
}

fn check_widen_has_no_false_negatives(ov: &TableOverlay, model: &Model, base_mask: &BitVec) {
    let widened = ov.widen_base_mask(base_mask.clone()).unwrap();
    assert_eq!(widened.len(), model.rows.len());
    for (p, slot) in model.rows.iter().enumerate() {
        let p64 = p as u64;
        if slot.is_none() {
            assert!(!widened[p], "deleted row {p} must be excluded");
        } else if p64 >= model.base_rows || model.updated_base.contains(&p64) || base_mask[p] {
            assert!(widened[p], "row {p} may match and must be included");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn overlay_matches_model(
        base_rows in prop::collection::vec(arb_row(), 0..12),
        actions in prop::collection::vec(arb_action(), 0..60),
        mask_seed in prop::collection::vec(any::<bool>(), 12),
    ) {
        let n = base_rows.len() as u64;
        let base: Vec<Vec<Value>> = (0..COLS)
            .map(|c| base_rows.iter().map(|r| r[c].clone()).collect())
            .collect();
        let mut model = Model {
            rows: base_rows.iter().cloned().map(Some).collect(),
            base_rows: n,
            updated_base: BTreeSet::new(),
        };
        let mut ov = TableOverlay::new(n, COLS);
        let mut accepted = Vec::new();

        for action in &actions {
            let model_ok = model.apply(action);
            let overlay_ok = apply_to_overlay(&mut ov, action);
            prop_assert_eq!(model_ok, overlay_ok, "acceptance differs for {:?}", action);
            if model_ok {
                accepted.push(to_op(action));
            }
            check_reads(&ov, &model, &base);
        }

        let base_mask: BitVec = mask_seed.iter().copied().take(base_rows.len()).collect();
        check_widen_has_no_false_negatives(&ov, &model, &base_mask);

        // Serialize accepted ops as log frames (split into two commits), parse,
        // and replay: the result must equal the overlay built directly.
        let mid = accepted.len() / 2;
        let mut log = WalHeader { base_checksum: 1, base_length: 2 }.to_bytes().to_vec();
        log.extend(encode_frame(&WalFrame::Commit { seq: 0, ops: accepted[..mid].to_vec() }).unwrap());
        log.extend(encode_frame(&WalFrame::Commit { seq: 1, ops: accepted[mid..].to_vec() }).unwrap());
        let parsed = parse_wal(&log).unwrap();
        let commits: Vec<&WalCommit> = parsed.commits().collect();
        let replayed = replay(&[table_meta(n)], commits).unwrap();
        prop_assert_eq!(&replayed[0], &ov);
    }
}
