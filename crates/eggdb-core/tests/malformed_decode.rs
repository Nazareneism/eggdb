//! Tests that verify each encoder returns `Err` (not panic) on malformed input.

use bitvec::prelude::*;
use eggdb_core::encoding::bool_bitmap::BoolBitmapEncoder;
use eggdb_core::encoding::constant::ConstEncoder;
use eggdb_core::encoding::delta::DeltaEncoder;
use eggdb_core::encoding::dictionary::DictEncoder;
use eggdb_core::encoding::null_bitmap;
use eggdb_core::encoding::raw::RawEncoder;
use eggdb_core::encoding::rle::RleEncoder;
use eggdb_core::encoding::ColumnEncoder;
use eggdb_core::Value;

// ─── Const ───────────────────────────────────────────────────────────

#[test]
fn const_invalid_bincode() {
    let data = vec![0xFF; 16];
    assert!(ConstEncoder.decode(&data, 1).is_err());
}

// ─── BoolBitmap ──────────────────────────────────────────────────────

#[test]
fn bool_bitmap_truncated_header_len() {
    // Data too short to read the u32 header length
    assert!(BoolBitmapEncoder.decode(&[0xFF], 1).is_err());
}

#[test]
fn bool_bitmap_header_len_exceeds_data() {
    // header_len = u32::MAX, but only 4 bytes of data total
    let data = u32::MAX.to_le_bytes();
    assert!(BoolBitmapEncoder.decode(&data, 1).is_err());
}

#[test]
fn bool_bitmap_invalid_bincode_in_header() {
    // header_len = 4, then 4 bytes of garbage bincode for Vec<Value>
    let mut data = vec![4u8, 0, 0, 0]; // header_len = 4
    data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // garbage bincode
    data.push(0); // bitmap byte
    assert!(BoolBitmapEncoder.decode(&data, 1).is_err());
}

#[test]
fn bool_bitmap_empty_distinct_values() {
    // Craft bincode for an empty Vec<Value> as the header
    let empty_vec: Vec<Value> = vec![];
    let header_bytes = bincode::serialize(&empty_vec).unwrap();
    let mut data = (header_bytes.len() as u32).to_le_bytes().to_vec();
    data.extend_from_slice(&header_bytes);
    data.push(0); // bitmap byte
    assert!(BoolBitmapEncoder.decode(&data, 1).is_err());
}

#[test]
fn bool_bitmap_three_distinct_values() {
    // Craft bincode for a Vec<Value> with 3 entries (invalid for BoolBitmap)
    let three_vals: Vec<Value> = vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)];
    let header_bytes = bincode::serialize(&three_vals).unwrap();
    let mut data = (header_bytes.len() as u32).to_le_bytes().to_vec();
    data.extend_from_slice(&header_bytes);
    data.push(0); // bitmap byte
    assert!(BoolBitmapEncoder.decode(&data, 1).is_err());
}

// ─── Delta ───────────────────────────────────────────────────────────

#[test]
fn delta_truncated_base() {
    // Only 3 bytes -- not enough for the 8-byte base value
    assert!(DeltaEncoder.decode(&[0xFF, 0xFF, 0xFF], 1).is_err());
}

#[test]
fn delta_truncated_varint() {
    // Valid 8-byte base, then a varint with continuation bit but no next byte
    let mut data = vec![0u8; 8]; // base value = 0
    data.push(0x80); // continuation bit set, no following byte
    assert!(DeltaEncoder.decode(&data, 2).is_err());
}

#[test]
fn delta_overlong_varint() {
    // Valid 8-byte base, then 10+ continuation bytes causing shift >= 64
    let mut data = vec![0u8; 8]; // base value = 0
    for _ in 0..10 {
        data.push(0x80); // continuation bytes
    }
    data.push(0x00); // terminator
    assert!(DeltaEncoder.decode(&data, 2).is_err());
}

#[test]
fn delta_not_enough_varints() {
    // Valid 8-byte base, but no deltas for non_null_count=5
    let data = vec![0u8; 8]; // base value = 0, no delta bytes
    assert!(DeltaEncoder.decode(&data, 5).is_err());
}

// ─── RLE ─────────────────────────────────────────────────────────────

#[test]
fn rle_truncated_num_runs() {
    // Only 3 bytes -- not enough for the u64 num_runs field
    assert!(RleEncoder.decode(&[0xFF, 0xFF, 0xFF], 1).is_err());
}

#[test]
fn rle_truncated_value_length() {
    // num_runs = 1, but data ends before we can read the value length
    let mut data = Vec::new();
    data.extend_from_slice(&1u64.to_le_bytes()); // num_runs = 1
                                                 // No value length bytes follow
    assert!(RleEncoder.decode(&data, 1).is_err());
}

#[test]
fn rle_value_length_exceeds_data() {
    // num_runs = 1, value_len = u32::MAX
    let mut data = Vec::new();
    data.extend_from_slice(&1u64.to_le_bytes()); // num_runs = 1
    data.extend_from_slice(&u32::MAX.to_le_bytes()); // value_len = huge
    assert!(RleEncoder.decode(&data, 1).is_err());
}

#[test]
fn rle_invalid_bincode_value() {
    // num_runs = 1, value_len = 4, then 4 bytes of garbage bincode
    let mut data = Vec::new();
    data.extend_from_slice(&1u64.to_le_bytes()); // num_runs = 1
    data.extend_from_slice(&4u32.to_le_bytes()); // value_len = 4
    data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // garbage bincode
    data.extend_from_slice(&1u64.to_le_bytes()); // run count (won't be reached)
    assert!(RleEncoder.decode(&data, 1).is_err());
}

