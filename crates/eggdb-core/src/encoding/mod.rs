pub mod analyzer;
pub mod bool_bitmap;
pub mod constant;
pub mod delta;
pub mod dictionary;
pub mod null_bitmap;
pub mod raw;
pub mod rle;

use std::fmt;

use bitvec::prelude::*;
use serde::{Deserialize, Serialize};

use crate::format::FormatError;
use crate::value::Value;

/// Column encoding strategy, selected per-column during `lay`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// Entire column is one repeated value.
    Const,
    /// Boolean or 2-value column, bitpacked.
    BoolBitmap,
    /// Monotonic numeric column, stored as base + zigzag varint deltas.
    Delta,
    /// Consecutive duplicates compressed as (value, run_length) pairs.
    Rle,
    /// Low-cardinality column, values mapped to dictionary indices.
    Dict,
    /// High-cardinality, rely on zstd compression only.
    Raw,
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Encoding::Const => write!(f, "Const"),
            Encoding::BoolBitmap => write!(f, "BoolBitmap"),
            Encoding::Delta => write!(f, "Delta"),
            Encoding::Rle => write!(f, "Rle"),
            Encoding::Dict => write!(f, "Dict"),
            Encoding::Raw => write!(f, "Raw"),
        }
    }
}

/// Helper to construct a `FormatError::Decode` error.
pub fn decode_err(encoding: &'static str, detail: impl Into<String>) -> FormatError {
    FormatError::Decode {
        encoding,
        detail: detail.into(),
    }
}

/// Filter operations for push-down predicates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Gte,
    Lte,
    Like,
    IsNull,
    IsNotNull,
}

/// Trait implemented by all column encoders.
///
/// **Contract**:
/// - `values` passed to `encode()` contain **only non-null values**, in row order.
///   The null bitmap is handled externally.
/// - `decode()` returns non-null values. The caller interleaves nulls using the bitmap.
/// - `non_null_count` is the number of non-null values (i.e., `values.len()` for encode,
///   and the expected output length for decode).
pub trait ColumnEncoder {
    /// Encode non-null values into a byte stream.
    fn encode(&self, values: &[Value]) -> Vec<u8>;

    /// Decode a byte stream back to non-null values.
    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError>;

    /// Filter without full decode. Returns a bitvec over non-null indices
    /// where set bits indicate matching rows.
    ///
    /// Default implementation decodes all values then filters.
    fn filter_indices(
        &self,
        data: &[u8],
        non_null_count: usize,
        op: &FilterOp,
        value: &Value,
    ) -> Result<BitVec, FormatError> {
        let decoded = self.decode(data, non_null_count)?;
        let mut result = bitvec![0; decoded.len()];
        for (i, v) in decoded.iter().enumerate() {
            if matches_filter(v, op, value) {
                result.set(i, true);
            }
        }
        Ok(result)
    }
}

/// Evaluate a filter predicate on a single value.
#[must_use]
pub fn matches_filter(v: &Value, op: &FilterOp, target: &Value) -> bool {
    use std::cmp::Ordering;
    match op {
        FilterOp::IsNull => v.is_null(),
        FilterOp::IsNotNull => !v.is_null(),
        FilterOp::Like => {
            if let (Value::Text(haystack), Value::Text(pattern)) = (v, target) {
                sql_like(haystack, pattern)
            } else {
                false
            }
        }
        _ => {
            if v.is_null() || target.is_null() {
                return false; // SQL semantics: NULL never matches comparisons
            }
            let ord = v.cmp(target);
            match op {
                FilterOp::Eq => ord == Ordering::Equal,
                FilterOp::Ne => ord != Ordering::Equal,
                FilterOp::Gt => ord == Ordering::Greater,
                FilterOp::Lt => ord == Ordering::Less,
                FilterOp::Gte => ord != Ordering::Less,
                FilterOp::Lte => ord != Ordering::Greater,
                _ => unreachable!(),
            }
        }
    }
}

/// Simple SQL LIKE matching (% and _ wildcards, no ESCAPE).
fn sql_like(haystack: &str, pattern: &str) -> bool {
    sql_like_inner(haystack.as_bytes(), pattern.as_bytes())
}

