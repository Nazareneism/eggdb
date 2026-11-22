//! Integration tests for snapshots, the log writer, and compaction.
//!
//! Crash recovery is tested by constructing, on disk, the exact state a crash
//! at each step would leave behind, then checking how it is read and repaired.

use std::io::Write;
use std::path::{Path, PathBuf};

use eggdb_core::{
    derive_key, encode_column, encode_frame, generate_salt, write_egg, EggReader, Header,
    HeaderFlags, SqliteType, TableInput, Value, WalFrame, WalHeader, WalOp, WriteOptions,
    Zeroizing,
};
use eggdb_store::{
    compact, compact_tmp_path, wal_path, CompactOptions, CompactOutcome, EggSnapshot, LogState,
    StoreError, WalWriter,
};

// ---------- fixtures ----------

fn int(n: i64) -> Value {
    Value::Integer(n)
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn users(n: i64) -> TableInput {
    let ids: Vec<Value> = (1..=n).map(int).collect();
    let names: Vec<Value> = (1..=n).map(|i| text(&format!("user{i}"))).collect();
    TableInput {
        table_name: "users".into(),
        row_count: n as u64,
        original_ddl: "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)".into(),
        columns: vec![
            encode_column("id", SqliteType::Integer, &ids),
            encode_column("name", SqliteType::Text, &names),
        ],
        index_definitions: vec![],
    }
}

fn tags() -> TableInput {
    let vals = vec![text("red"), text("green"), Value::Null];
    TableInput {
        table_name: "tags".into(),
        row_count: 3,
        original_ddl: "CREATE TABLE tags (tag TEXT)".into(),
        columns: vec![encode_column("tag", SqliteType::Text, &vals)],
        index_definitions: vec![],
    }
}

fn write_file(path: &Path, tables: &[TableInput], options: &WriteOptions) {
    std::fs::write(path, write_egg(tables, options).unwrap()).unwrap();
}

/// A temp dir holding `data.egg` with `users` (5 rows) and `tags` (3 rows).
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let egg = dir.path().join("data.egg");
    write_file(&egg, &[users(5), tags()], &WriteOptions::default());
    (dir, egg)
}

fn base_of(egg: &Path) -> WalHeader {
    EggSnapshot::open(egg, None).unwrap().base()
}

fn row(id: i64, name: &str) -> Vec<Value> {
    vec![int(id), text(name)]
}

/// Commit: insert (6,new), update rowid 0 → (1,renamed), delete rowid 4.
fn commit_sample_changes(egg: &Path) {
    let (mut w, _) = WalWriter::acquire(egg, Some(base_of(egg))).unwrap();
    w.append_commit(vec![
        WalOp::Insert {
            table: 0,
            row: row(6, "new"),
        },
        WalOp::Update {
            table: 0,
            rowid: 0,
            row: row(1, "renamed"),
        },
        WalOp::Delete { table: 0, rowid: 4 },
    ])
    .unwrap();
}

fn expected_users_after_sample() -> Vec<Vec<Value>> {
    vec![
        vec![int(1), int(2), int(3), int(4), int(6)],
        vec![
            text("renamed"),
            text("user2"),
            text("user3"),
            text("user4"),
            text("new"),
        ],
    ]
}

// ---------- snapshots and the writer ----------

#[test]
fn open_without_log_sees_main_file() {
    let (_dir, egg) = fixture();
    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert_eq!(snap.log_state(), &LogState::Absent);
    assert!(!snap.has_pending_changes());
    assert_eq!(snap.live_row_count(0), 5);
    assert_eq!(
        snap.visible_table(1).unwrap(),
        vec![vec![text("red"), text("green"), Value::Null]]
    );
}

#[test]
fn committed_changes_are_visible_after_reopen() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);

    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert!(matches!(
        snap.log_state(),
        LogState::Active {
            next_seq: 1,
            torn_tail_bytes: 0,
            ..
        }
    ));
    assert!(snap.has_pending_changes());
    assert_eq!(snap.live_row_count(0), 5);
    assert_eq!(
        snap.visible_table(0).unwrap(),
        expected_users_after_sample()
    );
    // Untouched table, and the main file itself, are unchanged.
    assert_eq!(snap.live_row_count(1), 3);
    assert_eq!(
        EggReader::open(snap.data()).unwrap().tables()[0].row_count,
        5
    );
}

