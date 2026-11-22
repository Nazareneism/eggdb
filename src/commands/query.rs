use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use comfy_table::{Cell, Table};
use rusqlite::types::ValueRef;

use crate::commands::pending::open_snapshot;

/// Run a query (YQL or SQL) against an .egg file.
pub fn run(egg: &Path, query: &str, passphrase: Option<&str>) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let sql = detect_and_transpile(query)?;
    let (conn, _tables) = setup_egg_connection(egg, passphrase)?;
    execute_and_print(&conn, &sql)
}

/// Detect whether input is YQL or SQL and transpile if needed.
pub(crate) fn detect_and_transpile(input: &str) -> Result<String> {
    let input = input.trim();
    anyhow::ensure!(!input.is_empty(), "empty query");

    if eggdb_yql::is_yql(input) {
        let sql =
            eggdb_yql::transpile(input).with_context(|| format!("failed to parse YQL: {input}"))?;
        eprintln!("{} {}", "SQL:".dimmed(), sql.dimmed());
        Ok(sql)
    } else {
        Ok(input.to_string())
    }
}

/// Open an .egg file and set up an in-memory SQLite connection with writable
/// virtual tables (one per egg table, same names).
pub(crate) fn setup_egg_connection(
    egg: &Path,
    passphrase: Option<&str>,
) -> Result<(rusqlite::Connection, Vec<eggdb_core::TableMeta>)> {
    // Table names need no decryption key; opening the snapshot here also
    // reports a broken or mismatched write log before any table is created.
    let tables = open_snapshot(egg, None)?.tables().to_vec();

    let conn = rusqlite::Connection::open_in_memory()
        .context("failed to open in-memory SQLite connection")?;

    eggdb_vtable::register_egg_module(&conn)
        .context("failed to register egg virtual table module")?;

    // Canonicalize the path so SQLite can find the file
    let abs_path = egg
        .canonicalize()
        .with_context(|| format!("failed to resolve path: {}", egg.display()))?;

    for table in &tables {
        let mut args = vec![
            sql_string(&abs_path.display().to_string()),
            sql_string(&table.table_name),
            sql_string("mode=rw"),
        ];
        if let Some(pass) = passphrase {
            args.push(sql_string(&format!("passphrase={pass}")));
        }
        let sql = format!(
            "CREATE VIRTUAL TABLE \"{}\" USING egg({})",
            table.table_name.replace('"', "\"\""),
            args.join(", ")
        );
        conn.execute_batch(&sql).with_context(|| {
            format!("failed to create virtual table for '{}'", table.table_name)
        })?;
    }

    Ok((conn, tables))
}

/// A single-quoted SQL string literal.
fn sql_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Execute a SQL statement. Queries print a table; statements that return
/// no columns (INSERT, UPDATE, DELETE, ...) print how many rows changed.
pub(crate) fn execute_and_print(conn: &rusqlite::Connection, sql: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("failed to prepare SQL: {sql}"))?;

    if stmt.column_count() == 0 {
        let changed = stmt.execute([]).context("statement failed")?;
        println!(
            "{}",
            format!(
                "{changed} row{} changed (saved; run `egg compact` later to fold changes into the main file)",
                if changed == 1 { "" } else { "s" }
            )
            .dimmed()
        );
        return Ok(());
    }

    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
        .collect();

    let rows: Vec<Vec<String>> = stmt
        .query_map([], |row| {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let val = match row.get_ref(i) {
                    Ok(ValueRef::Null) => "NULL".to_string(),
                    Ok(ValueRef::Integer(n)) => n.to_string(),
                    Ok(ValueRef::Real(f)) => f.to_string(),
                    Ok(ValueRef::Text(s)) => String::from_utf8_lossy(s).to_string(),
                    Ok(ValueRef::Blob(b)) => format!("<blob {} bytes>", b.len()),
                    Err(_) => "?".to_string(),
                };
                values.push(val);
            }
            Ok(values)
        })
        .context("query execution failed")?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to read query results")?;

    if rows.is_empty() {
        println!("{}", "(no results)".dimmed());
        return Ok(());
    }

    let mut table = Table::new();
    table.set_header(col_names.iter().map(Cell::new));
    for row in &rows {
        table.add_row(row.iter().map(Cell::new));
    }
    println!("{table}");
    println!(
        "{}",
        format!(
            "{} row{}",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        )
        .dimmed()
    );

    Ok(())
}
