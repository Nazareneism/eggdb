use crate::encoding::{decode_err, ColumnEncoder};
use crate::format::FormatError;
use crate::value::Value;

/// RLE (Run-Length Encoding) encoder: compresses consecutive duplicates.
///
/// Stores `(value, run_length)` pairs. Best for sorted columns or columns
/// with long runs of the same value.
///
/// Wire format:
/// ```text
/// [num_runs: u64 LE] [run0_value: bincode] [run0_length: u64 LE] ...
/// ```
pub struct RleEncoder;

impl ColumnEncoder for RleEncoder {
    fn encode(&self, values: &[Value]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        let runs = build_runs(values);
        let mut result = Vec::new();

        // Number of runs
        result.extend_from_slice(&(runs.len() as u64).to_le_bytes());

        for (value, count) in &runs {
            let value_bytes = bincode::serialize(value).expect("bincode serialize failed");
            result.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
            result.extend_from_slice(&value_bytes);
            result.extend_from_slice(&count.to_le_bytes());
        }

        result
    }

    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError> {
        if non_null_count == 0 || data.is_empty() {
            return Ok(Vec::new());
        }

        let mut pos = 0;
        if pos + 8 > data.len() {
            return Err(decode_err("Rle", "data too short for num_runs"));
        }
        let num_runs = u64::from_le_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]) as usize;
        pos += 8;

        // Cap num_runs to prevent excessive iteration on malformed data
        let num_runs = num_runs.min(non_null_count);

        let mut result = Vec::with_capacity(non_null_count);
        let mut total_decoded = 0usize;

        for _ in 0..num_runs {
            if pos + 4 > data.len() {
                return Err(decode_err(
                    "Rle",
                    "unexpected end of data reading value length",
                ));
            }
            let value_len =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            pos += 4;

            if pos + value_len > data.len() {
                return Err(decode_err(
                    "Rle",
                    format!("value length {} exceeds remaining data", value_len),
                ));
            }
            let value: Value = bincode::deserialize(&data[pos..pos + value_len])
                .map_err(|e| decode_err("Rle", e.to_string()))?;
            pos += value_len;

            if pos + 8 > data.len() {
                return Err(decode_err(
                    "Rle",
                    "unexpected end of data reading run count",
                ));
            }
            let count = u64::from_le_bytes([
                data[pos],
                data[pos + 1],
                data[pos + 2],
                data[pos + 3],
                data[pos + 4],
                data[pos + 5],
                data[pos + 6],
                data[pos + 7],
            ]) as usize;
            pos += 8;

            // Cap total decoded values at non_null_count
            let remaining = non_null_count.saturating_sub(total_decoded);
            let actual_count = count.min(remaining);
            for _ in 0..actual_count {
                result.push(value.clone());
            }
            total_decoded += actual_count;
            if total_decoded >= non_null_count {
                break;
            }
        }

        Ok(result)
    }
}

/// Build run-length pairs from a slice of values.
fn build_runs(values: &[Value]) -> Vec<(Value, u64)> {
    let mut runs = Vec::new();
    if values.is_empty() {
        return runs;
    }

    let mut current = values[0].clone();
    let mut count: u64 = 1;

    for v in &values[1..] {
        if v == &current {
            count += 1;
        } else {
            runs.push((current, count));
            current = v.clone();
            count = 1;
        }
    }
    runs.push((current, count));
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let enc = RleEncoder;
        let encoded = enc.encode(&[]);
        let decoded = enc.decode(&encoded, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_value() {
        let enc = RleEncoder;
        let values = vec![Value::Integer(42)];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn all_same() {
        let enc = RleEncoder;
        let values = vec![Value::Text("hello".into()); 10_000];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 10_000).unwrap();
        assert_eq!(decoded, values);
        // Should be very compact: one run
        assert!(encoded.len() < 100);
    }

    #[test]
    fn alternating() {
        let enc = RleEncoder;
        let values: Vec<Value> = (0..100).map(|i| Value::Integer(i % 2)).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 100).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn sorted_with_runs() {
        let enc = RleEncoder;
        let mut values = Vec::new();
        for i in 0..10 {
            for _ in 0..100 {
                values.push(Value::Integer(i));
            }
        }
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1000).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn mixed_types_in_runs() {
        let enc = RleEncoder;
        let values = vec![
            Value::Text("a".into()),
            Value::Text("a".into()),
            Value::Text("b".into()),
            Value::Text("b".into()),
            Value::Text("b".into()),
            Value::Integer(1),
            Value::Integer(1),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 7).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn no_consecutive_duplicates() {
        let enc = RleEncoder;
        let values: Vec<Value> = (0..50).map(Value::Integer).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 50).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn build_runs_correctness() {
        let values = vec![
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(2),
            Value::Integer(2),
            Value::Integer(1),
        ];
        let runs = build_runs(&values);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0], (Value::Integer(1), 2));
        assert_eq!(runs[1], (Value::Integer(2), 3));
        assert_eq!(runs[2], (Value::Integer(1), 1));
    }
}
