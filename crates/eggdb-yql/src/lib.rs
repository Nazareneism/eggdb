//! YQL (Yolk Query Language) transpiler.
//!
//! Transpiles YQL queries into SQL strings. YQL is a thin, single-table query
//! language designed as CLI sugar for common query patterns.
//!
//! # Example
//!
//! ```
//! let sql = eggdb_yql::transpile("count orders by status").unwrap();
//! assert_eq!(sql, "SELECT status, count(*) AS count FROM orders GROUP BY status");
//! ```

pub mod ast;
mod parser;
mod sql_gen;
mod tokenizer;

pub use ast::{Command, Query};

/// Errors that can occur during YQL parsing or transpilation.
#[derive(Debug, thiserror::Error)]
pub enum YqlError {
    #[error("unexpected character '{0}' at position {1}")]
    UnexpectedChar(char, usize),

    #[error("unterminated string starting at position {0}")]
    UnterminatedString(usize),

    #[error("unexpected token: expected {expected}, found {found}")]
    UnexpectedToken { expected: String, found: String },

    #[error("unexpected end of input, expected {0}")]
    UnexpectedEof(String),

    #[error("invalid number: {0}")]
    InvalidNumber(String),

    #[error("empty query")]
    EmptyQuery,
}

/// Parse a YQL string into a query AST.
///
/// Returns the parsed AST without generating SQL. Useful for inspection
/// or custom SQL generation.
pub fn parse(input: &str) -> Result<Query, YqlError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(YqlError::EmptyQuery);
    }
    let tokens = tokenizer::tokenize(input)?;
    parser::parse(&tokens)
}

/// Transpile a YQL string directly to a SQL string.
///
/// Equivalent to `parse(input)` followed by SQL generation.
pub fn transpile(input: &str) -> Result<String, YqlError> {
    let query = parse(input)?;
    Ok(sql_gen::generate_sql(&query))
}

/// Check whether an input string looks like a YQL query (starts with a YQL command word).
///
/// If it starts with a YQL command keyword, it's YQL. Otherwise treat as raw SQL.
pub fn is_yql(input: &str) -> bool {
    let first_word = input.split_ascii_whitespace().next().unwrap_or("");
    matches!(
        first_word.to_ascii_lowercase().as_str(),
        "show" | "find" | "list" | "count" | "sum" | "avg" | "min" | "max"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpile_simple() {
        let sql = transpile("show users limit 5").unwrap();
        assert_eq!(sql, "SELECT * FROM users LIMIT 5");
    }

    #[test]
    fn transpile_empty_error() {
        assert!(transpile("").is_err());
        assert!(transpile("   ").is_err());
    }

    #[test]
    fn is_yql_detection() {
        assert!(is_yql("show users"));
        assert!(is_yql("SHOW users"));
        assert!(is_yql("count orders by status"));
        assert!(is_yql("find products where price > 10"));
        assert!(is_yql("list users.name"));
        assert!(is_yql("sum orders.total"));
        assert!(is_yql("avg orders.total"));
        assert!(is_yql("min orders.total"));
        assert!(is_yql("max orders.total"));

        assert!(!is_yql("SELECT * FROM users"));
        assert!(!is_yql("select count(*) from orders"));
        assert!(!is_yql("INSERT INTO users VALUES (1)"));
        assert!(!is_yql(""));
    }

    #[test]
    fn parse_returns_ast() {
        let query = parse("count orders by status").unwrap();
        assert_eq!(query.command, Command::Count);
        assert_eq!(query.table, "orders");
    }
}
