use criterion::{black_box, criterion_group, criterion_main, Criterion};
use eggdb_core::encoding::bool_bitmap::BoolBitmapEncoder;
use eggdb_core::encoding::constant::ConstEncoder;
use eggdb_core::encoding::delta::DeltaEncoder;
use eggdb_core::encoding::dictionary::DictEncoder;
use eggdb_core::encoding::raw::RawEncoder;
use eggdb_core::encoding::rle::RleEncoder;
use eggdb_core::ColumnEncoder;
use eggdb_core::Value;

fn bench_const(c: &mut Criterion) {
    let values = vec![Value::Integer(42); 100_000];
    let enc = ConstEncoder;
    let encoded = enc.encode(&values);

    c.bench_function("const_encode_100k", |b| {
        b.iter(|| enc.encode(black_box(&values)))
    });
    c.bench_function("const_decode_100k", |b| {
        b.iter(|| enc.decode(black_box(&encoded), 100_000).unwrap())
    });
}

fn bench_bool_bitmap(c: &mut Criterion) {
    let values: Vec<Value> = (0..100_000).map(|i| Value::Integer(i % 2)).collect();
    let enc = BoolBitmapEncoder;
    let encoded = enc.encode(&values);

    c.bench_function("bool_bitmap_encode_100k", |b| {
        b.iter(|| enc.encode(black_box(&values)))
    });
    c.bench_function("bool_bitmap_decode_100k", |b| {
        b.iter(|| enc.decode(black_box(&encoded), 100_000).unwrap())
    });
}

fn bench_delta(c: &mut Criterion) {
    let values: Vec<Value> = (0..100_000).map(|i| Value::Integer(i as i64)).collect();
    let enc = DeltaEncoder;
    let encoded = enc.encode(&values);

    c.bench_function("delta_encode_100k", |b| {
        b.iter(|| enc.encode(black_box(&values)))
    });
    c.bench_function("delta_decode_100k", |b| {
        b.iter(|| enc.decode(black_box(&encoded), 100_000).unwrap())
    });
}

fn bench_rle(c: &mut Criterion) {
    // 100 runs of 1000 each
    let mut values = Vec::with_capacity(100_000);
    for i in 0..100 {
        for _ in 0..1000 {
            values.push(Value::Integer(i));
        }
    }
    let enc = RleEncoder;
    let encoded = enc.encode(&values);

    c.bench_function("rle_encode_100k", |b| {
        b.iter(|| enc.encode(black_box(&values)))
    });
    c.bench_function("rle_decode_100k", |b| {
        b.iter(|| enc.decode(black_box(&encoded), 100_000).unwrap())
    });
}

fn bench_dict(c: &mut Criterion) {
    let statuses = ["active", "inactive", "pending", "deleted", "archived"];
    let values: Vec<Value> = (0..100_000)
        .map(|i| Value::Text(statuses[i % 5].into()))
        .collect();
    let enc = DictEncoder;
    let encoded = enc.encode(&values);

    c.bench_function("dict_encode_100k", |b| {
        b.iter(|| enc.encode(black_box(&values)))
    });
    c.bench_function("dict_decode_100k", |b| {
        b.iter(|| enc.decode(black_box(&encoded), 100_000).unwrap())
    });
}

fn bench_raw(c: &mut Criterion) {
    let values: Vec<Value> = (0..100_000)
        .map(|i| Value::Text(format!("user_{i}@example.com")))
        .collect();
    let enc = RawEncoder;
    let encoded = enc.encode(&values);

    c.bench_function("raw_encode_100k", |b| {
        b.iter(|| enc.encode(black_box(&values)))
    });
    c.bench_function("raw_decode_100k", |b| {
        b.iter(|| enc.decode(black_box(&encoded), 100_000).unwrap())
    });
}

criterion_group!(
    benches,
    bench_const,
    bench_bool_bitmap,
    bench_delta,
    bench_rle,
    bench_dict,
    bench_raw,
);
criterion_main!(benches);
