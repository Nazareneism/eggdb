use eggdb_core::Value;

/// A decoded column — a vector of values with one entry per row (including nulls).
pub type DecodedColumn = Vec<Value>;
