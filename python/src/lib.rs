use pyo3::exceptions::{PyIOError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::path::PathBuf;
use zeroize::Zeroizing;

/// Intermediate value type for passing data across the GIL boundary.
enum RustValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A handle to an .egg file for reading metadata and running queries.
#[pyclass]
struct EggFile {
    path: PathBuf,
    data: Vec<u8>,
    passphrase: Option<Zeroizing<String>>,
}

#[pymethods]
impl EggFile {
    #[new]
    #[pyo3(signature = (path, passphrase=None))]
    fn new(path: &str, passphrase: Option<&str>) -> PyResult<Self> {
        let path = PathBuf::from(path);
        if !path.exists() {
            return Err(PyIOError::new_err(format!(
                "file not found: {}",
                path.display()
            )));
        }

        let data =
            std::fs::read(&path).map_err(|e| PyIOError::new_err(format!("read error: {e}")))?;

        // Validate the file is a valid egg
        let reader = if let Some(pass) = passphrase {
            eggdb_core::EggReader::open_with_passphrase(&data, pass)
        } else {
            eggdb_core::EggReader::open(&data)
        };
        reader.map_err(|e| PyValueError::new_err(format!("invalid egg file: {e}")))?;

        Ok(Self {
            path,
            data,
            passphrase: passphrase.map(|s| Zeroizing::new(String::from(s))),
        })
    }

    /// List of table names in the egg file.
    #[getter]
    fn tables(&self) -> PyResult<Vec<String>> {
        let reader = self.open_reader()?;
        Ok(reader
            .tables()
            .iter()
            .map(|t| t.table_name.clone())
            .collect())
    }

    /// File summary: size, compression ratio, table count, etc.
    fn summary(&self, py: Python<'_>) -> PyResult<PyObject> {
        let reader = self.open_reader()?;
        let header = reader.header();
        let file_size = self.data.len() as u64;
        let total_rows: u64 = reader.tables().iter().map(|t| t.row_count).sum();

        let dict = PyDict::new(py);
        dict.set_item("file_size", file_size)?;
        dict.set_item("original_size", header.original_size)?;
        dict.set_item("table_count", header.table_count)?;
        dict.set_item("total_rows", total_rows)?;
        dict.set_item("version", header.version)?;
        dict.set_item("encrypted", header.flags.has_encryption())?;
        dict.set_item("row_order_preserved", header.flags.row_order_preserved())?;
        if file_size > 0 {
            let ratio = header.original_size as f64 / file_size as f64;
            dict.set_item("compression_ratio", format!("{ratio:.1}x"))?;
        }
        Ok(dict.into())
    }

    /// Per-column detail for a table.
    fn columns(&self, py: Python<'_>, table: &str) -> PyResult<PyObject> {
        let reader = self.open_reader()?;

        let table_meta = reader
            .tables()
            .iter()
            .find(|t| t.table_name == table)
            .ok_or_else(|| PyValueError::new_err(format!("table not found: {table}")))?;

        let list = PyList::empty(py);
        for col in &table_meta.columns {
            let dict = PyDict::new(py);
            dict.set_item("name", &col.name)?;
            dict.set_item("type", col.sqlite_type.to_string())?;
            dict.set_item("encoding", col.encoding.to_string())?;
            dict.set_item("cardinality", col.cardinality)?;
            dict.set_item("null_count", col.null_count)?;
            dict.set_item("compressed_size", col.yolk_length)?;
            dict.set_item("uncompressed_size", col.uncompressed_size)?;
            list.append(dict)?;
        }
        Ok(list.into())
    }

    /// Run a SQL or YQL query against the egg file. Returns a list of dicts.
    fn query(&self, py: Python<'_>, query: &str) -> PyResult<PyObject> {
        // Detect YQL and transpile if needed (pure Rust, no GIL needed)
        let sql = if eggdb_yql::is_yql(query) {
            eggdb_yql::transpile(query)
                .map_err(|e| PyValueError::new_err(format!("YQL parse error: {e}")))?
        } else {
            query.to_string()
        };

        let data = &self.data;
        let path = &self.path;
        let passphrase = &self.passphrase;

        // Do all SQLite work without holding the GIL
        let (col_names, rows) = py
            .allow_threads(|| -> Result<(Vec<String>, Vec<Vec<RustValue>>), String> {
                let reader = if let Some(ref pass) = passphrase {
                    eggdb_core::EggReader::open_with_passphrase(data, pass)
                } else {
                    eggdb_core::EggReader::open(data)
                };
                let reader = reader.map_err(|e| format!("egg parse error: {e}"))?;

                let conn = rusqlite::Connection::open_in_memory()
                    .map_err(|e| format!("sqlite error: {e}"))?;

                eggdb_vtable::register_egg_module(&conn)
                    .map_err(|e| format!("vtable register error: {e}"))?;

                let path_str = path
                    .to_str()
                    .ok_or_else(|| "path is not valid UTF-8".to_string())?;

                for table in reader.tables() {
                    let escaped_ident = table.table_name.replace('"', "\"\"");
                    let escaped_name = table.table_name.replace('\'', "''");
                    let escaped_path = path_str.replace('\'', "''");
                    let create_sql = if let Some(ref pass) = passphrase {
                        format!(
                            "CREATE VIRTUAL TABLE \"{}\" USING egg('{}', '{}', '{}')",
                            escaped_ident,
                            escaped_path,
                            escaped_name,
                            pass.replace('\'', "''"),
                        )
                    } else {
                        format!(
                            "CREATE VIRTUAL TABLE \"{}\" USING egg('{}', '{}')",
                            escaped_ident, escaped_path, escaped_name,
                        )
                    };
                    conn.execute(&create_sql, [])
                        .map_err(|e| format!("create vtable error: {e}"))?;
                }

                let mut stmt = conn.prepare(&sql).map_err(|e| format!("SQL error: {e}"))?;

                let col_count = stmt.column_count();
                let col_names: Vec<String> = (0..col_count)
                    .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
                    .collect();

                let mapped = stmt
                    .query_map([], |row| {
                        let mut values = Vec::with_capacity(col_count);
                        for i in 0..col_count {
                            let val = row.get_ref(i).map_err(|e| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    i,
                                    rusqlite::types::Type::Null,
                                    Box::new(e),
                                )
                            })?;
                            let rv = match val {
                                rusqlite::types::ValueRef::Null => RustValue::Null,
                                rusqlite::types::ValueRef::Integer(n) => RustValue::Integer(n),
                                rusqlite::types::ValueRef::Real(f) => RustValue::Real(f),
                                rusqlite::types::ValueRef::Text(t) => {
                                    RustValue::Text(String::from_utf8_lossy(t).into_owned())
                                }
                                rusqlite::types::ValueRef::Blob(b) => RustValue::Blob(b.to_vec()),
                            };
                            values.push(rv);
                        }
                        Ok(values)
                    })
                    .map_err(|e| format!("query error: {e}"))?;

                let rows: Result<Vec<Vec<RustValue>>, _> = mapped.collect();
                let rows = rows.map_err(|e| format!("row error: {e}"))?;
                Ok((col_names, rows))
            })
            .map_err(PyRuntimeError::new_err)?;

        // Convert to Python objects (needs GIL)
        let result = PyList::empty(py);
        for row in &rows {
            let dict = PyDict::new(py);
            for (name, val) in col_names.iter().zip(row.iter()) {
                let py_val = rust_value_to_py(py, val);
                dict.set_item(name, py_val)?;
            }
            result.append(dict)?;
        }

        Ok(result.into())
    }

    fn __repr__(&self) -> String {
        let filename = self
            .path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("?");
        match self.open_reader() {
            Ok(reader) => {
                let table_count = reader.table_count();
                let encrypted = reader.header().flags.has_encryption();
                format!(
                    "EggFile('{}', tables={}, encrypted={})",
                    filename,
                    table_count,
                    if encrypted { "True" } else { "False" },
                )
            }
            Err(_) => format!("EggFile('{filename}', invalid=True)"),
        }
    }
}

