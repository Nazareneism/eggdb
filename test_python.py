#!/usr/bin/env python3
"""
Test script for the eggdb Python package.

Usage:
    # First, build and install the package:
    cd python
    python3 -m venv .venv
    source .venv/bin/activate
    pip install maturin
    maturin develop --release

    # Then run this script with any .egg file:
    python test_python.py <path-to-file.egg>

    # Example:
    python test_python.py /tmp/bible.egg
"""

import sys

import eggdb


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <path-to-file.egg>")
        sys.exit(1)

    path = sys.argv[1]
    egg = eggdb.open(path)
    print(f"Opened: {egg}")
    print()

    # --- Summary ---
    summary = egg.summary()
    print("=== Summary ===")
    print(f"  Tables:      {summary['table_count']}")
    print(f"  Total rows:  {summary['total_rows']}")
    print(f"  File size:   {summary['file_size']:,} bytes")
    print(f"  Original:    {summary['original_size']:,} bytes")
    print(f"  Ratio:       {summary['compression_ratio']}")
    print(f"  Encrypted:   {summary['encrypted']}")
    print(f"  Version:     {summary['version']}")
    print()

    # --- Tables and row counts ---
    print("=== Tables ===")
    tables = egg.tables
    for table in tables:
        rows = egg.query(f'SELECT count(*) AS cnt FROM "{table}"')
        print(f"  {table}: {rows[0]['cnt']:,} rows")
    print()

    # --- Column metadata for each table ---
    print("=== Schema ===")
    for table in tables:
        cols = egg.columns(table)
        print(f"  {table} ({len(cols)} columns):")
        for col in cols:
            print(
                f"    {col['name']:30} {col['type']:10} {col['encoding']:12} "
                f"card={col['cardinality']:<8} nulls={col['null_count']}"
            )
        print()

    # --- Sample data from each table (first 3 rows) ---
    print("=== Sample Data (first 3 rows per table) ===")
    for table in tables:
        rows = egg.query(f'SELECT * FROM "{table}" LIMIT 3')
        print(f"  {table}:")
        if not rows:
            print("    (empty)")
        for row in rows:
            # Truncate long values for display
            display = {}
            for k, v in row.items():
                s = str(v)
                display[k] = s[:60] + "..." if len(s) > 60 else s
            print(f"    {display}")
        print()

    # --- Run a user query interactively ---
    print("=== Interactive Query ===")
    print("Enter SQL or YQL queries (empty line to quit):")
    while True:
        try:
            query = input("> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            break
        if not query:
            break
        try:
            rows = egg.query(query)
            if not rows:
                print("  (no results)")
            for row in rows:
                print(f"  {row}")
            print(f"  ({len(rows)} row{'s' if len(rows) != 1 else ''})")
        except Exception as e:
            print(f"  Error: {e}")
        print()

    print("Done.")


if __name__ == "__main__":
    main()
