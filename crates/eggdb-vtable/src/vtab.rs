use std::marker::PhantomData;
use std::os::raw::c_int;
use std::sync::Arc;

use bitvec::prelude::*;
use rusqlite::ffi;
use rusqlite::types::ValueRef;
use rusqlite::vtab::{
    read_only_module, Context, CreateVTab, IndexConstraintOp, IndexInfo, Module, VTab,
    VTabConnection, VTabCursor, VTabKind, Values,
};
use rusqlite::Error;

use zeroize::Zeroizing;

use eggdb_core::{
    decode_column_from_parts_with_key, encoder_for, expand_non_null_indices,
    read_column_block_from_parts_with_key, Encoding, FilterOp, Header, SqliteType, TableMeta,
    Value,
};

use crate::column_cache::DecodedColumn;
use crate::error::VtableError;

/// Parsed file data shared across vtable instances (via Arc).
pub struct EggFileData {
    pub data: Vec<u8>,
    pub header: Header,
    pub tables: Vec<TableMeta>,
    pub encryption_key: Option<Zeroizing<[u8; 32]>>,
}

/// The virtual table implementation.
#[repr(C)]
pub struct EggTab {
    /// Must be first field for SQLite FFI.
    base: ffi::sqlite3_vtab,
    file_data: Arc<EggFileData>,
    table_idx: usize,
}

/// Op codes for filter constraints, encoded in idx_str.
const OP_EQ: u8 = 0;
const OP_GT: u8 = 1;
const OP_LT: u8 = 2;
const OP_GE: u8 = 3;
const OP_LE: u8 = 4;
const OP_NE: u8 = 5;
const OP_LIKE: u8 = 6;
const OP_ISNULL: u8 = 7;
const OP_ISNOTNULL: u8 = 8;

fn constraint_op_to_code(op: &IndexConstraintOp) -> Option<u8> {
    match op {
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ => Some(OP_EQ),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GT => Some(OP_GT),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LT => Some(OP_LT),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GE => Some(OP_GE),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LE => Some(OP_LE),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_NE => Some(OP_NE),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LIKE => Some(OP_LIKE),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_ISNULL => Some(OP_ISNULL),
        IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_ISNOTNULL => Some(OP_ISNOTNULL),
        _ => None,
    }
}

fn code_to_filter_op(code: u8) -> FilterOp {
    match code {
        OP_EQ => FilterOp::Eq,
        OP_GT => FilterOp::Gt,
        OP_LT => FilterOp::Lt,
        OP_GE => FilterOp::Gte,
        OP_LE => FilterOp::Lte,
        OP_NE => FilterOp::Ne,
        OP_LIKE => FilterOp::Like,
        OP_ISNULL => FilterOp::IsNull,
        OP_ISNOTNULL => FilterOp::IsNotNull,
        _ => FilterOp::Eq, // fallback, shouldn't happen
    }
}

/// Estimate cost for a given encoding + filter combination.
fn estimate_cost(
    encoding: Encoding,
    op: &IndexConstraintOp,
    row_count: u64,
    cardinality: u64,
) -> f64 {
    let n = row_count as f64;
    match encoding {
        Encoding::Const => match op {
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_NE
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_ISNULL
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_ISNOTNULL => 1.0,
            _ => 1.0,
        },
        Encoding::BoolBitmap => n * 0.5,
        Encoding::Delta => match op {
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GT
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GE
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LT
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LE => {
                if n > 1.0 {
                    n.log2()
                } else {
                    1.0
                }
            }
            _ => n,
        },
        Encoding::Dict => match op {
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_NE => cardinality as f64,
            _ => n,
        },
        Encoding::Rle => match op {
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_NE => cardinality as f64,
            _ => n,
        },
        Encoding::Raw => n,
    }
}

/// Build the CREATE TABLE statement for SQLite schema declaration.
fn build_schema_sql(table_meta: &TableMeta) -> String {
    let mut sql = format!("CREATE TABLE \"{}\"(", table_meta.table_name);
    for (i, col) in table_meta.columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push('"');
        // Escape double quotes in column names
        for ch in col.name.chars() {
            if ch == '"' {
                sql.push('"');
            }
            sql.push(ch);
        }
        sql.push('"');
        sql.push(' ');
        sql.push_str(&col.sqlite_type.to_string());
    }
    sql.push(')');
    sql
}

/// Dequote a string argument (remove surrounding quotes).
fn dequote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Parse a constraint plan from idx_str.
///
/// Format: `cols:<hex>|<col>:<op>;<col>:<op>...`
fn parse_idx_str(idx_str: &str) -> (u64, Vec<(usize, u8)>) {
    let mut col_used = u64::MAX; // default: all columns
    let mut constraints = Vec::new();

    let parts: Vec<&str> = idx_str.split('|').collect();
    if parts.is_empty() {
        return (col_used, constraints);
    }

    // Parse col_used
    if let Some(hex) = parts[0].strip_prefix("cols:") {
        col_used = u64::from_str_radix(hex, 16).unwrap_or(u64::MAX);
    }

    // Parse constraints
    if parts.len() > 1 && !parts[1].is_empty() {
        for entry in parts[1].split(';') {
            let kv: Vec<&str> = entry.split(':').collect();
            if kv.len() == 2 {
                if let (Ok(col), Ok(op)) = (kv[0].parse::<usize>(), kv[1].parse::<u8>()) {
                    constraints.push((col, op));
                }
            }
        }
    }

    (col_used, constraints)
}

/// Convert a rusqlite ValueRef to our Value type.
fn value_ref_to_value(vr: ValueRef<'_>) -> Value {
    match vr {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(s) => {
            let s = std::str::from_utf8(s).unwrap_or("");
            Value::Text(s.to_string())
        }
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    }
}

// Safety: EggTab has #[repr(C)] with sqlite3_vtab as the first field,
// which is required by the SQLite vtable API.
unsafe impl<'vtab> VTab<'vtab> for EggTab {
    type Aux = ();
    type Cursor = EggCursor<'vtab>;

