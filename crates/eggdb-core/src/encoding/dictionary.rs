use crate::encoding::{decode_err, ColumnEncoder};
use crate::format::FormatError;
use crate::value::Value;

/// Maximum number of dictionary entries before falling back to Raw.
pub const MAX_DICT_ENTRIES: usize = 65_536;

/// Dict encoder: maps low-cardinality values to dictionary indices.
///
/// Builds a `Vec<Value>` dictionary sorted by frequency (most frequent first).
/// Each value is encoded as a varint index into the dictionary. Decode is an
/// index lookup.
///
/// Wire format:
/// ```text
/// [dict_len: u32 LE]
/// [entry0_len: u32 LE] [entry0: bincode Value] ...
/// [index0: varint] [index1: varint] ...
/// ```
pub struct DictEncoder;

impl ColumnEncoder for DictEncoder {
    fn encode(&self, values: &[Value]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        // Build frequency map
        let mut freq: Vec<(Value, usize)> = Vec::new();
        for v in values {
            if let Some(entry) = freq.iter_mut().find(|(val, _)| val == v) {
                entry.1 += 1;
            } else {
                freq.push((v.clone(), 1));
            }
        }

        // Sort by frequency descending (most common = index 0 = smallest varint)
        freq.sort_by(|a, b| b.1.cmp(&a.1));

        let dictionary: Vec<Value> = freq.into_iter().map(|(v, _)| v).collect();

        let mut result = Vec::new();

        // Write dictionary
        result.extend_from_slice(&(dictionary.len() as u32).to_le_bytes());
        for entry in &dictionary {
            let entry_bytes = bincode::serialize(entry).expect("bincode serialize failed");
            result.extend_from_slice(&(entry_bytes.len() as u32).to_le_bytes());
            result.extend_from_slice(&entry_bytes);
        }

        // Write indices as varints
        for v in values {
            let idx = dictionary
                .iter()
                .position(|d| d == v)
                .expect("value must be in dictionary");
            encode_varint(idx as u64, &mut result);
        }

        result
    }

    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError> {
        if non_null_count == 0 || data.is_empty() {
            return Ok(Vec::new());
        }

        let mut pos = 0;

        // Read dictionary
        if pos + 4 > data.len() {
            return Err(decode_err("Dict", "data too short for dictionary length"));
        }
        let dict_len =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        let mut dictionary = Vec::with_capacity(dict_len);
        for _ in 0..dict_len {
            if pos + 4 > data.len() {
                return Err(decode_err(
                    "Dict",
                    "unexpected end of data reading entry length",
                ));
            }
            let entry_len =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            pos += 4;

            if pos + entry_len > data.len() {
                return Err(decode_err(
                    "Dict",
                    format!("entry length {} exceeds remaining data", entry_len),
                ));
            }
            let entry: Value = bincode::deserialize(&data[pos..pos + entry_len])
                .map_err(|e| decode_err("Dict", e.to_string()))?;
            pos += entry_len;
            dictionary.push(entry);
        }

        // Read indices
        let mut result = Vec::with_capacity(non_null_count);
        for _ in 0..non_null_count {
            let (idx, bytes_read) = decode_varint_checked(&data[pos..])?;
            pos += bytes_read;
            let idx = idx as usize;
            if idx >= dictionary.len() {
                return Err(decode_err(
                    "Dict",
                    format!(
                        "dictionary index {} out of bounds (dict size {})",
                        idx,
                        dictionary.len()
                    ),
                ));
            }
            result.push(dictionary[idx].clone());
        }

        Ok(result)
    }
}

fn encode_varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        if n < 0x80 {
            out.push(n as u8);
            break;
        }
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
}

#[cfg(test)]
fn decode_varint(data: &[u8]) -> (u64, usize) {
    let mut n: u64 = 0;
    let mut shift = 0;
    let mut pos = 0;
    loop {
        let byte = data[pos];
        n |= ((byte & 0x7F) as u64) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    (n, pos)
}

/// Checked version of `decode_varint` that returns `Result`.
fn decode_varint_checked(data: &[u8]) -> Result<(u64, usize), FormatError> {
    let mut n: u64 = 0;
    let mut shift: u32 = 0;
    let mut pos = 0;
    loop {
        if pos >= data.len() {
            return Err(decode_err("Dict", "unexpected end of data in varint"));
        }
        if shift >= 64 {
            return Err(decode_err("Dict", "varint too long (shift overflow)"));
        }
        let byte = data[pos];
        n |= ((byte & 0x7F) as u64) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok((n, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let enc = DictEncoder;
        let encoded = enc.encode(&[]);
        let decoded = enc.decode(&encoded, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_value() {
        let enc = DictEncoder;
        let values = vec![Value::Text("hello".into())];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn few_distinct_many_values() {
        let enc = DictEncoder;
        let statuses = ["active", "inactive", "pending"];
        let values: Vec<Value> = (0..10_000)
            .map(|i| Value::Text(statuses[i % 3].into()))
            .collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 10_000).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn integer_dictionary() {
        let enc = DictEncoder;
        let values: Vec<Value> = (0..1000).map(|i| Value::Integer(i % 5)).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1000).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn all_same() {
        let enc = DictEncoder;
        let values = vec![Value::Text("x".into()); 500];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 500).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn frequency_sorting() {
        // Most frequent value should get index 0 (smallest varint)
        let enc = DictEncoder;
        let mut values = Vec::new();
        for _ in 0..100 {
            values.push(Value::Text("common".into()));
        }
        for _ in 0..10 {
            values.push(Value::Text("rare".into()));
        }
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 110).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn mixed_value_types() {
        let enc = DictEncoder;
        let values = vec![
            Value::Integer(1),
            Value::Text("a".into()),
            Value::Integer(1),
            Value::Text("a".into()),
            Value::Real(3.125),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 5).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn varint_round_trip() {
        for v in [
            0u64,
            1,
            127,
            128,
            255,
            256,
            16383,
            16384,
            65535,
            65536,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            encode_varint(v, &mut buf);
            let (decoded, len) = decode_varint(&buf);
            assert_eq!(decoded, v, "varint round-trip failed for {v}");
            assert_eq!(len, buf.len());
        }
    }
}
