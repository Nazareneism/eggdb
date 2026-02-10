use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::format::{bincode_config, FormatError};

/// CRC32C checksum for a single column block in the yolk.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockChecksum {
    pub table_index: u32,
    pub column_index: u32,
    pub crc32c: u32,
}

/// The membrane section: integrity metadata for the `.egg` file.
///
/// Contains per-block CRC32C checksums, per-table row counts,
/// and a blake3 hash of the serialized albumen bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Membrane {
    pub block_checksums: Vec<BlockChecksum>,
    pub total_row_counts: Vec<u64>,
    pub albumen_hash: [u8; 32],
}

/// Serialize the membrane using the format's bincode configuration.
pub fn serialize_membrane(membrane: &Membrane) -> Result<Vec<u8>, FormatError> {
    bincode_config()
        .serialize(membrane)
        .map_err(FormatError::Bincode)
}

/// Deserialize the membrane from bytes.
pub fn deserialize_membrane(data: &[u8]) -> Result<Membrane, FormatError> {
    bincode_config()
        .deserialize(data)
        .map_err(FormatError::Bincode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let membrane = Membrane {
            block_checksums: Vec::new(),
            total_row_counts: Vec::new(),
            albumen_hash: [0u8; 32],
        };
        let bytes = serialize_membrane(&membrane).unwrap();
        let result = deserialize_membrane(&bytes).unwrap();
        assert!(result.block_checksums.is_empty());
        assert!(result.total_row_counts.is_empty());
        assert_eq!(result.albumen_hash, [0u8; 32]);
    }

    #[test]
    fn round_trip_with_data() {
        let hash: [u8; 32] = {
            let mut h = [0u8; 32];
            for (i, byte) in h.iter_mut().enumerate() {
                *byte = i as u8;
            }
            h
        };

        let membrane = Membrane {
            block_checksums: vec![
                BlockChecksum {
                    table_index: 0,
                    column_index: 0,
                    crc32c: 0xDEADBEEF,
                },
                BlockChecksum {
                    table_index: 0,
                    column_index: 1,
                    crc32c: 0xCAFEBABE,
                },
                BlockChecksum {
                    table_index: 1,
                    column_index: 0,
                    crc32c: 0x12345678,
                },
            ],
            total_row_counts: vec![1000, 500],
            albumen_hash: hash,
        };

        let bytes = serialize_membrane(&membrane).unwrap();
        let result = deserialize_membrane(&bytes).unwrap();

        assert_eq!(result.block_checksums.len(), 3);
        assert_eq!(result.block_checksums[0].crc32c, 0xDEADBEEF);
        assert_eq!(result.block_checksums[1].crc32c, 0xCAFEBABE);
        assert_eq!(result.block_checksums[2].table_index, 1);
        assert_eq!(result.total_row_counts, vec![1000, 500]);
        assert_eq!(result.albumen_hash, hash);
    }

    #[test]
    fn many_blocks() {
        let checksums: Vec<BlockChecksum> = (0..100)
            .map(|i| BlockChecksum {
                table_index: i / 10,
                column_index: i % 10,
                crc32c: i * 1000,
            })
            .collect();

        let membrane = Membrane {
            block_checksums: checksums.clone(),
            total_row_counts: vec![10_000; 10],
            albumen_hash: [0xAB; 32],
        };

        let bytes = serialize_membrane(&membrane).unwrap();
        let result = deserialize_membrane(&bytes).unwrap();

        assert_eq!(result.block_checksums.len(), 100);
        assert_eq!(result.block_checksums, checksums);
        assert_eq!(result.total_row_counts.len(), 10);
    }
}