    fn connect(
        _db: &mut VTabConnection,
        _aux: Option<&()>,
        args: &[&[u8]],
    ) -> rusqlite::Result<(String, Self)> {
        // args[0] = module name
        // args[1] = database name
        // args[2] = vtable name
        // args[3] = egg file path
        // args[4] = table name in egg
        // args[5] = optional passphrase for encrypted eggs
        if args.len() < 5 {
            return Err(Error::ModuleError(format!(
                "egg vtable requires 2 arguments (file path, table name), got {}",
                args.len().saturating_sub(3)
            )));
        }

        let file_path_raw = std::str::from_utf8(args[3])
            .map_err(|e| Error::ModuleError(format!("invalid file path: {e}")))?;
        let file_path = dequote(file_path_raw);

        let table_name_raw = std::str::from_utf8(args[4])
            .map_err(|e| Error::ModuleError(format!("invalid table name: {e}")))?;
        let table_name = dequote(table_name_raw);

        let passphrase = if args.len() >= 6 {
            let pp_raw = std::str::from_utf8(args[5])
                .map_err(|e| Error::ModuleError(format!("invalid passphrase: {e}")))?;
            Some(dequote(pp_raw))
        } else {
            None
        };

        // Read file
        let data = std::fs::read(file_path).map_err(|e| {
            Error::ModuleError(
                VtableError::FileRead {
                    path: file_path.to_string(),
                    source: e,
                }
                .to_string(),
            )
        })?;

        // Parse header
        let header = Header::from_bytes(&data)
            .map_err(|e| Error::ModuleError(VtableError::Format(e).to_string()))?;

        // Derive encryption key if passphrase provided and file is encrypted
        let encryption_key = if let Some(pp) = passphrase {
            if !header.flags.has_encryption() {
                return Err(Error::ModuleError(
                    "passphrase provided but file is not encrypted".into(),
                ));
            }
            let yolk_start = header.yolk_offset as usize;
            let salt_end = yolk_start + eggdb_core::SALT_SIZE;
            if salt_end > data.len() {
                return Err(Error::ModuleError(format!(
                    "yolk section too short for encryption salt: need {salt_end} bytes, file is {}",
                    data.len()
                )));
            }
            let mut salt = [0u8; eggdb_core::SALT_SIZE];
            salt.copy_from_slice(&data[yolk_start..salt_end]);
            let key = eggdb_core::derive_key(pp, &salt)
                .map_err(|e| Error::ModuleError(format!("key derivation failed: {e}")))?;
            Some(key)
        } else {
            if header.flags.has_encryption() {
                return Err(Error::ModuleError(
                    "file is encrypted but no passphrase provided".into(),
                ));
            }
            None
        };

        // Parse albumen
        let albumen_start = header.albumen_offset as usize;
        let albumen_end = header.yolk_offset as usize;
        if albumen_end > data.len() || albumen_start > albumen_end {
            return Err(Error::ModuleError(format!(
                "invalid albumen range [{albumen_start}..{albumen_end}] in file of size {}",
                data.len()
            )));
        }
        let tables =
            eggdb_core::format::albumen::deserialize_albumen(&data[albumen_start..albumen_end])
                .map_err(|e| Error::ModuleError(VtableError::Format(e).to_string()))?;

        // Find requested table
        let table_idx = tables
            .iter()
            .position(|t| t.table_name == table_name)
            .ok_or_else(|| {
                Error::ModuleError(VtableError::TableNotFound(table_name.to_string()).to_string())
            })?;

        let table_meta = &tables[table_idx];
        let schema_sql = build_schema_sql(table_meta);

        let file_data = Arc::new(EggFileData {
            data,
            header,
            tables,
            encryption_key,
        });

        let vtab = EggTab {
            base: ffi::sqlite3_vtab::default(),
            file_data,
            table_idx,
        };

        Ok((schema_sql, vtab))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<()> {
        let table_meta = &self.file_data.tables[self.table_idx];
        let row_count = table_meta.row_count;
        let col_used = info.col_used();

        let mut total_cost = row_count as f64;
        let mut idx_str_parts: Vec<String> = Vec::new();
        let mut argv_idx = 1i32; // 1-based for SQLite

        for (constraint, mut usage) in info.constraints_and_usages() {
            if !constraint.is_usable() {
                continue;
            }

            let col_idx = constraint.column();
            if col_idx < 0 {
                // ROWID constraint — we don't handle these specially
                continue;
            }
            let col_idx = col_idx as usize;
            if col_idx >= table_meta.columns.len() {
                continue;
            }

            let op = constraint.operator();
            let op_code = match constraint_op_to_code(&op) {
                Some(code) => code,
                None => continue,
            };

            let col_meta = &table_meta.columns[col_idx];
            let cost = estimate_cost(col_meta.encoding, &op, row_count, col_meta.cardinality);
            total_cost = total_cost.min(cost);

            // Tell SQLite to pass this constraint value to xFilter
            usage.set_argv_index(argv_idx);
            // Don't omit — let SQLite double-check for safety
            usage.set_omit(false);
            argv_idx += 1;

            idx_str_parts.push(format!("{col_idx}:{op_code}"));
        }

        let constraints_str = idx_str_parts.join(";");
        let idx_str = format!("cols:{col_used:x}|{constraints_str}");

        info.set_idx_str(&idx_str);
        info.set_idx_num((argv_idx - 1) as c_int);
        info.set_estimated_cost(total_cost);
        info.set_estimated_rows(row_count as i64);

        Ok(())
    }

    fn open(&'vtab mut self) -> rusqlite::Result<EggCursor<'vtab>> {
        let table_meta = &self.file_data.tables[self.table_idx];
        let num_cols = table_meta.columns.len();
        let row_count = table_meta.row_count as usize;

        Ok(EggCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            vtab: self,
            current_row: 0,
            row_count,
            columns: vec![None; num_cols],
            row_mask: None,
            _phantom: PhantomData,
        })
    }
}

impl CreateVTab<'_> for EggTab {
    const KIND: VTabKind = VTabKind::Default;
}

/// The cursor for iterating over virtual table rows.
#[repr(C)]
pub struct EggCursor<'vtab> {
    /// Must be first field for SQLite FFI.
    base: ffi::sqlite3_vtab_cursor,
    vtab: *mut EggTab,
    current_row: usize,
    row_count: usize,
    columns: Vec<Option<DecodedColumn>>,
    row_mask: Option<BitVec>,
    _phantom: PhantomData<&'vtab EggTab>,
}

