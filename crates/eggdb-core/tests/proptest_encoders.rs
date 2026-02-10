use eggdb_core::encoding::bool_bitmap::BoolBitmapEncoder;
use eggdb_core::encoding::constant::ConstEncoder;
use eggdb_core::encoding::delta::DeltaEncoder;
use eggdb_core::encoding::dictionary::DictEncoder;
use eggdb_core::encoding::raw::RawEncoder;
use eggdb_core::encoding::rle::RleEncoder;
use eggdb_core::ColumnEncoder;
use eggdb_core::Value;
use proptest::prelude::*;

// --- Value generators ---

fn arb_integer() -> impl Strategy<Value = Value> {
    any::<i64>().prop_map(Value::Integer)
}

fn arb_real() -> impl Strategy<Value = Value> {
    // Avoid NaN in property tests since NaN equality is special
    prop::num::f64::NORMAL.prop_map(Value::Real)
}

fn arb_text() -> impl Strategy<Value = Value> {
    "[a-zA-Z0-9 ]{0,100}".prop_map(Value::Text)
}

fn arb_blob() -> impl Strategy<Value = Value> {
    prop::collection::vec(any::<u8>(), 0..100).prop_map(Value::Blob)
}

fn arb_value() -> impl Strategy<Value = Value> {
    prop_oneof![arb_integer(), arb_real(), arb_text(), arb_blob(),]
}

