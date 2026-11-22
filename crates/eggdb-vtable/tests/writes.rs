//! Write support through the virtual table, tested differentially: every
//! statement runs against a plain SQLite copy of the source database and
//! against the egg tables. Success/failure, error messages, and the full table
//! contents (including value types) must match after every statement.

use std::path::{Path, PathBuf};

use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;

use eggdb_io::writer::{lay_file, LayOptions};
use eggdb_store::{compact, wal_path, CompactOptions, CompactOutcome, EggSnapshot};

const SCHEMA: &str = "
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    name TEXT,
    age INTEGER CHECK (age >= 0),
    status TEXT NOT NULL DEFAULT 'active',
    score REAL
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL,
    total REAL NOT NULL,
    note TEXT
);
CREATE UNIQUE INDEX orders_user_note ON orders (user_id, note);
";

const TABLES: [&str; 2] = ["users", "orders"];

/// Build the source database, lay it, and return (dir, baseline copy, egg).
fn fixture() -> (tempfile::TempDir, Connection, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.db");
    let conn = Connection::open(&src).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    for i in 1..=20 {
        conn.execute(
            "INSERT INTO users (email, name, age, status, score) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                format!("user{i}@example.com"),
                format!("user{i}"),
                18 + i,
                if i % 3 == 0 { "inactive" } else { "active" },
                i as f64 / 4.0
            ],
        )
        .unwrap();
    }
    for i in 1..=50 {
        let note = (i % 4 != 0).then(|| format!("note{}", i % 7));
        conn.execute(
            "INSERT INTO orders (user_id, total, note) VALUES (?1, ?2, ?3)",
            rusqlite::params![1 + i % 20, i as f64 * 1.25, note],
        )
        .unwrap();
    }
    drop(conn);

    let egg = dir.path().join("data.egg");
    lay_file(&src, &egg, &LayOptions::default()).unwrap();
    let baseline = Connection::open(&src).unwrap();
    (dir, baseline, egg)
}

/// An in-memory connection with writable egg tables named like the originals.
fn egg_conn(egg: &Path) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    eggdb_vtable::register_egg_module(&conn).unwrap();
    for t in TABLES {
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE {t} USING egg('{}', '{t}', 'mode=rw')",
            egg.display()
        ))
        .unwrap();
    }
    conn
}

