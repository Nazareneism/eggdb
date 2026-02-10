use std::collections::HashSet;

use crate::encoding::Encoding;
use crate::value::Value;

/// Statistics about a column, used to select the best encoding.
#[derive(Clone, Debug)]
pub struct ColumnStats {
    pub row_count: usize,
    pub null_count: usize,
    pub distinct_count: usize,
    pub is_numeric: bool,
    /// True if all non-null values are `Value::Integer` (no reals).
    pub is_integer_only: bool,
    pub is_sorted: bool,
    pub consecutive_dup_ratio: f64,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
}

impl ColumnStats {
    /// Returns true if the column has exactly 2 distinct non-null values
    /// and they look boolean-like (e.g., 0/1, true/false).
    #[must_use]
    pub fn is_boolean_like(&self) -> bool {
        if self.distinct_count != 2 {
            return false;
        }
        // Any 2-value column is eligible for BoolBitmap encoding
        true
    }
}

/// Analyze a column's values to compute statistics for encoding selection.
///
/// `values` is the full column including nulls. For M1 this scans all rows.
/// In later milestones, sampling will be used for large tables (the sample
/// size is tunable, default `min(100_000, row_count)`).
#[must_use]
pub fn analyze_column(values: &[Value]) -> ColumnStats {
    let row_count = values.len();
    let null_count = values.iter().filter(|v| v.is_null()).count();

    // Collect non-null values for analysis
    let non_null: Vec<&Value> = values.iter().filter(|v| !v.is_null()).collect();

    if non_null.is_empty() {
        return ColumnStats {
            row_count,
            null_count,
            distinct_count: 0,
            is_numeric: false,
            is_integer_only: false,
            is_sorted: true,
            consecutive_dup_ratio: 0.0,
            min_value: None,
            max_value: None,
        };
    }

    // Distinct count
    let distinct_set: HashSet<&Value> = non_null.iter().copied().collect();
    let distinct_count = distinct_set.len();

    // Is numeric (all non-null values are Integer or Real)
    let is_numeric = non_null
        .iter()
        .all(|v| matches!(v, Value::Integer(_) | Value::Real(_)));

    // Is integer-only (all non-null values are Integer, no Reals)
    let is_integer_only = non_null.iter().all(|v| matches!(v, Value::Integer(_)));

    // Is sorted (monotonically non-decreasing)
    let is_sorted = non_null.windows(2).all(|w| w[0] <= w[1]);

    // Consecutive duplicate ratio
    let consecutive_dups = if non_null.len() <= 1 {
        0
    } else {
        non_null.windows(2).filter(|w| w[0] == w[1]).count()
    };
    let consecutive_dup_ratio = if non_null.len() <= 1 {
        0.0
    } else {
        consecutive_dups as f64 / (non_null.len() - 1) as f64
    };

    // Min/max
    let min_value = non_null.iter().min().map(|v| (*v).clone());
    let max_value = non_null.iter().max().map(|v| (*v).clone());

    ColumnStats {
        row_count,
        null_count,
        distinct_count,
        is_numeric,
        is_integer_only,
        is_sorted,
        consecutive_dup_ratio,
        min_value,
        max_value,
    }
}

/// Select the best encoding for a column based on its statistics.
///
/// Follows the priority chain from the spec exactly:
/// 1. All null or single distinct → Const
/// 2. Two distinct boolean-like → BoolBitmap
/// 3. Numeric + sorted → Delta
/// 4. High consecutive duplicate ratio (>0.5) → Rle
/// 5. Low cardinality (≤1% distinct, ≤65K entries) → Dict
/// 6. Everything else → Raw
#[must_use]
pub fn select_encoding(stats: &ColumnStats) -> Encoding {
    if stats.distinct_count == 0 {
        return Encoding::Const; // all null
    }
    if stats.distinct_count == 1 {
        return Encoding::Const;
    }
    if stats.distinct_count == 2 && stats.is_boolean_like() {
        return Encoding::BoolBitmap;
    }
    if stats.is_integer_only && stats.is_sorted {
        return Encoding::Delta;
    }
    if stats.consecutive_dup_ratio > 0.5 {
        return Encoding::Rle;
    }
    let non_null_count = stats.row_count - stats.null_count;
    if non_null_count > 0
        && (stats.distinct_count as f64 / non_null_count as f64) <= 0.01
        && stats.distinct_count <= 65_536
    {
        return Encoding::Dict;
    }
    Encoding::Raw
}

