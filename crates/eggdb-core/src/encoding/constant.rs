use crate::encoding::{decode_err, ColumnEncoder};
use crate::format::FormatError;
use crate::value::Value;

/// Const encoder: entire column is one repeated value.
///
/// Stores a single `Value`. Decode repeats it `non_null_count` times.
/// Filter is a single comparison — return all or nothing.
pub struct ConstEncoder;

impl ColumnEncoder for ConstEncoder {
    fn encode(&self, values: &[Value]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }
        // All values must be the same; store just the first
        bincode::serialize(&values[0]).expect("bincode serialize of Value cannot fail")
    }

    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError> {
        if non_null_count == 0 || data.is_empty() {
            return Ok(Vec::new());
        }
        let value: Value =
            bincode::deserialize(data).map_err(|e| decode_err("Const", e.to_string()))?;
        Ok(vec![value; non_null_count])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let enc = ConstEncoder;
        let encoded = enc.encode(&[]);
        let decoded = enc.decode(&encoded, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_integer() {
        let enc = ConstEncoder;
        let values = vec![Value::Integer(42)];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn repeated_text() {
        let enc = ConstEncoder;
        let values = vec![Value::Text("hello".into()); 1000];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1000).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn repeated_real() {
        let enc = ConstEncoder;
        let values = vec![Value::Real(3.125); 500];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 500).unwrap();
        assert_eq!(decoded, values);
    }
}
