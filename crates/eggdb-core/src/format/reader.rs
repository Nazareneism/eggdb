use bitvec::prelude::*;
use zeroize::Zeroizing;

use crate::encoding::bool_bitmap::BoolBitmapEncoder;
use crate::encoding::constant::ConstEncoder;
use crate::encoding::delta::DeltaEncoder;
use crate::encoding::dictionary::DictEncoder;
use crate::encoding::null_bitmap;
use crate::encoding::raw::RawEncoder;
use crate::encoding::rle::RleEncoder;
use crate::encoding::{ColumnEncoder, Encoding};
use crate::format::albumen::{self, ColumnMeta, TableMeta};
use crate::format::header::Header;
use crate::format::membrane::{self, Membrane};
use crate::format::FormatError;
use crate::value::Value;

/// Read and decompress a column block from raw file data.
///
/// This is the standalone version of `EggReader::read_column_block` that operates
/// on raw parts rather than requiring an `EggReader` instance. Useful when you own
/// the file data (e.g., `Vec<u8>`) and have already parsed the metadata.
///
/// If `encryption_key` is provided, the stored block is decrypted before decompression.
///
/// Returns `(null_bitmap, encoded_values)` where `null_bitmap` has `row_count` bits
/// and `encoded_values` contains the encoder output for non-null values.
pub fn read_column_block_from_parts(
    file_data: &[u8],
    yolk_section_offset: u64,
    col_meta: &ColumnMeta,
    row_count: u64,
) -> Result<(BitVec, Vec<u8>), FormatError> {
    read_column_block_from_parts_with_key(file_data, yolk_section_offset, col_meta, row_count, None)
}

/// Read and decompress a column block, optionally decrypting first.
pub fn read_column_block_from_parts_with_key(
    file_data: &[u8],
    yolk_section_offset: u64,
    col_meta: &ColumnMeta,
    row_count: u64,
    encryption_key: Option<&[u8; 32]>,
) -> Result<(BitVec, Vec<u8>), FormatError> {
    let row_count = row_count as usize;
    let block = crate::format::checked_range(
        yolk_section_offset,
        col_meta.yolk_offset,
        col_meta.yolk_length,
        file_data.len(),
    )
    .ok_or_else(|| {
        FormatError::InvalidOffset(format!(
            "column block at yolk {yolk_section_offset} + {} (length {}) exceeds file size {}",
            col_meta.yolk_offset,
            col_meta.yolk_length,
            file_data.len()
        ))
    })?;

    let stored_data = &file_data[block];

    // Handle empty blocks (0 rows)
    if stored_data.is_empty() {
        return Ok((bitvec![], Vec::new()));
    }

    // Sanity check: reject obviously excessive uncompressed sizes (max 4 GB per column block)
    const MAX_UNCOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;
    if col_meta.uncompressed_size > MAX_UNCOMPRESSED_SIZE {
        return Err(FormatError::ColumnData(format!(
            "uncompressed_size {} exceeds maximum {} bytes",
            col_meta.uncompressed_size, MAX_UNCOMPRESSED_SIZE
        )));
    }

    // Decrypt if encrypted
    let compressed = if let Some(key) = encryption_key {
        crate::format::crypto::decrypt_block(stored_data, key)?
    } else {
        stored_data.to_vec()
    };

    // Decompress
    let decompressed = zstd::bulk::decompress(&compressed, col_meta.uncompressed_size as usize)
        .map_err(|e| FormatError::ZstdDecompress(e.to_string()))?;

    // Split into null bitmap bytes and encoded values
    let bitmap_byte_count = row_count.div_ceil(8);

    if decompressed.len() < bitmap_byte_count {
        return Err(FormatError::ColumnData(format!(
            "decompressed block too short: {} bytes, need at least {} for null bitmap ({} rows)",
            decompressed.len(),
            bitmap_byte_count,
            row_count
        )));
    }

    let bitmap_bytes = &decompressed[..bitmap_byte_count];
    let encoded_values = decompressed[bitmap_byte_count..].to_vec();
    let null_bitmap = null_bitmap::decode(bitmap_bytes, row_count);

    Ok((null_bitmap, encoded_values))
}

/// Decode a full column (including null interleaving) from raw file data.
///
/// This is the standalone version of `EggReader::decode_column` that operates
/// on raw parts rather than requiring an `EggReader` instance.
pub fn decode_column_from_parts(
    file_data: &[u8],
    yolk_section_offset: u64,
    col_meta: &ColumnMeta,
    row_count: u64,
) -> Result<Vec<Value>, FormatError> {
    decode_column_from_parts_with_key(file_data, yolk_section_offset, col_meta, row_count, None)
}