#[test]
fn second_writer_is_busy() {
    let (_dir, egg) = fixture();
    let (_held, _) = WalWriter::acquire(&egg, None).unwrap();
    let err = WalWriter::acquire(&egg, None).unwrap_err();
    assert!(matches!(err, StoreError::Busy { .. }), "{err}");
}

#[test]
fn writer_rejects_stale_view_of_main_file() {
    let (_dir, egg) = fixture();
    let old_base = base_of(&egg);
    write_file(
        &egg,
        &[users(2)],
        &WriteOptions {
            created_at: 42,
            ..WriteOptions::default()
        },
    );
    let err = WalWriter::acquire(&egg, Some(old_base)).unwrap_err();
    assert!(matches!(err, StoreError::Stale { .. }), "{err}");
}

#[test]
fn log_for_a_different_main_file_is_an_error_not_ignored() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    // Re-lay the main file without compacting: the log no longer matches.
    write_file(
        &egg,
        &[users(5), tags()],
        &WriteOptions {
            created_at: 7,
            ..WriteOptions::default()
        },
    );

    let err = EggSnapshot::open(&egg, None).unwrap_err();
    assert!(matches!(err, StoreError::ForeignLog { .. }), "{err}");
    let err = WalWriter::acquire(&egg, None).unwrap_err();
    assert!(matches!(err, StoreError::ForeignLog { .. }), "{err}");
}

#[test]
fn torn_tail_is_ignored_by_readers_and_truncated_by_writer() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    let partial = encode_frame(&WalFrame::Commit {
        seq: 1,
        ops: vec![WalOp::Delete { table: 1, rowid: 0 }],
    })
    .unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(wal_path(&egg))
        .unwrap();
    f.write_all(&partial[..partial.len() - 3]).unwrap();
    drop(f);

    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert!(
        matches!(snap.log_state(), LogState::Active { torn_tail_bytes, .. } if *torn_tail_bytes > 0)
    );
    assert_eq!(snap.live_row_count(1), 3, "partial frame must not apply");

    let (mut w, log) = WalWriter::acquire(&egg, Some(snap.base())).unwrap();
    assert_eq!(log.next_seq, 1);
    w.append_commit(vec![WalOp::Delete { table: 1, rowid: 2 }])
        .unwrap();
    drop(w);

    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert!(matches!(
        snap.log_state(),
        LogState::Active {
            next_seq: 2,
            torn_tail_bytes: 0,
            ..
        }
    ));
    assert_eq!(
        snap.visible_table(1).unwrap(),
        vec![vec![text("red"), text("green")]]
    );
}

#[test]
fn revert_cancels_the_last_commit_only() {
    let (_dir, egg) = fixture();
    let (mut w, _) = WalWriter::acquire(&egg, None).unwrap();
    let keep = w
        .append_commit(vec![WalOp::Delete { table: 1, rowid: 0 }])
        .unwrap();
    let undo = w
        .append_commit(vec![WalOp::Delete { table: 1, rowid: 1 }])
        .unwrap();
    assert!(
        w.append_revert(keep).is_err(),
        "only the last commit can be reverted"
    );
    w.append_revert(undo).unwrap();
    assert!(w.append_revert(undo).is_err(), "cannot revert twice");
    drop(w);

    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert_eq!(
        snap.visible_table(1).unwrap(),
        vec![vec![text("green"), Value::Null]]
    );
}

#[test]
fn interrupted_log_creation_reads_as_absent_and_is_reset() {
    let (_dir, egg) = fixture();
    std::fs::write(wal_path(&egg), b"EGGW\x01partial").unwrap();
    assert_eq!(
        EggSnapshot::open(&egg, None).unwrap().log_state(),
        &LogState::Absent
    );

    let (mut w, log) = WalWriter::acquire(&egg, None).unwrap();
    assert!(log.entries.is_empty());
    w.append_commit(vec![WalOp::Delete { table: 1, rowid: 0 }])
        .unwrap();
    drop(w);
    assert_eq!(EggSnapshot::open(&egg, None).unwrap().live_row_count(1), 2);
}

