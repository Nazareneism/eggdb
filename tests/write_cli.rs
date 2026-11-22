//! End-to-end write support through the real `egg` binary:
//! lay -> edit with `egg query` -> inspect -> hatch -> compact -> hatch,
//! compared row-by-row against a plain SQLite copy given the same edits.

use std::path::Path;
use std::process::{Command, Output};

use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;

fn egg(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egg"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run egg binary")
}

fn ok(args: &[&str]) -> String {
    let out = egg(args);
    assert!(
        out.status.success(),
        "egg {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn users(db: &Path) -> Vec<Vec<SqlValue>> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn.prepare("SELECT * FROM users ORDER BY id").unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| (0..n).map(|i| r.get::<_, SqlValue>(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

const EDITS: [&str; 4] = [
    "INSERT INTO users (email, name) VALUES ('new@x.com', 'New')",
    "UPDATE users SET name = 'Changed' WHERE id = 2",
    "DELETE FROM users WHERE id = 3",
    "INSERT INTO users (email) VALUES ('second@x.com')",
];

#[test]
fn edit_compact_and_hatch_through_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let p = |name: &str| dir.path().join(name);
    let (src, baseline, egg_file) = (p("src.db"), p("baseline.db"), p("data.egg"));
    let s = |path: &Path| path.to_str().unwrap().to_string();

    let conn = Connection::open(&src).unwrap();
    conn.execute_batch(
        "CREATE TABLE users (
             id INTEGER PRIMARY KEY,
             email TEXT NOT NULL UNIQUE,
             name TEXT,
             status TEXT NOT NULL DEFAULT 'active'
         );",
    )
    .unwrap();
    for i in 1..=10 {
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?1, ?2)",
            [format!("u{i}@x.com"), format!("User {i}")],
        )
        .unwrap();
    }
    drop(conn);
    std::fs::copy(&src, &baseline).unwrap();

    ok(&["lay", &s(&src), "-o", &s(&egg_file)]);

    // Edit through the CLI, and apply the same edits to the baseline.
    let base = Connection::open(&baseline).unwrap();
    for sql in EDITS {
        let out = ok(&["query", &s(&egg_file), sql]);
        assert!(out.contains("1 row changed"), "{sql}: {out}");
        base.execute_batch(sql).unwrap();
    }
    drop(base);

    // A rule violation fails loudly and changes nothing.
    let dup = egg(&[
        "query",
        &s(&egg_file),
        "INSERT INTO users (email) VALUES ('u1@x.com')",
    ]);
    assert!(!dup.status.success());
    assert!(String::from_utf8_lossy(&dup.stderr).contains("UNIQUE constraint failed: users.email"));

    // Reads see the pending changes.
    let count = ok(&["query", &s(&egg_file), "SELECT count(*) AS n FROM users"]);
    assert!(count.contains("11"), "{count}");
    let candle = ok(&["candle", &s(&egg_file)]);
    assert!(candle.contains("11 rows"), "{candle}");
    assert!(
        candle.contains("Pending changes: 2 inserted, 1 updated, 1 deleted"),
        "{candle}"
    );
    let validate = ok(&["validate", &s(&egg_file)]);
    assert!(validate.contains("write_log"), "{validate}");
    let crack = ok(&[
        "crack",
        &s(&egg_file),
        "--format",
        "csv",
        "--table",
        "users",
    ]);
    assert!(
        crack.contains("Changed") && !crack.contains("u3@x.com"),
        "{crack}"
    );

    // Overwriting an egg with pending changes is refused.
    let relay_over = egg(&["lay", &s(&src), "-o", &s(&egg_file)]);
    assert!(!relay_over.status.success());
    assert!(String::from_utf8_lossy(&relay_over.stderr).contains("pending changes"));

    // Hatch before compaction includes the changes.
    ok(&["hatch", &s(&egg_file), "-o", &s(&p("pre.sqlite"))]);
    assert_eq!(users(&p("pre.sqlite")), users(&baseline));

    // Compaction folds them in; hatch after compaction is identical.
    let compact = ok(&["compact", &s(&egg_file)]);
    assert!(compact.contains("write log was folded in"), "{compact}");
    assert!(!p("data.egg.wal").exists());
    ok(&["hatch", &s(&egg_file), "-o", &s(&p("post.sqlite"))]);
    assert_eq!(users(&p("post.sqlite")), users(&baseline));
    let again = ok(&["compact", &s(&egg_file)]);
    assert!(again.contains("nothing to compact"), "{again}");
}
