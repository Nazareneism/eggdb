//! Write log format: an append-only sidecar (`data.egg.wal`) that records
//! inserts, updates, and deletes against an immutable `.egg` main file.
//!
//! Pure byte encoding and parsing only. File IO lives in `eggdb-store`.
//! See `docs/v0.3_plan.md` (M13) for the layout and the parse rules.

use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::format::{bincode_config, FormatError};
use crate::value::Value;

/// Magic bytes at the start of every write log.
pub const WAL_MAGIC: [u8; 4] = *b"EGGW";

/// The only write log version this build understands.
pub const WAL_VERSION: u8 = 1;

/// Size of the fixed write log header in bytes.
pub const WAL_HEADER_SIZE: usize = 32;

/// Size of the prefix before each frame payload: `[len u32][crc u32]`.
pub const FRAME_PREFIX_SIZE: usize = 8;

/// Fixed 32-byte write log header.
///
/// Binds the log to exactly one generation of the main file, so a log is never
/// applied to (or silently dropped for) a different `.egg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalHeader {
    /// The main file's header checksum (CRC32C stored at bytes `[50..54]`).
    pub base_checksum: u32,
    /// The main file's length in bytes.
    pub base_length: u64,
}

/// One row-level change. `table` indexes the main file's albumen table list.
///
/// On-disk format: variant and field order are part of the bincode encoding.
/// Never reorder; only append new variants, and bump `WAL_VERSION` if needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOp {
    /// Append a new row. Its rowid is the table's next physical row index.
    Insert {
        /// Index into the main file's table list.
        table: u32,
        /// One value per column, in column order.
        row: Vec<Value>,
    },
    /// Replace every value of an existing live row, keeping its rowid.
    Update {
        /// Index into the main file's table list.
        table: u32,
        /// Physical row index of the row to replace.
        rowid: u64,
        /// The new values, one per column.
        row: Vec<Value>,
    },
    /// Tombstone an existing live row. Its rowid is never reused.
    Delete {
        /// Index into the main file's table list.
        table: u32,
        /// Physical row index of the row to delete.
        rowid: u64,
    },
}

/// One record in the write log. Same ordering rule as [`WalOp`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalFrame {
    /// One committed transaction.
    Commit {
        /// Counts Commit frames from 0, including later reverted ones.
        seq: u64,
        /// The transaction's operations, in the order they were applied.
        ops: Vec<WalOp>,
    },
    /// Cancels the Commit frame directly before it (rollback after sync).
    Revert {
        /// `seq` of the Commit being cancelled.
        seq: u64,
    },
    /// Compaction produced a new main file with this checksum and length.
    Compacted {
        /// Header checksum of the new main file.
        checksum: u32,
        /// Byte length of the new main file.
        length: u64,
    },
}

/// A committed transaction that survived parsing (not reverted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalCommit {
    /// Sequence number of the Commit frame.
    pub seq: u64,
    /// Operations in the order they were applied.
    pub ops: Vec<WalOp>,
}

/// A parsed log entry, with Revert frames already resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalEntry {
    /// A committed transaction.
    Commit(WalCommit),
    /// A compaction marker.
    Compacted {
        /// Header checksum of the main file the compaction produced.
        checksum: u32,
        /// Byte length of that main file.
        length: u64,
    },
}

/// Result of parsing a complete write log file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedWal {
    /// The validated header.
    pub header: WalHeader,
    /// Entries in file order, with reverted commits removed.
    pub entries: Vec<WalEntry>,
    /// The `seq` the next Commit frame must use.
    pub next_seq: u64,
    /// Byte length of the valid prefix (header + all valid frames).
    pub valid_length: u64,
    /// Bytes after `valid_length` discarded as an interrupted final write.
    pub torn_tail_bytes: u64,
}