impl<'vtab> EggCursor<'vtab> {
    /// Get a reference to the parent EggTab.
    ///
    /// # Safety
    /// Safe because SQLite guarantees pVtab points to the EggTab that opened
    /// this cursor, and the cursor's lifetime is bounded by the vtab's.
    fn vtab(&self) -> &EggTab {
        unsafe { &*self.vtab }
    }

    /// Advance current_row to the next set bit in row_mask (or next row if no mask).
    fn advance_to_next_match(&mut self) {
        if let Some(ref mask) = self.row_mask {
            while self.current_row < self.row_count && !mask[self.current_row] {
                self.current_row += 1;
            }
        }
    }
}

// Safety: EggCursor has #[repr(C)] with sqlite3_vtab_cursor as the first field.
unsafe impl VTabCursor for EggCursor<'_> {
    fn filter(
        &mut self,
        _idx_num: c_int,
        idx_str: Option<&str>,
        args: &Values<'_>,
    ) -> rusqlite::Result<()> {
        // Clone Arc to avoid borrowing self through vtab() — allows later mutation of self
        let (file_data, table_idx) = {
            let vtab = self.vtab();
            (Arc::clone(&vtab.file_data), vtab.table_idx)
        };
        let table_meta = &file_data.tables[table_idx];

        // Parse the query plan
        let (col_used, constraints) = match idx_str {
            Some(s) => parse_idx_str(s),
            None => (u64::MAX, Vec::new()),
        };

        // Determine which columns to decode
        let num_cols = table_meta.columns.len();
        let mut need_decode = vec![false; num_cols];

        // Columns requested by SQLite
        for (i, flag) in need_decode.iter_mut().enumerate() {
            if i < 64 {
                if col_used & (1u64 << i) != 0 {
                    *flag = true;
                }
            } else {
                // col_used only covers first 64 columns; decode the rest
                *flag = true;
            }
        }

        // Columns needed for filter constraints
        for &(col_idx, _) in &constraints {
            if col_idx < num_cols {
                need_decode[col_idx] = true;
            }
        }

        // Collect filter argument values up front
        let arg_values: Vec<Value> = args.iter().map(|vr| value_ref_to_value(vr)).collect();

        // Apply filter constraints to build row_mask
        let mut combined_mask: Option<BitVec> = None;
        let mut arg_idx = 0usize;

        for &(col_idx, op_code) in &constraints {
            if col_idx >= num_cols {
                arg_idx += 1;
                continue;
            }

            let col_meta = &table_meta.columns[col_idx];
            let filter_op = code_to_filter_op(op_code);

            // Every constraint consumes one arg (IS NULL/IS NOT NULL get a NULL value)
            let filter_value = arg_values.get(arg_idx).cloned().unwrap_or(Value::Null);
            arg_idx += 1;

            // Read the column block for filtering
            let (null_bitmap, encoded_bytes) = read_column_block_from_parts_with_key(
                &file_data.data,
                file_data.header.yolk_offset,
                col_meta,
                table_meta.row_count,
                file_data.encryption_key.as_deref(),
            )
            .map_err(|e| Error::ModuleError(format!("failed to read column block: {e}")))?;

            let row_mask_part = match filter_op {
                FilterOp::IsNull => {
                    // Return null bitmap directly (1=null=match)
                    null_bitmap.clone()
                }
                FilterOp::IsNotNull => {
                    // Invert null bitmap (0=present=match)
                    let mut result = null_bitmap.clone();
                    for mut bit in result.iter_mut() {
                        *bit = !*bit;
                    }
                    result
                }
                _ => {
                    // Skip push-down for cross-type comparisons (e.g., Integer col vs Real value)
                    // because our Value ordering is type-based, not numeric.
                    // SQLite will still apply its own filter (omit=false).
                    let col_type = col_meta.sqlite_type;
                    let val_type = filter_value.sqlite_type();
                    let type_compatible = match (col_type, val_type) {
                        // Same type — always compatible
                        _ if val_type == Some(col_type) => true,
                        // Integer/Real are numeric-compatible in SQL
                        (SqliteType::Integer, Some(SqliteType::Real))
                        | (SqliteType::Real, Some(SqliteType::Integer)) => false,
                        // NULL filter value — skip push-down (shouldn't happen, handled above)
                        (_, None) => false,
                        // Different non-numeric types — skip
                        _ => false,
                    };

                    if !type_compatible {
                        continue;
                    }

                    // Run filter on non-null values, then expand to full row space
                    let non_null_count = null_bitmap.iter().filter(|b| !**b).count();
                    let encoder = encoder_for(col_meta.encoding);
                    let non_null_matches = encoder
                        .filter_indices(&encoded_bytes, non_null_count, &filter_op, &filter_value)
                        .map_err(|e| Error::ModuleError(format!("filter_indices failed: {e}")))?;
                    expand_non_null_indices(&non_null_matches, &null_bitmap)
                }
            };

            combined_mask = Some(match combined_mask {
                Some(existing) => {
                    // AND the masks together
                    let mut result = existing;
                    result &= row_mask_part;
                    result
                }
                None => row_mask_part,
            });
        }

        self.row_mask = combined_mask;

        // Decode needed columns in parallel
        use rayon::prelude::*;

        // Initialize all slots to None
        for col_slot in self.columns.iter_mut() {
            *col_slot = None;
        }

        // Collect indices that need decoding
        let needed_indices: Vec<usize> = need_decode
            .iter()
            .enumerate()
            .filter_map(|(i, &needed)| if needed { Some(i) } else { None })
            .collect();

        // Decode needed columns in parallel
        let decoded_results: Result<Vec<(usize, Vec<Value>)>, Error> = needed_indices
            .par_iter()
            .map(|&i| {
                let col_meta = &table_meta.columns[i];
                decode_column_from_parts_with_key(
                    &file_data.data,
                    file_data.header.yolk_offset,
                    col_meta,
                    table_meta.row_count,
                    file_data.encryption_key.as_deref(),
                )
                .map(|decoded| (i, decoded))
                .map_err(|e| Error::ModuleError(format!("failed to decode column {i}: {e}")))
            })
            .collect();

        for (i, decoded) in decoded_results? {
            self.columns[i] = Some(decoded);
        }

        // Start at first matching row
        self.current_row = 0;
        self.advance_to_next_match();

        Ok(())
    }

    fn next(&mut self) -> rusqlite::Result<()> {
        self.current_row += 1;
        self.advance_to_next_match();
        Ok(())
    }

    fn eof(&self) -> bool {
        self.current_row >= self.row_count
    }

    fn column(&self, ctx: &mut Context, col: c_int) -> rusqlite::Result<()> {
        let col_idx = col as usize;

        let value = if let Some(Some(ref decoded)) = self.columns.get(col_idx) {
            if self.current_row < decoded.len() {
                &decoded[self.current_row]
            } else {
                &Value::Null
            }
        } else {
            &Value::Null
        };

        match value {
            Value::Null => ctx.set_result(&rusqlite::types::Null),
            Value::Integer(i) => ctx.set_result(i),
            Value::Real(f) => ctx.set_result(f),
            Value::Text(s) => ctx.set_result(&s.as_str()),
            Value::Blob(b) => ctx.set_result(&b.as_slice()),
        }
    }

    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok(self.current_row as i64)
    }
}

