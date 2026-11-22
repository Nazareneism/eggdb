//! Constraint enforcement for writes, delegated to SQLite itself.
//!
//! A scratch in-memory database holds:
//! - `main.<table>`: the table's original DDL, always empty. Each candidate
//!   row is inserted with `RETURNING` and rolled back, which applies SQLite's
//!   own type affinity, NOT NULL, and CHECK rules and returns the row exactly
//!   as `egg hatch` would store it.
//! - `keys.<table>`: only the columns of PRIMARY KEY / UNIQUE constraints for
//!   every live row, keyed by egg rowid, with matching unique indexes and
//!   collations. SQLite enforces uniqueness with its exact semantics, and its
//!   error messages name the real table and columns.

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{params_from_iter, Connection, Error};

use eggdb_core::{TableMeta, Value};

/// Introspected facts about a table's original definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSchema {
    /// `DEFAULT` expression text per egg column, as SQLite reports it.
    pub defaults: Vec<Option<String>>,
    /// Egg column that is an INTEGER PRIMARY KEY (rowid alias), if any.
    pub ipk: Option<usize>,
    /// Unique keys: egg column positions with their collations.
    pub unique_keys: Vec<Vec<(usize, String)>>,
}

/// Run the table's original DDL (and index DDL) in a fresh in-memory database
/// and read back its columns, defaults, and unique keys.
pub fn introspect(meta: &TableMeta) -> Result<(Connection, TableSchema), Error> {
    let conn = Connection::open_in_memory()?;
    let ddl = meta.original_ddl.trim();
    if !ddl
        .get(..12)
        .is_some_and(|p| p.eq_ignore_ascii_case("CREATE TABLE"))
    {
        return Err(write_err(
            meta,
            "its stored DDL is not a CREATE TABLE statement",
        ));
    }
    // `execute` rejects multiple statements, so the DDL cannot smuggle others in.
    conn.execute(ddl, [])
        .map_err(|e| write_err(meta, &format!("its DDL failed to run: {e}")))?;
    for index in &meta.index_definitions {
        conn.execute(index, [])
            .map_err(|e| write_err(meta, &format!("index DDL failed to run: {e}")))?;
    }
    let columns = check_columns(&conn, meta)?;
    let cids: Vec<i64> = columns.iter().map(|(cid, _)| *cid).collect();
    let schema = TableSchema {
        defaults: columns.into_iter().map(|(_, dflt)| dflt).collect(),
        ipk: find_ipk(&conn, meta, &cids)?,
        unique_keys: unique_keys(&conn, meta, &cids)?,
    };
    Ok((conn, schema))
}

