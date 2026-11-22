//! Exclusive, crash-safe appends to the write log.
//!
//! One writer at a time: the log file is locked with an OS file lock for the
//! writer's whole lifetime. Readers never lock. Every frame is followed by an
//! fsync, so a returned `Ok` means the frame survives a crash.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use eggdb_core::{encode_frame, Header, ParsedWal, WalFrame, WalHeader, WalOp, WAL_HEADER_SIZE};

use crate::fsutil::{sync_parent_dir, wal_path, FileIdentity};
use crate::snapshot::parse_log_bytes;
use crate::StoreError;

/// How many times to retry when the locked log is replaced underneath us.
const MAX_LOCK_ATTEMPTS: usize = 5;

/// Holder of the exclusive write lock on an egg's write log.
#[derive(Debug)]
pub struct WalWriter {
    egg_path: PathBuf,
    wal_path: PathBuf,
    file: File,
    base: WalHeader,
    valid_length: u64,
    next_seq: u64,
    /// `seq` of the frame most recently appended, if it was a Commit.
    last_commit: Option<u64>,
    /// A failed append could not be cleaned up: the file may hold a complete
    /// frame the caller believes was never written.
    uncertain: bool,
}

impl WalWriter {
    /// Take the write lock for `egg`, creating the log if needed.
    ///
    /// `expected` is the main file the caller has loaded; if the main file on
    /// disk is different, fails with [`StoreError::Stale`]. `None` accepts
    /// whatever main file is on disk (used by compaction).
    ///
    /// Returns the writer and the log as it is now, so the caller can catch up
    /// on frames other processes committed since it last read the log. A log
    /// left over from a finished compaction, or from an interrupted creation,
    /// is reset to empty.
    pub fn acquire(
        egg: &Path,
        expected: Option<WalHeader>,
    ) -> Result<(Self, ParsedWal), StoreError> {
        let wal = wal_path(egg);
        let file = lock_log_file(egg, &wal)?;
        let base = current_base(egg)?;
        if expected.is_some_and(|exp| exp != base) {
            return Err(StoreError::Stale {
                egg: egg.to_path_buf(),
            });
        }
        let bytes = read_all(&file, &wal)?;
        let mut writer = WalWriter {
            egg_path: egg.to_path_buf(),
            wal_path: wal,
            file,
            base,
            valid_length: 0,
            next_seq: 0,
            last_commit: None,
            uncertain: false,
        };
        let log = writer.adopt(&bytes)?;
        Ok((writer, log))
    }

    /// The main file identity this log is bound to.
    #[must_use]
    pub fn base(&self) -> WalHeader {
        self.base
    }

    /// The `seq` the next commit will use.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Length of the log, all frames durable.
    #[must_use]
    pub fn valid_length(&self) -> u64 {
        self.valid_length
    }

    /// Durably append one transaction. Returns its sequence number.
    pub fn append_commit(&mut self, ops: Vec<WalOp>) -> Result<u64, StoreError> {
        let seq = self.next_seq;
        self.append(&WalFrame::Commit { seq, ops })?;
        self.next_seq += 1;
        self.last_commit = Some(seq);
        Ok(seq)
    }

    /// Durably cancel the commit appended immediately before this call.
    pub fn append_revert(&mut self, seq: u64) -> Result<(), StoreError> {
        if self.last_commit != Some(seq) {
            return Err(StoreError::format(
                &self.wal_path,
                eggdb_core::FormatError::WriteLog(format!(
                    "cannot revert commit {seq}: it is not the last frame written"
                )),
            ));
        }
        self.append(&WalFrame::Revert { seq })?;
        self.last_commit = None;
        Ok(())
    }

    /// Durably record that a new main file with this identity was produced.
    pub fn append_compacted(&mut self, checksum: u32, length: u64) -> Result<(), StoreError> {
        self.append(&WalFrame::Compacted { checksum, length })?;
        self.last_commit = None;
        Ok(())
    }

    /// Delete the log (while still holding the lock), then release the lock.
    pub fn remove_log(self) -> Result<(), StoreError> {
        std::fs::remove_file(&self.wal_path)
            .map_err(|e| StoreError::io("removing", &self.wal_path, e))?;
        sync_parent_dir(&self.wal_path)
        // `self.file` drops here, releasing the lock on the unlinked inode.
        // Waiting writers notice the path changed and retry.
    }

