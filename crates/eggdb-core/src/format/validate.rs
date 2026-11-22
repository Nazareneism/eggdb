use crate::format::albumen;
use crate::format::header::Header;
use crate::format::membrane;
use crate::format::FormatError;

/// Result of a single validation check.
#[derive(Clone, Debug)]
pub struct ValidationResult {
    pub check: String,
    pub passed: bool,
    pub detail: String,
}

/// Validate an `.egg` file, running all integrity checks.
///
/// Returns a list of all checks performed with their pass/fail status.
/// Does NOT short-circuit: all checks run regardless of earlier failures.
///
/// Requires the file to have a valid header (magic, version, reserved, flags)
/// in order to locate the other sections. If the header is invalid, returns
/// an error rather than a partial result.
pub fn validate(data: &[u8]) -> Result<Vec<ValidationResult>, FormatError> {
    validate_with_key(data, None)
}

/// Validate an `.egg` file with an optional encryption key.
///
/// Behaves like [`validate`] but accepts an optional encryption key. When the
/// file has encryption enabled (header flag bit 0), the encryption status is
/// noted in the results. Block CRC checks always work because CRCs are computed
/// on the stored (post-encryption) bytes.
pub fn validate_with_key(
    data: &[u8],
    _encryption_key: Option<&[u8; 32]>,
) -> Result<Vec<ValidationResult>, FormatError> {
    let header = Header::from_bytes(data)?;
    let mut results = Vec::new();

    // 0. Encryption status
    {
        let encrypted = header.flags.has_encryption();
        results.push(ValidationResult {
            check: "encryption_status".into(),
            passed: true,
            detail: if encrypted {
                "file is encrypted (AES-256-GCM)".into()
            } else {
                "file is not encrypted".into()
            },
        });
    }

    // 1. File-level CRC32C
    {
        let computed = Header::compute_checksum(data);
        let passed = computed == header.checksum;
        results.push(ValidationResult {
            check: "file_crc32c".into(),
            passed,
            detail: if passed {
                format!("CRC32C {:#010x} matches", computed)
            } else {
                format!(
                    "CRC32C mismatch: header says {:#010x}, computed {:#010x}",
                    header.checksum, computed
                )
            },
        });
    }

    // 2. Section offset ordering
    {
        let valid = header.albumen_offset <= header.yolk_offset
            && header.yolk_offset <= header.membrane_offset
            && (header.membrane_offset as usize) <= data.len();
        results.push(ValidationResult {
            check: "section_offsets".into(),
            passed: valid,
            detail: if valid {
                format!(
                    "albumen={}, yolk={}, membrane={}, file_size={}",
                    header.albumen_offset,
                    header.yolk_offset,
                    header.membrane_offset,
                    data.len()
                )
            } else {
                format!(
                    "invalid section ordering: albumen={}, yolk={}, membrane={}, file_size={}",
                    header.albumen_offset,
                    header.yolk_offset,
                    header.membrane_offset,
                    data.len()
                )
            },
        });
    }

    // 3. Deserialize albumen
    let albumen_start = header.albumen_offset as usize;
    let albumen_end = header.yolk_offset as usize;

    let tables = if albumen_end <= data.len() && albumen_start <= albumen_end {
        match albumen::deserialize_albumen(&data[albumen_start..albumen_end]) {
            Ok(t) => {
                results.push(ValidationResult {
                    check: "albumen_deserialize".into(),
                    passed: true,
                    detail: format!("{} table(s) deserialized", t.len()),
                });
                Some(t)
            }
            Err(e) => {
                results.push(ValidationResult {
                    check: "albumen_deserialize".into(),
                    passed: false,
                    detail: format!("failed: {e}"),
                });
                None
            }
        }
    } else {
        results.push(ValidationResult {
            check: "albumen_deserialize".into(),
            passed: false,
            detail: "albumen section out of bounds".into(),
        });
        None
    };

    // 4. Table count consistency
    if let Some(ref tables) = tables {
        let passed = tables.len() == header.table_count as usize;
        results.push(ValidationResult {
            check: "table_count".into(),
            passed,
            detail: if passed {
                format!("header and albumen agree: {} table(s)", tables.len())
            } else {
                format!(
                    "header says {}, albumen has {}",
                    header.table_count,
                    tables.len()
                )
            },
        });
    }

    // 5. Albumen hash (blake3)
    {
        let albumen_bytes = if albumen_end <= data.len() && albumen_start <= albumen_end {
            Some(&data[albumen_start..albumen_end])
        } else {
            None
        };

        let membrane_start = header.membrane_offset as usize;
        let membrane_data = if membrane_start <= data.len() {
            membrane::deserialize_membrane(&data[membrane_start..]).ok()
        } else {
            None
        };

        if let (Some(albumen_bytes), Some(ref membrane_data)) = (albumen_bytes, &membrane_data) {
            let computed: [u8; 32] = blake3::hash(albumen_bytes).into();
            let passed = computed == membrane_data.albumen_hash;
            results.push(ValidationResult {
                check: "albumen_hash".into(),
                passed,
                detail: if passed {
                    "blake3 hash matches".into()
                } else {
                    "blake3 hash mismatch: metadata may be corrupt".into()
                },
            });

            // 6. Per-block CRC32C checksums
            if let Some(ref tables) = tables {
                for bc in &membrane_data.block_checksums {
                    let ti = bc.table_index as usize;
                    let ci = bc.column_index as usize;

                    let check_name =
                        format!("block_crc32c_t{}c{}", bc.table_index, bc.column_index);

                    if ti >= tables.len() || ci >= tables[ti].columns.len() {
                        results.push(ValidationResult {
                            check: check_name,
                            passed: false,
                            detail: format!(
                                "block checksum references invalid table {ti} column {ci}"
                            ),
                        });
                        continue;
                    }

                    let col = &tables[ti].columns[ci];
                    let Some(block) = crate::format::checked_range(
                        header.yolk_offset,
                        col.yolk_offset,
                        col.yolk_length,
                        data.len(),
                    ) else {
                        results.push(ValidationResult {
                            check: check_name,
                            passed: false,
                            detail: format!(
                                "block at yolk {} + {} (length {}) exceeds file size {}",
                                header.yolk_offset,
                                col.yolk_offset,
                                col.yolk_length,
                                data.len()
                            ),
                        });
                        continue;
                    };

                    let computed = crc32c::crc32c(&data[block]);
                    let passed = computed == bc.crc32c;
                    results.push(ValidationResult {
                        check: check_name,
                        passed,
                        detail: if passed {
                            format!("CRC32C {:#010x} matches", computed)
                        } else {
                            format!(
                                "CRC32C mismatch: membrane says {:#010x}, computed {:#010x}",
                                bc.crc32c, computed
                            )
                        },
                    });
                }
            }

            // 7. Row count consistency (membrane vs albumen)
            if let Some(ref tables) = tables {
                for (i, table) in tables.iter().enumerate() {
                    let check_name = format!("row_count_{}", table.table_name);
                    if i < membrane_data.total_row_counts.len() {
                        let membrane_count = membrane_data.total_row_counts[i];
                        let passed = table.row_count == membrane_count;
                        results.push(ValidationResult {
                            check: check_name,
                            passed,
                            detail: if passed {
                                format!("{} rows", table.row_count)
                            } else {
                                format!(
                                    "albumen says {}, membrane says {}",
                                    table.row_count, membrane_count
                                )
                            },
                        });
                    } else {
                        results.push(ValidationResult {
                            check: check_name,
                            passed: false,
                            detail: format!("membrane has no row count for table index {i}"),
                        });
                    }
                }
            }
        } else {
            results.push(ValidationResult {
                check: "albumen_hash".into(),
                passed: false,
                detail: "could not verify: albumen or membrane section unavailable".into(),
            });
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitvec::prelude::*;

    use crate::encoding::constant::ConstEncoder;
    use crate::encoding::ColumnEncoder;
    use crate::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
    use crate::value::{SqliteType, Value};
    use crate::Encoding;

    fn simple_egg() -> Vec<u8> {
        let encoder = ConstEncoder;
        let values = vec![Value::Integer(42); 10];
        let encoded = encoder.encode(&values);
        let table = TableInput {
            table_name: "test".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE test (v INTEGER)".into(),
            columns: vec![ColumnInput {
                name: "v".into(),
                sqlite_type: SqliteType::Integer,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 0,
                null_bitmap: bitvec![0; 10],
                encoded_values: encoded,
                dictionary: None,
                min_value: Some(Value::Integer(42)),
                max_value: Some(Value::Integer(42)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };
        write_egg(&[table], &WriteOptions::default()).unwrap()
    }

    #[test]
    fn valid_file_passes_all_checks() {
        let data = simple_egg();
        let results = validate(&data).unwrap();

        for r in &results {
            assert!(r.passed, "check '{}' failed: {}", r.check, r.detail);
        }
        // Should have at least: file_crc32c, section_offsets, albumen_deserialize,
        // table_count, albumen_hash, block_crc32c, row_count
        assert!(results.len() >= 7);
    }

    #[test]
    fn corrupt_crc_detected() {
        let mut data = simple_egg();
        // Flip a byte in the yolk to break the file CRC and block CRC
        let header = Header::from_bytes(&data).unwrap();
        let yolk_start = header.yolk_offset as usize;
        if yolk_start < data.len() {
            data[yolk_start] ^= 0xFF;
        }

        let results = validate(&data).unwrap();
        let file_crc = results.iter().find(|r| r.check == "file_crc32c").unwrap();
        assert!(!file_crc.passed);
    }

    #[test]
    fn corrupt_albumen_hash_detected() {
        let mut data = simple_egg();
        let header = Header::from_bytes(&data).unwrap();
        // Flip a byte in the albumen to break the albumen hash
        let albumen_start = header.albumen_offset as usize;
        if albumen_start + 1 < header.yolk_offset as usize {
            data[albumen_start] ^= 0xFF;
        }

        let results = validate(&data).unwrap();
        // albumen_hash should fail (or albumen_deserialize fails)
        let hash_check = results.iter().find(|r| r.check == "albumen_hash");
        let deser_check = results.iter().find(|r| r.check == "albumen_deserialize");

        // Either the hash fails or deserialization fails
        let failed =
            hash_check.is_some_and(|r| !r.passed) || deser_check.is_some_and(|r| !r.passed);
        assert!(failed, "corruption in albumen should be detected");
    }

    #[test]
    fn corrupt_block_crc_detected() {
        let mut data = simple_egg();
        let header = Header::from_bytes(&data).unwrap();

        // Parse albumen to find yolk block location
        let albumen_start = header.albumen_offset as usize;
        let albumen_end = header.yolk_offset as usize;
        let tables = albumen::deserialize_albumen(&data[albumen_start..albumen_end]).unwrap();
        let col = &tables[0].columns[0];
        let block_start = header.yolk_offset as usize + col.yolk_offset as usize;

        // Flip a byte inside the compressed block
        if block_start < data.len() {
            data[block_start] ^= 0xFF;
        }

        // Re-patch the file CRC so only the block CRC fails
        let new_crc = Header::compute_checksum(&data);
        data[50..54].copy_from_slice(&new_crc.to_le_bytes());

        let results = validate(&data).unwrap();
        let block_check = results
            .iter()
            .find(|r| r.check.starts_with("block_crc32c_"));
        assert!(block_check.is_some());
        assert!(
            !block_check.unwrap().passed,
            "block CRC corruption should be detected"
        );
    }

    #[test]
    fn all_checks_run_no_short_circuit() {
        let data = simple_egg();
        let results = validate(&data).unwrap();

        // Verify we have checks from multiple categories
        let checks: Vec<&str> = results.iter().map(|r| r.check.as_str()).collect();
        assert!(checks.contains(&"encryption_status"));
        assert!(checks.contains(&"file_crc32c"));
        assert!(checks.contains(&"section_offsets"));
        assert!(checks.contains(&"albumen_deserialize"));
        assert!(checks.contains(&"table_count"));
        assert!(checks.contains(&"albumen_hash"));
        assert!(checks.iter().any(|c| c.starts_with("block_crc32c_")));
        assert!(checks.iter().any(|c| c.starts_with("row_count_")));
    }

    #[test]
    fn multiple_tables_validation() {
        let encoder = ConstEncoder;

        let tables = vec![
            TableInput {
                table_name: "a".into(),
                row_count: 5,
                original_ddl: "CREATE TABLE a (id INTEGER)".into(),
                columns: vec![ColumnInput {
                    name: "id".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Const,
                    cardinality: 1,
                    null_count: 0,
                    null_bitmap: bitvec![0; 5],
                    encoded_values: encoder.encode(&vec![Value::Integer(1); 5]),
                    dictionary: None,
                    min_value: Some(Value::Integer(1)),
                    max_value: Some(Value::Integer(1)),
                    pre_compressed: None,
                }],
                index_definitions: Vec::new(),
            },
            TableInput {
                table_name: "b".into(),
                row_count: 3,
                original_ddl: "CREATE TABLE b (id INTEGER)".into(),
                columns: vec![ColumnInput {
                    name: "id".into(),
                    sqlite_type: SqliteType::Integer,
                    encoding: Encoding::Const,
                    cardinality: 1,
                    null_count: 0,
                    null_bitmap: bitvec![0; 3],
                    encoded_values: encoder.encode(&vec![Value::Integer(2); 3]),
                    dictionary: None,
                    min_value: Some(Value::Integer(2)),
                    max_value: Some(Value::Integer(2)),
                    pre_compressed: None,
                }],
                index_definitions: Vec::new(),
            },
        ];

        let data = write_egg(&tables, &WriteOptions::default()).unwrap();
        let results = validate(&data).unwrap();

        for r in &results {
            assert!(r.passed, "check '{}' failed: {}", r.check, r.detail);
        }

        // Should have block checks for both tables
        let block_checks: Vec<&ValidationResult> = results
            .iter()
            .filter(|r| r.check.starts_with("block_crc32c_"))
            .collect();
        assert_eq!(block_checks.len(), 2);

        // Should have row count checks for both tables
        let row_checks: Vec<&ValidationResult> = results
            .iter()
            .filter(|r| r.check.starts_with("row_count_"))
            .collect();
        assert_eq!(row_checks.len(), 2);
    }

    #[test]
    fn too_short_for_header() {
        let data = [0u8; 32];
        let err = validate(&data).unwrap_err();
        assert!(matches!(err, FormatError::DataTooShort { .. }));
    }

    #[test]
    fn encrypted_egg_validate_without_key() {
        use crate::format::crypto;
        use crate::format::header::HeaderFlags;

        let encoder = ConstEncoder;
        let values = vec![Value::Integer(42); 10];
        let encoded = encoder.encode(&values);

        let table = TableInput {
            table_name: "encrypted".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE encrypted (v INTEGER)".into(),
            columns: vec![ColumnInput {
                name: "v".into(),
                sqlite_type: SqliteType::Integer,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 0,
                null_bitmap: bitvec![0; 10],
                encoded_values: encoded,
                dictionary: None,
                min_value: Some(Value::Integer(42)),
                max_value: Some(Value::Integer(42)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };

        let salt = crypto::generate_salt();
        let key = crypto::derive_key("test-pass", &salt).unwrap();

        let opts = WriteOptions {
            flags: HeaderFlags::new(true, true),
            encryption_key: Some(key),
            encryption_salt: Some(salt),
            ..WriteOptions::default()
        };

        let data = write_egg(&[table], &opts).unwrap();

        // validate without key should still pass all structural checks
        let results = validate(&data).unwrap();

        // Encryption status check should be present
        let enc_check = results
            .iter()
            .find(|r| r.check == "encryption_status")
            .unwrap();
        assert!(enc_check.passed);
        assert!(enc_check.detail.contains("encrypted"));

        // File CRC should pass
        let crc_check = results.iter().find(|r| r.check == "file_crc32c").unwrap();
        assert!(crc_check.passed);

        // Block CRC should pass (CRC is on stored/encrypted bytes)
        for r in &results {
            if r.check.starts_with("block_crc32c_") {
                assert!(r.passed, "block CRC should pass: {}", r.detail);
            }
        }
    }

    #[test]
    fn encrypted_egg_validate_with_key() {
        use super::validate_with_key;
        use crate::format::crypto;
        use crate::format::header::HeaderFlags;

        let encoder = ConstEncoder;
        let values = vec![Value::Integer(42); 10];
        let encoded = encoder.encode(&values);

        let table = TableInput {
            table_name: "encrypted".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE encrypted (v INTEGER)".into(),
            columns: vec![ColumnInput {
                name: "v".into(),
                sqlite_type: SqliteType::Integer,
                encoding: Encoding::Const,
                cardinality: 1,
                null_count: 0,
                null_bitmap: bitvec![0; 10],
                encoded_values: encoded,
                dictionary: None,
                min_value: Some(Value::Integer(42)),
                max_value: Some(Value::Integer(42)),
                pre_compressed: None,
            }],
            index_definitions: Vec::new(),
        };

        let salt = crypto::generate_salt();
        let key = crypto::derive_key("test-pass", &salt).unwrap();
        let key_for_validate = key.clone();

        let opts = WriteOptions {
            flags: HeaderFlags::new(true, true),
            encryption_key: Some(key),
            encryption_salt: Some(salt),
            ..WriteOptions::default()
        };

        let data = write_egg(&[table], &opts).unwrap();

        // validate with key should pass all checks
        let results = validate_with_key(&data, Some(&key_for_validate)).unwrap();

        for r in &results {
            assert!(r.passed, "check '{}' failed: {}", r.check, r.detail);
        }
    }
}
