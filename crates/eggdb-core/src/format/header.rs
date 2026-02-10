use crate::format::FormatError;

/// Header flags packed into a `u16`.
///
/// Only bits 0 and 1 are defined. Bits 2-15 must be zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderFlags(u16);

impl HeaderFlags {
    /// Create flags from individual booleans.
    #[must_use]
    pub fn new(encryption: bool, row_order_preserved: bool) -> Self {
        let mut bits = 0u16;
        if encryption {
            bits |= 1;
        }
        if row_order_preserved {
            bits |= 1 << 1;
        }
        Self(bits)
    }

    /// Parse flags from a raw `u16`, validating that bits 2-15 are zero.
    pub fn from_raw(raw: u16) -> Result<Self, FormatError> {
        if raw & !0b11 != 0 {
            return Err(FormatError::UnknownFlags(raw));
        }
        Ok(Self(raw))
    }

    /// Return the raw `u16` representation.
    #[must_use]
    pub fn to_raw(self) -> u16 {
        self.0
    }

    /// Whether the encryption flag (bit 0) is set.
    #[must_use]
    pub fn has_encryption(self) -> bool {
        self.0 & 1 != 0
    }

    /// Whether the row-order-preserved flag (bit 1) is set.
    #[must_use]
    pub fn row_order_preserved(self) -> bool {
        self.0 & (1 << 1) != 0
    }
}

/// The fixed 64-byte `.egg` file header (the "shell").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub magic: [u8; 3],
    pub version: u8,
    pub table_count: u32,
    pub flags: HeaderFlags,
    pub created_at: u64,
    pub original_size: u64,
    pub albumen_offset: u64,
    pub yolk_offset: u64,
    pub membrane_offset: u64,
    pub checksum: u32,
}

impl Header {
    /// Magic bytes: `EGG` (0x45, 0x47, 0x47).
    pub const MAGIC: [u8; 3] = [0x45, 0x47, 0x47];

    /// Total header size in bytes.
    pub const SIZE: usize = 64;

    /// The only supported format version.
    pub const CURRENT_VERSION: u8 = 1;

    /// Serialize the header to a 64-byte little-endian array.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut buf = [0u8; 64];

        // [0..3] magic
        buf[0..3].copy_from_slice(&self.magic);
        // [3] version
        buf[3] = self.version;
        // [4..8] table_count (u32 LE)
        buf[4..8].copy_from_slice(&self.table_count.to_le_bytes());
        // [8..10] flags (u16 LE)
        buf[8..10].copy_from_slice(&self.flags.to_raw().to_le_bytes());
        // [10..18] created_at (u64 LE)
        buf[10..18].copy_from_slice(&self.created_at.to_le_bytes());
        // [18..26] original_size (u64 LE)
        buf[18..26].copy_from_slice(&self.original_size.to_le_bytes());
        // [26..34] albumen_offset (u64 LE)
        buf[26..34].copy_from_slice(&self.albumen_offset.to_le_bytes());
        // [34..42] yolk_offset (u64 LE)
        buf[34..42].copy_from_slice(&self.yolk_offset.to_le_bytes());
        // [42..50] membrane_offset (u64 LE)
        buf[42..50].copy_from_slice(&self.membrane_offset.to_le_bytes());
        // [50..54] checksum (u32 LE)
        buf[50..54].copy_from_slice(&self.checksum.to_le_bytes());
        // [54..64] reserved — already zero