    /// Classify the log against the main file and make it ready for appends.
    fn adopt(&mut self, bytes: &[u8]) -> Result<ParsedWal, StoreError> {
        let Some(parsed) = parse_log_bytes(&self.egg_path, bytes)? else {
            return self.reset(); // new file, or creation interrupted before any frame
        };
        if parsed.header == self.base {
            if parsed.torn_tail_bytes > 0 {
                self.truncate_durably(parsed.valid_length)?;
            }
            self.valid_length = parsed.valid_length;
            self.next_seq = parsed.next_seq;
            return Ok(parsed);
        }
        match parsed.last_compacted() {
            Some((checksum, length, false))
                if checksum == self.base.base_checksum && length == self.base.base_length =>
            {
                self.reset()
            }
            _ => Err(StoreError::ForeignLog {
                egg: self.egg_path.clone(),
                wal: self.wal_path.clone(),
            }),
        }
    }

    /// Empty the log and write a header bound to the current main file.
    fn reset(&mut self) -> Result<ParsedWal, StoreError> {
        // Truncate durably BEFORE writing the new header. Otherwise a crash
        // could persist the new header in front of old frames, and those
        // frames would be replayed against the wrong main file.
        self.truncate_durably(0)?;
        self.write_at(0, &self.base.to_bytes())
            .and_then(|()| self.file.sync_all())
            .map_err(|e| StoreError::io("writing", &self.wal_path, e))?;
        sync_parent_dir(&self.wal_path)?;
        self.valid_length = WAL_HEADER_SIZE as u64;
        self.next_seq = 0;
        Ok(ParsedWal {
            header: self.base,
            entries: Vec::new(),
            next_seq: 0,
            valid_length: self.valid_length,
            torn_tail_bytes: 0,
        })
    }

    fn append(&mut self, frame: &WalFrame) -> Result<(), StoreError> {
        let bytes = encode_frame(frame).map_err(|e| StoreError::format(&self.wal_path, e))?;
        // Always cut the file back to the valid prefix first, so leftovers of
        // an earlier failed write can never sit behind this frame.
        let written = self
            .file
            .set_len(self.valid_length)
            .and_then(|()| self.write_at(self.valid_length, &bytes))
            .and_then(|()| self.file.sync_data());
        if let Err(e) = written {
            // The frame may be complete on disk even though the fsync failed.
            // Remove it durably; if even that fails, readers might apply it.
            if self.truncate_durably(self.valid_length).is_err() {
                self.uncertain = true;
            }
            return Err(StoreError::io("appending to", &self.wal_path, e));
        }
        self.valid_length += bytes.len() as u64;
        Ok(())
    }

    /// True if a failed append might still be visible to readers.
    #[must_use]
    pub fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    fn truncate_durably(&mut self, len: u64) -> Result<(), StoreError> {
        self.file
            .set_len(len)
            .and_then(|()| self.file.sync_all())
            .map_err(|e| StoreError::io("truncating", &self.wal_path, e))
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(bytes)
    }
}

/// Open (creating if absent) and exclusively lock the log file, making sure
/// the locked file is still the one at `wal` (compaction may have removed it
/// while we waited).
fn lock_log_file(egg: &Path, wal: &Path) -> Result<File, StoreError> {
    for _ in 0..MAX_LOCK_ATTEMPTS {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(wal)
            .map_err(|e| StoreError::io("opening", wal, e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(StoreError::Busy {
                    egg: egg.to_path_buf(),
                })
            }
            Err(TryLockError::Error(e)) => return Err(StoreError::io("locking", wal, e)),
        }
        let locked = FileIdentity::of_file(&file, wal)?;
        if FileIdentity::of_path(wal)?.is_some_and(|now| now.same_file(&locked)) {
            return Ok(file);
        }
    }
    Err(StoreError::Busy {
        egg: egg.to_path_buf(),
    })
}

/// Identity of the main file on disk now. Refuses encrypted files.
fn current_base(egg: &Path) -> Result<WalHeader, StoreError> {
    let mut file = File::open(egg).map_err(|e| StoreError::io("opening", egg, e))?;
    let mut buf = [0u8; Header::SIZE];
    file.read_exact(&mut buf)
        .map_err(|e| StoreError::io("reading header of", egg, e))?;
    let header = Header::from_bytes(&buf).map_err(|e| StoreError::format(egg, e))?;
    if header.flags.has_encryption() {
        return Err(StoreError::EncryptedWrite {
            egg: egg.to_path_buf(),
        });
    }
    let len = file
        .metadata()
        .map_err(|e| StoreError::io("reading metadata of", egg, e))?
        .len();
    Ok(WalHeader {
        base_checksum: header.checksum,
        base_length: len,
    })
}

fn read_all(mut file: &File, path: &Path) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_to_end(&mut bytes))
        .map_err(|e| StoreError::io("reading", path, e))?;
    Ok(bytes)
}
