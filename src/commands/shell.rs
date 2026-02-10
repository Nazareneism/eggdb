use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::commands::query::{detect_and_transpile, execute_and_print, setup_egg_connection};

/// Run an interactive YQL/SQL REPL for an .egg file.
pub fn run(egg: &Path, passphrase: Option<&str>) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let (conn, tables) = setup_egg_connection(egg, passphrase)?;

    // Welcome banner
    println!(
        "{} {}",
        "Egg shell:".bold(),
        egg.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    println!(
        "  {} table{}: {}",
        tables.len(),
        if tables.len() == 1 { "" } else { "s" },
        tables
            .iter()
            .map(|t| t.table_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  Type {} for help, {} to exit",
        ".help".bold(),
        ".quit".bold()
    );
    println!();

    let mut rl = DefaultEditor::new().context("failed to initialize line editor")?;

    loop {
        match rl.readline("egg> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(line);

                // Dot-commands
                if let Some(cmd) = line.strip_prefix('.') {
                    match handle_dot_command(cmd, &tables, &conn) {
                        DotResult::Continue => continue,
                        DotResult::Quit => break,
                    }
                }

                // Query (YQL or SQL)
                match detect_and_transpile(line) {
                    Ok(sql) => {
                        if let Err(e) = execute_and_print(&conn, &sql) {
                            eprintln!("{}: {:#}", "Error".red(), e);
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: {:#}", "Error".red(), e);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: continue
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D: exit
                println!("Goodbye!");
                break;
            }
            Err(e) => {
                eprintln!("{}: {:#}", "Error".red(), e);
                break;
            }
        }
    }

    Ok(())
}

enum DotResult {
    Continue,
    Quit,
}

fn handle_dot_command(
    cmd: &str,
    tables: &[eggdb_core::TableMeta],
    conn: &rusqlite::Connection,
) -> DotResult {
    let parts: Vec<&str> = cmd.split_ascii_whitespace().collect();
    let command = parts.first().copied().unwrap_or("");

    match command {
        "help" => {
            println!("{}", "Commands:".bold());
            println!("  {} — list tables", ".tables".bold());
            println!(
                "  {} — show schema (optional table filter)",
                ".schema [table]".bold()
            );
            println!("  {} — show this help", ".help".bold());
            println!("  {} — exit the shell", ".quit / .exit".bold());
            println!();
            println!("{}", "Queries:".bold());
            println!("  Type YQL (e.g. 'show users limit 5') or SQL (e.g. 'SELECT * FROM users')");
        }
        "tables" => {
            for table in tables {
                println!(
                    "  {} ({} rows, {} columns)",
                    table.table_name.bold(),
                    table.row_count,
                    table.columns.len()
                );
            }
        }
        "schema" => {
            let filter = parts.get(1).copied();
            let mut found = false;
            for table in tables {
                if let Some(name) = filter {
                    if !table.table_name.eq_ignore_ascii_case(name) {
                        continue;
                    }
                }
                found = true;
                println!("{}", table.original_ddl);

                // Print index definitions
                for idx in &table.index_definitions {
                    if !idx.is_empty() {
                        println!("{idx}");
                    }
                }
                println!();
            }
            if !found {
                if let Some(name) = filter {
                    eprintln!("{}: table '{}' not found", "Error".red(), name);
                }
            }
        }
        "quit" | "exit" => {
            println!("Goodbye!");
            return DotResult::Quit;
        }
        _ => {
            // Try as a SQLite pragma/command
            let full = format!(".{cmd}");
            eprintln!(
                "{}: unknown command '{}'. Type .help for help.",
                "Error".red(),
                full
            );
            // Also try executing as raw SQL in case user forgot
            let _ = try_dot_as_pragma(conn, cmd);
        }
    }

    DotResult::Continue
}

fn try_dot_as_pragma(conn: &rusqlite::Connection, cmd: &str) -> Result<()> {
    // Some users might type ".tables" style commands expecting SQLite behavior
    // We've handled the common ones above, so this is a fallback
    let sql = format!("PRAGMA {cmd}");
    if let Ok(mut stmt) = conn.prepare(&sql) {
        let col_count = stmt.column_count();
        if col_count > 0 {
            let rows: Vec<Vec<String>> = stmt
                .query_map([], |row| {
                    let mut values = Vec::with_capacity(col_count);
                    for i in 0..col_count {
                        let val = match row.get_ref(i) {
                            Ok(rusqlite::types::ValueRef::Null) => "NULL".to_string(),
                            Ok(rusqlite::types::ValueRef::Integer(n)) => n.to_string(),
                            Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                            Ok(rusqlite::types::ValueRef::Text(s)) => {
                                String::from_utf8_lossy(s).to_string()
                            }
                            Ok(rusqlite::types::ValueRef::Blob(b)) => {
                                format!("<blob {} bytes>", b.len())
                            }
                            Err(_) => "?".to_string(),
                        };
                        values.push(val);
                    }
                    Ok(values)
                })?
                .collect::<Result<Vec<_>, _>>()?;

            for row in &rows {
                println!("{}", row.join(" | "));
            }
        }
    }
    Ok(())
}