/// Return the module descriptor for the egg virtual table.
pub fn egg_module() -> &'static Module<'static, EggTab> {
    read_only_module::<EggTab>()
}

/// Register the egg virtual table module on a connection.
pub fn register_egg_module(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.create_module("egg", egg_module(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggdb_core::{
        write_egg, ColumnInput, Encoding, SqliteType, TableInput, Value, WriteOptions,
    };
    use std::io::Write;

    /// Helper: create a test .egg file in a temp dir and return the path.
    fn write_test_egg(tables: &[TableInput]) -> (tempfile::TempDir, std::path::PathBuf) {
        let data = write_egg(tables, &WriteOptions::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.egg");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&data).unwrap();
        (dir, path)
    }

    /// Helper: create a ColumnInput from full column values.
    fn make_column(
        name: &str,
        sqlite_type: SqliteType,
        values: &[Value],
        encoding: Encoding,
    ) -> ColumnInput {
        let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
        let non_null: Vec<Value> = values.iter().filter(|v| !v.is_null()).cloned().collect();
        let encoder = encoder_for(encoding);
        let encoded = encoder.encode(&non_null);
        let cardinality = {
            let mut set = std::collections::HashSet::new();
            for v in &non_null {
                set.insert(v.clone());
            }
            set.len() as u64
        };
        let null_count = values.iter().filter(|v| v.is_null()).count() as u64;
        let min_value = non_null.iter().min().cloned();
        let max_value = non_null.iter().max().cloned();

        ColumnInput {
            name: name.into(),
            sqlite_type,
            encoding,
            cardinality,
            null_count,
            null_bitmap,
            encoded_values: encoded,
            dictionary: None,
            min_value,
            max_value,
            pre_compressed: None,
        }
    }

    #[test]
    fn basic_select_star() {
        let values = vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)];
        let table = TableInput {
            table_name: "nums".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE nums (id INTEGER)".into(),
            columns: vec![make_column(
                "id",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nums USING egg('{}', 'nums')",
            path.display()
        ))
        .unwrap();

        let mut stmt = conn.prepare("SELECT id FROM nums").unwrap();
        let rows: Vec<i64> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![1, 2, 3]);
    }

    #[test]
    fn select_with_nulls() {
        let values = vec![Value::Integer(10), Value::Null, Value::Integer(30)];
        let table = TableInput {
            table_name: "t".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE t (val INTEGER)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE t USING egg('{}', 't')",
            path.display()
        ))
        .unwrap();

        let mut stmt = conn.prepare("SELECT val FROM t").unwrap();
        let rows: Vec<Option<i64>> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![Some(10), None, Some(30)]);
    }

    #[test]
    fn empty_table() {
        let col = ColumnInput {
            name: "id".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Const,
            cardinality: 0,
            null_count: 0,
            null_bitmap: bitvec![],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        };
        let table = TableInput {
            table_name: "empty".into(),
            row_count: 0,
            original_ddl: "CREATE TABLE empty (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE empty USING egg('{}', 'empty')",
            path.display()
        ))
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM empty", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn multi_column_table() {
        let ids: Vec<Value> = (1..=5).map(Value::Integer).collect();
        let names: Vec<Value> = (1..=5).map(|i| Value::Text(format!("user_{i}"))).collect();

        let table = TableInput {
            table_name: "users".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE users (id INTEGER, name TEXT)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &ids, Encoding::Delta),
                make_column("name", SqliteType::Text, &names, Encoding::Raw),
            ],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE users USING egg('{}', 'users')",
            path.display()
        ))
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT id, name FROM users ORDER BY id")
            .unwrap();
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], (1, "user_1".to_string()));
        assert_eq!(rows[4], (5, "user_5".to_string()));
    }

    #[test]
    fn select_specific_columns() {
        let ids: Vec<Value> = (1..=3).map(Value::Integer).collect();
        let names: Vec<Value> = vec![
            Value::Text("alice".into()),
            Value::Text("bob".into()),
            Value::Text("carol".into()),
        ];
        let scores: Vec<Value> = vec![Value::Real(95.5), Value::Real(87.0), Value::Real(92.3)];

        let table = TableInput {
            table_name: "students".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE students (id INTEGER, name TEXT, score REAL)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &ids, Encoding::Delta),
                make_column("name", SqliteType::Text, &names, Encoding::Raw),
                make_column("score", SqliteType::Real, &scores, Encoding::Raw),
            ],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE students USING egg('{}', 'students')",
            path.display()
        ))
        .unwrap();

        // Only select name column
        let mut stmt = conn.prepare("SELECT name FROM students").unwrap();
        let names_result: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            names_result,
            vec!["alice".to_string(), "bob".to_string(), "carol".to_string()]
        );
    }

    #[test]
    fn where_clause_equality() {
        let names: Vec<Value> = vec![
            Value::Text("alice".into()),
            Value::Text("bob".into()),
            Value::Text("carol".into()),
            Value::Text("alice".into()),
        ];

        let table = TableInput {
            table_name: "people".into(),
            row_count: 4,
            original_ddl: "CREATE TABLE people (name TEXT)".into(),
            columns: vec![make_column("name", SqliteType::Text, &names, Encoding::Raw)],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE people USING egg('{}', 'people')",
            path.display()
        ))
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM people WHERE name = 'alice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn where_clause_range() {
        let values: Vec<Value> = (1..=10).map(Value::Integer).collect();

        let table = TableInput {
            table_name: "nums".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE nums (n INTEGER)".into(),
            columns: vec![make_column(
                "n",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nums USING egg('{}', 'nums')",
            path.display()
        ))
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM nums WHERE n > 5", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 5);

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM nums WHERE n >= 5 AND n <= 7",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn where_is_null() {
        let values = vec![
            Value::Integer(1),
            Value::Null,
            Value::Integer(3),
            Value::Null,
        ];

        let table = TableInput {
            table_name: "t".into(),
            row_count: 4,
            original_ddl: "CREATE TABLE t (val INTEGER)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE t USING egg('{}', 't')",
            path.display()
        ))
        .unwrap();

        let null_count: i64 = conn
            .query_row("SELECT count(*) FROM t WHERE val IS NULL", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(null_count, 2);

        let not_null_count: i64 = conn
            .query_row("SELECT count(*) FROM t WHERE val IS NOT NULL", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(not_null_count, 2);
    }

    #[test]
    fn multiple_tables_in_egg() {
        let t1_values = vec![Value::Integer(1), Value::Integer(2)];
        let t2_values = vec![
            Value::Text("a".into()),
            Value::Text("b".into()),
            Value::Text("c".into()),
        ];

        let tables = vec![
            TableInput {
                table_name: "t1".into(),
                row_count: 2,
                original_ddl: "CREATE TABLE t1 (id INTEGER)".into(),
                columns: vec![make_column(
                    "id",
                    SqliteType::Integer,
                    &t1_values,
                    Encoding::Raw,
                )],
                index_definitions: Vec::new(),
            },
            TableInput {
                table_name: "t2".into(),
                row_count: 3,
                original_ddl: "CREATE TABLE t2 (name TEXT)".into(),
                columns: vec![make_column(
                    "name",
                    SqliteType::Text,
                    &t2_values,
                    Encoding::Raw,
                )],
                index_definitions: Vec::new(),
            },
        ];
        let (_dir, path) = write_test_egg(&tables);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();

        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE t1 USING egg('{}', 't1');\
             CREATE VIRTUAL TABLE t2 USING egg('{}', 't2');",
            path.display(),
            path.display()
        ))
        .unwrap();

        let c1: i64 = conn
            .query_row("SELECT count(*) FROM t1", [], |row| row.get(0))
            .unwrap();
        let c2: i64 = conn
            .query_row("SELECT count(*) FROM t2", [], |row| row.get(0))
            .unwrap();
        assert_eq!(c1, 2);
        assert_eq!(c2, 3);
    }

    #[test]
    fn all_encoding_types() {
        // Test that each encoding type works through the vtable
        let const_values = vec![Value::Integer(42); 10];
        let bool_values: Vec<Value> = (0..10).map(|i| Value::Integer(i % 2)).collect();
        let delta_values: Vec<Value> = (0..10).map(Value::Integer).collect();
        let rle_values: Vec<Value> = (0..10)
            .map(|i| Value::Text(if i < 5 { "a" } else { "b" }.into()))
            .collect();
        let _dict_values: Vec<Value> = (0..300)
            .map(|i| Value::Text(["x", "y", "z"][i % 3].into()))
            .collect();
        let raw_values: Vec<Value> = (0..10)
            .map(|i| Value::Text(format!("unique_{i}")))
            .collect();

        let table = TableInput {
            table_name: "enc".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE enc (c INTEGER, b INTEGER, d INTEGER, r TEXT, ra TEXT)"
                .into(),
            columns: vec![
                make_column("c", SqliteType::Integer, &const_values, Encoding::Const),
                make_column("b", SqliteType::Integer, &bool_values, Encoding::BoolBitmap),
                make_column("d", SqliteType::Integer, &delta_values, Encoding::Delta),
                make_column("r", SqliteType::Text, &rle_values, Encoding::Rle),
                make_column("ra", SqliteType::Text, &raw_values, Encoding::Raw),
            ],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE enc USING egg('{}', 'enc')",
            path.display()
        ))
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM enc", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 10);

        // Verify const column
        let mut stmt = conn.prepare("SELECT c FROM enc").unwrap();
        let vals: Vec<i64> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(vals.iter().all(|&v| v == 42));

        // Verify delta column
        let mut stmt = conn.prepare("SELECT d FROM enc ORDER BY rowid").unwrap();
        let vals: Vec<i64> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(vals, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn single_row_table() {
        let table = TableInput {
            table_name: "single".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE single (val TEXT)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Text,
                &[Value::Text("hello".into())],
                Encoding::Const,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE single USING egg('{}', 'single')",
            path.display()
        ))
        .unwrap();

        let val: String = conn
            .query_row("SELECT val FROM single", [], |row| row.get(0))
            .unwrap();
        assert_eq!(val, "hello");
    }

    #[test]
    fn table_not_found_error() {
        let table = TableInput {
            table_name: "real".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE real (id INTEGER)".into(),
            columns: vec![make_column(
                "id",
                SqliteType::Integer,
                &[Value::Integer(1)],
                Encoding::Const,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();

        let result = conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE t USING egg('{}', 'nonexistent')",
            path.display()
        ));
        assert!(result.is_err());
    }

    #[test]
    fn blob_column() {
        let values = vec![
            Value::Blob(vec![1, 2, 3]),
            Value::Blob(vec![4, 5]),
            Value::Blob(vec![]),
        ];

        let table = TableInput {
            table_name: "blobs".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE blobs (data BLOB)".into(),
            columns: vec![make_column(
                "data",
                SqliteType::Blob,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE blobs USING egg('{}', 'blobs')",
            path.display()
        ))
        .unwrap();

        let mut stmt = conn.prepare("SELECT data FROM blobs").unwrap();
        let rows: Vec<Vec<u8>> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows[0], vec![1, 2, 3]);
        assert_eq!(rows[1], vec![4, 5]);
        assert_eq!(rows[2], Vec::<u8>::new());
    }

    #[test]
    fn all_null_column() {
        let col = ColumnInput {
            name: "nul".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Const,
            cardinality: 0,
            null_count: 5,
            null_bitmap: bitvec![1; 5],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        };

        let table = TableInput {
            table_name: "nulls".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE nulls (nul INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nulls USING egg('{}', 'nulls')",
            path.display()
        ))
        .unwrap();

        let null_count: i64 = conn
            .query_row("SELECT count(*) FROM nulls WHERE nul IS NULL", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(null_count, 5);
    }

    #[test]
    fn join_egg_with_sqlite_table() {
        let ids: Vec<Value> = (1..=3).map(Value::Integer).collect();
        let names: Vec<Value> = vec![
            Value::Text("alice".into()),
            Value::Text("bob".into()),
            Value::Text("carol".into()),
        ];

        let table = TableInput {
            table_name: "users".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE users (id INTEGER, name TEXT)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &ids, Encoding::Delta),
                make_column("name", SqliteType::Text, &names, Encoding::Raw),
            ],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();

        // Create regular SQLite table
        conn.execute_batch(
            "CREATE TABLE orders (id INTEGER, user_id INTEGER, total REAL);\
             INSERT INTO orders VALUES (1, 1, 99.99);\
             INSERT INTO orders VALUES (2, 2, 50.00);\
             INSERT INTO orders VALUES (3, 1, 25.00);",
        )
        .unwrap();

        // Create egg vtable
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE users USING egg('{}', 'users')",
            path.display()
        ))
        .unwrap();

        // Join egg vtable with regular table
        let mut stmt = conn
            .prepare(
                "SELECT u.name, sum(o.total) as total \
                 FROM users u JOIN orders o ON u.id = o.user_id \
                 GROUP BY u.name ORDER BY u.name",
            )
            .unwrap();
        let rows: Vec<(String, f64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "alice");
        assert!((rows[0].1 - 124.99).abs() < 0.001);
        assert_eq!(rows[1].0, "bob");
        assert!((rows[1].1 - 50.0).abs() < 0.001);
    }

    #[test]
    fn join_two_egg_tables() {
        let user_ids: Vec<Value> = (1..=3).map(Value::Integer).collect();
        let user_names: Vec<Value> = vec![
            Value::Text("alice".into()),
            Value::Text("bob".into()),
            Value::Text("carol".into()),
        ];
        let order_ids: Vec<Value> = (1..=4).map(Value::Integer).collect();
        let order_user_ids: Vec<Value> = vec![
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(1),
            Value::Integer(3),
        ];
        let order_totals: Vec<Value> = vec![
            Value::Real(10.0),
            Value::Real(20.0),
            Value::Real(30.0),
            Value::Real(40.0),
        ];

        let tables = vec![
            TableInput {
                table_name: "users".into(),
                row_count: 3,
                original_ddl: "CREATE TABLE users (id INTEGER, name TEXT)".into(),
                columns: vec![
                    make_column("id", SqliteType::Integer, &user_ids, Encoding::Delta),
                    make_column("name", SqliteType::Text, &user_names, Encoding::Raw),
                ],
                index_definitions: Vec::new(),
            },
            TableInput {
                table_name: "orders".into(),
                row_count: 4,
                original_ddl: "CREATE TABLE orders (id INTEGER, user_id INTEGER, total REAL)"
                    .into(),
                columns: vec![
                    make_column("id", SqliteType::Integer, &order_ids, Encoding::Delta),
                    make_column(
                        "user_id",
                        SqliteType::Integer,
                        &order_user_ids,
                        Encoding::Raw,
                    ),
                    make_column("total", SqliteType::Real, &order_totals, Encoding::Raw),
                ],
                index_definitions: Vec::new(),
            },
        ];
        let (_dir, path) = write_test_egg(&tables);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE users USING egg('{}', 'users');\
             CREATE VIRTUAL TABLE orders USING egg('{}', 'orders');",
            path.display(),
            path.display()
        ))
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM users u JOIN orders o ON u.id = o.user_id",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn unicode_text() {
        let values = vec![
            Value::Text("hello world".into()),
            Value::Text("\u{1F423}".into()), // hatching chick emoji
            Value::Text("\u{00E9}\u{00F1}\u{00FC}".into()), // accented chars
            Value::Text("\u{4E16}\u{754C}".into()), // Chinese characters
            Value::Text(String::new()),      // empty string
        ];

        let table = TableInput {
            table_name: "texts".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE texts (val TEXT)".into(),
            columns: vec![make_column("val", SqliteType::Text, &values, Encoding::Raw)],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE texts USING egg('{}', 'texts')",
            path.display()
        ))
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT val FROM texts ORDER BY rowid")
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows[0], "hello world");
        assert_eq!(rows[1], "\u{1F423}");
        assert_eq!(rows[2], "\u{00E9}\u{00F1}\u{00FC}");
        assert_eq!(rows[3], "\u{4E16}\u{754C}");
        assert_eq!(rows[4], "");
    }

    #[test]
    fn dict_encoding_through_vtable() {
        // Dict encoding needs a low-cardinality column with at least 1% ratio
        let statuses = ["active", "inactive", "pending"];
        let values: Vec<Value> = (0..300)
            .map(|i| Value::Text(statuses[i % 3].into()))
            .collect();

        let table = TableInput {
            table_name: "users".into(),
            row_count: 300,
            original_ddl: "CREATE TABLE users (status TEXT)".into(),
            columns: vec![make_column(
                "status",
                SqliteType::Text,
                &values,
                Encoding::Dict,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE users USING egg('{}', 'users')",
            path.display()
        ))
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM users", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 300);

        // Filter on dict-encoded column
        let active_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM users WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_count, 100);

        // LIKE on dict column
        let like_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM users WHERE status LIKE 'in%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(like_count, 100);
    }

    #[test]
    fn aggregation_queries() {
        let values: Vec<Value> = (1..=100).map(Value::Integer).collect();

        let table = TableInput {
            table_name: "nums".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE nums (n INTEGER)".into(),
            columns: vec![make_column(
                "n",
                SqliteType::Integer,
                &values,
                Encoding::Delta,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nums USING egg('{}', 'nums')",
            path.display()
        ))
        .unwrap();

        let sum: i64 = conn
            .query_row("SELECT sum(n) FROM nums", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sum, 5050);

        let avg: f64 = conn
            .query_row("SELECT avg(n) FROM nums", [], |row| row.get(0))
            .unwrap();
        assert!((avg - 50.5).abs() < 0.001);

        let min: i64 = conn
            .query_row("SELECT min(n) FROM nums", [], |row| row.get(0))
            .unwrap();
        assert_eq!(min, 1);

        let max: i64 = conn
            .query_row("SELECT max(n) FROM nums", [], |row| row.get(0))
            .unwrap();
        assert_eq!(max, 100);
    }

    #[test]
    fn real_column_values() {
        let values = vec![
            Value::Real(3.14159),
            Value::Real(-273.15),
            Value::Real(0.0),
            Value::Real(f64::INFINITY),
            Value::Real(f64::NEG_INFINITY),
        ];

        let table = TableInput {
            table_name: "reals".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE reals (val REAL)".into(),
            columns: vec![make_column("val", SqliteType::Real, &values, Encoding::Raw)],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE reals USING egg('{}', 'reals')",
            path.display()
        ))
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT val FROM reals ORDER BY rowid")
            .unwrap();
        let rows: Vec<f64> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!((rows[0] - 3.14159).abs() < 1e-10);
        assert!((rows[1] - (-273.15)).abs() < 1e-10);
        assert!((rows[2] - 0.0).abs() < 1e-10);
        assert!(rows[3].is_infinite() && rows[3] > 0.0);
        assert!(rows[4].is_infinite() && rows[4] < 0.0);
    }

    #[test]
    fn mixed_null_and_non_null_with_filter() {
        // Tests the null expansion logic carefully
        let values = vec![
            Value::Integer(1),
            Value::Null,
            Value::Integer(2),
            Value::Null,
            Value::Integer(1),
            Value::Integer(3),
            Value::Null,
            Value::Integer(1),
        ];

        let table = TableInput {
            table_name: "mixed".into(),
            row_count: 8,
            original_ddl: "CREATE TABLE mixed (val INTEGER)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE mixed USING egg('{}', 'mixed')",
            path.display()
        ))
        .unwrap();

        // EQ filter — should find value=1 at rows 0, 4, 7
        let eq_count: i64 = conn
            .query_row("SELECT count(*) FROM mixed WHERE val = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(eq_count, 3);

        // NULL never matches equality
        let eq_null_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM mixed WHERE val = 1 OR val IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(eq_null_count, 6); // 3 ones + 3 nulls

        // GT filter
        let gt_count: i64 = conn
            .query_row("SELECT count(*) FROM mixed WHERE val > 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(gt_count, 2); // 2 and 3

        // NOT NULL
        let not_null: i64 = conn
            .query_row(
                "SELECT count(*) FROM mixed WHERE val IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(not_null, 5);
    }

    #[test]
    fn rle_encoding_filter() {
        // RLE with large runs — verify filter works correctly
        let mut values = Vec::new();
        for _ in 0..50 {
            values.push(Value::Text("alpha".into()));
        }
        for _ in 0..30 {
            values.push(Value::Text("beta".into()));
        }
        for _ in 0..20 {
            values.push(Value::Text("gamma".into()));
        }

        let table = TableInput {
            table_name: "runs".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE runs (group_name TEXT)".into(),
            columns: vec![make_column(
                "group_name",
                SqliteType::Text,
                &values,
                Encoding::Rle,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE runs USING egg('{}', 'runs')",
            path.display()
        ))
        .unwrap();

        let alpha_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM runs WHERE group_name = 'alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alpha_count, 50);

        let beta_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM runs WHERE group_name = 'beta'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(beta_count, 30);

        let ne_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM runs WHERE group_name != 'alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ne_count, 50);
    }

    #[test]
    fn const_encoding_filter() {
        let values = vec![Value::Integer(42); 100];

        let table = TableInput {
            table_name: "consts".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE consts (val INTEGER)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Integer,
                &values,
                Encoding::Const,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE consts USING egg('{}', 'consts')",
            path.display()
        ))
        .unwrap();

        let match_count: i64 = conn
            .query_row("SELECT count(*) FROM consts WHERE val = 42", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(match_count, 100);

        let no_match: i64 = conn
            .query_row("SELECT count(*) FROM consts WHERE val = 99", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(no_match, 0);
    }

    #[test]
    fn subquery_support() {
        let values: Vec<Value> = (1..=10).map(Value::Integer).collect();

        let table = TableInput {
            table_name: "nums".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE nums (n INTEGER)".into(),
            columns: vec![make_column(
                "n",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nums USING egg('{}', 'nums')",
            path.display()
        ))
        .unwrap();

        let result: i64 = conn
            .query_row(
                "SELECT count(*) FROM nums WHERE n > (SELECT avg(n) FROM nums)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result, 5); // values 6-10 are > avg(5.5)
    }

    #[test]
    fn many_columns() {
        // Test with more than a few columns
        let row_count = 10usize;
        let num_cols = 20;
        let mut columns = Vec::new();
        let mut ddl_parts = Vec::new();

        for c in 0..num_cols {
            let values: Vec<Value> = (0..row_count)
                .map(|i| Value::Integer((c * row_count + i) as i64))
                .collect();
            columns.push(make_column(
                &format!("col_{c}"),
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            ));
            ddl_parts.push(format!("col_{c} INTEGER"));
        }

        let table = TableInput {
            table_name: "wide".into(),
            row_count: row_count as u64,
            original_ddl: format!("CREATE TABLE wide ({})", ddl_parts.join(", ")),
            columns,
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE wide USING egg('{}', 'wide')",
            path.display()
        ))
        .unwrap();

        // Select first and last columns only
        let mut stmt = conn
            .prepare("SELECT col_0, col_19 FROM wide ORDER BY rowid LIMIT 1")
            .unwrap();
        let (first, last): (i64, i64) = stmt
            .query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(first, 0);
        assert_eq!(last, 190);
    }

    /// Round-trip query equivalence: lay a SQLite DB, query via vtable, compare with original.
    #[test]
    fn round_trip_query_equivalence() {
        // Build a SQLite DB in memory
        let source = rusqlite::Connection::open_in_memory().unwrap();
        source
            .execute_batch(
                "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL, in_stock INTEGER);\
                 INSERT INTO products VALUES (1, 'Widget', 9.99, 1);\
                 INSERT INTO products VALUES (2, 'Gadget', 24.99, 0);\
                 INSERT INTO products VALUES (3, 'Gizmo', 14.50, 1);\
                 INSERT INTO products VALUES (4, 'Doohickey', NULL, 1);\
                 INSERT INTO products VALUES (5, NULL, 5.00, 0);",
            )
            .unwrap();

        // Lay it to an egg file using eggdb-io
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("source.db");
        let egg_path = dir.path().join("test.egg");

        // Write source DB to disk
        source
            .execute_batch(&format!("VACUUM INTO '{}'", db_path.display()))
            .unwrap();
        drop(source);

        // Use eggdb-io to lay
        let source_conn = rusqlite::Connection::open(&db_path).unwrap();
        let egg_data =
            eggdb_io::writer::lay(&source_conn, &eggdb_io::writer::LayOptions::default()).unwrap();
        std::fs::write(&egg_path, &egg_data).unwrap();
        drop(source_conn);

        // Query via vtable
        let vtab_conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&vtab_conn).unwrap();
        vtab_conn
            .execute_batch(&format!(
                "CREATE VIRTUAL TABLE products USING egg('{}', 'products')",
                egg_path.display()
            ))
            .unwrap();

        // Also hatch back to compare
        let hatch_conn = rusqlite::Connection::open_in_memory().unwrap();
        eggdb_io::reader::hatch(
            &egg_data,
            &hatch_conn,
            &eggdb_io::reader::HatchOptions::default(),
        )
        .unwrap();

        // Compare query results
        let queries = [
            "SELECT count(*) FROM products",
            "SELECT name FROM products WHERE price > 10 ORDER BY name",
            "SELECT count(*) FROM products WHERE in_stock = 1",
            "SELECT count(*) FROM products WHERE name IS NULL",
            "SELECT count(*) FROM products WHERE price IS NOT NULL",
        ];

        for query in &queries {
            let vtab_result: Vec<String> = {
                let mut stmt = vtab_conn.prepare(query).unwrap();
                stmt.query_map([], |row| {
                    let val: rusqlite::types::Value = row.get(0).unwrap();
                    Ok(format!("{val:?}"))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
            };

            let hatch_result: Vec<String> = {
                let mut stmt = hatch_conn.prepare(query).unwrap();
                stmt.query_map([], |row| {
                    let val: rusqlite::types::Value = row.get(0).unwrap();
                    Ok(format!("{val:?}"))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
            };

            assert_eq!(
                vtab_result, hatch_result,
                "Query results differ for: {query}"
            );
        }
    }

    /// Helper: create an encrypted test .egg file and return (dir, path).
    fn write_encrypted_test_egg(
        tables: &[TableInput],
        passphrase: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let salt = eggdb_core::generate_salt();
        let key = eggdb_core::derive_key(passphrase, &salt).unwrap();
        let opts = WriteOptions {
            flags: eggdb_core::HeaderFlags::new(true, false),
            encryption_key: Some(key),
            encryption_salt: Some(salt),
            ..WriteOptions::default()
        };
        let data = write_egg(tables, &opts).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("encrypted.egg");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&data).unwrap();
        (dir, path)
    }

    #[test]
    fn encrypted_vtable_query() {
        let passphrase = "vtable-test-pass";
        let values = vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)];
        let table = TableInput {
            table_name: "nums".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE nums (id INTEGER)".into(),
            columns: vec![make_column(
                "id",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_encrypted_test_egg(&[table], passphrase);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nums USING egg('{}', 'nums', '{}')",
            path.display(),
            passphrase
        ))
        .unwrap();

        let mut stmt = conn.prepare("SELECT id FROM nums").unwrap();
        let rows: Vec<i64> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![1, 2, 3]);
    }

    #[test]
    fn encrypted_vtable_no_passphrase_fails() {
        let passphrase = "secret";
        let values = vec![Value::Integer(42)];
        let table = TableInput {
            table_name: "t".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE t (val INTEGER)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Integer,
                &values,
                Encoding::Const,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_encrypted_test_egg(&[table], passphrase);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();

        // Creating vtable without passphrase on encrypted file should fail
        let result = conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE t USING egg('{}', 't')",
            path.display()
        ));
        assert!(result.is_err());
    }

    #[test]
    fn encrypted_vtable_wrong_passphrase() {
        let passphrase = "correct-pass";
        let values = vec![Value::Integer(10), Value::Integer(20)];
        let table = TableInput {
            table_name: "data".into(),
            row_count: 2,
            original_ddl: "CREATE TABLE data (val INTEGER)".into(),
            columns: vec![make_column(
                "val",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_encrypted_test_egg(&[table], passphrase);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();

        // Creating vtable with wrong passphrase should succeed (key derivation works)
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE data USING egg('{}', 'data', 'wrong-pass')",
            path.display()
        ))
        .unwrap();

        // But querying should fail because decryption will fail
        let result = conn.query_row("SELECT val FROM data LIMIT 1", [], |row| {
            row.get::<_, i64>(0)
        });
        assert!(result.is_err());
    }

    #[test]
    fn encrypted_vtable_with_filter() {
        let passphrase = "filter-test";
        let values: Vec<Value> = (1..=10).map(Value::Integer).collect();
        let table = TableInput {
            table_name: "nums".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE nums (n INTEGER)".into(),
            columns: vec![make_column(
                "n",
                SqliteType::Integer,
                &values,
                Encoding::Raw,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_encrypted_test_egg(&[table], passphrase);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE nums USING egg('{}', 'nums', '{}')",
            path.display(),
            passphrase
        ))
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM nums WHERE n > 5", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn unencrypted_vtable_with_passphrase_fails() {
        // Providing a passphrase for a non-encrypted file should error
        let values = vec![Value::Integer(1)];
        let table = TableInput {
            table_name: "t".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![make_column(
                "id",
                SqliteType::Integer,
                &values,
                Encoding::Const,
            )],
            index_definitions: Vec::new(),
        };
        let (_dir, path) = write_test_egg(&[table]);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        register_egg_module(&conn).unwrap();

        let result = conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE t USING egg('{}', 't', 'some-pass')",
            path.display()
        ));
        assert!(result.is_err());
    }
}
