pub mod encoding;
pub mod format;
pub mod value;

pub use encoding::{expand_non_null_indices, matches_filter, ColumnEncoder, Encoding, FilterOp};
pub use format::albumen::{ColumnMeta, TableMeta};
pub use format::crypto::{decrypt_block, derive_key, encrypt_block, generate_salt, SALT_SIZE};
pub use format::header::{Header, HeaderFlags};
pub use format::membrane::{BlockChecksum, Membrane};
pub use format::overlay::{apply_op, replay, PendingCounts, TableOverlay};
#[cfg(feature = "parallel")]
pub use format::reader::decode_columns_parallel;
pub use format::reader::{
    decode_column_from_parts, decode_column_from_parts_with_key, encoder_for,
    read_column_block_from_parts, read_column_block_from_parts_with_key, EggReader,
};
pub use format::rebuild::{encode_column, encode_table};
pub use format::validate::{validate, validate_with_key, ValidationResult};
pub use format::wal::{
    encode_frame, parse_wal, ParsedWal, WalCommit, WalEntry, WalFrame, WalHeader, WalOp,
    WAL_HEADER_SIZE,
};
pub use format::writer::{
    compress_column_block, write_egg, ColumnInput, PreCompressedBlock, TableInput, WriteOptions,
};
pub use format::FormatError;
pub use value::{SqliteType, Value};
pub use zeroize::Zeroizing;
