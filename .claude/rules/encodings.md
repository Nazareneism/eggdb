---
paths:
  - "crates/eggdb-core/src/encoding/**"
---

# Encoding Rules

Every encoder must implement the `ColumnEncoder` trait with both `encode()` and
`decode()`. The contract:

- `encode()` receives **only non-null values** in row order. Nulls are handled
  separately by the null bitmap.
- `decode()` returns `Result<Vec<Value>, FormatError>` with **only non-null values**.
  The caller interleaves nulls using the bitmap to reconstruct the full column.
  Must never panic on malformed input — return `Err(FormatError::Decode { .. })` instead.
- Round-trip invariant: `decode(encode(values))? == values` for any valid input.
  This must hold for empty inputs, single values, and millions of rows.

## Encoding Selection Priority

1. `distinct_count == 0` → Const (all null)
2. `distinct_count == 1` → Const
3. `distinct_count == 2` and boolean-like → BoolBitmap
4. Numeric and sorted → Delta
5. `consecutive_dup_ratio > 0.5` → Rle
6. `distinct_count / row_count <= 0.01` and `distinct_count <= 65_536` → Dict
7. Else → Raw

## filter_indices()

Optional optimization per encoding. Returns `Result<BitVec, FormatError>` of matching
non-null row indices. The caller expands to full row space using the null bitmap.
Nulls never match equality/comparison filters (SQL NULL semantics).