impl EggFile {
    fn open_reader(&self) -> PyResult<eggdb_core::EggReader<'_>> {
        let reader = if let Some(ref pass) = self.passphrase {
            eggdb_core::EggReader::open_with_passphrase(&self.data, pass)
        } else {
            eggdb_core::EggReader::open(&self.data)
        };
        reader.map_err(|e| PyValueError::new_err(format!("egg parse error: {e}")))
    }
}

fn rust_value_to_py(py: Python<'_>, val: &RustValue) -> PyObject {
    match val {
        RustValue::Null => py.None(),
        RustValue::Integer(i) => i.into_pyobject(py).unwrap().into_any().unbind(),
        RustValue::Real(f) => f.into_pyobject(py).unwrap().into_any().unbind(),
        RustValue::Text(s) => s.into_pyobject(py).unwrap().into_any().unbind(),
        RustValue::Blob(b) => PyBytes::new(py, b).into_any().unbind(),
    }
}

/// Open an .egg file for reading and querying.
#[pyfunction]
#[pyo3(signature = (path, passphrase=None))]
fn open(path: &str, passphrase: Option<&str>) -> PyResult<EggFile> {
    EggFile::new(path, passphrase)
}

#[pymodule]
fn _eggdb(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<EggFile>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    Ok(())
}
