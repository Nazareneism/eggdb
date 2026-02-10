---
paths:
  - "crates/**/*.rs"
  - "src/**/*.rs"
---

# Rust Rules

- No `.unwrap()` or `.expect()` in library crates. Use `?` with proper error types.
- `unwrap()` is acceptable only in tests and the CLI binary's `main()`.
- All `pub` items must have doc comments (`///`).
- Prefer iterators and combinators over manual loops where readable.
- Use `#[cfg(test)]` modules at the bottom of each file, not separate test files,
  for unit tests. Integration tests go in `tests/`.
- Derive `Debug` on all public types. Derive `Clone`, `PartialEq`, `Eq` where sensible.
- Keep functions under 50 lines. Extract helpers for complex logic.
- Use `Vec<u8>` for binary data, not `String`. Use `Cow<'_, str>` when you need
  to sometimes borrow and sometimes own strings.
