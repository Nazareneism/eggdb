use bitvec::prelude::*;

use crate::format::FormatError;

/// Encode a null bitmap to bytes.
///
/// Polarity: **1 = null, 0 = present**.
/// Packs bits into bytes, LSB first within each byte.
#[must_use]
pub fn encode(nulls: &BitVec) -> Vec<u8> {
    if nulls.is_empty() {
        return Vec::new();
    }
    // bitvec stores bits in a way we can extract the raw bytes
    // We pack manually to guarantee our exact wire format
    let byte_count = nulls.len().div_ceil(8);
    let mut bytes = vec![0u8; byte_count];
    for (i, bit) in nulls.iter().enumerate() {
        if *bit {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    bytes
}

/// Decode bytes back to a null bitmap.
///
/// `row_count` specifies the expected number of bits.
/// Polarity: **1 = null, 0 = present**.
#[must_use]
pub fn decode(data: &[u8], row_count: usize) -> BitVec {
    let mut result = bitvec![0; row_count];
    for i in 0..row_count {
        if i / 8 < data.len() && (data[i / 8] >> (i % 8)) & 1 == 1 {
            result.set(i, true);
        }
    }
    result
}

/// Count the number of non-null (present) values in a null bitmap.
#[must_use]
pub fn non_null_count(nulls: &BitVec) -> usize {
    nulls.iter().filter(|b| !**b).count()
}

/// Interleave non-null values back into a full column using the null bitmap.
///
/// Inverse of `extract_non_null`. Given non-null values and the bitmap,
/// reconstructs the full column with `Value::Null` at null positions.
///
/// Returns an error if `non_null_values.len()` does not equal the number of
/// 0-bits in `null_bitmap`.
pub fn interleave_nulls(
    non_null_values: &[crate::value::Value],
    null_bitmap: &BitVec,
) -> Result<Vec<crate::value::Value>, FormatError> {
    let expected_non_null = non_null_count(null_bitmap);
    if non_null_values.len() != expected_non_null {
        return Err(FormatError::ColumnData(format!(
            "non_null_values length ({}) must equal non-null count in bitmap ({})",
            non_null_values.len(),
            expected_non_null,
        )));
    }

    let mut result = Vec::with_capacity(null_bitmap.len());
    let mut value_idx = 0;
    for is_null in null_bitmap.iter() {
        if *is_null {
            result.push(crate::value::Value::Null);
        } else {
            result.push(non_null_values[value_idx].clone());
            value_idx += 1;
        }
    }
    Ok(result)
}

/// Extract non-null values from a full column, given the null bitmap.
///
/// Returns only values where the corresponding bitmap bit is 0 (present).
pub fn extract_non_null<T: Clone>(values: &[T], null_bitmap: &BitVec) -> Vec<T> {
    assert_eq!(
        values.len(),
        null_bitmap.len(),
        "values and null_bitmap must have the same length"
    );
    values
        .iter()
        .zip(null_bitmap.iter())
        .filter(|(_, is_null)| !**is_null)
        .map(|(v, _)| v.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let bv = bitvec![];
        let encoded = encode(&bv);
        assert!(encoded.is_empty());
        let decoded = decode(&encoded, 0);
        assert_eq!(decoded, bv);
    }

    #[test]
    fn round_trip_all_present() {
        let bv = bitvec![0, 0, 0, 0, 0];
        let encoded = encode(&bv);
        let decoded = decode(&encoded, 5);
        assert_eq!(decoded, bv);
    }

    #[test]
    fn round_trip_all_null() {
        let bv = bitvec![1, 1, 1, 1];
        let encoded = encode(&bv);
        let decoded = decode(&encoded, 4);
        assert_eq!(decoded, bv);
    }

    #[test]
    fn round_trip_mixed() {
        let bv = bitvec![0, 1, 0, 0, 1, 1, 0, 0, 1];
        let encoded = encode(&bv);
        let decoded = decode(&encoded, 9);
        assert_eq!(decoded, bv);
    }

    #[test]
    fn non_null_count_test() {
        let bv = bitvec![0, 1, 0, 0, 1, 1, 0];
        assert_eq!(non_null_count(&bv), 4);
    }

    #[test]
    fn extract_non_null_test() {
        let values = vec![10, 20, 30, 40, 50];
        let bitmap = bitvec![0, 1, 0, 1, 0];
        let result = extract_non_null(&values, &bitmap);
        assert_eq!(result, vec![10, 30, 50]);
    }

    #[test]
    fn byte_boundary_exact() {
        // Exactly 8 bits
        let bv = bitvec![1, 0, 1, 0, 1, 0, 1, 0];
        let encoded = encode(&bv);
        assert_eq!(encoded.len(), 1);
        let decoded = decode(&encoded, 8);
        assert_eq!(decoded, bv);
    }

    #[test]
    fn byte_boundary_plus_one() {
        // 9 bits = 2 bytes
        let bv = bitvec![1, 0, 1, 0, 1, 0, 1, 0, 1];
        let encoded = encode(&bv);
        assert_eq!(encoded.len(), 2);
        let decoded = decode(&encoded, 9);
        assert_eq!(decoded, bv);
    }

    #[test]
    fn interleave_nulls_empty() {
        let bitmap = bitvec![];
        let result = interleave_nulls(&[], &bitmap).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn interleave_nulls_no_nulls() {
        use crate::value::Value;
        let bitmap = bitvec![0, 0, 0];
        let values = vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)];
        let result = interleave_nulls(&values, &bitmap).unwrap();
        assert_eq!(result, values);
    }

    #[test]
    fn interleave_nulls_all_null() {
        use crate::value::Value;
        let bitmap = bitvec![1, 1, 1];
        let result = interleave_nulls(&[], &bitmap).unwrap();
        assert_eq!(result, vec![Value::Null, Value::Null, Value::Null]);
    }

    #[test]
    fn interleave_nulls_mixed() {
        use crate::value::Value;
        // Original: [10, NULL, 20, 30, NULL, 40]
        let bitmap = bitvec![0, 1, 0, 0, 1, 0];
        let non_null = vec![
            Value::Integer(10),
            Value::Integer(20),
            Value::Integer(30),
            Value::Integer(40),
        ];
        let result = interleave_nulls(&non_null, &bitmap).unwrap();
        assert_eq!(
            result,
            vec![
                Value::Integer(10),
                Value::Null,
                Value::Integer(20),
                Value::Integer(30),
                Value::Null,
                Value::Integer(40),
            ]
        );
    }

    #[test]
    fn interleave_is_inverse_of_extract() {
        use crate::value::Value;
        let original = vec![
            Value::Text("a".into()),
            Value::Null,
            Value::Text("b".into()),
            Value::Null,
            Value::Null,
            Value::Text("c".into()),
        ];
        let bitmap = bitvec![0, 1, 0, 1, 1, 0];
        let extracted = extract_non_null(&original, &bitmap);
        let reconstructed = interleave_nulls(&extracted, &bitmap).unwrap();
        assert_eq!(reconstructed, original);
    }
}