#[test]
fn rle_truncated_run_count() {
    // num_runs = 1, valid value, but data ends before the run count
    let mut data = Vec::new();
    data.extend_from_slice(&1u64.to_le_bytes()); // num_runs = 1
    let value_bytes = bincode::serialize(&Value::Integer(0)).unwrap();
    data.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
    data.extend_from_slice(&value_bytes);
    // No run count bytes
    assert!(RleEncoder.decode(&data, 1).is_err());
}

// ─── Dict ────────────────────────────────────────────────────────────

#[test]
fn dict_truncated_dict_len() {
    // Only 2 bytes -- not enough for the u32 dict_len
    assert!(DictEncoder.decode(&[0xFF, 0xFF], 1).is_err());
}

#[test]
fn dict_header_len_exceeds_data() {
    // dict_len = 1, entry_len = u32::MAX
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes()); // dict_len = 1
    data.extend_from_slice(&u32::MAX.to_le_bytes()); // entry_len = huge
    assert!(DictEncoder.decode(&data, 1).is_err());
}

#[test]
fn dict_invalid_bincode_entry() {
    // dict_len = 1, entry_len = 4, then 4 bytes of garbage bincode
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes()); // dict_len = 1
    data.extend_from_slice(&4u32.to_le_bytes()); // entry_len = 4
    data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // garbage bincode
    assert!(DictEncoder.decode(&data, 1).is_err());
}

#[test]
fn dict_index_out_of_bounds() {
    // 1-entry dictionary, but encoded index = 99
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes()); // dict_len = 1
    let entry = bincode::serialize(&Value::Integer(0)).unwrap();
    data.extend_from_slice(&(entry.len() as u32).to_le_bytes());
    data.extend_from_slice(&entry);
    data.push(99); // varint index 99 -- out of bounds for 1-entry dict
    assert!(DictEncoder.decode(&data, 1).is_err());
}

#[test]
fn dict_truncated_varint() {
    // Valid 1-entry dictionary, then a varint with continuation bit but no next byte
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes()); // dict_len = 1
    let entry = bincode::serialize(&Value::Integer(0)).unwrap();
    data.extend_from_slice(&(entry.len() as u32).to_le_bytes());
    data.extend_from_slice(&entry);
    data.push(0x80); // continuation bit set, no following byte
    assert!(DictEncoder.decode(&data, 1).is_err());
}

#[test]
fn dict_overlong_varint() {
    // Valid 1-entry dictionary, then 10+ continuation bytes causing shift >= 64
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes()); // dict_len = 1
    let entry = bincode::serialize(&Value::Integer(0)).unwrap();
    data.extend_from_slice(&(entry.len() as u32).to_le_bytes());
    data.extend_from_slice(&entry);
    for _ in 0..10 {
        data.push(0x80); // continuation bytes
    }
    data.push(0x00); // terminator
    assert!(DictEncoder.decode(&data, 1).is_err());
}

#[test]
fn dict_truncated_entry_length() {
    // dict_len = 1, but data ends before entry_len can be read
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes()); // dict_len = 1
                                                 // No entry length bytes follow
    assert!(DictEncoder.decode(&data, 1).is_err());
}

// ─── Raw ─────────────────────────────────────────────────────────────

#[test]
fn raw_truncated_value_length() {
    // Only 2 bytes -- not enough for the u32 value length
    assert!(RawEncoder.decode(&[0xFF, 0xFF], 1).is_err());
}

#[test]
fn raw_value_length_exceeds_data() {
    // value_len = u32::MAX, but only 4 bytes total
    let data = u32::MAX.to_le_bytes();
    assert!(RawEncoder.decode(&data, 1).is_err());
}

#[test]
fn raw_invalid_bincode_value() {
    // value_len = 4, then 4 bytes of garbage bincode
    let mut data = Vec::new();
    data.extend_from_slice(&4u32.to_le_bytes()); // value_len = 4
    data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // garbage bincode
    assert!(RawEncoder.decode(&data, 1).is_err());
}

#[test]
fn raw_not_enough_values() {
    // One valid value, but non_null_count = 5
    let value_bytes = bincode::serialize(&Value::Integer(42)).unwrap();
    let mut data = Vec::new();
    data.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
    data.extend_from_slice(&value_bytes);
    // Only 1 value encoded, but we ask for 5
    assert!(RawEncoder.decode(&data, 5).is_err());
}

// ─── interleave_nulls ────────────────────────────────────────────────

#[test]
fn interleave_nulls_too_few_values() {
    // Bitmap says 2 non-null positions, but only 1 value provided
    let bitmap = bitvec![0, 1, 0]; // 2 non-null positions
    let values = vec![Value::Integer(1)]; // only 1 value -- mismatch
    assert!(null_bitmap::interleave_nulls(&values, &bitmap).is_err());
}

#[test]
fn interleave_nulls_too_many_values() {
    // Bitmap says 1 non-null position, but 3 values provided
    let bitmap = bitvec![1, 0, 1]; // 1 non-null position
    let values = vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)]; // 3 values -- mismatch
    assert!(null_bitmap::interleave_nulls(&values, &bitmap).is_err());
}

#[test]
fn interleave_nulls_all_null_with_values() {
    // Bitmap says all null, but values provided
    let bitmap = bitvec![1, 1, 1]; // 0 non-null positions
    let values = vec![Value::Integer(1)]; // 1 value -- mismatch
    assert!(null_bitmap::interleave_nulls(&values, &bitmap).is_err());
}

#[test]
fn interleave_nulls_no_nulls_wrong_count() {
    // Bitmap says 3 non-null positions, but only 2 values
    let bitmap = bitvec![0, 0, 0]; // 3 non-null positions
    let values = vec![Value::Integer(1), Value::Integer(2)]; // 2 values -- mismatch
    assert!(null_bitmap::interleave_nulls(&values, &bitmap).is_err());
}
