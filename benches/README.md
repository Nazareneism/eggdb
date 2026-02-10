# benches/ — Criterion Benchmarks

Micro-benchmarks for encoding throughput using [Criterion](https://github.com/bheisler/criterion.rs).

## Files

| File | What It Benchmarks |
|------|--------------------|
| `encoding.rs` | Encode/decode throughput for all 6 encodings (100K values each) |

## Running

```bash
# Run all benchmarks
cargo bench -p eggdb-core

# Run a specific benchmark
cargo bench -p eggdb-core -- delta

# Generate HTML reports (output in target/criterion/)
cargo bench -p eggdb-core
open target/criterion/report/index.html
```

## Current Results (Apple Silicon, 100K values)

| Encoding | Encode | Decode |
|----------|--------|--------|
| Const | 17 ns | 251 us |
| BoolBitmap | 384 us | 268 us |
| Delta | 112 us | 150 us |
| Rle | 189 us | 261 us |
| Dict | 1.60 ms | 2.18 ms |
| Raw | 3.53 ms | 3.44 ms |

## Configuration

The benchmark harness is configured in `crates/eggdb-core/Cargo.toml`:

```toml
[[bench]]
name = "encoding"
harness = false
path = "../../benches/encoding.rs"
```

The bench file lives at the workspace root but is registered under `eggdb-core`
since it benchmarks encoding algorithms.
