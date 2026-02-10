use eggdb_core::SqliteType;
use rusqlite::Connection;

use crate::IoError;

/// Column definition from SQLite schema introspection.
#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub name: String,
    pub declared_type: Option<String>,
    pub notnull: bool,
}

/// Quote a SQLite identifier (table or column name) to prevent SQL injection.
///
/// Wraps the name in double quotes and escapes any embedded double quotes
/// by doubling them, following SQL standard identifier quoting rules.
#[must_use]
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Get all user table names in the database.
///
/// Excludes internal SQLite tables (`sqlite_*`) and returns only
/// `type='table'` entries (not views or virtual tables).
pub fn get_table_names(conn: &Connection) -> Result<Vec<String>, IoError> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// Get the CREATE TABLE DDL for a table.
pub fn get_table_ddl(conn: &Connection, table_name: &str) -> Result<String, IoError> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
        [table_name],
        |row| row.get(0),
    )?;
    Ok(sql)
}

/// Get index definitions (CREATE INDEX statements) for a table.
///
/// Returns only explicitly created indexes (sql IS NOT NULL), excluding
/// autoindexes and primary key indexes that SQLite creates implicitly.
pub fn get_index_definitions(conn: &Connection, table_name: &str) -> Result<Vec<String>, IoError> {
    let mut stmt = conn.prepare(
        "SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL",
    )?;
    let indexes = stmt
        .query_map([table_name], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(indexes)
}

/// Get the row count for a table.
pub fn get_row_count(conn: &Connection, table_name: &str) -> Result<u64, IoError> {
    let quoted = quote_identifier(table_name);
    let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {quoted}"), [], |row| {
        row.get(0)
    })?;
    Ok(count as u64)
}

