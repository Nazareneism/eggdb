use crate::encoding::{decode_err, ColumnEncoder};
use crate::format::FormatError;
use crate::value::Value;

/// BoolBitmap encoder: bitpacks boolean or 2-value columns.
///
/// For 2-value columns, the two distinct values are stored in a length-prefixed
/// header (sorted order). Each value maps to bit 0 or 1. For single-distinct-value
/// inputs, all bits are 0.
///
/// Wire format:
/// ```text
/// [header_len: u32 LE] [header: bincode Vec<Value>] [bitmap: ceil(n/8) bytes]
/// ```
pub struct BoolBitmapEncoder;

impl ColumnEncoder for BoolBitmapEncoder {
    fn encode(&self, values: &[Value]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        // Collect up to 2 distinct values (sorted)
        let mut distinct: Vec<Value> = Vec::with_capacity(2);
        for v in values {
            if !distinct.contains(v) {
                distinct.push(v.clone());
                if distinct.len() > 2 {
                    break;
                }
            }
        }
        distinct.sort();

        // Length-prefixed header
        let header_bytes = bincode::serialize(&distinct).expect("bincode serialize failed");
        let mut result = Vec::new();
        result.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        result.extend_from_slice(&header_bytes);

        // Bitpack: value == distinct[1] → bit 1, else bit 0
        let byte_count = values.len().div_ceil(8);
        let mut bitmap = vec![0u8; byte_count];
        for (i, v) in values.iter().enumerate() {
            if distinct.len() == 2 && v == &distinct[1] {
                bitmap[i / 8] |= 1 << (i % 8);
            }
        }
        result.extend_from_slice(&bitmap);
        result
    }

    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError> {
        if non_null_count == 0 || data.is_empty() {
            return Ok(Vec::new());
        }

        if data.len() < 4 {
            return Err(decode_err("BoolBitmap", "data too short for header length"));
        }
        let header_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if 4 + header_len > data.len() {
            return Err(decode_err(
                "BoolBitmap",
                format!(
                    "header length {} exceeds data size {}",
                    header_len,
                    data.len() - 4
                ),
            ));
        }
        let distinct: Vec<Value> = bincode::deserialize(&data[4..4 + header_len])
            .map_err(|e| decode_err("BoolBitmap", e.to_string()))?;
        if distinct.is_empty() || distinct.len() > 2 {
            return Err(decode_err(
                "BoolBitmap",
                format!("expected 1 or 2 distinct values, got {}", distinct.len()),
            ));
        }
        let bitmap_data = &data[4 + header_len..];

        let mut result = Vec::with_capacity(non_null_count);
        for i in 0..non_null_count {
            let bit = if i / 8 < bitmap_data.len() {
                (bitmap_data[i / 8] >> (i % 8)) & 1
            } else {
                0
            };
            if bit == 1 && distinct.len() == 2 {
                result.push(distinct[1].clone());
            } else {
                result.push(distinct[0].clone());
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let enc = BoolBitmapEncoder;
        let encoded = enc.encode(&[]);
        let decoded = enc.decode(&encoded, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_value() {
        let enc = BoolBitmapEncoder;
        let values = vec![Value::Integer(1)];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn boolean_values() {
        let enc = BoolBitmapEncoder;
        let values = vec![
            Value::Integer(0),
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(0),
            Value::Integer(1),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 5).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn text_two_values() {
        let enc = BoolBitmapEncoder;
        let values = vec![
            Value::Text("yes".into()),
            Value::Text("no".into()),
            Value::Text("yes".into()),
            Value::Text("yes".into()),
            Value::Text("no".into()),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 5).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn all_same() {
        let enc = BoolBitmapEncoder;
        let values = vec![Value::Integer(42); 100];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 100).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn boundary_8_values() {
        let enc = BoolBitmapEncoder;
        let values: Vec<Value> = (0..8).map(|i| Value::Integer(i % 2)).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 8).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn boundary_9_values() {
        let enc = BoolBitmapEncoder;
        let values: Vec<Value> = (0..9).map(|i| Value::Integer(i % 2)).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 9).unwrap();
        assert_eq!(decoded, values);
    }
}