/// Verify the DDL's normal columns are exactly the egg's columns, in order.
/// Returns each column's `cid` and default expression.
fn check_columns(conn: &Connection, meta: &TableMeta) -> Result<Vec<(i64, Option<String>)>, Error> {
    let rows: Vec<(i64, String, Option<String>)> = conn
        .prepare("SELECT cid, name, dflt_value FROM pragma_table_xinfo(?1) WHERE hidden = 0")?
        .query_map([&meta.table_name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let ddl_names: Vec<&str> = rows.iter().map(|(_, n, _)| n.as_str()).collect();
    let egg_names: Vec<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();
    if ddl_names != egg_names {
        return Err(write_err(
            meta,
            &format!("its DDL columns {ddl_names:?} do not match the stored columns {egg_names:?}"),
        ));
    }
    Ok(rows.into_iter().map(|(cid, _, d)| (cid, d)).collect())
}

/// The single INTEGER PRIMARY KEY column, if it is a rowid alias. A primary
/// key that is not an alias (e.g. `INTEGER PRIMARY KEY DESC`, or WITHOUT
/// ROWID) gets an automatic index with origin 'pk' and is a normal unique key.
fn find_ipk(conn: &Connection, meta: &TableMeta, cids: &[i64]) -> Result<Option<usize>, Error> {
    let pk_cols: Vec<(i64, String)> = conn
        .prepare("SELECT cid, upper(type) FROM pragma_table_xinfo(?1) WHERE pk > 0")?
        .query_map([&meta.table_name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let has_pk_index: bool = conn.query_row(
        "SELECT count(*) > 0 FROM pragma_index_list(?1) WHERE origin = 'pk'",
        [&meta.table_name],
        |r| r.get(0),
    )?;
    Ok(match pk_cols.as_slice() {
        [(cid, ty)] if ty == "INTEGER" && !has_pk_index => cids.iter().position(|c| c == cid),
        _ => None,
    })
}

fn unique_keys(
    conn: &Connection,
    meta: &TableMeta,
    cids: &[i64],
) -> Result<Vec<Vec<(usize, String)>>, Error> {
    let indexes: Vec<(String, bool)> = conn
        .prepare("SELECT name, partial FROM pragma_index_list(?1) WHERE \"unique\" = 1")?
        .query_map([&meta.table_name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut keys = Vec::with_capacity(indexes.len());
    for (name, partial) in indexes {
        if partial {
            return Err(write_err(
                meta,
                &format!("partial unique index {name} is not supported"),
            ));
        }
        let parts: Vec<(i64, String)> = conn
            .prepare("SELECT cid, coll FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno")?
            .query_map([&name], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let key = parts
            .into_iter()
            .map(|(cid, coll)| {
                cids.iter()
                    .position(|c| *c == cid)
                    .map(|p| (p, coll))
                    .ok_or_else(|| {
                        write_err(
                            meta,
                            &format!("unique index {name} uses an expression or rowid"),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        keys.push(key);
    }
    Ok(keys)
}

/// Checks and normalizes rows for one table, and enforces uniqueness.
pub struct RowValidator {
    conn: Connection,
    /// Quoted column names, in egg order.
    columns: Vec<String>,
    /// Quoted table name (same name in `main` and `keys`).
    table: String,
    /// Quoted rowid column of the keys table.
    rowid_col: String,
    /// Egg columns stored in the keys table, ascending.
    key_columns: Vec<usize>,
    ipk: Option<usize>,
    /// Whether each column has a DEFAULT in the original DDL.
    has_default: Vec<bool>,
}

impl std::fmt::Debug for RowValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowValidator")
            .field("table", &self.table)
            .field("key_columns", &self.key_columns)
            .field("ipk", &self.ipk)
            .finish_non_exhaustive()
    }
}

impl RowValidator {
    /// Build a validator. `load_keys` receives the egg columns that need
    /// uniqueness tracking and returns `(rowid, values)` for every live row,
    /// with `values` aligned to those columns.
    pub fn build(
        meta: &TableMeta,
        load_keys: impl FnOnce(&[usize]) -> Result<Vec<(u64, Vec<Value>)>, Error>,
    ) -> Result<Self, Error> {
        let (conn, schema) = introspect(meta)?;
        let mut key_columns: Vec<usize> = schema
            .unique_keys
            .iter()
            .flatten()
            .map(|(c, _)| *c)
            .collect();
        key_columns.extend(schema.ipk);
        key_columns.sort_unstable();
        key_columns.dedup();

        let mut v = RowValidator {
            conn,
            columns: meta.columns.iter().map(|c| quote(&c.name)).collect(),
            table: quote(&meta.table_name),
            rowid_col: rowid_column_name(meta),
            key_columns,
            ipk: schema.ipk,
            has_default: schema.defaults.iter().map(Option::is_some).collect(),
        };
        if !v.key_columns.is_empty() {
            v.create_keys_table(&schema)?;
            let rows = load_keys(&v.key_columns)?;
            v.load(&rows)?;
        }
        Ok(v)
    }

    /// Fill an omitted INTEGER PRIMARY KEY, then run the row through the
    /// original table definition. Returns the row as SQLite would store it.
    ///
    /// With `fill_defaults` (INSERT), a NULL in a column that has a DEFAULT is
    /// treated as omitted, so the original DEFAULT applies. SQLite does not
    /// tell a virtual table which columns an INSERT named, and it discards a
    /// virtual table's declared defaults, so an omitted column and an explicit
    /// NULL are indistinguishable here. UPDATE never applies defaults.
    pub fn prepare_row(
        &mut self,
        mut row: Vec<Value>,
        fill_defaults: bool,
    ) -> Result<Vec<Value>, Error> {
        if let Some(c) = self.ipk {
            if row[c].is_null() {
                row[c] = Value::Integer(self.next_ipk(c)?);
            }
        }
        let given: Vec<usize> = (0..row.len())
            .filter(|&i| !(fill_defaults && self.has_default[i] && row[i].is_null()))
            .collect();
        let all_cols = self.columns.join(", ");
        let target = if given.is_empty() {
            "DEFAULT VALUES".to_string()
        } else {
            let cols: Vec<&str> = given.iter().map(|&i| self.columns[i].as_str()).collect();
            let params: Vec<String> = (1..=given.len()).map(|i| format!("?{i}")).collect();
            format!("({}) VALUES ({})", cols.join(", "), params.join(", "))
        };
        let sql = format!(
            "INSERT INTO main.{} {target} RETURNING {all_cols}",
            self.table
        );
        let params = given
            .iter()
            .map(|&i| ToSqlOutput::Borrowed(value_ref(&row[i])));

        self.conn.execute_batch("SAVEPOINT egg_check")?;
        let result = self.conn.prepare_cached(&sql).and_then(|mut stmt| {
            stmt.query_row(params_from_iter(params), |r| {
                (0..row.len())
                    .map(|i| from_value_ref(r.get_ref(i)?))
                    .collect()
            })
        });
        self.conn
            .execute_batch("ROLLBACK TO egg_check; RELEASE egg_check")?;
        result
    }

    /// Record a new row's keys; fails on a uniqueness violation.
    pub fn insert_key(&mut self, rowid: u64, row: &[Value]) -> Result<(), Error> {
        let values = self.key_values(row);
        self.insert_key_values(rowid, &values)
    }

    /// Replace a row's keys; fails on a uniqueness violation with another row.
    pub fn update_key(&mut self, rowid: u64, row: &[Value]) -> Result<(), Error> {
        if self.key_columns.is_empty() {
            return Ok(());
        }
        let assignments: Vec<String> = self
            .key_columns
            .iter()
            .enumerate()
            .map(|(i, &c)| format!("{} = ?{}", self.columns[c], i + 2))
            .collect();
        let sql = format!(
            "UPDATE keys.{} SET {} WHERE {} = ?1",
            self.table,
            assignments.join(", "),
            self.rowid_col
        );
        let values = self.key_values(row);
        let params =
            std::iter::once(ValueRef::Integer(to_i64(rowid)?)).chain(values.iter().copied());
        self.conn
            .prepare_cached(&sql)?
            .execute(params_from_iter(params.map(ToSqlOutput::Borrowed)))?;
        Ok(())
    }

    /// Forget a deleted row's keys.
    pub fn delete_key(&mut self, rowid: u64) -> Result<(), Error> {
        if self.key_columns.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "DELETE FROM keys.{} WHERE {} = ?1",
            self.table, self.rowid_col
        );
        self.conn.prepare_cached(&sql)?.execute([to_i64(rowid)?])?;
        Ok(())
    }

    fn key_values<'a>(&self, row: &'a [Value]) -> Vec<ValueRef<'a>> {
        self.key_columns
            .iter()
            .map(|&c| value_ref(&row[c]))
            .collect()
    }

    fn insert_key_values(&mut self, rowid: u64, values: &[ValueRef<'_>]) -> Result<(), Error> {
        if self.key_columns.is_empty() {
            return Ok(());
        }
        let cols: Vec<&str> = self
            .key_columns
            .iter()
            .map(|&c| self.columns[c].as_str())
            .collect();
        let params: Vec<String> = (2..=cols.len() + 1).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "INSERT INTO keys.{} ({}, {}) VALUES (?1, {})",
            self.table,
            self.rowid_col,
            cols.join(", "),
            params.join(", ")
        );
        let all = std::iter::once(ValueRef::Integer(to_i64(rowid)?)).chain(values.iter().copied());
        self.conn
            .prepare_cached(&sql)?
            .execute(params_from_iter(all.map(ToSqlOutput::Borrowed)))?;
        Ok(())
    }

    fn create_keys_table(&mut self, schema: &TableSchema) -> Result<(), Error> {
        let cols: Vec<&str> = self
            .key_columns
            .iter()
            .map(|&c| self.columns[c].as_str())
            .collect();
        let mut sql = format!(
            "ATTACH ':memory:' AS keys; CREATE TABLE keys.{} ({} INTEGER PRIMARY KEY, {});",
            self.table,
            self.rowid_col,
            cols.join(", ")
        );
        let mut all_keys = schema.unique_keys.clone();
        all_keys.extend(schema.ipk.map(|c| vec![(c, "BINARY".to_string())]));
        for (i, key) in all_keys.iter().enumerate() {
            let parts: Vec<String> = key
                .iter()
                .map(|(c, coll)| format!("{} COLLATE {}", self.columns[*c], quote(coll)))
                .collect();
            sql.push_str(&format!(
                "CREATE UNIQUE INDEX keys.egg_key_{i} ON {} ({});",
                self.table,
                parts.join(", ")
            ));
        }
        self.conn.execute_batch(&sql)
    }

    fn load(&mut self, rows: &[(u64, Vec<Value>)]) -> Result<(), Error> {
        self.conn.execute_batch("BEGIN")?;
        let result = rows.iter().try_for_each(|(rowid, values)| {
            let refs: Vec<ValueRef<'_>> = values.iter().map(value_ref).collect();
            self.insert_key_values(*rowid, &refs)
        });
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT"),
            Err(e) => {
                self.conn.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }

    fn next_ipk(&mut self, c: usize) -> Result<i64, Error> {
        let sql = format!("SELECT max({}) FROM keys.{}", self.columns[c], self.table);
        let max: Option<i64> = self
            .conn
            .prepare_cached(&sql)?
            .query_row([], |r| r.get(0))?;
        match max {
            None => Ok(1),
            Some(i64::MAX) => Err(Error::ModuleError(
                "INTEGER PRIMARY KEY values are exhausted (largest id is already i64::MAX)".into(),
            )),
            Some(m) => Ok(m + 1),
        }
    }
}

/// A rowid column name for the keys table that no real column uses.
fn rowid_column_name(meta: &TableMeta) -> String {
    let mut name = "egg_rowid".to_string();
    while meta
        .columns
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(&name))
    {
        name.push('_');
    }
    quote(&name)
}

/// Quote an identifier for SQL.
#[must_use]
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Borrow a [`Value`] as a SQLite value.
#[must_use]
pub fn value_ref(v: &Value) -> ValueRef<'_> {
    match v {
        Value::Null => ValueRef::Null,
        Value::Integer(i) => ValueRef::Integer(*i),
        Value::Real(f) => ValueRef::Real(*f),
        Value::Text(s) => ValueRef::Text(s.as_bytes()),
        Value::Blob(b) => ValueRef::Blob(b),
    }
}

/// Convert a SQLite value to a [`Value`]. Text must be valid UTF-8: storing a
/// lossy replacement would silently change the data.
pub fn from_value_ref(v: ValueRef<'_>) -> Result<Value, Error> {
    Ok(match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(t) => Value::Text(
            std::str::from_utf8(t)
                .map_err(|e| Error::ModuleError(format!("text value is not valid UTF-8: {e}")))?
                .to_string(),
        ),
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    })
}

fn to_i64(rowid: u64) -> Result<i64, Error> {
    i64::try_from(rowid).map_err(|_| Error::ModuleError(format!("rowid {rowid} out of range")))
}

fn write_err(meta: &TableMeta, why: &str) -> Error {
    Error::ModuleError(format!(
        "cannot write to table '{}': {why}",
        meta.table_name
    ))
}