/// Get column definitions for a table via PRAGMA table_info.
///
/// Returns columns in declaration order.
pub fn get_columns(conn: &Connection, table_name: &str) -> Result<Vec<ColumnDef>, IoError> {
    let quoted = quote_identifier(table_name);
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({quoted})"))?;
    let columns = stmt
        .query_map([], |row| {
            Ok(ColumnDef {
                name: row.get::<_, String>(1)?,
                declared_type: row.get::<_, Option<String>>(2)?,
                notnull: row.get::<_, bool>(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

/// Map a declared SQLite type to our SqliteType enum using SQLite's
/// type affinity rules.
///
/// SQLite type affinity (from the spec):
/// 1. Contains "INT" → INTEGER
/// 2. Contains "CHAR", "CLOB", or "TEXT" → TEXT
/// 3. Contains "BLOB" or no type declared → BLOB
/// 4. Contains "REAL", "FLOA", or "DOUB" → REAL
/// 5. Otherwise → TEXT (NUMERIC affinity, we map to Text for safety)
#[must_use]
pub fn infer_sqlite_type(declared_type: Option<&str>) -> SqliteType {
    let Some(dt) = declared_type else {
        return SqliteType::Blob; // No type → BLOB affinity
    };

    let upper = dt.to_ascii_uppercase();

    if upper.contains("INT") {
        SqliteType::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        SqliteType::Text
    } else if upper.contains("BLOB") || upper.is_empty() {
        SqliteType::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        SqliteType::Real
    } else {
        SqliteType::Text // NUMERIC affinity → Text for safety
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_simple_name() {
        assert_eq!(quote_identifier("users"), "\"users\"");
    }

    #[test]
    fn quote_name_with_double_quote() {
        assert_eq!(quote_identifier("my\"table"), "\"my\"\"table\"");
    }

    #[test]
    fn quote_empty_name() {
        assert_eq!(quote_identifier(""), "\"\"");
    }

    #[test]
    fn infer_integer_types() {
        assert_eq!(infer_sqlite_type(Some("INTEGER")), SqliteType::Integer);
        assert_eq!(infer_sqlite_type(Some("INT")), SqliteType::Integer);
        assert_eq!(infer_sqlite_type(Some("BIGINT")), SqliteType::Integer);
        assert_eq!(infer_sqlite_type(Some("TINYINT")), SqliteType::Integer);
        assert_eq!(infer_sqlite_type(Some("MEDIUMINT")), SqliteType::Integer);
        assert_eq!(infer_sqlite_type(Some("int")), SqliteType::Integer);
    }

    #[test]
    fn infer_text_types() {
        assert_eq!(infer_sqlite_type(Some("TEXT")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("VARCHAR(255)")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("CLOB")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("CHARACTER(20)")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("NCHAR(10)")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("NVARCHAR(100)")), SqliteType::Text);
    }

    #[test]
    fn infer_blob_types() {
        assert_eq!(infer_sqlite_type(Some("BLOB")), SqliteType::Blob);
        assert_eq!(infer_sqlite_type(None), SqliteType::Blob);
        assert_eq!(infer_sqlite_type(Some("")), SqliteType::Blob);
    }

    #[test]
    fn infer_real_types() {
        assert_eq!(infer_sqlite_type(Some("REAL")), SqliteType::Real);
        assert_eq!(infer_sqlite_type(Some("DOUBLE")), SqliteType::Real);
        assert_eq!(
            infer_sqlite_type(Some("DOUBLE PRECISION")),
            SqliteType::Real
        );
        assert_eq!(infer_sqlite_type(Some("FLOAT")), SqliteType::Real);
    }

    #[test]
    fn infer_numeric_fallback() {
        assert_eq!(infer_sqlite_type(Some("NUMERIC")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("DECIMAL(10,2)")), SqliteType::Text);
        assert_eq!(infer_sqlite_type(Some("BOOLEAN")), SqliteType::Text);
    }

    #[test]
    fn get_table_names_basic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL);",
        )
        .unwrap();

        let names = get_table_names(&conn).unwrap();
        assert_eq!(names, vec!["orders", "users"]);
    }

    #[test]
    fn get_table_names_excludes_sqlite_internals() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
        // sqlite_master and sqlite_sequence should not appear
        let names = get_table_names(&conn).unwrap();
        assert_eq!(names, vec!["t"]);
    }

    #[test]
    fn get_table_ddl_basic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .unwrap();

        let ddl = get_table_ddl(&conn, "users").unwrap();
        assert!(ddl.contains("CREATE TABLE"));
        assert!(ddl.contains("users"));
    }

    #[test]
    fn get_index_definitions_basic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);
             CREATE INDEX idx_email ON users(email);",
        )
        .unwrap();

        let indexes = get_index_definitions(&conn, "users").unwrap();
        assert_eq!(indexes.len(), 1);
        assert!(indexes[0].contains("idx_email"));
    }

    #[test]
    fn get_index_definitions_excludes_autoindexes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE);")
            .unwrap();

        // The UNIQUE constraint creates an autoindex with sql=NULL
        let indexes = get_index_definitions(&conn, "users").unwrap();
        // Autoindexes have sql IS NULL, so they should be excluded
        assert!(indexes.is_empty());
    }

    #[test]
    fn get_row_count_basic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER);
             INSERT INTO t VALUES (1);
             INSERT INTO t VALUES (2);
             INSERT INTO t VALUES (3);",
        )
        .unwrap();

        assert_eq!(get_row_count(&conn, "t").unwrap(), 3);
    }

    #[test]
    fn get_row_count_empty() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
        assert_eq!(get_row_count(&conn, "t").unwrap(), 0);
    }

    #[test]
    fn get_columns_basic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT, age INTEGER);",
        )
        .unwrap();

        let cols = get_columns(&conn, "users").unwrap();
        assert_eq!(cols.len(), 4);

        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].declared_type.as_deref(), Some("INTEGER"));

        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[1].declared_type.as_deref(), Some("TEXT"));
        assert!(cols[1].notnull);

        assert_eq!(cols[2].name, "email");
        assert_eq!(cols[2].declared_type.as_deref(), Some("TEXT"));
        assert!(!cols[2].notnull);

        assert_eq!(cols[3].name, "age");
        assert_eq!(cols[3].declared_type.as_deref(), Some("INTEGER"));
    }

    #[test]
    fn get_columns_preserves_order() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (z TEXT, a INTEGER, m REAL);")
            .unwrap();

        let cols = get_columns(&conn, "t").unwrap();
        assert_eq!(cols[0].name, "z");
        assert_eq!(cols[1].name, "a");
        assert_eq!(cols[2].name, "m");
    }

    #[test]
    fn table_with_special_name() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE \"my table\" (id INTEGER);")
            .unwrap();

        let names = get_table_names(&conn).unwrap();
        assert_eq!(names, vec!["my table"]);

        let count = get_row_count(&conn, "my table").unwrap();
        assert_eq!(count, 0);

        let ddl = get_table_ddl(&conn, "my table").unwrap();
        assert!(ddl.contains("my table"));
    }
}