impl WalHeader {
    /// Serialize to the fixed 32-byte layout, including the header CRC.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; WAL_HEADER_SIZE] {
        let mut buf = [0u8; WAL_HEADER_SIZE];
        buf[0..4].copy_from_slice(&WAL_MAGIC);
        buf[4] = WAL_VERSION;
        buf[8..12].copy_from_slice(&self.base_checksum.to_le_bytes());
        buf[12..20].copy_from_slice(&self.base_length.to_le_bytes());
        let crc = crc32c::crc32c(&buf[0..28]);
        buf[28..32].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Parse and validate the header from the start of `data`.
    pub fn from_bytes(data: &[u8]) -> Result<Self, FormatError> {
        let h = data
            .get(..WAL_HEADER_SIZE)
            .ok_or(FormatError::DataTooShort {
                expected: WAL_HEADER_SIZE,
                actual: data.len(),
            })?;
        if h[0..4] != WAL_MAGIC {
            return Err(wal_err("not a write log (bad magic bytes)"));
        }
        if h[4] != WAL_VERSION {
            return Err(wal_err(format!(
                "unsupported write log version {} (only version {WAL_VERSION} is supported)",
                h[4]
            )));
        }
        if h[5..8].iter().chain(&h[20..28]).any(|&b| b != 0) {
            return Err(wal_err("non-zero reserved bytes in header"));
        }
        if read_u32(&h[28..32]) != crc32c::crc32c(&h[0..28]) {
            return Err(wal_err("header checksum mismatch"));
        }
        Ok(Self {
            base_checksum: read_u32(&h[8..12]),
            base_length: read_u64(&h[12..20]),
        })
    }
}

impl WalOp {
    /// The table index this operation targets.
    #[must_use]
    pub fn table(&self) -> u32 {
        match self {
            WalOp::Insert { table, .. }
            | WalOp::Update { table, .. }
            | WalOp::Delete { table, .. } => *table,
        }
    }
}

impl ParsedWal {
    /// Committed transactions in order.
    pub fn commits(&self) -> impl Iterator<Item = &WalCommit> {
        self.entries.iter().filter_map(|e| match e {
            WalEntry::Commit(c) => Some(c),
            WalEntry::Compacted { .. } => None,
        })
    }

    /// The most recent compaction marker, and whether any commit follows it.
    #[must_use]
    pub fn last_compacted(&self) -> Option<(u32, u64, bool)> {
        let idx = self
            .entries
            .iter()
            .rposition(|e| matches!(e, WalEntry::Compacted { .. }))?;
        match &self.entries[idx] {
            WalEntry::Compacted { checksum, length } => {
                Some((*checksum, *length, idx + 1 < self.entries.len()))
            }
            WalEntry::Commit(_) => None,
        }
    }
}

