pub mod albumen;
pub mod crypto;
pub mod header;
pub mod membrane;
pub mod overlay;
pub mod reader;
pub mod rebuild;
pub mod validate;
pub mod wal;
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

    /// The write log (`.egg.wal`) is malformed, or an operation in it is
    /// inconsistent with the table it targets.
    #[error("write log error: {0}")]
    WriteLog(String),
}

/// The byte range `[base + offset, base + offset + len)` if it lies entirely
/// inside a file of `file_len` bytes.
///
/// Offsets and lengths come from untrusted headers and metadata, so every
/// addition is checked: an overflow or an out-of-file range is `None`, never
/// a panic or a wrapped-around read.
#[must_use]
pub fn checked_range(
    base: u64,
    offset: u64,
    len: u64,
    file_len: usize,
) -> Option<std::ops::Range<usize>> {
    let start = base.checked_add(offset)?;
    let end = start.checked_add(len)?;
    let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
    (end <= file_len).then_some(start..end)
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

#[cfg(test)]
mod tests {
    use super::checked_range;

    #[test]
    fn checked_range_accepts_ranges_inside_the_file() {
        assert_eq!(checked_range(10, 5, 3, 18), Some(15..18));
        assert_eq!(checked_range(0, 0, 0, 0), Some(0..0));
    }

    #[test]
    fn checked_range_rejects_overflow_and_out_of_file() {
        assert_eq!(checked_range(u64::MAX, 1, 0, usize::MAX), None);
        assert_eq!(checked_range(1, u64::MAX, 0, usize::MAX), None);
        assert_eq!(checked_range(0, 1, u64::MAX, usize::MAX), None);
        assert_eq!(checked_range(10, 5, 4, 18), None, "one byte past the end");
    }
}