#[test]
fn corrupt_frame_in_the_middle_is_an_error() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    let (mut w, _) = WalWriter::acquire(&egg, None).unwrap();
    w.append_commit(vec![WalOp::Delete { table: 1, rowid: 0 }])
        .unwrap();
    drop(w);
    let wal = wal_path(&egg);
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes[eggdb_core::WAL_HEADER_SIZE + 10] ^= 0xFF;
    std::fs::write(&wal, bytes).unwrap();
    let err = EggSnapshot::open(&egg, None).unwrap_err();
    assert!(matches!(err, StoreError::Format { .. }), "{err}");
}

#[test]
fn writes_to_encrypted_eggs_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let egg = dir.path().join("secret.egg");
    let salt = generate_salt();
    let key = derive_key("pw", &salt).unwrap();
    let options = WriteOptions {
        flags: HeaderFlags::new(true, true),
        encryption_key: Some(Zeroizing::new(*key)),
        encryption_salt: Some(salt),
        ..WriteOptions::default()
    };
    write_file(&egg, &[tags()], &options);
    let err = WalWriter::acquire(&egg, None).unwrap_err();
    assert!(matches!(err, StoreError::EncryptedWrite { .. }), "{err}");
    // Reading still works with the passphrase.
    let snap = EggSnapshot::open(&egg, Some("pw")).unwrap();
    assert_eq!(snap.visible_table(0).unwrap()[0].len(), 3);
}

// ---------- compaction ----------

#[test]
fn compaction_folds_changes_and_removes_log() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    let before = EggSnapshot::open(&egg, None).unwrap();
    let old_tags_block = block_bytes(&before, 1, 0);

    let outcome = compact(&egg, &CompactOptions::default()).unwrap();
    let CompactOutcome::Compacted(report) = outcome else {
        panic!("expected compaction, got {outcome:?}");
    };
    assert_eq!(report.tables[0].rows_before, 5);
    assert_eq!(report.tables[0].rows_after, 5);
    assert_eq!(report.tables[0].changes.inserted, 1);
    assert!(report.tables[0].rewritten);
    assert!(
        !report.tables[1].rewritten,
        "unchanged table must be copied"
    );
    assert!(!wal_path(&egg).exists());
    assert!(!compact_tmp_path(&egg).exists());

    let after = EggSnapshot::open(&egg, None).unwrap();
    assert_eq!(after.log_state(), &LogState::Absent);
    assert_eq!(
        after.visible_table(0).unwrap(),
        expected_users_after_sample()
    );
    assert_eq!(after.tables()[0].row_count, 5);
    assert_eq!(
        block_bytes(&after, 1, 0),
        old_tags_block,
        "reused block must be byte-identical"
    );
    assert!(eggdb_core::validate(after.data())
        .unwrap()
        .iter()
        .all(|r| r.passed));
}

#[test]
fn compaction_with_nothing_pending() {
    let (_dir, egg) = fixture();
    assert_eq!(
        compact(&egg, &CompactOptions::default()).unwrap(),
        CompactOutcome::NothingPending
    );

    // An empty log (writer that never committed) is removed.
    drop(WalWriter::acquire(&egg, None).unwrap());
    assert!(wal_path(&egg).exists());
    assert_eq!(
        compact(&egg, &CompactOptions::default()).unwrap(),
        CompactOutcome::NothingPending
    );
    assert!(!wal_path(&egg).exists());
}