/// Decode a full column, optionally decrypting first.
pub fn decode_column_from_parts_with_key(
    file_data: &[u8],
    yolk_section_offset: u64,
    col_meta: &ColumnMeta,
    row_count: u64,
    encryption_key: Option<&[u8; 32]>,
) -> Result<Vec<Value>, FormatError> {
    if row_count == 0 {
        return Ok(Vec::new());
    }

    let (null_bitmap, encoded_values) = read_column_block_from_parts_with_key(
        file_data,
        yolk_section_offset,
        col_meta,
        row_count,
        encryption_key,
    )?;
    let non_null_count = null_bitmap::non_null_count(&null_bitmap);

    let encoder = encoder_for(col_meta.encoding);
    let non_null_values = encoder.decode(&encoded_values, non_null_count)?;

    null_bitmap::interleave_nulls(&non_null_values, &null_bitmap)
}

/// Decode multiple columns of a table in parallel.
///
/// Each column block is independently decompressed and decoded using rayon's
/// parallel iterator. Returns one `Vec<Value>` per column, in the same order
/// as the input `columns` slice.
#[cfg(feature = "parallel")]
pub fn decode_columns_parallel(
    file_data: &[u8],
    yolk_section_offset: u64,
    columns: &[ColumnMeta],
    row_count: u64,
    encryption_key: Option<&[u8; 32]>,
) -> Result<Vec<Vec<Value>>, FormatError> {
    use rayon::prelude::*;
    columns
        .par_iter()
        .map(|col_meta| {
            decode_column_from_parts_with_key(
                file_data,
                yolk_section_offset,
                col_meta,
                row_count,
                encryption_key,
            )
        })
        .collect()
}

/// A reader for `.egg` file data held in memory.
///
/// Parses the header, albumen (metadata), and membrane (integrity) sections
/// on construction. Column data in the yolk is decoded on demand.
#[derive(Debug)]
pub struct EggReader<'a> {
    data: &'a [u8],
    header: Header,
    tables: Vec<TableMeta>,
    membrane: Membrane,
    encryption_key: Option<Zeroizing<[u8; 32]>>,
}

impl<'a> EggReader<'a> {
    /// Open an `.egg` file from a byte slice.
    ///
    /// Parses the header, validates magic and version, then deserializes
    /// the albumen and membrane sections.
    pub fn open(data: &'a [u8]) -> Result<Self, FormatError> {
        Self::open_internal(data, None)
    }

    /// Open an encrypted `.egg` file using a passphrase.
    ///
    /// Reads the salt from the yolk section header, derives the encryption
    /// key using Argon2id, and stores it for decrypting column blocks.
    pub fn open_with_passphrase(data: &'a [u8], passphrase: &str) -> Result<Self, FormatError> {
        let header = Header::from_bytes(data)?;

        if !header.flags.has_encryption() {
            return Err(FormatError::Encryption(
                "file is not encrypted but passphrase was provided".into(),
            ));
        }

        // Read salt from the start of the yolk section
        let salt_size = crate::format::crypto::SALT_SIZE;
        let salt_range =
            crate::format::checked_range(header.yolk_offset, 0, salt_size as u64, data.len())
                .ok_or_else(|| {
                    FormatError::InvalidOffset(format!(
                        "yolk section too short for encryption salt: yolk at {}, file is {} bytes",
                        header.yolk_offset,
                        data.len()
                    ))
                })?;

        let mut salt = [0u8; crate::format::crypto::SALT_SIZE];
        salt.copy_from_slice(&data[salt_range]);

        let key = crate::format::crypto::derive_key(passphrase, &salt)?;

        Self::open_internal(data, Some(key))
    }

    fn open_internal(
        data: &'a [u8],
        encryption_key: Option<Zeroizing<[u8; 32]>>,
    ) -> Result<Self, FormatError> {
        let header = Header::from_bytes(data)?;

        let albumen_start = header.albumen_offset as usize;
        let albumen_end = header.yolk_offset as usize;

        if albumen_end > data.len() || albumen_start > albumen_end {
            return Err(FormatError::InvalidOffset(format!(
                "albumen range [{albumen_start}..{albumen_end}] exceeds file size {}",
                data.len()
            )));
        }

        let tables = albumen::deserialize_albumen(&data[albumen_start..albumen_end])?;

        if tables.len() != header.table_count as usize {
            return Err(FormatError::TableCountMismatch {
                expected: header.table_count,
                actual: tables.len() as u32,
            });
        }

        let membrane_start = header.membrane_offset as usize;
        if membrane_start > data.len() {
            return Err(FormatError::InvalidOffset(format!(
                "membrane offset {membrane_start} exceeds file size {}",
                data.len()
            )));
        }

        let membrane_data = membrane::deserialize_membrane(&data[membrane_start..])?;

        Ok(EggReader {
            data,
            header,
            tables,
            membrane: membrane_data,
            encryption_key,
        })
    }