        buf
    }

    /// Deserialize a header from at least 64 bytes.
    ///
    /// Validates magic, version (must be 1), reserved bytes (must be zero),
    /// and flags (bits 2-15 must be zero).
    pub fn from_bytes(data: &[u8]) -> Result<Self, FormatError> {
        if data.len() < Self::SIZE {
            return Err(FormatError::DataTooShort {
                expected: Self::SIZE,
                actual: data.len(),
            });
        }

        // Magic
        let magic: [u8; 3] = [data[0], data[1], data[2]];
        if magic != Self::MAGIC {
            return Err(FormatError::InvalidMagic(magic));
        }

        // Version
        let version = data[3];
        if version != Self::CURRENT_VERSION {
            return Err(FormatError::UnsupportedVersion(version));
        }

        // Reserved bytes [54..64] must all be zero
        if data[54..64].iter().any(|&b| b != 0) {
            return Err(FormatError::NonZeroReserved);
        }

        let table_count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let flags = HeaderFlags::from_raw(u16::from_le_bytes([data[8], data[9]]))?;
        let created_at = u64::from_le_bytes(data[10..18].try_into().unwrap());
        let original_size = u64::from_le_bytes(data[18..26].try_into().unwrap());
        let albumen_offset = u64::from_le_bytes(data[26..34].try_into().unwrap());
        let yolk_offset = u64::from_le_bytes(data[34..42].try_into().unwrap());
        let membrane_offset = u64::from_le_bytes(data[42..50].try_into().unwrap());
        let checksum = u32::from_le_bytes(data[50..54].try_into().unwrap());

        Ok(Header {
            magic,
            version,
            table_count,
            flags,
            created_at,
            original_size,
            albumen_offset,
            yolk_offset,
            membrane_offset,
            checksum,
        })
    }

    /// Compute the file-level CRC32C checksum.
    ///
    /// The checksum covers the entire file with bytes [50..54] (the checksum
    /// field itself) treated as `[0x00, 0x00, 0x00, 0x00]`.
    #[must_use]
    pub fn compute_checksum(file_data: &[u8]) -> u32 {
        let crc = crc32c::crc32c(&file_data[..50]);
        let crc = crc32c::crc32c_append(crc, &[0, 0, 0, 0]);
        crc32c::crc32c_append(crc, &file_data[54..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> Header {
        Header {
            magic: Header::MAGIC,
            version: Header::CURRENT_VERSION,
            table_count: 3,
            flags: HeaderFlags::new(false, true),
            created_at: 1_700_000_000,
            original_size: 1_000_000,
            albumen_offset: 64,
            yolk_offset: 256,
            membrane_offset: 4096,
            checksum: 0xDEADBEEF,
        }
    }

    #[test]
    fn round_trip() {
        let header = sample_header();
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), 64);
        let parsed = Header::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn magic_bytes_correct() {
        let bytes = sample_header().to_bytes();
        assert_eq!(&bytes[0..3], b"EGG");
    }

    #[test]
    fn invalid_magic() {
        let mut bytes = sample_header().to_bytes();
        bytes[0] = 0xFF;
        let err = Header::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FormatError::InvalidMagic(_)));
    }

    #[test]
    fn bad_version_zero() {
        let mut bytes = sample_header().to_bytes();
        bytes[3] = 0;
        let err = Header::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FormatError::UnsupportedVersion(0)));
    }

    #[test]
    fn bad_version_two() {
        let mut bytes = sample_header().to_bytes();
        bytes[3] = 2;
        let err = Header::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FormatError::UnsupportedVersion(2)));
    }

    #[test]
    fn non_zero_reserved() {
        let mut bytes = sample_header().to_bytes();
        bytes[60] = 1;
        let err = Header::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FormatError::NonZeroReserved));
    }

    #[test]
    fn unknown_flags() {
        let mut bytes = sample_header().to_bytes();
        // Set bit 2 in flags field
        bytes[8] = 0x04;
        bytes[9] = 0x00;
        let err = Header::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, FormatError::UnknownFlags(_)));
    }

    #[test]
    fn data_too_short() {
        let bytes = [0u8; 32];
        let err = Header::from_bytes(&bytes).unwrap_err();
        assert!(matches!(
            err,
            FormatError::DataTooShort {
                expected: 64,
                actual: 32
            }
        ));
    }

    #[test]
    fn flags_none_set() {
        let flags = HeaderFlags::new(false, false);
        assert!(!flags.has_encryption());
        assert!(!flags.row_order_preserved());
        assert_eq!(flags.to_raw(), 0);
    }

    #[test]
    fn flags_encryption_only() {
        let flags = HeaderFlags::new(true, false);
        assert!(flags.has_encryption());
        assert!(!flags.row_order_preserved());
        assert_eq!(flags.to_raw(), 1);
    }

    #[test]
    fn flags_row_order_only() {
        let flags = HeaderFlags::new(false, true);
        assert!(!flags.has_encryption());
        assert!(flags.row_order_preserved());
        assert_eq!(flags.to_raw(), 2);
    }

    #[test]
    fn flags_both_set() {
        let flags = HeaderFlags::new(true, true);
        assert!(flags.has_encryption());
        assert!(flags.row_order_preserved());
        assert_eq!(flags.to_raw(), 3);
    }

    #[test]
    fn flags_from_raw_valid() {
        for raw in 0..=3u16 {
            assert!(HeaderFlags::from_raw(raw).is_ok());
        }
    }

    #[test]
    fn flags_from_raw_invalid() {
        for raw in [4u16, 0xFF, 0xFFFF] {
            assert!(HeaderFlags::from_raw(raw).is_err());
        }
    }

    #[test]
    fn compute_checksum_consistent() {
        let header = Header {
            checksum: 0,
            ..sample_header()
        };
        let mut file_data = header.to_bytes().to_vec();
        // Append some "albumen + yolk + membrane" payload
        file_data.extend_from_slice(&[0xAA; 100]);

        let crc1 = Header::compute_checksum(&file_data);
        let crc2 = Header::compute_checksum(&file_data);
        assert_eq!(crc1, crc2);
        assert_ne!(crc1, 0); // extremely unlikely to be zero on real data
    }

    #[test]
    fn compute_checksum_ignores_checksum_field() {
        let header = Header {
            checksum: 0,
            ..sample_header()
        };
        let mut file_data = header.to_bytes().to_vec();
        file_data.extend_from_slice(&[0xBB; 50]);

        let crc_before = Header::compute_checksum(&file_data);

        // Change bytes[50..54] (the checksum field) to something else
        file_data[50] = 0xFF;
        file_data[51] = 0xFF;
        file_data[52] = 0xFF;
        file_data[53] = 0xFF;

        let crc_after = Header::compute_checksum(&file_data);
        assert_eq!(crc_before, crc_after);
    }

    #[test]
    fn edge_case_max_values() {
        let header = Header {
            magic: Header::MAGIC,
            version: Header::CURRENT_VERSION,
            table_count: u32::MAX,
            flags: HeaderFlags::new(true, true),
            created_at: u64::MAX,
            original_size: u64::MAX,
            albumen_offset: u64::MAX,
            yolk_offset: u64::MAX,
            membrane_offset: u64::MAX,
            checksum: u32::MAX,
        };
        let bytes = header.to_bytes();
        let parsed = Header::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn edge_case_zero_values() {
        let header = Header {
            magic: Header::MAGIC,
            version: Header::CURRENT_VERSION,
            table_count: 0,
            flags: HeaderFlags::new(false, false),
            created_at: 0,
            original_size: 0,
            albumen_offset: 0,
            yolk_offset: 0,
            membrane_offset: 0,
            checksum: 0,
        };
        let bytes = header.to_bytes();
        let parsed = Header::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, header);
    }
}
