use crate::ast::*;
use crate::tokenizer::Token;
use crate::YqlError;

/// Parse a sequence of tokens into a YQL query AST.
pub fn parse(tokens: &[Token]) -> Result<Query, YqlError> {
    let mut parser = Parser::new(tokens);
    parser.parse_query()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.pos);
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn expect_token(&mut self, expected: &Token) -> Result<(), YqlError> {
        match self.advance() {
            Some(t) if t == expected => Ok(()),
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: format!("{expected:?}"),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::UnexpectedEof(format!("{expected:?}"))),
        }
    }

    fn parse_query(&mut self) -> Result<Query, YqlError> {
        let command = self.parse_command()?;
        let table = self.parse_table_name()?;

        // Parse optional columns (for list: after table; for aggregates: the target column)
        let columns = self.parse_columns(&command)?;

        let filter = self.parse_filter()?;
        let group_by = self.parse_group_by()?;
        let order_by = self.parse_order_by()?;
        let limit = self.parse_limit()?;

        Ok(Query {
            command,
            table,
            columns,
            filter,
            group_by,
            order_by,
            limit,
        })
    }

    fn parse_command(&mut self) -> Result<Command, YqlError> {
        match self.advance() {
            Some(Token::Show) => Ok(Command::Show),
            Some(Token::Find) => Ok(Command::Find),
            Some(Token::List) => Ok(Command::List),
            Some(Token::Count) => Ok(Command::Count),
            Some(Token::Sum) => Ok(Command::Sum),
            Some(Token::Avg) => Ok(Command::Avg),
            Some(Token::Min) => Ok(Command::Min),
            Some(Token::Max) => Ok(Command::Max),
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: "command (show, find, list, count, sum, avg, min, max)".into(),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::EmptyQuery),
        }
    }

    fn parse_table_name(&mut self) -> Result<String, YqlError> {
        // The table name comes from the first identifier-like token.
        // For aggregate commands with dotted notation like "avg orders.total",
        // the table is "orders" and "total" is a column — but the table comes
        // from the first identifier before a dot or a clause keyword.
        //
        // We handle two patterns:
        //   1. "command table columns..." — table is a bare identifier
        //   2. "command table.col" — table is before the dot (handled by column parsing)
        match self.peek() {
            Some(Token::Ident(_)) => {
                // Check if next is dot — if so, this is "table.col" format
                // The table name is extracted, column handled later
                if let Token::Ident(name) = self.advance().unwrap().clone() {
                    Ok(name)
                } else {
                    unreachable!()
                }
            }
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: "table name".into(),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::UnexpectedEof("table name".into())),
        }
    }

    fn parse_columns(&mut self, command: &Command) -> Result<Vec<ColumnExpr>, YqlError> {
        // Handle dotted table.column: if next token is Dot, the table name
        // was already consumed and what follows the dot is a column name.
        let leading_col = if self.peek() == Some(&Token::Dot) {
            self.advance(); // consume Dot
            let col_name = self.parse_bare_column_name()?;
            Some(ColumnExpr {
                function: None,
                column: col_name,
            })
        } else {
            None
        };

        match command {
            // show/find: optional columns before where/by/sort/limit
            Command::Show | Command::Find => {
                if let Some(col) = leading_col {
                    let mut cols = vec![col];
                    while self.peek() == Some(&Token::Comma) {
                        self.advance();
                        cols.push(self.parse_column_expr()?);
                    }
                    Ok(cols)
                } else if self.at_clause_or_end() {
                    Ok(vec![])
                } else {
                    self.parse_column_list()
                }
            }
            // list: columns required
            Command::List => {
                if let Some(col) = leading_col {
                    let mut cols = vec![col];
                    while self.peek() == Some(&Token::Comma) {
                        self.advance();
                        cols.push(self.parse_column_expr()?);
                    }
                    Ok(cols)
                } else {
                    self.parse_column_list()
                }
            }
            // aggregate commands: optional single target column (may be dotted)
            Command::Count | Command::Sum | Command::Avg | Command::Min | Command::Max => {
                if let Some(col) = leading_col {
                    let mut cols = vec![col];
                    while self.peek() == Some(&Token::Comma) {
                        self.advance();
                        cols.push(self.parse_column_expr()?);
                    }
                    Ok(cols)
                } else if self.at_clause_or_end() {
                    Ok(vec![])
                } else {
                    self.parse_column_list()
                }
            }
        }
    }

    fn parse_column_list(&mut self) -> Result<Vec<ColumnExpr>, YqlError> {
        let mut cols = vec![self.parse_column_expr()?];
        while self.peek() == Some(&Token::Comma) {
            self.advance(); // consume comma
            cols.push(self.parse_column_expr()?);
        }
        Ok(cols)
    }

    fn parse_column_expr(&mut self) -> Result<ColumnExpr, YqlError> {
        // Check for function call: func(col)
        if let Some(func) = self.try_parse_function_token() {
            self.expect_token(&Token::LParen)?;
            let col_name = self.parse_bare_column_name()?;
            self.expect_token(&Token::RParen)?;
            return Ok(ColumnExpr {
                function: Some(func),
                column: col_name,
            });
        }

        // Bare column, possibly dotted: table.col → strip to col
        let name = self.parse_bare_column_name()?;

        // Check for dot — strip table prefix
        if self.peek() == Some(&Token::Dot) {
            self.advance(); // consume dot
            let col_name = self.parse_bare_column_name()?;
            return Ok(ColumnExpr {
                function: None,
                column: col_name,
            });
        }

        Ok(ColumnExpr {
            function: None,
            column: name,
        })
    }

    fn try_parse_function_token(&mut self) -> Option<Function> {
        // Only consume if next token is a function keyword AND followed by '('
        if self.pos + 1 < self.tokens.len() && self.tokens[self.pos + 1] == Token::LParen {
            let func = match self.peek()? {
                Token::Month => Some(Function::Month),
                Token::Year => Some(Function::Year),
                Token::Day => Some(Function::Day),
                Token::Date => Some(Function::Date),
                Token::Lower => Some(Function::Lower),
                Token::Upper => Some(Function::Upper),
                Token::Length => Some(Function::Length),
                _ => None,
            };
            if func.is_some() {
                self.advance(); // consume the function token
            }
            func
        } else {
            None
        }
    }

    fn parse_bare_column_name(&mut self) -> Result<String, YqlError> {
        match self.advance() {
            Some(Token::Ident(name)) => Ok(name.clone()),
            // Function keywords used as column names (unlikely but handle gracefully)
            Some(Token::Month) => Ok("month".into()),
            Some(Token::Year) => Ok("year".into()),
            Some(Token::Day) => Ok("day".into()),
            Some(Token::Date) => Ok("date".into()),
            Some(Token::Lower) => Ok("lower".into()),
            Some(Token::Upper) => Ok("upper".into()),
            Some(Token::Length) => Ok("length".into()),
            // Also allow other keywords as identifiers when used as column names
            Some(Token::Count) => Ok("count".into()),
            Some(Token::Sum) => Ok("sum".into()),
            Some(Token::Avg) => Ok("avg".into()),
            Some(Token::Min) => Ok("min".into()),
            Some(Token::Max) => Ok("max".into()),
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: "column name".into(),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::UnexpectedEof("column name".into())),
        }
    }

    fn parse_filter(&mut self) -> Result<Option<Filter>, YqlError> {
        if self.peek() != Some(&Token::Where) {
            return Ok(None);
        }
        self.advance(); // consume WHERE

        let mut conditions = vec![self.parse_condition()?];
        while self.peek() == Some(&Token::And) {
            self.advance(); // consume AND
            conditions.push(self.parse_condition()?);
        }

        Ok(Some(Filter { conditions }))
    }

    fn parse_condition(&mut self) -> Result<Condition, YqlError> {
        let column = self.parse_column_expr()?;

        match self.peek() {
            Some(Token::Between) => {
                self.advance(); // consume BETWEEN
                let low = self.parse_literal()?;
                self.expect_token(&Token::And)?;
                let high = self.parse_literal()?;
                Ok(Condition::Between { column, low, high })
            }
            Some(Token::In) => {
                self.advance(); // consume IN
                self.expect_token(&Token::LParen)?;
                let mut values = vec![self.parse_literal()?];
                while self.peek() == Some(&Token::Comma) {
                    self.advance(); // consume comma
                    values.push(self.parse_literal()?);
                }
                self.expect_token(&Token::RParen)?;
                Ok(Condition::In { column, values })
            }
            Some(Token::Like) => {
                self.advance(); // consume LIKE
                let value = self.parse_literal()?;
                Ok(Condition::Comparison {
                    column,
                    op: ComparisonOp::Like,
                    value,
                })
            }
            _ => {
                let op = self.parse_comparison_op()?;
                let value = self.parse_literal()?;
                Ok(Condition::Comparison { column, op, value })
            }
        }
    }

    fn parse_comparison_op(&mut self) -> Result<ComparisonOp, YqlError> {
        match self.advance() {
            Some(Token::Eq) => Ok(ComparisonOp::Eq),
            Some(Token::Ne) => Ok(ComparisonOp::Ne),
            Some(Token::Gt) => Ok(ComparisonOp::Gt),
            Some(Token::Lt) => Ok(ComparisonOp::Lt),
            Some(Token::Gte) => Ok(ComparisonOp::Gte),
            Some(Token::Lte) => Ok(ComparisonOp::Lte),
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: "comparison operator (=, !=, >, <, >=, <=, like)".into(),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::UnexpectedEof("comparison operator".into())),
        }
    }

    fn parse_literal(&mut self) -> Result<LiteralValue, YqlError> {
        match self.advance() {
            Some(Token::StringLit(s)) => Ok(LiteralValue::String(s.clone())),
            Some(Token::NumberLit(n)) => Ok(LiteralValue::Number(*n)),
            Some(Token::Null) => Ok(LiteralValue::Null),
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: "literal value (string, number, or null)".into(),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::UnexpectedEof("literal value".into())),
        }
    }

    fn parse_group_by(&mut self) -> Result<Vec<ColumnExpr>, YqlError> {
        if self.peek() != Some(&Token::By) {
            return Ok(vec![]);
        }
        self.advance(); // consume BY
        self.parse_column_list()
    }

    fn parse_order_by(&mut self) -> Result<Option<OrderBy>, YqlError> {
        if self.peek() != Some(&Token::Sort) {
            return Ok(None);
        }
        self.advance(); // consume SORT

        let column = self.parse_column_expr()?;
        let direction = match self.peek() {
            Some(Token::Asc) => {
                self.advance();
                SortDirection::Asc
            }
            Some(Token::Desc) => {
                self.advance();
                SortDirection::Desc
            }
            _ => SortDirection::Asc, // default
        };

        Ok(Some(OrderBy { column, direction }))
    }

    fn parse_limit(&mut self) -> Result<Option<u64>, YqlError> {
        if self.peek() != Some(&Token::Limit) {
            return Ok(None);
        }
        self.advance(); // consume LIMIT

        match self.advance() {
            Some(Token::NumberLit(n)) => {
                if *n < 0.0 || n.fract() != 0.0 {
                    return Err(YqlError::InvalidNumber(format!(
                        "limit must be a positive integer, got {n}"
                    )));
                }
                Ok(Some(*n as u64))
            }
            Some(t) => Err(YqlError::UnexpectedToken {
                expected: "positive integer".into(),
                found: format!("{t:?}"),
            }),
            None => Err(YqlError::UnexpectedEof("limit value".into())),
        }
    }

    /// Check if we're at a clause keyword or end of input.
    fn at_clause_or_end(&self) -> bool {
        matches!(
            self.peek(),
            None | Some(Token::Where) | Some(Token::By) | Some(Token::Sort) | Some(Token::Limit)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::tokenize;

    fn parse_yql(input: &str) -> Result<Query, YqlError> {
        let tokens = tokenize(input)?;
        parse(&tokens)
    }

    #[test]
    fn parse_show_all() {
        let q = parse_yql("show users").unwrap();
        assert_eq!(q.command, Command::Show);
        assert_eq!(q.table, "users");
        assert!(q.columns.is_empty());
        assert!(q.filter.is_none());
        assert!(q.group_by.is_empty());
        assert!(q.order_by.is_none());
        assert!(q.limit.is_none());
    }

    #[test]
    fn parse_show_with_filter_sort_limit() {
        let q = parse_yql("show users where x = 1 sort name limit 10").unwrap();
        assert_eq!(q.command, Command::Show);
        assert_eq!(q.table, "users");
        assert!(q.columns.is_empty());
        assert!(q.filter.is_some());
        let conditions = &q.filter.unwrap().conditions;
        assert_eq!(conditions.len(), 1);
        assert!(matches!(&conditions[0], Condition::Comparison { .. }));
        assert_eq!(q.order_by.unwrap().direction, SortDirection::Asc);
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn parse_count_with_group_by() {
        let q = parse_yql("count orders by status").unwrap();
        assert_eq!(q.command, Command::Count);
        assert_eq!(q.table, "orders");
        assert!(q.columns.is_empty());
        assert_eq!(q.group_by.len(), 1);
        assert_eq!(q.group_by[0].column, "status");
    }

    #[test]
    fn parse_avg_with_dotted_column() {
        let q = parse_yql("avg orders.total where status = \"done\" by month(created_at)").unwrap();
        assert_eq!(q.command, Command::Avg);
        assert_eq!(q.table, "orders");
        assert_eq!(q.columns.len(), 1);
        assert_eq!(q.columns[0].column, "total");
        assert!(q.filter.is_some());
        assert_eq!(q.group_by.len(), 1);
        assert_eq!(q.group_by[0].function, Some(Function::Month));
    }

    #[test]
    fn parse_between() {
        let q = parse_yql("find products where price between 10 and 50").unwrap();
        assert_eq!(q.command, Command::Find);
        let cond = &q.filter.unwrap().conditions[0];
        assert!(matches!(cond, Condition::Between { .. }));
    }

    #[test]
    fn parse_between_and_regular_and() {
        let q =
            parse_yql("find products where price between 10 and 50 and status = 'active'").unwrap();
        let conditions = &q.filter.unwrap().conditions;
        assert_eq!(conditions.len(), 2);
        assert!(matches!(&conditions[0], Condition::Between { .. }));
        assert!(matches!(&conditions[1], Condition::Comparison { .. }));
    }

    #[test]
    fn parse_in_list() {
        let q = parse_yql(r#"list users.name, users.email where country in ("US", "UK")"#).unwrap();
        assert_eq!(q.command, Command::List);
        assert_eq!(q.columns.len(), 2);
        assert_eq!(q.columns[0].column, "name");
        assert_eq!(q.columns[1].column, "email");
        let cond = &q.filter.unwrap().conditions[0];
        if let Condition::In { values, .. } = cond {
            assert_eq!(values.len(), 2);
        } else {
            panic!("expected In condition");
        }
    }

    #[test]
    fn parse_sum() {
        let q = parse_yql("sum orders.total").unwrap();
        assert_eq!(q.command, Command::Sum);
        assert_eq!(q.table, "orders");
        assert_eq!(q.columns.len(), 1);
        assert_eq!(q.columns[0].column, "total");
    }

    #[test]
    fn parse_sort_desc() {
        let q = parse_yql("show users sort name desc").unwrap();
        assert_eq!(q.order_by.unwrap().direction, SortDirection::Desc);
    }

    #[test]
    fn parse_multiple_and_conditions() {
        let q = parse_yql("show users where a = 1 and b = 2 and c = 3").unwrap();
        assert_eq!(q.filter.unwrap().conditions.len(), 3);
    }

    #[test]
    fn parse_like_operator() {
        let q = parse_yql("find users where name like '%Alice%'").unwrap();
        let cond = &q.filter.unwrap().conditions[0];
        assert!(matches!(
            cond,
            Condition::Comparison {
                op: ComparisonOp::Like,
                ..
            }
        ));
    }

    #[test]
    fn parse_null_value() {
        let q = parse_yql("find users where name = null").unwrap();
        let cond = &q.filter.unwrap().conditions[0];
        if let Condition::Comparison { value, .. } = cond {
            assert_eq!(*value, LiteralValue::Null);
        } else {
            panic!("expected Comparison condition");
        }
    }

    #[test]
    fn parse_all_functions() {
        for func in ["month", "year", "day", "date", "lower", "upper", "length"] {
            let input = format!("show users by {func}(col)");
            let q = parse_yql(&input).unwrap();
            assert!(
                q.group_by[0].function.is_some(),
                "function {func} not parsed"
            );
        }
    }

    #[test]
    fn empty_input() {
        let result = parse_yql("");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), YqlError::EmptyQuery));
    }

    #[test]
    fn parse_case_insensitive() {
        let q = parse_yql("SHOW Users WHERE X = 1").unwrap();
        assert_eq!(q.command, Command::Show);
        assert_eq!(q.table, "Users");
    }

    #[test]
    fn parse_min_max_commands() {
        let q = parse_yql("min orders.total").unwrap();
        assert_eq!(q.command, Command::Min);
        let q = parse_yql("max orders.total").unwrap();
        assert_eq!(q.command, Command::Max);
    }
}
