use crate::encoding::{decode_err, ColumnEncoder};
use crate::format::FormatError;
use crate::value::Value;

/// Raw encoder: stores values sequentially with length-prefixed encoding.
///
/// No pre-compression — relies entirely on zstd at the column block level.
/// Used for high-cardinality text (emails, names, UUIDs) and blob columns.
///
/// Wire format:
/// ```text
/// [value0_len: u32 LE] [value0: bincode Value]
/// [value1_len: u32 LE] [value1: bincode Value]
/// ...
/// ```
pub struct RawEncoder;

impl ColumnEncoder for RawEncoder {
    fn encode(&self, values: &[Value]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::new();
        for v in values {
            let value_bytes = bincode::serialize(v).expect("bincode serialize failed");
            result.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
            result.extend_from_slice(&value_bytes);
        }
        result
    }

    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError> {
        if non_null_count == 0 || data.is_empty() {
            return Ok(Vec::new());
        }

        let mut pos = 0;
        let mut result = Vec::with_capacity(non_null_count);

        for _ in 0..non_null_count {
            if pos + 4 > data.len() {
                return Err(decode_err(
                    "Raw",
                    "unexpected end of data reading value length",
                ));
            }
            let value_len =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            pos += 4;

            if pos + value_len > data.len() {
                return Err(decode_err(
                    "Raw",
                    format!("value length {} exceeds remaining data", value_len),
                ));
            }
            let value: Value = bincode::deserialize(&data[pos..pos + value_len])
                .map_err(|e| decode_err("Raw", e.to_string()))?;
            pos += value_len;
            result.push(value);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let enc = RawEncoder;
        let encoded = enc.encode(&[]);
        let decoded = enc.decode(&encoded, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_value() {
        let enc = RawEncoder;
        let values = vec![Value::Text("hello".into())];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn mixed_types() {
        let enc = RawEncoder;
        let values = vec![
            Value::Integer(42),
            Value::Real(3.125),
            Value::Text("hello world".into()),
            Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            Value::Integer(-1),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 5).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn many_strings() {
        let enc = RawEncoder;
        let values: Vec<Value> = (0..1000)
            .map(|i| Value::Text(format!("user_{i}@example.com")))
            .collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1000).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn empty_strings() {
        let enc = RawEncoder;
        let values = vec![
            Value::Text(String::new()),
            Value::Text(String::new()),
            Value::Text("notempty".into()),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 3).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn blobs() {
        let enc = RawEncoder;
        let values = vec![
            Value::Blob(vec![]),
            Value::Blob(vec![1, 2, 3]),
            Value::Blob(vec![0; 1000]),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 3).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn all_same() {
        let enc = RawEncoder;
        let values = vec![Value::Integer(7); 500];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 500).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn unicode_text() {
        let enc = RawEncoder;
        let values = vec![
            Value::Text("Hello, 世界!".into()),
            Value::Text("こんにちは".into()),
            Value::Text("🥚🐣".into()),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 3).unwrap();
        assert_eq!(decoded, values);
    }
}