    /// Returns the parsed header.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Returns metadata for all tables.
    #[must_use]
    pub fn tables(&self) -> &[TableMeta] {
        &self.tables
    }

    /// Returns the membrane (integrity metadata).
    #[must_use]
    pub fn membrane(&self) -> &Membrane {
        &self.membrane
    }

    /// Returns the number of tables in the file.
    #[must_use]
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Returns the encryption key if the reader was opened with a passphrase.
    #[must_use]
    pub fn encryption_key(&self) -> Option<&[u8; 32]> {
        self.encryption_key.as_deref()
    }

    /// Read and decompress a single column block from the yolk.
    ///
    /// Returns `(null_bitmap, encoded_values)` where `null_bitmap` has
    /// `row_count` bits and `encoded_values` contains the encoder output
    /// for non-null values.
    pub fn read_column_block(
        &self,
        table_idx: usize,
        col_idx: usize,
    ) -> Result<(BitVec, Vec<u8>), FormatError> {
        let table = self.tables.get(table_idx).ok_or_else(|| {
            FormatError::InvalidOffset(format!("table index {table_idx} out of range"))
        })?;
        let col = table.columns.get(col_idx).ok_or_else(|| {
            FormatError::InvalidOffset(format!("column index {col_idx} out of range"))
        })?;

        read_column_block_from_parts_with_key(
            self.data,
            self.header.yolk_offset,
            col,
            table.row_count,
            self.encryption_key.as_deref(),
        )
    }

    /// Decode a full column, returning all values including nulls.
    ///
    /// Reads and decompresses the column block, dispatches to the
    /// appropriate encoder, and interleaves nulls from the bitmap.
    pub fn decode_column(
        &self,
        table_idx: usize,
        col_idx: usize,
    ) -> Result<Vec<Value>, FormatError> {
        let table = self.tables.get(table_idx).ok_or_else(|| {
            FormatError::InvalidOffset(format!("table index {table_idx} out of range"))
        })?;
        let col = table.columns.get(col_idx).ok_or_else(|| {
            FormatError::InvalidOffset(format!("column index {col_idx} out of range"))
        })?;

        decode_column_from_parts_with_key(
            self.data,
            self.header.yolk_offset,
            col,
            table.row_count,
            self.encryption_key.as_deref(),
        )
    }

    /// Decode all columns of a table in parallel.
    ///
    /// Uses rayon to decompress and decode every column concurrently.
    /// Returns one `Vec<Value>` per column, in column order.
    #[cfg(feature = "parallel")]
    pub fn decode_all_columns_parallel(
        &self,
        table_idx: usize,
    ) -> Result<Vec<Vec<Value>>, FormatError> {
        let table = self.tables.get(table_idx).ok_or_else(|| {
            FormatError::InvalidOffset(format!("table index {table_idx} out of range"))
        })?;
        decode_columns_parallel(
            self.data,
            self.header.yolk_offset,
            &table.columns,
            table.row_count,
            self.encryption_key.as_deref(),
        )
    }
}