/// Encode a frame as `[len u32][crc u32][payload]`.
///
/// The CRC32C covers the length bytes followed by the payload, so a damaged
/// length field is detected whenever the frame lies inside the file.
pub fn encode_frame(frame: &WalFrame) -> Result<Vec<u8>, FormatError> {
    let payload = bincode_config().serialize(frame)?;
    let len = u32::try_from(payload.len())
        .map_err(|_| wal_err("frame payload exceeds 4 GiB; split the transaction"))?;
    let len_bytes = len.to_le_bytes();
    let crc = crc32c::crc32c_append(crc32c::crc32c(&len_bytes), &payload);

    let mut out = Vec::with_capacity(FRAME_PREFIX_SIZE + payload.len());
    out.extend_from_slice(&len_bytes);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Parse a complete write log.
///
/// An unreadable frame with nothing meaningful after it (it reaches EOF, or
/// only zero bytes follow) is an interrupted final write: it is excluded and
/// counted in `torn_tail_bytes`. An unreadable frame followed by other data is
/// corruption and returns an error.
pub fn parse_wal(data: &[u8]) -> Result<ParsedWal, FormatError> {
    let header = WalHeader::from_bytes(data)?;
    let mut builder = EntryBuilder::default();
    let mut pos = WAL_HEADER_SIZE;

    while pos < data.len() {
        match read_frame(&data[pos..]) {
            FrameRead::Valid { frame, size } => {
                builder.push(frame, pos)?;
                pos += size;
            }
            FrameRead::Unreadable { claimed_end } => {
                if is_torn_tail(data, pos, claimed_end) {
                    break;
                }
                return Err(wal_err(format!(
                    "frame at byte {pos} is corrupt (checksum mismatch) and is followed by more data"
                )));
            }
            FrameRead::Undecodable(e) => {
                return Err(wal_err(format!(
                    "frame at byte {pos} has a valid checksum but cannot be decoded \
                     (written by an incompatible version?): {e}"
                )));
            }
        }
    }

    Ok(ParsedWal {
        header,
        entries: builder.entries,
        next_seq: builder.next_seq,
        valid_length: pos as u64,
        torn_tail_bytes: (data.len() - pos) as u64,
    })
}

/// Outcome of reading one frame at the start of a slice.
enum FrameRead {
    /// CRC-valid frame that deserialized; `size` includes the prefix.
    Valid { frame: WalFrame, size: usize },
    /// Incomplete or CRC-mismatched frame. `claimed_end` is the end offset
    /// relative to the slice start, when the prefix was complete.
    Unreadable { claimed_end: Option<usize> },
    /// CRC-valid frame whose payload does not deserialize. Never a torn
    /// write: it was written by an incompatible writer.
    Undecodable(FormatError),
}

fn read_frame(rest: &[u8]) -> FrameRead {
    let Some(prefix) = rest.get(..FRAME_PREFIX_SIZE) else {
        return FrameRead::Unreadable { claimed_end: None };
    };
    let len = read_u32(&prefix[0..4]) as usize;
    let stored_crc = read_u32(&prefix[4..8]);
    let end = FRAME_PREFIX_SIZE.saturating_add(len);
    let Some(payload) = rest.get(FRAME_PREFIX_SIZE..end) else {
        return FrameRead::Unreadable { claimed_end: None };
    };
    let crc = crc32c::crc32c_append(crc32c::crc32c(&prefix[0..4]), payload);
    if len == 0 || crc != stored_crc {
        return FrameRead::Unreadable {
            claimed_end: Some(end),
        };
    }
    match decode_payload(payload) {
        Ok(frame) => FrameRead::Valid { frame, size: end },
        Err(e) => FrameRead::Undecodable(e),
    }
}

fn decode_payload(payload: &[u8]) -> Result<WalFrame, FormatError> {
    Ok(bincode_config()
        .with_limit(payload.len() as u64)
        .reject_trailing_bytes()
        .deserialize(payload)?)
}

/// Decide whether an unreadable frame at `pos` is an interrupted final write.
fn is_torn_tail(data: &[u8], pos: usize, claimed_end: Option<usize>) -> bool {
    let only_zeros_follow = data[pos..].iter().all(|&b| b == 0);
    match claimed_end {
        // Prefix or payload cut short by EOF.
        None => true,
        // Complete but bad frame: torn only if it is the last thing in the file.
        Some(end) => only_zeros_follow || pos.checked_add(end) == Some(data.len()),
    }
}

/// Applies frames in order, resolving Revert frames and checking sequence rules.
#[derive(Default)]
struct EntryBuilder {
    entries: Vec<WalEntry>,
    next_seq: u64,
}

impl EntryBuilder {
    fn push(&mut self, frame: WalFrame, pos: usize) -> Result<(), FormatError> {
        match frame {
            WalFrame::Commit { seq, ops } => {
                if seq != self.next_seq {
                    return Err(wal_err(format!(
                        "frame at byte {pos}: commit sequence {seq}, expected {}",
                        self.next_seq
                    )));
                }
                self.next_seq += 1;
                self.entries.push(WalEntry::Commit(WalCommit { seq, ops }));
            }
            WalFrame::Revert { seq } => match self.entries.last() {
                Some(WalEntry::Commit(c)) if c.seq == seq => {
                    self.entries.pop();
                }
                _ => {
                    return Err(wal_err(format!(
                        "frame at byte {pos}: revert of commit {seq} does not directly follow it"
                    )));
                }
            },
            WalFrame::Compacted { checksum, length } => {
                self.entries.push(WalEntry::Compacted { checksum, length });
            }
        }
        Ok(())
    }
}

fn wal_err(msg: impl Into<String>) -> FormatError {
    FormatError::WriteLog(msg.into())
}

fn read_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn read_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> WalHeader {
        WalHeader {
            base_checksum: 0xDEAD_BEEF,
            base_length: 4096,
        }
    }

    fn commit(seq: u64, ops: Vec<WalOp>) -> WalFrame {
        WalFrame::Commit { seq, ops }
    }

    fn insert(table: u32, n: i64) -> WalOp {
        WalOp::Insert {
            table,
            row: vec![Value::Integer(n), Value::Text(format!("row{n}"))],
        }
    }

    fn log_with(frames: &[WalFrame]) -> Vec<u8> {
        let mut data = header().to_bytes().to_vec();
        for f in frames {
            data.extend(encode_frame(f).unwrap());
        }
        data
    }

    #[test]
    fn header_round_trip() {
        let bytes = header().to_bytes();
        assert_eq!(&bytes[0..4], b"EGGW");
        assert_eq!(WalHeader::from_bytes(&bytes).unwrap(), header());
    }

    #[test]
    fn header_rejects_bad_magic_version_reserved_and_crc() {
        let good = header().to_bytes();
        for (idx, val) in [(0usize, b'X'), (4, 2), (6, 1), (25, 1), (30, 0xFF)] {
            let mut bad = good;
            bad[idx] = val;
            // Keep the CRC valid for all but the CRC case, to test each check.
            if idx != 30 {
                let crc = crc32c::crc32c(&bad[0..28]);
                bad[28..32].copy_from_slice(&crc.to_le_bytes());
            }
            assert!(
                matches!(WalHeader::from_bytes(&bad), Err(FormatError::WriteLog(_))),
                "byte {idx} = {val} should be rejected"
            );
        }
        assert!(matches!(
            WalHeader::from_bytes(&good[..10]),
            Err(FormatError::DataTooShort { .. })
        ));
    }

    #[test]
    fn empty_log_parses() {
        let parsed = parse_wal(&log_with(&[])).unwrap();
        assert!(parsed.entries.is_empty());
        assert_eq!(parsed.next_seq, 0);
        assert_eq!(parsed.valid_length, WAL_HEADER_SIZE as u64);
        assert_eq!(parsed.torn_tail_bytes, 0);
    }

    #[test]
    fn frames_round_trip_in_order() {
        let frames = vec![
            commit(0, vec![insert(0, 1), insert(1, 2)]),
            commit(
                1,
                vec![
                    WalOp::Update {
                        table: 0,
                        rowid: 5,
                        row: vec![Value::Null, Value::Blob(vec![1, 2])],
                    },
                    WalOp::Delete { table: 1, rowid: 0 },
                ],
            ),
            WalFrame::Compacted {
                checksum: 7,
                length: 99,
            },
        ];
        let data = log_with(&frames);
        let parsed = parse_wal(&data).unwrap();
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.next_seq, 2);
        assert_eq!(parsed.valid_length, data.len() as u64);
        let ops: Vec<usize> = parsed.commits().map(|c| c.ops.len()).collect();
        assert_eq!(ops, vec![2, 2]);
        assert_eq!(parsed.last_compacted(), Some((7, 99, false)));
    }

    #[test]
    fn torn_tail_at_every_offset_is_discarded() {
        let base = log_with(&[commit(0, vec![insert(0, 1)])]);
        let last = encode_frame(&commit(1, vec![insert(0, 2), insert(0, 3)])).unwrap();
        for cut in 0..last.len() {
            let mut data = base.clone();
            data.extend_from_slice(&last[..cut]);
            let parsed = parse_wal(&data).unwrap();
            assert_eq!(parsed.commits().count(), 1, "cut at {cut}");
            assert_eq!(parsed.valid_length, base.len() as u64);
            assert_eq!(parsed.torn_tail_bytes, cut as u64);
            assert_eq!(parsed.next_seq, 1);
        }
    }

    #[test]
    fn zero_filled_tail_is_discarded() {
        let mut data = log_with(&[commit(0, vec![insert(0, 1)])]);
        let valid = data.len();
        data.extend(std::iter::repeat_n(0u8, 300));
        let parsed = parse_wal(&data).unwrap();
        assert_eq!(parsed.valid_length, valid as u64);
        assert_eq!(parsed.torn_tail_bytes, 300);
    }

    #[test]
    fn corrupt_last_frame_is_torn_but_corrupt_middle_frame_errors() {
        let first = encode_frame(&commit(0, vec![insert(0, 1)])).unwrap();
        let second = encode_frame(&commit(1, vec![insert(0, 2)])).unwrap();
        let mut data = header().to_bytes().to_vec();
        data.extend(&first);
        data.extend(&second);

        // Flip a payload byte in the last frame: interrupted write, discarded.
        let mut torn = data.clone();
        let last_idx = torn.len() - 1;
        torn[last_idx] ^= 0xFF;
        assert_eq!(parse_wal(&torn).unwrap().commits().count(), 1);

        // Flip a payload byte in the first frame: corruption, error.
        let mut corrupt = data.clone();
        corrupt[WAL_HEADER_SIZE + FRAME_PREFIX_SIZE] ^= 0xFF;
        assert!(matches!(parse_wal(&corrupt), Err(FormatError::WriteLog(_))));

        // Damage the first frame's length so it ends inside the file: error.
        let mut bad_len = data;
        bad_len[WAL_HEADER_SIZE] = bad_len[WAL_HEADER_SIZE].wrapping_sub(1);
        assert!(matches!(parse_wal(&bad_len), Err(FormatError::WriteLog(_))));
    }

    #[test]
    fn revert_removes_directly_preceding_commit() {
        let data = log_with(&[
            commit(0, vec![insert(0, 1)]),
            commit(1, vec![insert(0, 2)]),
            WalFrame::Revert { seq: 1 },
            commit(2, vec![insert(0, 3)]),
        ]);
        let parsed = parse_wal(&data).unwrap();
        let seqs: Vec<u64> = parsed.commits().map(|c| c.seq).collect();
        assert_eq!(seqs, vec![0, 2]);
        assert_eq!(parsed.next_seq, 3);
    }

    #[test]
    fn misplaced_revert_is_an_error() {
        for frames in [
            vec![WalFrame::Revert { seq: 0 }],
            vec![commit(0, vec![]), WalFrame::Revert { seq: 1 }],
            vec![
                commit(0, vec![]),
                WalFrame::Compacted {
                    checksum: 1,
                    length: 1,
                },
                WalFrame::Revert { seq: 0 },
            ],
            vec![
                commit(0, vec![]),
                WalFrame::Revert { seq: 0 },
                WalFrame::Revert { seq: 0 },
            ],
        ] {
            assert!(
                matches!(parse_wal(&log_with(&frames)), Err(FormatError::WriteLog(_))),
                "{frames:?}"
            );
        }
    }

    #[test]
    fn out_of_order_sequence_is_an_error() {
        let data = log_with(&[commit(0, vec![]), commit(2, vec![])]);
        assert!(matches!(parse_wal(&data), Err(FormatError::WriteLog(_))));
    }

    #[test]
    fn crc_valid_garbage_payload_is_an_error_not_torn() {
        // A frame whose CRC is correct but whose payload is not a WalFrame
        // means an incompatible writer, even at the end of the file.
        let payload = [0xFFu8; 6];
        let len = (payload.len() as u32).to_le_bytes();
        let crc = crc32c::crc32c_append(crc32c::crc32c(&len), &payload);
        let mut data = header().to_bytes().to_vec();
        data.extend_from_slice(&len);
        data.extend_from_slice(&crc.to_le_bytes());
        data.extend_from_slice(&payload);
        assert!(matches!(parse_wal(&data), Err(FormatError::WriteLog(_))));
    }

    #[test]
    fn last_compacted_reports_trailing_commits() {
        let data = log_with(&[
            WalFrame::Compacted {
                checksum: 1,
                length: 2,
            },
            commit(0, vec![insert(0, 1)]),
        ]);
        assert_eq!(
            parse_wal(&data).unwrap().last_compacted(),
            Some((1, 2, true))
        );
        assert_eq!(parse_wal(&log_with(&[])).unwrap().last_compacted(), None);
    }
}
