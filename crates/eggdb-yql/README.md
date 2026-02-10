# eggdb-yql

YQL (Yolk Query Language) transpiler. Parses a friendlier query syntax and produces
standard SQL strings. Pure string transformation — no query execution, no external
eggdb dependencies.

## Usage

```rust
use eggdb_yql::{transpile, parse, is_yql};

// Transpile YQL to SQL
let sql = transpile("count orders by status")?;
assert_eq!(sql, "SELECT status, count(*) AS count FROM orders GROUP BY status");

// Check if input looks like YQL (vs raw SQL)
assert!(is_yql("show users where active = true"));
assert!(!is_yql("SELECT * FROM users"));

// Parse to AST (for inspection)
let ast = parse("show users where x = 1 sort name limit 10")?;
```

## YQL Grammar

```
query      = command table [columns] [filter] [grouping] [ordering] [limit]
command    = "show" | "count" | "sum" | "avg" | "min" | "max" | "find" | "list"
table      = identifier
columns    = column_expr ("," column_expr)*
filter     = "where" condition ("and" condition)*
condition  = column_expr operator value
           | column_expr "between" value "and" value
           | column_expr "in" "(" value_list ")"
grouping   = "by" column_expr ("," column_expr)*
ordering   = "sort" column_expr ["asc" | "desc"]
limit      = "limit" number
```

### Commands

| Command | SQL Equivalent |
|---------|----------------|
| `show` / `find` / `list` | `SELECT *` (or specified columns) |
| `count` | `SELECT count(*)` |
| `sum` | `SELECT sum(column)` |
| `avg` | `SELECT avg(column)` |
| `min` | `SELECT min(column)` |
| `max` | `SELECT max(column)` |

### Functions

| YQL | SQL |
|-----|-----|
| `month(col)` | `strftime('%m', col)` |
| `year(col)` | `strftime('%Y', col)` |
| `day(col)` | `strftime('%d', col)` |
| `date(col)` | `date(col)` |
| `lower(col)` | `lower(col)` |
| `upper(col)` | `upper(col)` |
| `length(col)` | `length(col)` |

### Examples

```
show users where status = "active" sort name limit 10
  -> SELECT * FROM users WHERE status = 'active' ORDER BY name ASC LIMIT 10

count orders by status
  -> SELECT status, count(*) AS count FROM orders GROUP BY status

avg orders.total where status = "done" by month(created_at)
  -> SELECT strftime('%m', created_at) AS month, avg(total) AS avg_total
     FROM orders WHERE status = 'done' GROUP BY strftime('%m', created_at)

find products where price between 10 and 50
  -> SELECT * FROM products WHERE price BETWEEN 10 AND 50

list users.name, users.email where country in ("US", "UK")
  -> SELECT name, email FROM users WHERE country IN ('US', 'UK')
```

## Design Constraints

- **Single-table only** — no joins or subqueries. Use SQL for multi-table queries.
- **Dotted identifiers** (`users.name`) are stripped to bare column (`name`) since the table is always unambiguous.
- **`between ... and ...`** is a single ternary keyword. The `and` inside `between` is consumed by the between rule, not by condition chaining.
- **String literals** are always output as single-quoted (SQLite convention), with `'` escaped as `''`.

## Modules

| File | Purpose |
|------|---------|
| `lib.rs` | Public API: `parse()`, `transpile()`, `is_yql()`, `YqlError` |
| `tokenizer.rs` | Lexer: keywords, identifiers, literals, operators -> `Token` stream |
| `parser.rs` | Recursive descent parser: tokens -> `Query` AST |
| `ast.rs` | AST types: `Query`, `Command`, `ColumnExpr`, `Condition`, `OrderBy` |
| `sql_gen.rs` | SQL generator: AST -> SQL string with proper quoting |

## Dependencies

```
thiserror   — Error types
```

No other dependencies. No eggdb crate dependencies. This is a standalone transpiler
that could be used independently.

## Testing

```bash
cargo test -p eggdb-yql
cargo +nightly fuzz run fuzz_yql_parser  # Fuzz with arbitrary strings
```

Tests include:
- Valid YQL inputs produce correct SQL
- Invalid inputs produce clear error messages
- Edge cases: empty strings, unicode, deeply nested conditions
- `between...and...` parsed correctly (not confused with condition `and`)
- `is_yql()` correctly distinguishes YQL from SQL
