---
name: bench
description: Run benchmarks and report results. Optionally compare against a baseline branch.
disable-model-invocation: true
allowed-tools: Bash(cargo bench *), Bash(git stash *), Bash(git checkout *), Bash(git switch *), Read, Grep, Glob
argument-hint: [crate-name] [--compare main]
---

Run EggDB benchmarks and report results.

## Usage

- `/bench` — Run all benchmarks
- `/bench eggdb-core` — Run benchmarks for a specific crate
- `/bench --compare main` — Run benchmarks on current branch, then on `main`, and compare

## Steps

### Basic Run

1. If a crate name is given in `$ARGUMENTS`, run `cargo bench -p <crate>`.
   Otherwise, run `cargo bench --workspace`.
2. Summarize results in a table: benchmark name, time (ns/iter or ms), throughput if available.

### Comparison Mode (when `--compare` is in arguments)

1. Run benchmarks on the current branch. Save output.
2. Stash any uncommitted changes with `git stash`.
3. Switch to the comparison branch.
4. Run the same benchmarks. Save output.
5. Switch back to the original branch.
6. Pop the stash with `git stash pop`.
7. Present a comparison table showing: benchmark name, baseline time, current time, delta (%).

## Reporting

- Highlight any benchmark that regressed by more than 10%.
- Highlight any benchmark that improved by more than 10%.
- Keep the table concise. Omit benchmarks with < 1% change in comparison mode.