#[test]
fn crash_after_marker_before_rename_replays_normally() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    // Compaction got as far as writing the tmp file and the marker.
    std::fs::write(compact_tmp_path(&egg), b"half-written new main file").unwrap();
    let (mut w, _) = WalWriter::acquire(&egg, None).unwrap();
    w.append_compacted(0x1234_5678, 999).unwrap();
    drop(w);

    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert!(matches!(snap.log_state(), LogState::Active { .. }));
    assert_eq!(
        snap.visible_table(0).unwrap(),
        expected_users_after_sample()
    );

    // A later compaction overwrites the stray tmp file and succeeds.
    assert!(matches!(
        compact(&egg, &CompactOptions::default()).unwrap(),
        CompactOutcome::Compacted(_)
    ));
    assert_eq!(
        EggSnapshot::open(&egg, None)
            .unwrap()
            .visible_table(0)
            .unwrap(),
        expected_users_after_sample()
    );
}

#[test]
fn crash_after_rename_before_log_removal_is_recognized_as_absorbed() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    let old_log = std::fs::read(wal_path(&egg)).unwrap();
    compact(&egg, &CompactOptions::default()).unwrap();

    // Recreate the log as it was just before removal: old frames + marker.
    let new_main = std::fs::read(&egg).unwrap();
    let checksum = Header::from_bytes(&new_main).unwrap().checksum;
    let mut log = old_log.clone();
    log.extend(
        encode_frame(&WalFrame::Compacted {
            checksum,
            length: new_main.len() as u64,
        })
        .unwrap(),
    );
    std::fs::write(wal_path(&egg), &log).unwrap();

    let snap = EggSnapshot::open(&egg, None).unwrap();
    assert_eq!(snap.log_state(), &LogState::Absorbed);
    assert!(
        !snap.has_pending_changes(),
        "absorbed changes must not apply twice"
    );
    assert_eq!(
        snap.visible_table(0).unwrap(),
        expected_users_after_sample()
    );

    // The next writer resets the absorbed log and continues normally.
    let (mut w, parsed) = WalWriter::acquire(&egg, Some(snap.base())).unwrap();
    assert!(parsed.entries.is_empty());
    w.append_commit(vec![WalOp::Delete { table: 1, rowid: 0 }])
        .unwrap();
    drop(w);
    assert_eq!(EggSnapshot::open(&egg, None).unwrap().live_row_count(1), 2);

    // A marker for some other file is not "absorbed": it is foreign.
    let mut wrong = old_log;
    wrong.extend(
        encode_frame(&WalFrame::Compacted {
            checksum: checksum ^ 1,
            length: 1,
        })
        .unwrap(),
    );
    std::fs::write(wal_path(&egg), &wrong).unwrap();
    assert!(matches!(
        EggSnapshot::open(&egg, None),
        Err(StoreError::ForeignLog { .. })
    ));
}

#[test]
fn compaction_refuses_a_corrupt_main_file_and_keeps_the_log() {
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    let snap = EggSnapshot::open(&egg, None).unwrap();
    let tags_col = &snap.tables()[1].columns[0];
    let offset = (snap.header().yolk_offset + tags_col.yolk_offset) as usize;
    let mut data = std::fs::read(&egg).unwrap();
    data[offset] ^= 0xFF;
    std::fs::write(&egg, &data).unwrap();
    let log_before = std::fs::read(wal_path(&egg)).unwrap();

    let err = compact(&egg, &CompactOptions::default()).unwrap_err();
    assert!(matches!(err, StoreError::Integrity { .. }), "{err}");
    assert_eq!(
        std::fs::read(wal_path(&egg)).unwrap(),
        log_before,
        "log must be untouched"
    );
    assert_eq!(
        std::fs::read(&egg).unwrap(),
        data,
        "main file must be untouched"
    );
}

#[cfg(unix)]
#[test]
fn compaction_preserves_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, egg) = fixture();
    commit_sample_changes(&egg);
    std::fs::set_permissions(&egg, std::fs::Permissions::from_mode(0o640)).unwrap();
    compact(&egg, &CompactOptions::default()).unwrap();
    let mode = std::fs::metadata(&egg).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
}

fn block_bytes(snap: &EggSnapshot, t: usize, c: usize) -> Vec<u8> {
    let col = &snap.tables()[t].columns[c];
    let start = (snap.header().yolk_offset + col.yolk_offset) as usize;
    snap.data()[start..start + col.yolk_length as usize].to_vec()
}