/// Create an encoder instance for the given encoding type.
#[must_use]
pub fn encoder_for(encoding: Encoding) -> Box<dyn ColumnEncoder> {
    match encoding {
        Encoding::Const => Box::new(ConstEncoder),
        Encoding::BoolBitmap => Box::new(BoolBitmapEncoder),
        Encoding::Delta => Box::new(DeltaEncoder),
        Encoding::Rle => Box::new(RleEncoder),
        Encoding::Dict => Box::new(DictEncoder),
        Encoding::Raw => Box::new(RawEncoder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
    use crate::value::SqliteType;

    /// Helper: create a ColumnInput from full column values (including nulls).
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
    fn open_and_read_const_column() {
        let values = vec![Value::Integer(42); 10];
        let col = make_column("id", SqliteType::Integer, &values, Encoding::Const);
        let table = TableInput {
            table_name: "test".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE test (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();

        assert_eq!(reader.table_count(), 1);
        assert_eq!(reader.tables()[0].table_name, "test");
        assert_eq!(reader.tables()[0].row_count, 10);

        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn open_and_read_with_nulls() {
        let values = vec![
            Value::Integer(10),
            Value::Null,
            Value::Integer(20),
            Value::Integer(30),
            Value::Null,
        ];
        let col = make_column("val", SqliteType::Integer, &values, Encoding::Raw);
        let table = TableInput {
            table_name: "nullable".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE nullable (val INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn open_and_read_raw_column() {
        let values: Vec<Value> = (0..20).map(|i| Value::Text(format!("item_{i}"))).collect();
        let col = make_column("name", SqliteType::Text, &values, Encoding::Raw);
        let table = TableInput {
            table_name: "items".into(),
            row_count: 20,
            original_ddl: "CREATE TABLE items (name TEXT)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn open_and_read_delta_column() {
        let values: Vec<Value> = (0..100).map(Value::Integer).collect();
        let col = make_column("seq", SqliteType::Integer, &values, Encoding::Delta);
        let table = TableInput {
            table_name: "seq".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE seq (seq INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn open_and_read_rle_column() {
        let mut values = Vec::new();
        for i in 0..5 {
            for _ in 0..20 {
                values.push(Value::Text(format!("group_{i}")));
            }
        }
        let col = make_column("grp", SqliteType::Text, &values, Encoding::Rle);
        let table = TableInput {
            table_name: "groups".into(),
            row_count: 100,
            original_ddl: "CREATE TABLE groups (grp TEXT)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn open_and_read_bool_bitmap_column() {
        let values: Vec<Value> = (0..50).map(|i| Value::Integer(i % 2)).collect();
        let col = make_column("flag", SqliteType::Integer, &values, Encoding::BoolBitmap);
        let table = TableInput {
            table_name: "flags".into(),
            row_count: 50,
            original_ddl: "CREATE TABLE flags (flag INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn open_and_read_dict_column() {
        let statuses = ["active", "inactive", "pending"];
        let values: Vec<Value> = (0..300)
            .map(|i| Value::Text(statuses[i % 3].into()))
            .collect();
        let col = make_column("status", SqliteType::Text, &values, Encoding::Dict);
        let table = TableInput {
            table_name: "users".into(),
            row_count: 300,
            original_ddl: "CREATE TABLE users (status TEXT)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
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

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn multiple_tables() {
        let t1_values = vec![Value::Integer(1); 5];
        let t2_values: Vec<Value> = (0..10).map(|i| Value::Text(format!("v{i}"))).collect();

        let tables = vec![
            TableInput {
                table_name: "t1".into(),
                row_count: 5,
                original_ddl: "CREATE TABLE t1 (a INTEGER)".into(),
                columns: vec![make_column(
                    "a",
                    SqliteType::Integer,
                    &t1_values,
                    Encoding::Const,
                )],
                index_definitions: Vec::new(),
            },
            TableInput {
                table_name: "t2".into(),
                row_count: 10,
                original_ddl: "CREATE TABLE t2 (b TEXT)".into(),
                columns: vec![make_column(
                    "b",
                    SqliteType::Text,
                    &t2_values,
                    Encoding::Raw,
                )],
                index_definitions: Vec::new(),
            },
        ];

        let data = write_egg(&tables, &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();

        assert_eq!(reader.table_count(), 2);

        let d1 = reader.decode_column(0, 0).unwrap();
        assert_eq!(d1, t1_values);

        let d2 = reader.decode_column(1, 0).unwrap();
        assert_eq!(d2, t2_values);
    }

    #[test]
    fn multiple_columns_in_table() {
        let col_a_values: Vec<Value> = (0..20).map(Value::Integer).collect();
        let col_b_values: Vec<Value> = (0..20).map(|i| Value::Text(format!("row_{i}"))).collect();

        let table = TableInput {
            table_name: "multi".into(),
            row_count: 20,
            original_ddl: "CREATE TABLE multi (a INTEGER, b TEXT)".into(),
            columns: vec![
                make_column("a", SqliteType::Integer, &col_a_values, Encoding::Delta),
                make_column("b", SqliteType::Text, &col_b_values, Encoding::Raw),
            ],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();

        let da = reader.decode_column(0, 0).unwrap();
        let db = reader.decode_column(0, 1).unwrap();
        assert_eq!(da, col_a_values);
        assert_eq!(db, col_b_values);
    }

    #[test]
    fn all_null_column() {
        let values = vec![Value::Null; 10];
        // All-null columns use Const encoding with empty encoded_values
        let col = ColumnInput {
            name: "nul".into(),
            sqlite_type: SqliteType::Integer,
            encoding: Encoding::Const,
            cardinality: 0,
            null_count: 10,
            null_bitmap: bitvec![1; 10],
            encoded_values: Vec::new(),
            dictionary: None,
            min_value: None,
            max_value: None,
            pre_compressed: None,
        };
        let table = TableInput {
            table_name: "nulls".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE nulls (nul INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn read_column_block_returns_correct_split() {
        let values = vec![Value::Integer(1), Value::Null, Value::Integer(3)];
        let col = make_column("x", SqliteType::Integer, &values, Encoding::Const);
        let table = TableInput {
            table_name: "t".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE t (x INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();

        let (bitmap, encoded) = reader.read_column_block(0, 0).unwrap();
        assert_eq!(bitmap.len(), 3);
        assert!(!bitmap[0]); // present
        assert!(bitmap[1]); // null
        assert!(!bitmap[2]); // present
        assert!(!encoded.is_empty());
    }

    #[test]
    fn invalid_table_index() {
        let col = make_column(
            "id",
            SqliteType::Integer,
            &[Value::Integer(1)],
            Encoding::Const,
        );
        let table = TableInput {
            table_name: "t".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();

        let err = reader.decode_column(5, 0).unwrap_err();
        assert!(matches!(err, FormatError::InvalidOffset(_)));
    }

    #[test]
    fn invalid_column_index() {
        let col = make_column(
            "id",
            SqliteType::Integer,
            &[Value::Integer(1)],
            Encoding::Const,
        );
        let table = TableInput {
            table_name: "t".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let reader = EggReader::open(&data).unwrap();

        let err = reader.decode_column(0, 5).unwrap_err();
        assert!(matches!(err, FormatError::InvalidOffset(_)));
    }

    #[test]
    fn encoder_for_all_variants() {
        // Ensure encoder_for returns something for all encoding types
        let encodings = [
            Encoding::Const,
            Encoding::BoolBitmap,
            Encoding::Delta,
            Encoding::Rle,
            Encoding::Dict,
            Encoding::Raw,
        ];
        for enc in encodings {
            let encoder = encoder_for(enc);
            // Basic smoke test: encode empty, decode empty
            let encoded = encoder.encode(&[]);
            let decoded = encoder.decode(&encoded, 0).unwrap();
            assert!(decoded.is_empty());
        }
    }

    #[test]
    fn truncated_file_rejected() {
        let col = make_column(
            "id",
            SqliteType::Integer,
            &[Value::Integer(1)],
            Encoding::Const,
        );
        let table = TableInput {
            table_name: "t".into(),
            row_count: 1,
            original_ddl: "CREATE TABLE t (id INTEGER)".into(),
            columns: vec![col],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();

        // Truncate to just the header
        let err = EggReader::open(&data[..64]).unwrap_err();
        assert!(matches!(
            err,
            FormatError::InvalidOffset(_) | FormatError::Bincode(_)
        ));
    }

    /// Regression (found by fuzz_egg_reader in CI): an encrypted header whose
    /// yolk offset is near u64::MAX made the salt-range addition overflow and
    /// panic. It must be an error.
    #[test]
    fn huge_yolk_offset_in_encrypted_header_is_an_error_not_a_panic() {
        let header = Header {
            magic: Header::MAGIC,
            version: Header::CURRENT_VERSION,
            table_count: 0,
            flags: crate::format::header::HeaderFlags::new(true, true),
            created_at: 0,
            original_size: 0,
            albumen_offset: 64,
            yolk_offset: u64::MAX,
            membrane_offset: 64,
            checksum: 0,
        };
        let data = header.to_bytes();
        let err = EggReader::open_with_passphrase(&data, "pw").unwrap_err();
        assert!(matches!(err, FormatError::InvalidOffset(_)), "{err}");
    }

    /// Column offsets come from untrusted metadata; overflowing ones must be
    /// an error, not a panic or a wrapped-around read.
    #[test]
    fn overflowing_column_block_range_is_an_error() {
        let col = make_column(
            "c",
            SqliteType::Integer,
            &[Value::Integer(1)],
            Encoding::Raw,
        );
        let data = write_egg(
            &[TableInput {
                table_name: "t".into(),
                row_count: 1,
                original_ddl: "CREATE TABLE t (c INTEGER)".into(),
                columns: vec![col],
                index_definitions: vec![],
            }],
            &WriteOptions::default(),
        )
        .unwrap();
        let reader = EggReader::open(&data).unwrap();
        let mut meta = reader.tables()[0].columns[0].clone();
        for (offset, length) in [(u64::MAX, 1), (1, u64::MAX), (u64::MAX - 3, 8)] {
            meta.yolk_offset = offset;
            meta.yolk_length = length;
            let err = read_column_block_from_parts(&data, reader.header().yolk_offset, &meta, 1)
                .unwrap_err();
            assert!(matches!(err, FormatError::InvalidOffset(_)), "{err}");
        }
    }
}