fn rows(conn: &Connection, sql: &str) -> Vec<Vec<SqlValue>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| (0..n).map(|i| r.get::<_, SqlValue>(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn assert_same_contents(base: &Connection, egg: &Connection, context: &str) {
    for t in TABLES {
        let sql = format!("SELECT * FROM {t} ORDER BY id");
        assert_eq!(
            rows(base, &sql),
            rows(egg, &sql),
            "table {t} differs after: {context}"
        );
    }
}

/// Run `sql` on both; outcomes and resulting contents must match.
fn both(base: &Connection, egg: &Connection, sql: &str) {
    let a = base.execute_batch(sql);
    let b = egg.execute_batch(sql);
    match (&a, &b) {
        (Ok(()), Ok(())) => {}
        (Err(ea), Err(eb)) => assert_eq!(
            ea.sqlite_error_code(),
            eb.sqlite_error_code(),
            "{sql}\n  sqlite: {ea}\n  egg:    {eb}"
        ),
        _ => panic!("outcome differs for {sql}\n  sqlite: {a:?}\n  egg:    {b:?}"),
    }
    assert_same_contents(base, egg, sql);
}

#[test]
fn inserts_match_sqlite_including_defaults_affinity_and_ids() {
    let (_dir, base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);
    assert_same_contents(&base, &egg, "fixture");

    // Omitted INTEGER PRIMARY KEY gets max+1; omitted status gets its DEFAULT.
    both(
        &base,
        &egg,
        "INSERT INTO users (email, name, age) VALUES ('new@x.com', 'New', 30)",
    );
    // Text '42' in an INTEGER column becomes the integer 42 (affinity).
    both(
        &base,
        &egg,
        "INSERT INTO users (email, age, score) VALUES ('aff@x.com', '42', '2.5')",
    );
    // Explicit id, then the next automatic id continues after it.
    both(
        &base,
        &egg,
        "INSERT INTO users (id, email) VALUES (1000, 'big@x.com')",
    );
    both(
        &base,
        &egg,
        "INSERT INTO users (email) VALUES ('after@x.com')",
    );
    // Multi-row insert from a query.
    both(&base, &egg, "INSERT INTO orders (user_id, total, note) SELECT id, 9.99, 'bulk' FROM users WHERE id > 15");
}

#[test]
fn constraint_violations_fail_exactly_like_sqlite() {
    let (_dir, base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);

    // UNIQUE with COLLATE NOCASE: different case still conflicts.
    both(
        &base,
        &egg,
        "INSERT INTO users (email) VALUES ('USER1@EXAMPLE.COM')",
    );
    // NOT NULL. (An explicit NULL into a column with a DEFAULT is a documented
    // difference on INSERT; see `defaults_apply_on_insert_only`.)
    both(&base, &egg, "INSERT INTO users (name) VALUES ('no email')");
    both(&base, &egg, "UPDATE users SET status = NULL WHERE id = 1");
    // CHECK.
    both(
        &base,
        &egg,
        "INSERT INTO users (email, age) VALUES ('neg@x.com', -1)",
    );
    // INTEGER PRIMARY KEY duplicate and type mismatch.
    both(
        &base,
        &egg,
        "INSERT INTO users (id, email) VALUES (3, 'dup@x.com')",
    );
    both(
        &base,
        &egg,
        "INSERT INTO users (id, email) VALUES ('abc', 'type@x.com')",
    );
    // Composite UNIQUE: NULL notes never conflict, equal non-NULL ones do.
    both(
        &base,
        &egg,
        "INSERT INTO orders (user_id, total, note) VALUES (5, 1, NULL)",
    );
    both(
        &base,
        &egg,
        "INSERT INTO orders (user_id, total, note) VALUES (5, 1, NULL)",
    );
    both(
        &base,
        &egg,
        "INSERT INTO orders (user_id, total, note) VALUES (7, 1, 'x')",
    );
    both(
        &base,
        &egg,
        "INSERT INTO orders (user_id, total, note) VALUES (7, 1, 'x')",
    );
    // An UPDATE that collides with another row's key; one that keeps its own.
    both(
        &base,
        &egg,
        "UPDATE users SET email = 'user2@example.com' WHERE id = 3",
    );
    both(
        &base,
        &egg,
        "UPDATE users SET email = 'USER3@example.com' WHERE id = 3",
    );
    // A failing statement in autocommit mode leaves no partial changes.
    both(&base, &egg, "UPDATE users SET age = age - 25");
    // Freed keys can be reused after delete.
    both(&base, &egg, "DELETE FROM users WHERE id = 4");
    both(
        &base,
        &egg,
        "INSERT INTO users (email) VALUES ('user4@example.com')",
    );
}

#[test]
fn updates_and_deletes_match_sqlite() {
    let (_dir, base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);

    both(
        &base,
        &egg,
        "UPDATE users SET name = 'Senior' WHERE age > 30",
    );
    both(
        &base,
        &egg,
        "UPDATE users SET score = score * 2, status = 'vip' WHERE id IN (2, 4, 6)",
    );
    both(&base, &egg, "UPDATE users SET id = id + 500 WHERE id = 5");
    both(
        &base,
        &egg,
        "DELETE FROM users WHERE status = 'inactive' AND age < 30",
    );
    both(&base, &egg, "DELETE FROM orders WHERE note IS NULL");
    both(
        &base,
        &egg,
        "UPDATE orders SET note = 'edited' WHERE id = 1",
    );
    both(
        &base,
        &egg,
        "INSERT INTO users (email) VALUES ('late@x.com')",
    );
    both(
        &base,
        &egg,
        "UPDATE users SET name = 'Late' WHERE email = 'late@x.com'",
    );
    both(&base, &egg, "DELETE FROM users WHERE email = 'late@x.com'");
    both(&base, &egg, "DELETE FROM orders");
}

#[test]
fn filters_see_updated_and_deleted_rows_correctly() {
    let (_dir, base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);
    both(&base, &egg, "UPDATE users SET name = 'needle' WHERE id = 2");
    both(&base, &egg, "DELETE FROM users WHERE id = 7");
    both(
        &base,
        &egg,
        "INSERT INTO users (email, name) VALUES ('n2@x.com', 'needle')",
    );

    // Pushed-down filters run on encoded main-file data; changed rows must
    // still be found (updated, inserted) or skipped (deleted).
    for sql in [
        "SELECT id FROM users WHERE name = 'needle' ORDER BY id",
        "SELECT id FROM users WHERE name = 'user2'",
        "SELECT id FROM users WHERE name = 'user7'",
        "SELECT count(*) FROM users WHERE age > 20",
        "SELECT id FROM users WHERE name IS NULL ORDER BY id",
        "SELECT count(*) FROM users",
    ] {
        assert_eq!(rows(&base, sql), rows(&egg, sql), "{sql}");
    }
}

#[test]
fn transactions_commit_and_roll_back() {
    let (_dir, base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);

    both(&base, &egg, "BEGIN; INSERT INTO users (email) VALUES ('t1@x.com'); DELETE FROM users WHERE id = 1; ROLLBACK;");
    both(&base, &egg, "BEGIN; INSERT INTO users (email) VALUES ('t2@x.com'); UPDATE users SET name = 'tx' WHERE id = 2; COMMIT;");

    // Reads inside a transaction see its own uncommitted writes.
    egg.execute_batch("BEGIN; INSERT INTO users (email, name) VALUES ('mine@x.com', 'mine')")
        .unwrap();
    assert_eq!(
        rows(&egg, "SELECT count(*) FROM users WHERE name = 'mine'"),
        vec![vec![SqlValue::Integer(1)]]
    );
    egg.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        rows(&egg, "SELECT count(*) FROM users WHERE name = 'mine'"),
        vec![vec![SqlValue::Integer(0)]]
    );
    assert_same_contents(&base, &egg, "after read-your-writes rollback");
}

#[test]
fn one_transaction_across_tables_is_one_log_frame() {
    let (_dir, _base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);
    egg.execute_batch(
        "BEGIN;
         INSERT INTO users (email) VALUES ('multi@x.com');
         INSERT INTO orders (user_id, total) VALUES (21, 5.0);
         COMMIT;",
    )
    .unwrap();
    let log = eggdb_core::parse_wal(&std::fs::read(wal_path(&egg_path)).unwrap()).unwrap();
    let commits: Vec<_> = log.commits().collect();
    assert_eq!(
        commits.len(),
        1,
        "both tables' changes must be in one frame"
    );
    assert_eq!(commits[0].ops.len(), 2);
}

#[test]
fn rolled_back_and_failed_statements_leave_no_trace_in_the_log() {
    let (_dir, _base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);
    egg.execute_batch("BEGIN; INSERT INTO users (email) VALUES ('gone@x.com'); ROLLBACK;")
        .unwrap();
    assert!(egg
        .execute_batch("INSERT INTO users (email) VALUES ('USER1@example.com')")
        .is_err());
    let snap = EggSnapshot::open(&egg_path, None).unwrap();
    assert!(!snap.has_pending_changes());
}

#[test]
fn changes_persist_across_connections_and_compaction() {
    let (_dir, base, egg_path) = fixture();
    {
        let egg = egg_conn(&egg_path);
        both(
            &base,
            &egg,
            "INSERT INTO users (email, name) VALUES ('keep@x.com', 'Keep')",
        );
        both(&base, &egg, "UPDATE users SET age = 99 WHERE id = 1");
        both(&base, &egg, "DELETE FROM orders WHERE id <= 10");
    }
    // A fresh connection replays the write log.
    assert_same_contents(&base, &egg_conn(&egg_path), "reopen");

    // Compaction folds the log into a new main file with the same contents.
    let outcome = compact(&egg_path, &CompactOptions::default()).unwrap();
    assert!(matches!(outcome, CompactOutcome::Compacted(_)));
    assert!(!wal_path(&egg_path).exists());
    let egg = egg_conn(&egg_path);
    assert_same_contents(&base, &egg, "compaction");

    // Writing continues normally after compaction (new rowids, keys rebuilt).
    both(
        &base,
        &egg,
        "INSERT INTO users (email) VALUES ('post@x.com')",
    );
    both(
        &base,
        &egg,
        "INSERT INTO users (email) VALUES ('KEEP@x.com')",
    );
}

#[test]
fn read_only_tables_reject_writes_without_touching_disk() {
    let (_dir, _base, egg_path) = fixture();
    let conn = Connection::open_in_memory().unwrap();
    eggdb_vtable::register_egg_module(&conn).unwrap();
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE users USING egg('{}', 'users')",
        egg_path.display()
    ))
    .unwrap();
    let err = conn
        .execute_batch("INSERT INTO users (email) VALUES ('x@x.com')")
        .unwrap_err();
    assert!(err.to_string().contains("mode=rw"), "{err}");
    assert!(
        !wal_path(&egg_path).exists(),
        "a read-only table must not create a log"
    );
}