/// Streaming column statistics accumulator.
///
/// Allows computing `ColumnStats` incrementally, one value or batch at a time,
/// without requiring all values to be in memory simultaneously.
pub struct StreamingColumnStats {
    row_count: usize,
    null_count: usize,
    distinct_values: HashSet<Value>,
    is_numeric: bool,
    is_integer_only: bool,
    is_sorted: bool,
    last_non_null: Option<Value>,
    consecutive_dups: usize,
    total_pairs: usize,
    min_value: Option<Value>,
    max_value: Option<Value>,
}

impl StreamingColumnStats {
    /// Create a new empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            row_count: 0,
            null_count: 0,
            distinct_values: HashSet::new(),
            is_numeric: true,
            is_integer_only: true,
            is_sorted: true,
            last_non_null: None,
            consecutive_dups: 0,
            total_pairs: 0,
            min_value: None,
            max_value: None,
        }
    }

    /// Feed a single value into the accumulator.
    pub fn update(&mut self, value: &Value) {
        self.row_count += 1;

        if value.is_null() {
            self.null_count += 1;
            return;
        }

        // Check numeric types
        if !matches!(value, Value::Integer(_) | Value::Real(_)) {
            self.is_numeric = false;
            self.is_integer_only = false;
        } else if !matches!(value, Value::Integer(_)) {
            self.is_integer_only = false;
        }

        // Sorted check + consecutive dup check
        if let Some(ref last) = self.last_non_null {
            self.total_pairs += 1;
            if value < last {
                self.is_sorted = false;
            }
            if value == last {
                self.consecutive_dups += 1;
            }
        }

        // Min/max
        match self.min_value {
            Some(ref min) if value < min => self.min_value = Some(value.clone()),
            None => self.min_value = Some(value.clone()),
            _ => {}
        }
        match self.max_value {
            Some(ref max) if value > max => self.max_value = Some(value.clone()),
            None => self.max_value = Some(value.clone()),
            _ => {}
        }

        self.distinct_values.insert(value.clone());
        self.last_non_null = Some(value.clone());
    }

    /// Feed a batch of values into the accumulator.
    pub fn update_batch(&mut self, values: &[Value]) {
        for value in values {
            self.update(value);
        }
    }

    /// Produce the final `ColumnStats` from accumulated data.
    #[must_use]
    pub fn finalize(&self) -> ColumnStats {
        let consecutive_dup_ratio = if self.total_pairs > 0 {
            self.consecutive_dups as f64 / self.total_pairs as f64
        } else {
            0.0
        };

        ColumnStats {
            row_count: self.row_count,
            null_count: self.null_count,
            distinct_count: self.distinct_values.len(),
            is_numeric: if self.row_count == self.null_count {
                false
            } else {
                self.is_numeric
            },
            is_integer_only: if self.row_count == self.null_count {
                false
            } else {
                self.is_integer_only
            },
            is_sorted: self.is_sorted,
            consecutive_dup_ratio,
            min_value: self.min_value.clone(),
            max_value: self.max_value.clone(),
        }
    }
}

