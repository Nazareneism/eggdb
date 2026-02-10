use crate::ast::*;

/// Generate a SQL string from a parsed YQL query AST.
pub fn generate_sql(query: &Query) -> String {
    let mut sql = String::new();

    // SELECT clause
    sql.push_str("SELECT ");
    sql.push_str(&generate_select(query));

    // FROM clause
    sql.push_str(" FROM ");
    sql.push_str(&query.table);

    // WHERE clause
    if let Some(ref filter) = query.filter {
        sql.push_str(" WHERE ");
        sql.push_str(&generate_where(filter));
    }

    // GROUP BY clause
    if !query.group_by.is_empty() {
        sql.push_str(" GROUP BY ");
        sql.push_str(&generate_group_by(&query.group_by));
    }

    // ORDER BY clause
    if let Some(ref order_by) = query.order_by {
        sql.push_str(" ORDER BY ");
        sql.push_str(&generate_order_by(order_by));
    }

    // LIMIT clause
    if let Some(limit) = query.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }

    sql
}

fn generate_select(query: &Query) -> String {
    match query.command {
        Command::Show | Command::Find => {
            if query.columns.is_empty() {
                "*".into()
            } else {
                query
                    .columns
                    .iter()
                    .map(column_expr_sql)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
        Command::List => query
            .columns
            .iter()
            .map(column_expr_sql)
            .collect::<Vec<_>>()
            .join(", "),
        Command::Count => {
            if query.group_by.is_empty() && query.columns.is_empty() {
                "count(*) AS count".into()
            } else {
                // With GROUP BY: select group columns + count(*)
                let mut parts: Vec<String> =
                    query.group_by.iter().map(column_expr_with_alias).collect();
                parts.push("count(*) AS count".into());
                parts.join(", ")
            }
        }
        Command::Sum | Command::Avg | Command::Min | Command::Max => {
            let func_name = match query.command {
                Command::Sum => "sum",
                Command::Avg => "avg",
                Command::Min => "min",
                Command::Max => "max",
                _ => unreachable!(),
            };

            let col = if let Some(c) = query.columns.first() {
                &c.column
            } else {
                "*"
            };

            let alias = format!("{func_name}_{col}");

            if query.group_by.is_empty() {
                format!("{func_name}({col}) AS {alias}")
            } else {
                let mut parts: Vec<String> =
                    query.group_by.iter().map(column_expr_with_alias).collect();
                parts.push(format!("{func_name}({col}) AS {alias}"));
                parts.join(", ")
            }
        }
    }
}

fn column_expr_sql(expr: &ColumnExpr) -> String {
    match &expr.function {
        Some(func) => function_sql(func, &expr.column),
        None => expr.column.clone(),
    }
}

fn column_expr_with_alias(expr: &ColumnExpr) -> String {
    match &expr.function {
        Some(func) => {
            let sql = function_sql(func, &expr.column);
            let alias = function_alias(func);
            format!("{sql} AS {alias}")
        }
        None => expr.column.clone(),
    }
}

fn function_sql(func: &Function, column: &str) -> String {
    match func {
        Function::Month => format!("strftime('%m', {column})"),
        Function::Year => format!("strftime('%Y', {column})"),
        Function::Day => format!("strftime('%d', {column})"),
        Function::Date => format!("date({column})"),
        Function::Lower => format!("lower({column})"),
        Function::Upper => format!("upper({column})"),
        Function::Length => format!("length({column})"),
    }
}

fn function_alias(func: &Function) -> &'static str {
    match func {
        Function::Month => "month",
        Function::Year => "year",
        Function::Day => "day",
        Function::Date => "date",
        Function::Lower => "lower",
        Function::Upper => "upper",
        Function::Length => "length",
    }
}

fn generate_where(filter: &Filter) -> String {
    filter
        .conditions
        .iter()
        .map(condition_sql)
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn condition_sql(cond: &Condition) -> String {
    match cond {
        Condition::Comparison { column, op, value } => {
            let col = column_expr_sql(column);
            let op_str = comparison_op_sql(op);
            let val = literal_sql(value);
            format!("{col} {op_str} {val}")
        }
        Condition::Between { column, low, high } => {
            let col = column_expr_sql(column);
            let lo = literal_sql(low);
            let hi = literal_sql(high);
            format!("{col} BETWEEN {lo} AND {hi}")
        }
        Condition::In { column, values } => {
            let col = column_expr_sql(column);
            let vals: Vec<String> = values.iter().map(literal_sql).collect();
            format!("{col} IN ({})", vals.join(", "))
        }
    }
}

fn comparison_op_sql(op: &ComparisonOp) -> &'static str {
    match op {
        ComparisonOp::Eq => "=",
        ComparisonOp::Ne => "!=",
        ComparisonOp::Gt => ">",
        ComparisonOp::Lt => "<",
        ComparisonOp::Gte => ">=",
        ComparisonOp::Lte => "<=",
        ComparisonOp::Like => "LIKE",
    }
}

fn literal_sql(value: &LiteralValue) -> String {
    match value {
        LiteralValue::String(s) => {
            // Escape single quotes by doubling them
            let escaped = s.replace('\'', "''");
            format!("'{escaped}'")
        }
        LiteralValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        LiteralValue::Null => "NULL".into(),
    }
}

fn generate_group_by(columns: &[ColumnExpr]) -> String {
    columns
        .iter()
        .map(column_expr_sql)
        .collect::<Vec<_>>()
        .join(", ")
}

fn generate_order_by(order_by: &OrderBy) -> String {
    let col = column_expr_sql(&order_by.column);
    let dir = match order_by.direction {
        SortDirection::Asc => "ASC",
        SortDirection::Desc => "DESC",
    };
    format!("{col} {dir}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use crate::tokenizer::tokenize;

    fn transpile(input: &str) -> String {
        let tokens = tokenize(input).unwrap();
        let query = parse(&tokens).unwrap();
        generate_sql(&query)
    }

    // All 6 v0_plan.md examples

    #[test]
    fn plan_example_1_show_where_sort_limit() {
        assert_eq!(
            transpile("show users where x = 1 sort name limit 10"),
            "SELECT * FROM users WHERE x = 1 ORDER BY name ASC LIMIT 10"
        );
    }

    #[test]
    fn plan_example_2_count_by() {
        assert_eq!(
            transpile("count orders by status"),
            "SELECT status, count(*) AS count FROM orders GROUP BY status"
        );
    }

    #[test]
    fn plan_example_3_avg_dotted_where_by_function() {
        assert_eq!(
            transpile(r#"avg orders.total where status = "done" by month(created_at)"#),
            "SELECT strftime('%m', created_at) AS month, avg(total) AS avg_total FROM orders WHERE status = 'done' GROUP BY strftime('%m', created_at)"
        );
    }

    #[test]
    fn plan_example_4_find_between() {
        assert_eq!(
            transpile("find products where price between 10 and 50"),
            "SELECT * FROM products WHERE price BETWEEN 10 AND 50"
        );
    }

    #[test]
    fn plan_example_5_list_dotted_in() {
        assert_eq!(
            transpile(r#"list users.name, users.email where country in ("US", "UK")"#),
            "SELECT name, email FROM users WHERE country IN ('US', 'UK')"
        );
    }

    #[test]
    fn plan_example_6_sum() {
        assert_eq!(
            transpile("sum orders.total"),
            "SELECT sum(total) AS sum_total FROM orders"
        );
    }

    // Additional edge case tests

    #[test]
    fn between_and_regular_and() {
        assert_eq!(
            transpile("find products where price between 10 and 50 and status = 'active'"),
            "SELECT * FROM products WHERE price BETWEEN 10 AND 50 AND status = 'active'"
        );
    }

    #[test]
    fn count_without_group() {
        assert_eq!(
            transpile("count orders"),
            "SELECT count(*) AS count FROM orders"
        );
    }

    #[test]
    fn sort_desc() {
        assert_eq!(
            transpile("show users sort name desc"),
            "SELECT * FROM users ORDER BY name DESC"
        );
    }

    #[test]
    fn multiple_and_conditions() {
        assert_eq!(
            transpile("show users where a = 1 and b = 2 and c = 3"),
            "SELECT * FROM users WHERE a = 1 AND b = 2 AND c = 3"
        );
    }

    #[test]
    fn like_operator() {
        assert_eq!(
            transpile("find users where name like '%Alice%'"),
            "SELECT * FROM users WHERE name LIKE '%Alice%'"
        );
    }

    #[test]
    fn null_comparison() {
        assert_eq!(
            transpile("find users where name = null"),
            "SELECT * FROM users WHERE name = NULL"
        );
    }

    #[test]
    fn min_command() {
        assert_eq!(
            transpile("min orders.total"),
            "SELECT min(total) AS min_total FROM orders"
        );
    }

    #[test]
    fn max_command_with_group() {
        assert_eq!(
            transpile("max orders.total by status"),
            "SELECT status, max(total) AS max_total FROM orders GROUP BY status"
        );
    }

    #[test]
    fn select_specific_columns() {
        assert_eq!(
            transpile("show users name, email"),
            "SELECT name, email FROM users"
        );
    }

    #[test]
    fn string_with_single_quote_escape() {
        assert_eq!(
            transpile("find users where name = \"O'Brien\""),
            "SELECT * FROM users WHERE name = 'O''Brien'"
        );
    }

    #[test]
    fn all_functions_in_group_by() {
        assert_eq!(
            transpile("count events by year(created_at)"),
            "SELECT strftime('%Y', created_at) AS year, count(*) AS count FROM events GROUP BY strftime('%Y', created_at)"
        );
    }

    #[test]
    fn function_day() {
        assert_eq!(
            transpile("count events by day(created_at)"),
            "SELECT strftime('%d', created_at) AS day, count(*) AS count FROM events GROUP BY strftime('%d', created_at)"
        );
    }

    #[test]
    fn function_date() {
        assert_eq!(
            transpile("count events by date(created_at)"),
            "SELECT date(created_at) AS date, count(*) AS count FROM events GROUP BY date(created_at)"
        );
    }

    #[test]
    fn function_lower_upper_length() {
        assert_eq!(
            transpile("show users lower(name)"),
            "SELECT lower(name) FROM users"
        );
        assert_eq!(
            transpile("show users upper(name)"),
            "SELECT upper(name) FROM users"
        );
        assert_eq!(
            transpile("show users length(name)"),
            "SELECT length(name) FROM users"
        );
    }

    #[test]
    fn dotted_in_where() {
        assert_eq!(
            transpile("show orders where orders.status = 'done'"),
            "SELECT * FROM orders WHERE status = 'done'"
        );
    }

    #[test]
    fn dotted_in_sort() {
        assert_eq!(
            transpile("show orders sort orders.total desc"),
            "SELECT * FROM orders ORDER BY total DESC"
        );
    }

    #[test]
    fn dotted_in_group_by() {
        assert_eq!(
            transpile("count orders by orders.status"),
            "SELECT status, count(*) AS count FROM orders GROUP BY status"
        );
    }
}
