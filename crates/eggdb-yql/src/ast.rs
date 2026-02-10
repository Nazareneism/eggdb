/// A parsed YQL query.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub command: Command,
    pub table: String,
    pub columns: Vec<ColumnExpr>,
    pub filter: Option<Filter>,
    pub group_by: Vec<ColumnExpr>,
    pub order_by: Option<OrderBy>,
    pub limit: Option<u64>,
}

/// The YQL command keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Show,
    Find,
    List,
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// A column expression, optionally wrapped in a function call.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnExpr {
    pub function: Option<Function>,
    pub column: String,
}

/// SQL functions available in YQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Month,
    Year,
    Day,
    Date,
    Lower,
    Upper,
    Length,
}

/// A WHERE filter with one or more AND-chained conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub conditions: Vec<Condition>,
}

/// A single filter condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    Comparison {
        column: ColumnExpr,
        op: ComparisonOp,
        value: LiteralValue,
    },
    Between {
        column: ColumnExpr,
        low: LiteralValue,
        high: LiteralValue,
    },
    In {
        column: ColumnExpr,
        values: Vec<LiteralValue>,
    },
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Gte,
    Lte,
    Like,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// An ORDER BY clause.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    pub column: ColumnExpr,
    pub direction: SortDirection,
}

/// A literal value in YQL.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    String(String),
    Number(f64),
    Null,
}