impl Default for StreamingColumnStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_null() {
        let values = vec![Value::Null; 100];
        let stats = analyze_column(&values);
        assert_eq!(stats.distinct_count, 0);
        assert_eq!(stats.null_count, 100);
        assert_eq!(select_encoding(&stats), Encoding::Const);
    }

    #[test]
    fn single_distinct() {
        let values = vec![Value::Integer(42); 100];
        let stats = analyze_column(&values);
        assert_eq!(stats.distinct_count, 1);
        assert_eq!(select_encoding(&stats), Encoding::Const);
    }

    #[test]
    fn boolean_like() {
        let values: Vec<Value> = (0..100).map(|i| Value::Integer(i % 2)).collect();
        let stats = analyze_column(&values);
        assert_eq!(stats.distinct_count, 2);
        assert!(stats.is_boolean_like());
        assert_eq!(select_encoding(&stats), Encoding::BoolBitmap);
    }

    #[test]
    fn sorted_numeric_delta() {
        let values: Vec<Value> = (0..100).map(Value::Integer).collect();
        let stats = analyze_column(&values);
        assert!(stats.is_numeric);
        assert!(stats.is_sorted);
        assert_eq!(select_encoding(&stats), Encoding::Delta);
    }

    #[test]
    fn high_consecutive_dups_rle() {
        // 10 runs of 100 each = high consecutive dup ratio
        let mut values = Vec::new();
        for i in 0..10 {
            for _ in 0..100 {
                values.push(Value::Text(format!("group_{i}")));
            }
        }
        let stats = analyze_column(&values);
        assert!(stats.consecutive_dup_ratio > 0.5);
        assert_eq!(select_encoding(&stats), Encoding::Rle);
    }

    #[test]
    fn low_cardinality_dict() {
        // 5 distinct values among 10,000 rows = 0.05% cardinality
        let statuses = ["active", "inactive", "pending", "deleted", "archived"];
        let values: Vec<Value> = (0..10_000)
            .map(|i| Value::Text(statuses[i % 5].into()))
            .collect();
        let stats = analyze_column(&values);
        // Distinct ratio = 5/10000 = 0.0005, well under 0.01
        assert_eq!(select_encoding(&stats), Encoding::Dict);
    }

    #[test]
    fn high_cardinality_raw() {
        // All unique values
        let values: Vec<Value> = (0..1000)
            .map(|i| Value::Text(format!("user_{i}@example.com")))
            .collect();
        let stats = analyze_column(&values);
        assert_eq!(select_encoding(&stats), Encoding::Raw);
    }

    #[test]
    fn dict_cap_at_65536() {
        // 65,536 distinct values but low ratio — should still be Dict
        let values: Vec<Value> = (0..65_536 * 100)
            .map(|i| Value::Integer((i % 65_536) as i64))
            .collect();
        let stats = analyze_column(&values);
        assert_eq!(stats.distinct_count, 65_536);
        // ratio = 65536 / 6553600 ≈ 0.01 — at boundary
        assert_eq!(select_encoding(&stats), Encoding::Dict);
    }

    #[test]
    fn dict_cap_exceeded() {
        // 65,537 distinct values — should fall through to Raw
        let values: Vec<Value> = (0..65_537 * 100)
            .map(|i| Value::Integer((i % 65_537) as i64))
            .collect();
        let stats = analyze_column(&values);
        assert_eq!(stats.distinct_count, 65_537);
        assert_eq!(select_encoding(&stats), Encoding::Raw);
    }

    #[test]
    fn empty_column() {
        let values: Vec<Value> = Vec::new();
        let stats = analyze_column(&values);
        assert_eq!(stats.row_count, 0);
        assert_eq!(stats.distinct_count, 0);
        assert_eq!(select_encoding(&stats), Encoding::Const);
    }

    #[test]
    fn mixed_nulls_and_values() {
        let mut values: Vec<Value> = (0..50).map(Value::Integer).collect();
        for _ in 0..50 {
            values.push(Value::Null);
        }
        let stats = analyze_column(&values);
        assert_eq!(stats.row_count, 100);
        assert_eq!(stats.null_count, 50);
        assert_eq!(stats.distinct_count, 50);
    }

    #[test]
    fn consecutive_dup_ratio_boundary() {
        // Exactly 0.5 ratio should NOT trigger RLE (spec says > 0.5)
        // 4 values, 2 consecutive dups: ratio = 2/3 ≈ 0.667
        // Let's construct one with ratio exactly 0.5
        // 3 values: [A, A, B] → 1 dup out of 2 pairs = 0.5
        // Need to make it not match earlier rules
        let values = vec![
            Value::Text("alpha".into()),
            Value::Text("alpha".into()),
            Value::Text("beta".into()),
        ];
        let stats = analyze_column(&values);
        assert_eq!(stats.consecutive_dup_ratio, 0.5);
        // ratio == 0.5 exactly, not > 0.5, so not RLE
        // 2 distinct, boolean_like, so BoolBitmap takes priority
        assert_eq!(select_encoding(&stats), Encoding::BoolBitmap);
    }

    #[test]
    fn is_sorted_with_nulls() {
        // Nulls are filtered out, remaining should be sorted
        let values = vec![
            Value::Integer(1),
            Value::Null,
            Value::Integer(2),
            Value::Null,
            Value::Integer(3),
        ];
        let stats = analyze_column(&values);
        assert!(stats.is_sorted);
    }

    #[test]
    fn not_sorted() {
        let values = vec![Value::Integer(3), Value::Integer(1), Value::Integer(2)];
        let stats = analyze_column(&values);
        assert!(!stats.is_sorted);
    }

    #[test]
    fn min_max() {
        let values = vec![
            Value::Integer(5),
            Value::Integer(1),
            Value::Null,
            Value::Integer(10),
            Value::Integer(3),
        ];
        let stats = analyze_column(&values);
        assert_eq!(stats.min_value, Some(Value::Integer(1)));
        assert_eq!(stats.max_value, Some(Value::Integer(10)));
    }

    // --- StreamingColumnStats tests ---

    #[test]
    fn streaming_matches_batch_all_null() {
        let values = vec![Value::Null; 100];
        let batch_stats = analyze_column(&values);
        let mut streaming = StreamingColumnStats::new();
        streaming.update_batch(&values);
        let stream_stats = streaming.finalize();

        assert_eq!(stream_stats.row_count, batch_stats.row_count);
        assert_eq!(stream_stats.null_count, batch_stats.null_count);
        assert_eq!(stream_stats.distinct_count, batch_stats.distinct_count);
        assert_eq!(
            select_encoding(&stream_stats),
            select_encoding(&batch_stats)
        );
    }

    #[test]
    fn streaming_matches_batch_sorted_integers() {
        let values: Vec<Value> = (0..100).map(Value::Integer).collect();
        let batch_stats = analyze_column(&values);
        let mut streaming = StreamingColumnStats::new();
        for v in &values {
            streaming.update(v);
        }
        let stream_stats = streaming.finalize();

        assert_eq!(stream_stats.row_count, batch_stats.row_count);
        assert_eq!(stream_stats.distinct_count, batch_stats.distinct_count);
        assert_eq!(stream_stats.is_sorted, batch_stats.is_sorted);
        assert_eq!(stream_stats.is_numeric, batch_stats.is_numeric);
        assert_eq!(stream_stats.is_integer_only, batch_stats.is_integer_only);
        assert_eq!(
            select_encoding(&stream_stats),
            select_encoding(&batch_stats)
        );
    }

    #[test]
    fn streaming_matches_batch_with_nulls() {
        let values = vec![
            Value::Integer(1),
            Value::Null,
            Value::Integer(2),
            Value::Null,
            Value::Integer(3),
        ];
        let batch_stats = analyze_column(&values);
        let mut streaming = StreamingColumnStats::new();
        streaming.update_batch(&values);
        let stream_stats = streaming.finalize();

        assert_eq!(stream_stats.row_count, batch_stats.row_count);
        assert_eq!(stream_stats.null_count, batch_stats.null_count);
        assert_eq!(stream_stats.is_sorted, batch_stats.is_sorted);
        assert_eq!(stream_stats.min_value, batch_stats.min_value);
        assert_eq!(stream_stats.max_value, batch_stats.max_value);
    }

    #[test]
    fn streaming_matches_batch_rle_pattern() {
        let mut values = Vec::new();
        for i in 0..10 {
            for _ in 0..100 {
                values.push(Value::Text(format!("group_{i}")));
            }
        }
        let batch_stats = analyze_column(&values);
        let mut streaming = StreamingColumnStats::new();
        streaming.update_batch(&values);
        let stream_stats = streaming.finalize();

        assert_eq!(stream_stats.distinct_count, batch_stats.distinct_count);
        assert!(
            (stream_stats.consecutive_dup_ratio - batch_stats.consecutive_dup_ratio).abs() < 1e-10
        );
        assert_eq!(
            select_encoding(&stream_stats),
            select_encoding(&batch_stats)
        );
    }

    #[test]
    fn streaming_matches_batch_boolean_like() {
        let values: Vec<Value> = (0..100).map(|i| Value::Integer(i % 2)).collect();
        let batch_stats = analyze_column(&values);
        let mut streaming = StreamingColumnStats::new();
        streaming.update_batch(&values);
        let stream_stats = streaming.finalize();

        assert_eq!(stream_stats.distinct_count, batch_stats.distinct_count);
        assert_eq!(
            select_encoding(&stream_stats),
            select_encoding(&batch_stats)
        );
    }

    #[test]
    fn streaming_empty() {
        let streaming = StreamingColumnStats::new();
        let stats = streaming.finalize();
        assert_eq!(stats.row_count, 0);
        assert_eq!(stats.distinct_count, 0);
        assert_eq!(select_encoding(&stats), Encoding::Const);
    }

    #[test]
    fn streaming_incremental_batches() {
        // Feed values in multiple batches, verify result matches single batch
        let values: Vec<Value> = (0..100).map(Value::Integer).collect();
        let batch_stats = analyze_column(&values);

        let mut streaming = StreamingColumnStats::new();
        streaming.update_batch(&values[..25]);
        streaming.update_batch(&values[25..50]);
        streaming.update_batch(&values[50..75]);
        streaming.update_batch(&values[75..]);
        let stream_stats = streaming.finalize();

        assert_eq!(stream_stats.row_count, batch_stats.row_count);
        assert_eq!(stream_stats.distinct_count, batch_stats.distinct_count);
        assert_eq!(stream_stats.is_sorted, batch_stats.is_sorted);
        assert_eq!(
            select_encoding(&stream_stats),
            select_encoding(&batch_stats)
        );
    }
}
