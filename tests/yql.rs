use eggdb_io::writer::{lay, LayOptions};
use eggdb_yql;
use rusqlite::Connection;
use std::io::Write;

/// Create a test SQLite database with users and orders tables.
fn create_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute_batch(
        "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT,
            country TEXT,
            status TEXT DEFAULT 'active'
        );

        INSERT INTO users (id, name, email, country, status) VALUES
            (1, 'Alice', 'alice@example.com', 'US', 'active'),
            (2, 'Bob', 'bob@example.com', 'UK', 'active'),
            (3, 'Charlie', 'charlie@example.com', 'US', 'inactive'),
            (4, 'Diana', 'diana@example.com', 'DE', 'active'),
            (5, 'Eve', 'eve@example.com', 'UK', 'inactive');

        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER,
            total REAL,
            status TEXT,
            created_at TEXT
        );

        INSERT INTO orders (id, user_id, total, status, created_at) VALUES
            (1, 1, 99.99, 'done', '2024-01-15'),
            (2, 1, 45.50, 'done', '2024-02-20'),
            (3, 2, 120.00, 'pending', '2024-03-10'),
            (4, 3, 30.00, 'done', '2024-01-25'),
            (5, 4, 200.00, 'cancelled', '2024-04-05'),
            (6, 2, 75.25, 'done', '2024-05-18'),
            (7, 5, 55.00, 'pending', '2024-06-01'),
            (8, 1, 180.00, 'done', '2024-03-12');
        ",
    )
    .unwrap();

    conn
}

/// Lay a test database to a temp .egg file, return the path.
fn create_test_egg() -> tempfile::NamedTempFile {
    let conn = create_test_db();
    let egg_data = lay(&conn, &LayOptions::default()).unwrap();
    let mut tmp = tempfile::NamedTempFile::with_suffix(".egg").unwrap();
    tmp.write_all(&egg_data).unwrap();
    tmp.flush().unwrap();
    tmp
}

/// Helper: set up SQLite connection with egg virtual tables.
fn setup_vtable_conn(egg_path: &std::path::Path) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    eggdb_vtable::register_egg_module(&conn).unwrap();

    let abs_path = egg_path.canonicalize().unwrap();
    for table_name in &["users", "orders"] {
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE \"{table_name}\" USING egg('{}', '{table_name}')",
            abs_path.display()
        ))
        .unwrap();
    }
    conn
}

/// Helper: run a SQL query and return results as Vec<Vec<String>>.
fn query_results(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let col_count = stmt.column_count();
    stmt.query_map([], |row| {
        let mut values = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let val = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => "NULL".to_string(),
                Ok(rusqlite::types::ValueRef::Integer(n)) => n.to_string(),
                Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                Ok(rusqlite::types::ValueRef::Text(s)) => String::from_utf8_lossy(s).to_string(),
                Ok(rusqlite::types::ValueRef::Blob(b)) => format!("<blob {} bytes>", b.len()),
                Err(_) => "?".to_string(),
            };
            values.push(val);
        }
        Ok(values)
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

/// Run a YQL query through transpile + vtable and compare against direct SQL.
/// Results are sorted before comparison since row order may differ without ORDER BY.
fn assert_yql_matches_sql(
    vtable_conn: &Connection,
    direct_conn: &Connection,
    yql_input: &str,
    expected_sql: &str,
) {
    let transpiled = eggdb_yql::transpile(yql_input).unwrap();
    assert_eq!(
        transpiled, expected_sql,
        "YQL transpilation mismatch for: {yql_input}"
    );

    let mut vtable_results = query_results(vtable_conn, &transpiled);
    let mut direct_results = query_results(direct_conn, expected_sql);
    vtable_results.sort();
    direct_results.sort();
    assert_eq!(
        vtable_results, direct_results,
        "Result mismatch for YQL: {yql_input}\nSQL: {transpiled}"
    );
}

// --- v0_plan.md example tests ---

#[test]
fn plan_example_show_where_sort_limit() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "show users where id = 1 sort name limit 10",
        "SELECT * FROM users WHERE id = 1 ORDER BY name ASC LIMIT 10",
    );
}

#[test]
fn plan_example_count_by() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "count orders by status",
        "SELECT status, count(*) AS count FROM orders GROUP BY status",
    );
}

#[test]
fn plan_example_find_between() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "find orders where total between 50 and 150",
        "SELECT * FROM orders WHERE total BETWEEN 50 AND 150",
    );
}

#[test]
fn plan_example_list_in() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        r#"list users.name, users.email where country in ("US", "UK")"#,
        "SELECT name, email FROM users WHERE country IN ('US', 'UK')",
    );
}

#[test]
fn plan_example_sum() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "sum orders.total",
        "SELECT sum(total) AS sum_total FROM orders",
    );
}

// --- Additional integration tests ---

#[test]
fn sql_passthrough() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    let sql = "SELECT count(*) FROM users WHERE status = 'active'";
    let vtable_results = query_results(&vtable_conn, sql);
    let direct_results = query_results(&direct_conn, sql);
    assert_eq!(vtable_results, direct_results);
}

#[test]
fn is_yql_detection() {
    assert!(eggdb_yql::is_yql("show users"));
    assert!(eggdb_yql::is_yql("count orders by status"));
    assert!(!eggdb_yql::is_yql("SELECT * FROM users"));
    assert!(!eggdb_yql::is_yql(""));
}

#[test]
fn yql_error_on_invalid_input() {
    assert!(eggdb_yql::transpile("").is_err());
    assert!(eggdb_yql::transpile("   ").is_err());
    assert!(eggdb_yql::transpile("show").is_err()); // missing table
}

#[test]
fn count_without_group() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "count users",
        "SELECT count(*) AS count FROM users",
    );
}

#[test]
fn avg_with_group() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "avg orders.total by status",
        "SELECT status, avg(total) AS avg_total FROM orders GROUP BY status",
    );
}

#[test]
fn show_with_limit() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "show orders limit 3",
        "SELECT * FROM orders LIMIT 3",
    );
}

#[test]
fn between_and_regular_and() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "find orders where total between 50 and 150 and status = 'done'",
        "SELECT * FROM orders WHERE total BETWEEN 50 AND 150 AND status = 'done'",
    );
}

#[test]
fn min_max_commands() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "min orders.total",
        "SELECT min(total) AS min_total FROM orders",
    );

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "max orders.total",
        "SELECT max(total) AS max_total FROM orders",
    );
}

#[test]
fn sort_desc() {
    let egg = create_test_egg();
    let vtable_conn = setup_vtable_conn(egg.path());
    let direct_conn = create_test_db();

    assert_yql_matches_sql(
        &vtable_conn,
        &direct_conn,
        "show users sort name desc limit 3",
        "SELECT * FROM users ORDER BY name DESC LIMIT 3",
    );
}
