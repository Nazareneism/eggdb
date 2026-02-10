# eggdb

Read and query compressed `.egg` database files from Python.

EggDB converts SQLite databases into highly compressed columnar `.egg` files.
This package lets you query them directly — no decompression step needed.

## Install

```bash
pip install eggdb
```

## Usage

```python
import eggdb

# Open an .egg file
egg = eggdb.open("data.egg")

# List tables
print(egg.tables)  # ['users', 'orders', ...]

# File summary
print(egg.summary())
# {'file_size': 176000000, 'original_size': 1500000000, 'compression_ratio': '8.5x', ...}

# Query with SQL
rows = egg.query("SELECT * FROM users WHERE status = 'active' LIMIT 10")
for row in rows:
    print(row)  # {'id': 1, 'name': 'Alice', 'status': 'active'}

# Query with YQL (a friendlier syntax)
rows = egg.query("count orders by status")
for row in rows:
    print(row)  # {'status': 'shipped', 'count': 4200}

# Column metadata
cols = egg.columns("users")
for col in cols:
    print(f"{col['name']}: {col['encoding']}")  # id: Delta, name: Dict, ...
```

### Encrypted files

```python
egg = eggdb.open("encrypted.egg", passphrase="secret")
rows = egg.query("SELECT * FROM users LIMIT 5")
```

## Development

To build from source and test locally:

```bash
cd python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin
maturin develop --release
```

Then run the test script from the repo root with any `.egg` file:

```bash
python test_python.py path/to/file.egg
```

This prints a full summary (tables, row counts, schema, encoding details, sample data)
and drops into an interactive SQL/YQL query prompt:

```
=== Summary ===
  Tables:      11
  Total rows:  1,497,017
  Ratio:       8.5x

=== Tables ===
  Book: 2,601 rows
  Chapter: 46,104 rows
  ChapterVerse: 1,219,609 rows
  ...

=== Interactive Query ===
Enter SQL or YQL queries (empty line to quit):
> SELECT count(*) AS cnt FROM ChapterVerse WHERE bookId = 'ROM'
  {'cnt': 9320}
  (1 row)

> count Book by translationId sort count desc limit 5
  {'translationId': 'eng_abt', 'count': 84}
  ...
```

## Requirements

- **Python 3.9+** (CPython only)
- **Platforms**: Linux (x86_64, aarch64), macOS (Apple Silicon, Intel)
- **Memory**: The entire `.egg` file is loaded into memory when opened. For a 300 MB
  `.egg` file, expect ~300 MB of RAM usage. Plan accordingly for multi-GB files.

## How it works

Under the hood, `eggdb` uses a SQLite virtual table extension compiled from Rust.
When you call `query()`, it:

1. Opens an in-memory SQLite connection
2. Registers the egg virtual table module
3. Creates virtual tables pointing to your `.egg` file
4. Runs your query through SQLite's optimizer
5. Only decompresses the columns you actually reference

This means joins, aggregations, window functions — everything SQLite supports — works
out of the box. Column pruning ensures only referenced columns are decompressed.

## License

MIT OR Apache-2.0