#[test]
fn defaults_apply_on_insert_only() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("ev.db");
    Connection::open(&src)
        .unwrap()
        .execute_batch(
            "CREATE TABLE ev (
                 id INTEGER PRIMARY KEY,
                 kind TEXT NOT NULL DEFAULT 'misc',
                 created TEXT DEFAULT CURRENT_TIMESTAMP,
                 n INTEGER DEFAULT (1 + 2)
             );
             INSERT INTO ev (kind) VALUES ('seed');",
        )
        .unwrap();
    let egg_path = dir.path().join("ev.egg");
    lay_file(&src, &egg_path, &LayOptions::default()).unwrap();
    let conn = Connection::open_in_memory().unwrap();
    eggdb_vtable::register_egg_module(&conn).unwrap();
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE ev USING egg('{}', 'ev', 'mode=rw')",
        egg_path.display()
    ))
    .unwrap();
    let row = |id: i64| -> (String, Option<String>, Option<i64>) {
        conn.query_row("SELECT kind, created, n FROM ev WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
    };

    // Omitted columns get their defaults, evaluated at insert time.
    conn.execute_batch("INSERT INTO ev (id) VALUES (10)")
        .unwrap();
    let (kind, created, n) = row(10);
    assert_eq!((kind.as_str(), n), ("misc", Some(3)));
    assert!(
        created.is_some_and(|c| c.len() == 19 && c.starts_with("20")),
        "CURRENT_TIMESTAMP"
    );

    // Documented difference: SQLite would store NULL in `n` (and reject the
    // NULL `kind`); a virtual table cannot tell explicit NULL from omitted.
    conn.execute_batch("INSERT INTO ev (id, kind, n) VALUES (11, NULL, NULL)")
        .unwrap();
    assert_eq!(row(11).0, "misc");
    assert_eq!(row(11).2, Some(3));

    // UPDATE never applies defaults: NULL stays NULL, NOT NULL still fails.
    conn.execute_batch("UPDATE ev SET n = NULL WHERE id = 10")
        .unwrap();
    assert_eq!(row(10).2, None);
    assert!(conn
        .execute_batch("UPDATE ev SET kind = NULL WHERE id = 10")
        .is_err());
}