fn sql_like_inner(s: &[u8], p: &[u8]) -> bool {
    match (s.first(), p.first()) {
        (_, Some(b'%')) => {
            // '%' matches zero or more characters
            sql_like_inner(s, &p[1..]) || (!s.is_empty() && sql_like_inner(&s[1..], p))
        }
        (Some(sc), Some(b'_')) => {
            // '_' matches exactly one character
            let _ = sc;
            sql_like_inner(&s[1..], &p[1..])
        }
        (Some(sc), Some(pc)) => sc.eq_ignore_ascii_case(pc) && sql_like_inner(&s[1..], &p[1..]),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => {
            // If pattern is only '%' remaining, it can match empty
            p.iter().all(|&c| c == b'%')
        }
    }
}

/// Expand a bitvec of non-null match indices to full row-space indices.
///
/// Given a `non_null_matches` bitvec (one bit per non-null value) and the
/// `null_bitmap` (1 = null, 0 = present), produce a bitvec over all rows
/// where set bits indicate matching rows. Null rows are always unset.
#[must_use]
pub fn expand_non_null_indices(non_null_matches: &BitVec, null_bitmap: &BitVec) -> BitVec {
    let row_count = null_bitmap.len();
    let mut result = bitvec![0; row_count];
    let mut non_null_idx = 0usize;
    for row in 0..row_count {
        if !null_bitmap[row] {
            // This row is non-null
            if non_null_idx < non_null_matches.len() && non_null_matches[non_null_idx] {
                result.set(row, true);
            }
            non_null_idx += 1;
        }
        // Null rows stay 0
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_like_basic() {
        assert!(sql_like("hello", "hello"));
        assert!(sql_like("hello", "%"));
        assert!(sql_like("hello", "h%"));
        assert!(sql_like("hello", "%o"));
        assert!(sql_like("hello", "h_llo"));
        assert!(sql_like("hello", "%ll%"));
        assert!(!sql_like("hello", "world"));
        assert!(!sql_like("hello", "h_o"));
    }

    #[test]
    fn sql_like_case_insensitive() {
        assert!(sql_like("Hello", "hello"));
        assert!(sql_like("HELLO", "hello"));
    }

    #[test]
    fn expand_indices_basic() {
        // Rows: [A, NULL, B, A, NULL, C]
        // null_bitmap: [0, 1, 0, 0, 1, 0]
        // Non-null values: [A, B, A, C] (indices 0,1,2,3)
        // Suppose filter matches A: non_null_matches = [1, 0, 1, 0]
        let null_bitmap = bitvec![0, 1, 0, 0, 1, 0];
        let non_null_matches = bitvec![1, 0, 1, 0];
        let result = expand_non_null_indices(&non_null_matches, &null_bitmap);
        // Expected row matches: row 0 (A), row 3 (A)
        assert_eq!(result, bitvec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn expand_indices_no_nulls() {
        let null_bitmap = bitvec![0, 0, 0];
        let non_null_matches = bitvec![1, 0, 1];
        let result = expand_non_null_indices(&non_null_matches, &null_bitmap);
        assert_eq!(result, bitvec![1, 0, 1]);
    }

    #[test]
    fn expand_indices_all_null() {
        let null_bitmap = bitvec![1, 1, 1];
        let non_null_matches = bitvec![];
        let result = expand_non_null_indices(&non_null_matches, &null_bitmap);
        assert_eq!(result, bitvec![0, 0, 0]);
    }

    #[test]
    fn matches_filter_null_semantics() {
        // NULL never matches comparison operators
        assert!(!matches_filter(
            &Value::Null,
            &FilterOp::Eq,
            &Value::Integer(1)
        ));
        assert!(!matches_filter(
            &Value::Integer(1),
            &FilterOp::Eq,
            &Value::Null
        ));
        // But IS NULL / IS NOT NULL work
        assert!(matches_filter(
            &Value::Null,
            &FilterOp::IsNull,
            &Value::Null
        ));
        assert!(!matches_filter(
            &Value::Integer(1),
            &FilterOp::IsNull,
            &Value::Null
        ));
        assert!(matches_filter(
            &Value::Integer(1),
            &FilterOp::IsNotNull,
            &Value::Null
        ));
    }
}
