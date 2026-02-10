---
name: validate
description: Run the full pre-commit validation pipeline (fmt, clippy, test). Use before committing changes.
disable-model-invocation: true
allowed-tools: Bash(cargo fmt *), Bash(cargo clippy *), Bash(cargo test *)
argument-hint: [--fix]
---

Run the full EggDB pre-commit validation pipeline.

## Steps

1. **Format check**: Run `cargo fmt --all -- --check`
2. **Lint**: Run `cargo clippy --workspace -- -D warnings`
3. **Test**: Run `cargo test --workspace`

If `$ARGUMENTS` contains `--fix`, run `cargo fmt --all` (without `--check`) first
to auto-fix formatting, then continue with the remaining steps.

## On Failure

- If format check fails and `--fix` was not passed, suggest running `/validate --fix`.
- If clippy fails, show the warnings/errors and suggest fixes.
- If tests fail, show the failing test names and relevant output.

## On Success

Report: "Validation passed: fmt, clippy, N tests passed."