#[test]
fn explicit_rowids_are_rejected() {
    let (_dir, _base, egg_path) = fixture();
    let egg = egg_conn(&egg_path);
    let err = egg
        .execute_batch("INSERT INTO users (rowid, email) VALUES (5000, 'r@x.com')")
        .unwrap_err();
    assert!(err.to_string().contains("explicit rowid"), "{err}");
}

#[test]
fn second_connection_waits_for_the_writer_then_catches_up() {
    let (_dir, _base, egg_path) = fixture();
    let a = egg_conn(&egg_path);
    let b = egg_conn(&egg_path);

    a.execute_batch("BEGIN; INSERT INTO users (email) VALUES ('a@x.com')")
        .unwrap();
    let err = b
        .execute_batch("INSERT INTO users (email) VALUES ('b@x.com')")
        .unwrap_err();
    assert!(err.to_string().contains("another process"), "{err}");
    a.execute_batch("COMMIT").unwrap();

    // B catches up on A's commit before writing: ids and uniqueness see it.
    assert!(b
        .execute_batch("INSERT INTO users (email) VALUES ('A@x.com')")
        .is_err());
    b.execute_batch("INSERT INTO users (email) VALUES ('b@x.com')")
        .unwrap();
    let ids = rows(
        &b,
        "SELECT id FROM users WHERE email IN ('a@x.com', 'b@x.com') ORDER BY id",
    );
    assert_eq!(
        ids,
        vec![vec![SqlValue::Integer(21)], vec![SqlValue::Integer(22)]]
    );
}
