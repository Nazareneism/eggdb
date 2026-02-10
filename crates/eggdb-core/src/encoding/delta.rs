use crate::encoding::{decode_err, ColumnEncoder};
use crate::format::FormatError;
use crate::value::Value;

/// Delta encoder: stores base value + zigzag-varint-encoded deltas.
///
/// Best for monotonic numeric columns (e.g., auto-increment IDs, timestamps).
/// Decode reconstructs values via prefix sum.
///
/// Wire format:
/// ```text
/// [base: i64 LE] [delta0: zigzag varint] [delta1: zigzag varint] ...
/// ```
///
/// Only works on Integer values. Real/Text/Blob values will be encoded as
/// raw i64 bit patterns (Real) or panic in debug builds (Text/Blob).
pub struct DeltaEncoder;

impl ColumnEncoder for DeltaEncoder {
    fn encode(&self, values: &[Value]) -> Vec<u8> {
        if values.is_empty() {
            return Vec::new();
        }

        let ints: Vec<i64> = values.iter().map(value_to_i64).collect();

        let mut result = Vec::new();
        // Store base value as i64 LE
        result.extend_from_slice(&ints[0].to_le_bytes());

        // Store deltas as zigzag varints
        let mut prev = ints[0];
        for &val in &ints[1..] {
            let delta = val.wrapping_sub(prev);
            encode_zigzag_varint(delta, &mut result);
            prev = val;
        }

        result
    }

    fn decode(&self, data: &[u8], non_null_count: usize) -> Result<Vec<Value>, FormatError> {
        if non_null_count == 0 || data.is_empty() {
            return Ok(Vec::new());
        }

        if data.len() < 8 {
            return Err(decode_err("Delta", "data too short for base value"));
        }
        let base = i64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);

        let mut result = Vec::with_capacity(non_null_count);
        result.push(Value::Integer(base));

        let mut pos = 8;
        let mut current = base;
        for _ in 1..non_null_count {
            let (delta, bytes_read) = decode_zigzag_varint_checked(&data[pos..])?;
            pos += bytes_read;
            current = current.wrapping_add(delta);
            result.push(Value::Integer(current));
        }

        Ok(result)
    }
}

fn value_to_i64(v: &Value) -> i64 {
    match v {
        Value::Integer(i) => *i,
        Value::Real(f) => f.to_bits() as i64,
        _ => panic!("Delta encoder only supports Integer and Real values"),
    }
}

/// Zigzag encode: maps signed integers to unsigned (0→0, -1→1, 1→2, -2→3, ...)
fn zigzag_encode(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

/// Zigzag decode: maps unsigned back to signed
fn zigzag_decode(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

/// Write a zigzag-encoded varint to the output buffer.
fn encode_zigzag_varint(value: i64, out: &mut Vec<u8>) {
    let mut n = zigzag_encode(value);
    loop {
        if n < 0x80 {
            out.push(n as u8);
            break;
        }
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
}

/// Read a zigzag-encoded varint from the input. Returns (value, bytes_consumed).
#[cfg(test)]
fn decode_zigzag_varint(data: &[u8]) -> (i64, usize) {
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
    (zigzag_decode(n), pos)
}

/// Checked version of `decode_zigzag_varint` that returns `Result`.
fn decode_zigzag_varint_checked(data: &[u8]) -> Result<(i64, usize), FormatError> {
    let mut n: u64 = 0;
    let mut shift: u32 = 0;
    let mut pos = 0;
    loop {
        if pos >= data.len() {
            return Err(decode_err("Delta", "unexpected end of data in varint"));
        }
        if shift >= 64 {
            return Err(decode_err("Delta", "varint too long (shift overflow)"));
        }
        let byte = data[pos];
        n |= ((byte & 0x7F) as u64) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok((zigzag_decode(n), pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let enc = DeltaEncoder;
        let encoded = enc.encode(&[]);
        let decoded = enc.decode(&encoded, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_value() {
        let enc = DeltaEncoder;
        let values = vec![Value::Integer(42)];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn monotonic_increasing() {
        let enc = DeltaEncoder;
        let values: Vec<Value> = (0..100).map(Value::Integer).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 100).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn monotonic_decreasing() {
        let enc = DeltaEncoder;
        let values: Vec<Value> = (0..50).rev().map(Value::Integer).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 50).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn negative_values() {
        let enc = DeltaEncoder;
        let values: Vec<Value> = (-50..50).map(Value::Integer).collect();
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 100).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn large_deltas() {
        let enc = DeltaEncoder;
        let values = vec![
            Value::Integer(0),
            Value::Integer(i64::MAX),
            Value::Integer(0),
            Value::Integer(i64::MIN),
        ];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 4).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn all_same() {
        let enc = DeltaEncoder;
        let values = vec![Value::Integer(7); 1000];
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, 1000).unwrap();
        assert_eq!(decoded, values);
        // All-same should compress well: base(8) + 999 zero deltas (1 byte each)
        assert!(encoded.len() < 1100);
    }

    #[test]
    fn zigzag_round_trip() {
        for v in [0i64, 1, -1, 2, -2, i64::MAX, i64::MIN, 127, -128, 255, -256] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v, "failed for {v}");
        }
    }

    #[test]
    fn varint_round_trip() {
        for v in [0i64, 1, -1, 127, -128, 10000, -10000, i64::MAX, i64::MIN] {
            let mut buf = Vec::new();
            encode_zigzag_varint(v, &mut buf);
            let (decoded, len) = decode_zigzag_varint(&buf);
            assert_eq!(decoded, v, "varint round-trip failed for {v}");
            assert_eq!(len, buf.len());
        }
    }
}
