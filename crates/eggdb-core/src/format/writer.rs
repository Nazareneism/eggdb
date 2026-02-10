use bitvec::prelude::*;
use zeroize::Zeroizing;

use crate::encoding::null_bitmap;
use crate::encoding::Encoding;
use crate::format::albumen::{serialize_albumen, ColumnMeta, TableMeta};
use crate::format::header::{Header, HeaderFlags};
use crate::format::membrane::{serialize_membrane, BlockChecksum, Membrane};
use crate::format::FormatError;
use crate::value::{SqliteType, Value};

/// A pre-compressed column block that can be written directly into the yolk.
pub struct PreCompressedBlock {
    pub compressed_data: Vec<u8>,
    pub uncompressed_size: u64,
    pub crc32c: u32,
}

/// Input data for a single column to be written into an `.egg` file.
pub struct ColumnInput {
    pub name: String,
    pub sqlite_type: SqliteType,
    pub encoding: Encoding,
    pub cardinality: u64,
    pub null_count: u64,
    /// The raw null bitmap for this column (1 = null, 0 = present).
    pub null_bitmap: BitVec,
    /// Output of `ColumnEncoder::encode()` (non-null values only).
    pub encoded_values: Vec<u8>,
    pub dictionary: Option<Vec<Value>>,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
    /// Pre-compressed block data. When set, `write_egg()` uses this directly
    /// instead of compressing from null_bitmap + encoded_values.
    pub pre_compressed: Option<PreCompressedBlock>,
}

/// Input data for a single table to be written into an `.egg` file.
pub struct TableInput {
    pub table_name: String,
    pub row_count: u64,
    pub original_ddl: String,
    pub columns: Vec<ColumnInput>,
    pub index_definitions: Vec<String>,
}

/// Options controlling `.egg` file assembly.
pub struct WriteOptions {
    pub created_at: u64,
    pub original_size: u64,
    pub flags: HeaderFlags,
    /// Zstd compression level (default 3).
    pub zstd_level: i32,
    /// Optional per-column zstd levels, indexed as `[table_idx][col_idx]`.
    /// When set, overrides `zstd_level` for the specified columns.
    pub column_zstd_levels: Option<Vec<Vec<i32>>>,
    /// AES-256-GCM encryption key (derived from passphrase). When set,
    /// yolk blocks are encrypted after compression. Wrapped in `Zeroizing`
    /// so the key is securely zeroed on drop.
    pub encryption_key: Option<Zeroizing<[u8; 32]>>,
    /// Salt used for key derivation. Stored in the yolk section header
    /// so that readers can re-derive the key from the passphrase.
    pub encryption_salt: Option<[u8; 16]>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            created_at: 0,
            original_size: 0,
            flags: HeaderFlags::new(false, false),
            zstd_level: 3,
            column_zstd_levels: None,
            encryption_key: None,
            encryption_salt: None,
        }
    }
}

/// Compress a single column block (null bitmap + encoded values) with zstd.
///
/// Returns the compressed bytes suitable for writing into the yolk section.
pub fn compress_column_block(
    null_bitmap_bv: &BitVec,
    encoded_values: &[u8],
    zstd_level: i32,
) -> Result<Vec<u8>, FormatError> {
    let bitmap_bytes = null_bitmap::encode(null_bitmap_bv);
    let mut uncompressed = Vec::with_capacity(bitmap_bytes.len() + encoded_values.len());
    uncompressed.extend_from_slice(&bitmap_bytes);
    uncompressed.extend_from_slice(encoded_values);

    zstd::bulk::compress(&uncompressed, zstd_level)
        .map_err(|e| FormatError::ZstdCompress(e.to_string()))
}

