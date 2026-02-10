pub mod albumen;
pub mod crypto;
pub mod header;
pub mod membrane;
pub mod reader;
pub mod validate;
pub mod writer;

use thiserror::Error;

/// Errors that can occur when reading, writing, or validating `.egg` files.
#[derive(Debug, Error)]
pub enum FormatError {
    #[error("invalid magic bytes: expected [0x45, 0x47, 0x47], got {0:?}")]
    InvalidMagic([u8; 3]),

    #[error("unsupported format version: {0} (only version 1 is supported)")]
    UnsupportedVersion(u8),

    #[error("data too short: expected at least {expected} bytes, got {actual}")]
    DataTooShort { expected: usize, actual: usize },

    #[error("header checksum mismatch: expected {expected:#010x}, computed {computed:#010x}")]
    HeaderChecksumMismatch { expected: u32, computed: u32 },

    #[error("block checksum mismatch for table {table_index} column {column_index}: expected {expected:#010x}, computed {computed:#010x}")]
    BlockChecksumMismatch {
        table_index: u32,
        column_index: u32,
        expected: u32,
        computed: u32,
    },

    #[error("albumen hash mismatch: metadata may be corrupt or tampered")]
    AlbumenHashMismatch,

    #[error("bincode serialization error: {0}")]
    Bincode(#[from] bincode::Error),

    #[error("zstd compression error: {0}")]
    ZstdCompress(String),

    #[error("zstd decompression error: {0}")]
    ZstdDecompress(String),

    #[error("invalid offset: {0}")]
    InvalidOffset(String),

    #[error("non-zero reserved bytes in header")]
    NonZeroReserved,

    #[error("unknown flags set: {0:#06x} (bits 2-15 must be zero)")]
    UnknownFlags(u16),

    #[error("row count mismatch for table {table_name}: metadata says {expected}, membrane says {actual}")]
    RowCountMismatch {
        table_name: String,
        expected: u64,
        actual: u64,
    },

    #[error("table count mismatch: header says {expected}, albumen has {actual}")]
    TableCountMismatch { expected: u32, actual: u32 },

    #[error("column data error: {0}")]
    ColumnData(String),

    #[error("encryption error: {0}")]
    Encryption(String),

    #[error("decode error in {encoding} encoder: {detail}")]
    Decode {
        encoding: &'static str,
        detail: String,
    },
}

/// Returns the bincode configuration for format version 1.
///
/// Version 1 uses little-endian byte order and varint integer encoding.
/// This must be used for ALL albumen and membrane serialization.
pub fn bincode_config() -> impl bincode::Options {
    use bincode::Options;
    bincode::options()
        .with_little_endian()
        .with_varint_encoding()
}