// --- Round-trip property: decode(encode(values)) == values ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Const encoder: all values must be the same
    #[test]
    fn const_round_trip(v in arb_value(), count in 1..500usize) {
        let values: Vec<Value> = vec![v; count];
        let enc = ConstEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, count).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // BoolBitmap encoder: exactly 2 distinct values
    #[test]
    fn bool_bitmap_round_trip(bits in prop::collection::vec(any::<bool>(), 1..1000)) {
        let values: Vec<Value> = bits.iter().map(|b| Value::Integer(if *b { 1 } else { 0 })).collect();
        let enc = BoolBitmapEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // BoolBitmap with text values
    #[test]
    fn bool_bitmap_text_round_trip(bits in prop::collection::vec(any::<bool>(), 1..500)) {
        let values: Vec<Value> = bits.iter().map(|b| {
            Value::Text(if *b { "yes".into() } else { "no".into() })
        }).collect();
        let enc = BoolBitmapEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Delta encoder: any sequence of integers
    #[test]
    fn delta_round_trip(values in prop::collection::vec(any::<i64>(), 1..1000)) {
        let values: Vec<Value> = values.into_iter().map(Value::Integer).collect();
        let enc = DeltaEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Delta encoder: sorted integers (common use case)
    #[test]
    fn delta_sorted_round_trip(mut ints in prop::collection::vec(any::<i64>(), 1..1000)) {
        ints.sort();
        let values: Vec<Value> = ints.into_iter().map(Value::Integer).collect();
        let enc = DeltaEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // RLE encoder: any values
    #[test]
    fn rle_round_trip_integers(values in prop::collection::vec(0..10i64, 1..500)) {
        let values: Vec<Value> = values.into_iter().map(Value::Integer).collect();
        let enc = RleEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // RLE with text values
    #[test]
    fn rle_round_trip_text(indices in prop::collection::vec(0..5usize, 1..500)) {
        let words = ["alpha", "beta", "gamma", "delta", "epsilon"];
        let values: Vec<Value> = indices.iter().map(|i| Value::Text(words[*i].into())).collect();
        let enc = RleEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Dict encoder: low cardinality
    #[test]
    fn dict_round_trip(indices in prop::collection::vec(0..20usize, 1..500)) {
        let values: Vec<Value> = indices.iter().map(|i| Value::Integer(*i as i64)).collect();
        let enc = DictEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Dict encoder: text values
    #[test]
    fn dict_text_round_trip(indices in prop::collection::vec(0..10usize, 1..500)) {
        let words: Vec<String> = (0..10).map(|i| format!("word_{i}")).collect();
        let values: Vec<Value> = indices.iter().map(|i| Value::Text(words[*i].clone())).collect();
        let enc = DictEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Raw encoder: any mix of value types
    #[test]
    fn raw_round_trip(values in prop::collection::vec(arb_value(), 1..200)) {
        let enc = RawEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Raw encoder: integers
    #[test]
    fn raw_integer_round_trip(values in prop::collection::vec(any::<i64>(), 1..500)) {
        let values: Vec<Value> = values.into_iter().map(Value::Integer).collect();
        let enc = RawEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }

    // Raw encoder: text
    #[test]
    fn raw_text_round_trip(values in prop::collection::vec("[a-z]{0,50}", 1..200)) {
        let values: Vec<Value> = values.into_iter().map(Value::Text).collect();
        let enc = RawEncoder;
        let encoded = enc.encode(&values);
        let decoded = enc.decode(&encoded, values.len()).unwrap();
        prop_assert_eq!(decoded, values);
    }
}

// --- Edge cases that every encoder should handle ---

macro_rules! test_encoder_edge_cases {
    ($encoder:expr, $name:ident, $make_values:expr) => {
        mod $name {
            use super::*;

            #[test]
            fn empty_input() {
                let enc = $encoder;
                let encoded = enc.encode(&[]);
                let decoded = enc.decode(&encoded, 0).unwrap();
                assert!(decoded.is_empty());
            }

            #[test]
            fn single_value() {
                let enc = $encoder;
                let values = $make_values(1);
                let encoded = enc.encode(&values);
                let decoded = enc.decode(&encoded, 1).unwrap();
                assert_eq!(decoded, values);
            }

            #[test]
            fn many_values() {
                let enc = $encoder;
                let values = $make_values(10_000);
                let encoded = enc.encode(&values);
                let decoded = enc.decode(&encoded, 10_000).unwrap();
                assert_eq!(decoded, values);
            }
        }
    };
}

test_encoder_edge_cases!(ConstEncoder, const_edges, |n: usize| {
    vec![Value::Integer(42); n]
});

test_encoder_edge_cases!(
    BoolBitmapEncoder,
    bool_bitmap_edges,
    |n: usize| -> Vec<Value> { (0..n).map(|i| Value::Integer((i % 2) as i64)).collect() }
);

test_encoder_edge_cases!(DeltaEncoder, delta_edges, |n: usize| -> Vec<Value> {
    (0..n).map(|i| Value::Integer(i as i64)).collect()
});

test_encoder_edge_cases!(RleEncoder, rle_edges, |n: usize| -> Vec<Value> {
    (0..n)
        .map(|i| Value::Text(format!("val_{}", i % 10)))
        .collect()
});

test_encoder_edge_cases!(DictEncoder, dict_edges, |n: usize| -> Vec<Value> {
    (0..n).map(|i| Value::Integer((i % 5) as i64)).collect()
});

test_encoder_edge_cases!(RawEncoder, raw_edges, |n: usize| -> Vec<Value> {
    (0..n).map(|i| Value::Text(format!("unique_{i}"))).collect()
});

// --- Null bitmap + encoder integration ---

#[test]
fn null_bitmap_with_encoder_integration() {
    use bitvec::prelude::*;
    use eggdb_core::encoding::null_bitmap;
    use eggdb_core::expand_non_null_indices;

    // Full column: [10, NULL, 20, 30, NULL, 40]
    let full_column = [
        Value::Integer(10),
        Value::Null,
        Value::Integer(20),
        Value::Integer(30),
        Value::Null,
        Value::Integer(40),
    ];

    // Build null bitmap
    let null_bm: BitVec = full_column.iter().map(|v| v.is_null()).collect();
    assert_eq!(null_bm, bitvec![0, 1, 0, 0, 1, 0]);

    // Extract non-null values
    let non_null: Vec<Value> = full_column
        .iter()
        .filter(|v| !v.is_null())
        .cloned()
        .collect();
    assert_eq!(non_null.len(), 4);

    // Encode null bitmap
    let bm_bytes = null_bitmap::encode(&null_bm);
    let bm_decoded = null_bitmap::decode(&bm_bytes, 6);
    assert_eq!(bm_decoded, null_bm);

    // Encode values with RawEncoder
    let enc = RawEncoder;
    let encoded = enc.encode(&non_null);
    let decoded = enc.decode(&encoded, 4).unwrap();
    assert_eq!(decoded, non_null);

    // Filter: find values > 15
    let filter_result = enc
        .filter_indices(&encoded, 4, &eggdb_core::FilterOp::Gt, &Value::Integer(15))
        .unwrap();
    // Non-null values [10, 20, 30, 40], matching > 15: indices 1, 2, 3
    assert_eq!(filter_result, bitvec![0, 1, 1, 1]);

    // Expand to row space
    let row_matches = expand_non_null_indices(&filter_result, &null_bm);
    // Rows: [10, NULL, 20, 30, NULL, 40]
    // Matching: row 2 (20), row 3 (30), row 5 (40)
    assert_eq!(row_matches, bitvec![0, 0, 1, 1, 0, 1]);
}

// --- Large-scale test (1M rows) ---

#[test]
fn million_row_delta() {
    let values: Vec<Value> = (0..1_000_000).map(|i| Value::Integer(i as i64)).collect();
    let enc = DeltaEncoder;
    let encoded = enc.encode(&values);
    let decoded = enc.decode(&encoded, 1_000_000).unwrap();
    assert_eq!(decoded.len(), 1_000_000);
    assert_eq!(decoded[0], Value::Integer(0));
    assert_eq!(decoded[999_999], Value::Integer(999_999));
}

#[test]
fn million_row_rle() {
    // 1000 runs of 1000 each
    let mut values = Vec::with_capacity(1_000_000);
    for i in 0..1000 {
        for _ in 0..1000 {
            values.push(Value::Integer(i));
        }
    }
    let enc = RleEncoder;
    let encoded = enc.encode(&values);
    let decoded = enc.decode(&encoded, 1_000_000).unwrap();
    assert_eq!(decoded.len(), 1_000_000);
    assert_eq!(decoded[0], Value::Integer(0));
    assert_eq!(decoded[999_999], Value::Integer(999));
}

#[test]
fn million_row_dict() {
    let values: Vec<Value> = (0..1_000_000)
        .map(|i| Value::Integer((i % 100) as i64))
        .collect();
    let enc = DictEncoder;
    let encoded = enc.encode(&values);
    let decoded = enc.decode(&encoded, 1_000_000).unwrap();
    assert_eq!(decoded.len(), 1_000_000);
    assert_eq!(decoded[0], Value::Integer(0));
    assert_eq!(decoded[999_999], Value::Integer(99));
}