/// Assemble a complete `.egg` file from table inputs and options.
///
/// Returns the full file contents as a byte vector with a valid CRC32C checksum.
pub fn write_egg(tables: &[TableInput], options: &WriteOptions) -> Result<Vec<u8>, FormatError> {
    // --- Phase 1: Compress all column blocks, track offsets and checksums ---
    struct CompressedBlock {
        data: Vec<u8>,
        uncompressed_size: u64,
    }

    let mut all_blocks: Vec<Vec<CompressedBlock>> = Vec::with_capacity(tables.len());
    let mut block_checksums: Vec<BlockChecksum> = Vec::new();

    for (table_idx, table) in tables.iter().enumerate() {
        let mut table_blocks = Vec::with_capacity(table.columns.len());
        for (col_idx, col) in table.columns.iter().enumerate() {
            if let Some(ref pre) = col.pre_compressed {
                // Use the pre-compressed block directly (e.g., from incremental lay)
                block_checksums.push(BlockChecksum {
                    table_index: table_idx as u32,
                    column_index: col_idx as u32,
                    crc32c: pre.crc32c,
                });
                table_blocks.push(CompressedBlock {
                    data: pre.compressed_data.clone(),
                    uncompressed_size: pre.uncompressed_size,
                });
            } else {
                let level = options
                    .column_zstd_levels
                    .as_ref()
                    .and_then(|levels| levels.get(table_idx))
                    .and_then(|cols| cols.get(col_idx))
                    .copied()
                    .unwrap_or(options.zstd_level);
                let compressed =
                    compress_column_block(&col.null_bitmap, &col.encoded_values, level)?;

                let bitmap_bytes_len = col.null_bitmap.len().div_ceil(8);
                let uncompressed_size = (bitmap_bytes_len + col.encoded_values.len()) as u64;

                // Optionally encrypt the compressed block
                let stored_data = if let Some(ref key) = options.encryption_key {
                    crate::format::crypto::encrypt_block(&compressed, key)?
                } else {
                    compressed
                };

                // CRC is computed on the stored bytes (post-encryption if encrypted)
                let crc = crc32c::crc32c(&stored_data);

                block_checksums.push(BlockChecksum {
                    table_index: table_idx as u32,
                    column_index: col_idx as u32,
                    crc32c: crc,
                });

                table_blocks.push(CompressedBlock {
                    data: stored_data,
                    uncompressed_size,
                });
            }
        }
        all_blocks.push(table_blocks);
    }

    // --- Phase 2: Build TableMeta with computed yolk offsets ---
    let mut table_metas: Vec<TableMeta> = Vec::with_capacity(tables.len());
    // When encrypted, the yolk section starts with a 16-byte salt.
    // Column offsets are relative to yolk section start, so they must
    // skip past the salt.
    let salt_prefix_size: u64 = if options.encryption_key.is_some() {
        crate::format::crypto::SALT_SIZE as u64
    } else {
        0
    };
    let mut yolk_running_offset: u64 = salt_prefix_size;

    for (table, blocks) in tables.iter().zip(all_blocks.iter()) {
        let mut column_metas: Vec<ColumnMeta> = Vec::with_capacity(table.columns.len());

        for (col, block) in table.columns.iter().zip(blocks.iter()) {
            column_metas.push(ColumnMeta {
                name: col.name.clone(),
                sqlite_type: col.sqlite_type,
                encoding: col.encoding,
                cardinality: col.cardinality,
                null_count: col.null_count,
                yolk_offset: yolk_running_offset,
                yolk_length: block.data.len() as u64,
                uncompressed_size: block.uncompressed_size,
                dictionary: col.dictionary.clone(),
                min_value: col.min_value.clone(),
                max_value: col.max_value.clone(),
            });
            yolk_running_offset += block.data.len() as u64;
        }

        table_metas.push(TableMeta {
            table_name: table.table_name.clone(),
            row_count: table.row_count,
            original_ddl: table.original_ddl.clone(),
            columns: column_metas,
            index_definitions: table.index_definitions.clone(),
        });
    }

    // --- Phase 3: Serialize albumen ---
    let albumen_bytes = serialize_albumen(&table_metas)?;

    // --- Phase 4: Compute albumen hash ---
    let albumen_hash = blake3::hash(&albumen_bytes);

    // --- Phase 5: Compute section offsets ---
    let albumen_offset = Header::SIZE as u64; // always 64
    let yolk_offset = albumen_offset + albumen_bytes.len() as u64;
    let blocks_size: u64 = all_blocks
        .iter()
        .flat_map(|tb| tb.iter())
        .map(|b| b.data.len() as u64)
        .sum();
    let total_yolk_size: u64 = salt_prefix_size + blocks_size;
    let membrane_offset = yolk_offset + total_yolk_size;

    // --- Phase 6: Build and serialize membrane ---
    let membrane = Membrane {
        block_checksums,
        total_row_counts: tables.iter().map(|t| t.row_count).collect(),
        albumen_hash: *albumen_hash.as_bytes(),
    };
    let membrane_bytes = serialize_membrane(&membrane)?;

    // --- Phase 7: Build header with placeholder checksum ---
    let header = Header {
        magic: Header::MAGIC,
        version: Header::CURRENT_VERSION,
        table_count: tables.len() as u32,
        flags: options.flags,
        created_at: options.created_at,
        original_size: options.original_size,
        albumen_offset,
        yolk_offset,
        membrane_offset,
        checksum: 0, // placeholder
    };
    let header_bytes = header.to_bytes();

    // --- Phase 8: Assemble the full file ---
    let total_size =
        Header::SIZE + albumen_bytes.len() + total_yolk_size as usize + membrane_bytes.len();
    let mut file_data = Vec::with_capacity(total_size);

    file_data.extend_from_slice(&header_bytes);
    file_data.extend_from_slice(&albumen_bytes);

    // Write encryption salt at the start of the yolk section (if encrypted)
    if let Some(ref salt) = options.encryption_salt {
        file_data.extend_from_slice(salt);
    }

    for table_blocks in &all_blocks {
        for block in table_blocks {
            file_data.extend_from_slice(&block.data);
        }
    }
    file_data.extend_from_slice(&membrane_bytes);

    // --- Phase 9: Compute and patch the file-level CRC32C ---
    let crc = Header::compute_checksum(&file_data);
    file_data[50..54].copy_from_slice(&crc.to_le_bytes());

    Ok(file_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::albumen::deserialize_albumen;
    use crate::format::header::Header;

    /// Helper: create a single-column const table with no data.
    fn empty_table() -> TableInput {
        TableInput {
            table_name: "empty".into(),
            row_count: 0,
            original_ddl: "CREATE TABLE empty (id INTEGER)".into(),
            columns: vec![ColumnInput {
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
            }],
            index_definitions: Vec::new(),
        }
    }

    /// Helper: create a single-column table with a few encoded values.
    fn simple_table() -> TableInput {
        // Simulate a Const column: 5 rows, all value 42, no nulls
        // Const encoder output is just the bincode-serialized value
        let value = Value::Integer(42);
        let encoded = bincode::serialize(&value).expect("bincode serialize");
        TableInput {
            table_name: "nums".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE nums (v INTEGER)".into(),
            columns: vec![ColumnInput {
                name: "v".into(),
                sqlite_type: SqliteType::Integer,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 0,
                null_bitmap: bitvec![0; 5],
                encoded_values: encoded,
                dictionary: None,
                min_value: Some(Value::Integer(42)),
                max_value: Some(Value::Integer(42)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        }
    }

    #[test]
    fn single_table_header_fields() {
        let table = simple_table();
        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        assert_eq!(header.magic, Header::MAGIC);
        assert_eq!(header.version, Header::CURRENT_VERSION);
        assert_eq!(header.table_count, 1);
        assert_eq!(header.albumen_offset, 64);
        assert!(header.yolk_offset > header.albumen_offset);
        assert!(header.membrane_offset >= header.yolk_offset);
    }

    #[test]
    fn single_table_checksum_valid() {
        let table = simple_table();
        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);
    }

    #[test]
    fn albumen_offset_always_64() {
        let table = empty_table();
        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();
        assert_eq!(header.albumen_offset, 64);
    }

    #[test]
    fn empty_table_produces_valid_egg() {
        let table = empty_table();
        let data = write_egg(&[table], &WriteOptions::default()).unwrap();

        // Should be at least 64 bytes (header)
        assert!(data.len() >= 64);

        let header = Header::from_bytes(&data).unwrap();
        assert_eq!(header.table_count, 1);

        // Checksum should be valid
        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);
    }

    #[test]
    fn write_options_default() {
        let opts = WriteOptions::default();
        assert_eq!(opts.created_at, 0);
        assert_eq!(opts.original_size, 0);
        assert_eq!(opts.flags.to_raw(), 0);
        assert_eq!(opts.zstd_level, 3);
    }

    #[test]
    fn multiple_columns_sequential_offsets() {
        let table = TableInput {
            table_name: "multi".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE multi (a INTEGER, b TEXT)".into(),
            columns: vec![
                ColumnInput {
                    name: "a".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Const,
                    cardinality: 1,
                    null_count: 0,
                    null_bitmap: bitvec![0; 10],
                    encoded_values: bincode::serialize(&Value::Integer(7))
                        .expect("bincode serialize"),
                    dictionary: None,
                    min_value: Some(Value::Integer(7)),
                    max_value: Some(Value::Integer(7)),
                    pre_compressed: None,
                },
                ColumnInput {
                    name: "b".into(),
                    sqlite_type: SqliteType::Text,
                    encoding: Encoding::Const,
                    cardinality: 1,
                    null_count: 0,
                    null_bitmap: bitvec![0; 10],
                    encoded_values: bincode::serialize(&Value::Text("hello".into()))
                        .expect("bincode serialize"),
                    dictionary: None,
                    min_value: Some(Value::Text("hello".into())),
                    max_value: Some(Value::Text("hello".into())),
                    pre_compressed: None,
                },
            ],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        // Parse albumen to check column offsets
        let albumen_start = header.albumen_offset as usize;
        let albumen_end = header.yolk_offset as usize;
        let albumen_slice = &data[albumen_start..albumen_end];
        let metas = deserialize_albumen(albumen_slice).unwrap();

        assert_eq!(metas.len(), 1);
        let cols = &metas[0].columns;
        assert_eq!(cols.len(), 2);

        // First column starts at offset 0 within yolk
        assert_eq!(cols[0].yolk_offset, 0);
        // Second column starts right after the first
        assert_eq!(cols[1].yolk_offset, cols[0].yolk_length);
        // No gaps between columns
        assert_eq!(
            cols[0].yolk_length + cols[1].yolk_length,
            header.membrane_offset - header.yolk_offset
        );
    }

    #[test]
    fn write_options_custom() {
        let opts = WriteOptions {
            created_at: 1_700_000_000,
            original_size: 999_999,
            flags: HeaderFlags::new(true, true),
            zstd_level: 1,
            column_zstd_levels: None,
            encryption_key: None,
            encryption_salt: None,
        };
        let table = simple_table();
        let data = write_egg(&[table], &opts).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        assert_eq!(header.created_at, 1_700_000_000);
        assert_eq!(header.original_size, 999_999);
        assert!(header.flags.has_encryption());
        assert!(header.flags.row_order_preserved());
    }

    #[test]
    fn compress_column_block_round_trip() {
        let bitmap = bitvec![0, 1, 0, 0, 1]; // 2 nulls, 3 present
        let encoded_values = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let compressed = compress_column_block(&bitmap, &encoded_values, 3).unwrap();

        // Decompress and verify contents
        let decompressed =
            zstd::bulk::decompress(&compressed, 1024).expect("zstd decompress failed");

        let bitmap_byte_len = bitmap.len().div_ceil(8);
        assert_eq!(decompressed.len(), bitmap_byte_len + encoded_values.len());

        // Verify bitmap bytes
        let expected_bitmap_bytes = null_bitmap::encode(&bitmap);
        assert_eq!(&decompressed[..bitmap_byte_len], &expected_bitmap_bytes);

        // Verify encoded values follow bitmap
        assert_eq!(&decompressed[bitmap_byte_len..], &encoded_values);
    }

    #[test]
    fn compress_column_block_empty() {
        let bitmap = bitvec![];
        let compressed = compress_column_block(&bitmap, &[], 3).unwrap();
        let decompressed =
            zstd::bulk::decompress(&compressed, 1024).expect("zstd decompress failed");
        assert!(decompressed.is_empty());
    }

    #[test]
    fn multiple_tables() {
        let t1 = TableInput {
            table_name: "users".into(),
            row_count: 3,
            original_ddl: "CREATE TABLE users (id INTEGER)".into(),
            columns: vec![ColumnInput {
                name: "id".into(),
                sqlite_type: SqliteType::Integer,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 0,
                null_bitmap: bitvec![0; 3],
                encoded_values: bincode::serialize(&Value::Integer(1)).expect("bincode serialize"),
                dictionary: None,
                min_value: Some(Value::Integer(1)),
                max_value: Some(Value::Integer(1)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };
        let t2 = TableInput {
            table_name: "orders".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE orders (total REAL)".into(),
            columns: vec![ColumnInput {
                name: "total".into(),
                sqlite_type: SqliteType::Real,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 0,
                null_bitmap: bitvec![0; 5],
                encoded_values: bincode::serialize(&Value::Real(9.99)).expect("bincode serialize"),
                dictionary: None,
                min_value: Some(Value::Real(9.99)),
                max_value: Some(Value::Real(9.99)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[t1, t2], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();
        assert_eq!(header.table_count, 2);

        // Checksum valid
        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);

        // Parse albumen
        let albumen_slice = &data[header.albumen_offset as usize..header.yolk_offset as usize];
        let metas = deserialize_albumen(albumen_slice).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].table_name, "users");
        assert_eq!(metas[0].row_count, 3);
        assert_eq!(metas[1].table_name, "orders");
        assert_eq!(metas[1].row_count, 5);
    }

    #[test]
    fn section_offsets_contiguous() {
        let table = simple_table();
        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        // Header ends at 64, albumen starts at 64
        assert_eq!(header.albumen_offset, 64);
        // Yolk starts right after albumen
        assert!(header.yolk_offset > header.albumen_offset);
        // Membrane starts right after yolk
        assert!(header.membrane_offset >= header.yolk_offset);
        // File ends after membrane
        assert!(data.len() as u64 > header.membrane_offset);
    }

    #[test]
    fn no_tables() {
        let data = write_egg(&[], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        assert_eq!(header.table_count, 0);
        assert_eq!(header.albumen_offset, 64);
        // With no yolk data, yolk_offset == membrane_offset
        assert_eq!(header.yolk_offset, header.membrane_offset);

        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);
    }

    #[test]
    fn column_with_nulls() {
        let table = TableInput {
            table_name: "nullable".into(),
            row_count: 4,
            original_ddl: "CREATE TABLE nullable (v TEXT)".into(),
            columns: vec![ColumnInput {
                name: "v".into(),
                sqlite_type: SqliteType::Text,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 2,
                // Row pattern: [present, null, present, null]
                null_bitmap: bitvec![0, 1, 0, 1],
                encoded_values: bincode::serialize(&Value::Text("hi".into()))
                    .expect("bincode serialize"),
                dictionary: None,
                min_value: Some(Value::Text("hi".into())),
                max_value: Some(Value::Text("hi".into())),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };

        let data = write_egg(&[table], &WriteOptions::default()).unwrap();
        let header = Header::from_bytes(&data).unwrap();

        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);

        let albumen_slice = &data[header.albumen_offset as usize..header.yolk_offset as usize];
        let metas = deserialize_albumen(albumen_slice).unwrap();
        assert_eq!(metas[0].columns[0].null_count, 2);
    }

    #[test]
    fn column_zstd_levels_override() {
        // Create a table with two columns so we can verify per-column levels
        let table = TableInput {
            table_name: "multi".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE multi (a INTEGER, b TEXT)".into(),
            columns: vec![
                ColumnInput {
                    name: "a".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Const,
                    cardinality: 1,
                    null_count: 0,
                    null_bitmap: bitvec![0; 10],
                    encoded_values: bincode::serialize(&Value::Integer(7))
                        .expect("bincode serialize"),
                    dictionary: None,
                    min_value: Some(Value::Integer(7)),
                    max_value: Some(Value::Integer(7)),
                    pre_compressed: None,
                },
                ColumnInput {
                    name: "b".into(),
                    sqlite_type: SqliteType::Text,
                    encoding: Encoding::Raw,
                    cardinality: 1,
                    null_count: 0,
                    null_bitmap: bitvec![0; 10],
                    encoded_values: bincode::serialize(&Value::Text("hello".into()))
                        .expect("bincode serialize"),
                    dictionary: None,
                    min_value: Some(Value::Text("hello".into())),
                    max_value: Some(Value::Text("hello".into())),
                    pre_compressed: None,
                },
            ],
            index_definitions: Vec::new(),
        };

        // Use per-column levels: col 0 gets level 1, col 1 gets level 9
        let opts = WriteOptions {
            column_zstd_levels: Some(vec![vec![1, 9]]),
            ..WriteOptions::default()
        };
        let data = write_egg(&[table], &opts).unwrap();

        // File should be valid
        let header = Header::from_bytes(&data).unwrap();
        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);

        // Verify it's still readable
        let albumen_slice = &data[header.albumen_offset as usize..header.yolk_offset as usize];
        let metas = deserialize_albumen(albumen_slice).unwrap();
        assert_eq!(metas[0].columns.len(), 2);
    }

    #[test]
    fn column_zstd_levels_partial_override() {
        // Only override the first table, second table falls back to default
        let t1 = simple_table();
        let t2 = empty_table();

        let opts = WriteOptions {
            column_zstd_levels: Some(vec![vec![9]]),
            zstd_level: 1,
            ..WriteOptions::default()
        };
        let data = write_egg(&[t1, t2], &opts).unwrap();

        let header = Header::from_bytes(&data).unwrap();
        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);
        assert_eq!(header.table_count, 2);
    }

    #[test]
    fn write_options_default_has_no_column_levels() {
        let opts = WriteOptions::default();
        assert!(opts.column_zstd_levels.is_none());
    }

    #[test]
    fn encrypted_egg_round_trip() {
        use crate::format::crypto;
        use crate::format::reader::EggReader;

        let values = vec![
            Value::Integer(10),
            Value::Null,
            Value::Integer(20),
            Value::Integer(30),
            Value::Null,
        ];
        let non_null: Vec<Value> = values.iter().filter(|v| !v.is_null()).cloned().collect();
        let null_bitmap: bitvec::vec::BitVec = values.iter().map(|v| v.is_null()).collect();

        let encoder = crate::format::reader::encoder_for(Encoding::Raw);
        let encoded = encoder.encode(&non_null);

        let table = TableInput {
            table_name: "secret".into(),
            row_count: 5,
            original_ddl: "CREATE TABLE secret (val INTEGER)".into(),
            columns: vec![ColumnInput {
                name: "val".into(),
                sqlite_type: SqliteType::Integer,
                encoding: Encoding::Raw,
                cardinality: 3,
                null_count: 2,
                null_bitmap,
                encoded_values: encoded,
                dictionary: None,
                min_value: Some(Value::Integer(10)),
                max_value: Some(Value::Integer(30)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };

        let passphrase = "test-passphrase-123";
        let salt = crypto::generate_salt();
        let key = crypto::derive_key(passphrase, &salt).unwrap();

        let opts = WriteOptions {
            flags: HeaderFlags::new(true, true),
            encryption_key: Some(key),
            encryption_salt: Some(salt),
            ..WriteOptions::default()
        };

        let data = write_egg(&[table], &opts).unwrap();

        // Verify header
        let header = Header::from_bytes(&data).unwrap();
        assert!(header.flags.has_encryption());
        let computed = Header::compute_checksum(&data);
        assert_eq!(header.checksum, computed);

        // Opening without passphrase should succeed (can read metadata)
        let reader = EggReader::open(&data).unwrap();
        assert_eq!(reader.table_count(), 1);
        assert_eq!(reader.tables()[0].table_name, "secret");

        // But decoding a column without the key should fail
        let err = reader.decode_column(0, 0);
        assert!(
            err.is_err(),
            "decoding encrypted data without key should fail"
        );

        // Opening with passphrase should allow decoding
        let reader = EggReader::open_with_passphrase(&data, passphrase).unwrap();
        let decoded = reader.decode_column(0, 0).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn encrypted_egg_wrong_passphrase() {
        use crate::format::crypto;
        use crate::format::reader::EggReader;

        let table = simple_table();

        let passphrase = "correct-horse-battery-staple";
        let salt = crypto::generate_salt();
        let key = crypto::derive_key(passphrase, &salt).unwrap();

        let opts = WriteOptions {
            flags: HeaderFlags::new(true, false),
            encryption_key: Some(key),
            encryption_salt: Some(salt),
            ..WriteOptions::default()
        };

        let data = write_egg(&[table], &opts).unwrap();

        // Wrong passphrase should derive a different key and fail on decrypt
        let reader = EggReader::open_with_passphrase(&data, "wrong-password").unwrap();
        let err = reader.decode_column(0, 0);
        assert!(err.is_err(), "wrong passphrase should fail decryption");
    }

    #[test]
    fn encrypted_egg_multiple_columns() {
        use crate::format::crypto;
        use crate::format::reader::EggReader;

        let col_a_values: Vec<Value> = (0..20).map(Value::Integer).collect();
        let col_b_values: Vec<Value> = (0..20).map(|i| Value::Text(format!("row_{i}"))).collect();

        let make_col = |name: &str, st: SqliteType, vals: &[Value], enc: Encoding| {
            let null_bitmap: bitvec::vec::BitVec = vals.iter().map(|v| v.is_null()).collect();
            let non_null: Vec<Value> = vals.iter().filter(|v| !v.is_null()).cloned().collect();
            let encoder = crate::format::reader::encoder_for(enc);
            let encoded = encoder.encode(&non_null);
            let null_count = vals.iter().filter(|v| v.is_null()).count() as u64;
            let min_value = non_null.iter().min().cloned();
            let max_value = non_null.iter().max().cloned();
            ColumnInput {
                name: name.into(),
                sqlite_type: st,
                encoding: enc,
                cardinality: non_null.len() as u64,
                null_count,
                null_bitmap,
                encoded_values: encoded,
                dictionary: None,
                min_value,
                max_value,
                pre_compressed: None,
            }
        };

        let table = TableInput {
            table_name: "multi".into(),
            row_count: 20,
            original_ddl: "CREATE TABLE multi (a INTEGER, b TEXT)".into(),
            columns: vec![
                make_col("a", SqliteType::Integer, &col_a_values, Encoding::Delta),
                make_col("b", SqliteType::Text, &col_b_values, Encoding::Raw),
            ],
            index_definitions: Vec::new(),
        };

        let passphrase = "multi-col-test";
        let salt = crypto::generate_salt();
        let key = crypto::derive_key(passphrase, &salt).unwrap();

        let opts = WriteOptions {
            flags: HeaderFlags::new(true, true),
            encryption_key: Some(key),
            encryption_salt: Some(salt),
            ..WriteOptions::default()
        };

        let data = write_egg(&[table], &opts).unwrap();
        let reader = EggReader::open_with_passphrase(&data, passphrase).unwrap();

        let da = reader.decode_column(0, 0).unwrap();
        let db = reader.decode_column(0, 1).unwrap();
        assert_eq!(da, col_a_values);
        assert_eq!(db, col_b_values);
    }
}
