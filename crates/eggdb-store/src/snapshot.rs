//! A consistent, read-only view of an `.egg` main file plus its write log.

use std::path::{Path, PathBuf};

use eggdb_core::{
    decode_columns_parallel, parse_wal, replay, EggReader, Header, Membrane, ParsedWal, TableMeta,
    TableOverlay, Value, WalHeader, Zeroizing, WAL_HEADER_SIZE,
};

use crate::fsutil::{wal_path, FileIdentity};
use crate::StoreError;

/// How many times to re-read when the main file changes mid-read.
const MAX_READ_ATTEMPTS: usize = 5;

/// Status of the write log relative to the main file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogState {
    /// No write log (or only an interrupted creation with no frames).
    Absent,
    /// The log applies to this main file.
    Active {
        /// The `seq` the next Commit frame must use.
        next_seq: u64,
        /// Length of the valid log prefix in bytes.
        valid_length: u64,
        /// Bytes after the valid prefix, left by an interrupted write.
        torn_tail_bytes: u64,
    },
    /// A compaction already folded this log into the main file but crashed
    /// before removing it. The next writer or compaction removes it.
    Absorbed,
}

/// The main file's bytes and metadata, with every table's committed changes
/// replayed into a [`TableOverlay`].
pub struct EggSnapshot {
    egg_path: PathBuf,
    data: Vec<u8>,
    header: Header,
    tables: Vec<TableMeta>,
    membrane: Membrane,
    encryption_key: Option<Zeroizing<[u8; 32]>>,
    overlays: Vec<TableOverlay>,
    log: LogState,
}

/// Manual impl so the decryption key never appears in debug output.
impl std::fmt::Debug for EggSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EggSnapshot")
            .field("egg_path", &self.egg_path)
            .field("data_len", &self.data.len())
            .field("header", &self.header)
            .field("tables", &self.tables.len())
            .field("encrypted", &self.encryption_key.is_some())
            .field("log", &self.log)
            .finish_non_exhaustive()
    }
}

impl EggSnapshot {
    /// Read `egg` and its write log (if any) as one consistent snapshot.
    pub fn open(egg: &Path, passphrase: Option<&str>) -> Result<Self, StoreError> {
        let (data, wal) = read_consistent(egg)?;
        let parsed = wal
            .as_deref()
            .map(|bytes| parse_log_bytes(egg, bytes))
            .transpose()?
            .flatten();
        Self::from_parts(egg, data, parsed.as_ref(), passphrase)
    }

    /// Build a snapshot from main-file bytes and an already parsed log.
    pub fn from_parts(
        egg: &Path,
        data: Vec<u8>,
        log: Option<&ParsedWal>,
        passphrase: Option<&str>,
    ) -> Result<Self, StoreError> {
        let (header, tables, membrane, encryption_key) = parse_main(egg, &data, passphrase)?;
        let base = WalHeader {
            base_checksum: header.checksum,
            base_length: data.len() as u64,
        };
        let (overlays, log) = match log {
            None => (empty_overlays(&tables), LogState::Absent),
            Some(parsed) => classify_log(egg, base, &tables, parsed)?,
        };
        Ok(Self {
            egg_path: egg.to_path_buf(),
            data,
            header,
            tables,
            membrane,
            encryption_key,
            overlays,
            log,
        })
    }

    /// Path of the main file.
    #[must_use]
    pub fn egg_path(&self) -> &Path {
        &self.egg_path
    }

    /// Raw main-file bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Parsed main-file header.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Table metadata from the main file. `row_count` there excludes logged
    /// changes; use [`live_row_count`](Self::live_row_count) for query counts.
    #[must_use]
    pub fn tables(&self) -> &[TableMeta] {
        &self.tables
    }

    /// Index of the table called `name`.
    #[must_use]
    pub fn table_index(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.table_name == name)
    }

    /// The main file's integrity section.
    #[must_use]
    pub fn membrane(&self) -> &Membrane {
        &self.membrane
    }

    /// The derived decryption key, when opened with a passphrase.
    #[must_use]
    pub fn encryption_key(&self) -> Option<&[u8; 32]> {
        self.encryption_key.as_deref()
    }

    /// The identity a write log must carry to apply to this main file.
    #[must_use]
    pub fn base(&self) -> WalHeader {
        WalHeader {
            base_checksum: self.header.checksum,
            base_length: self.data.len() as u64,
        }
    }

    /// Status of the write log at snapshot time.
    #[must_use]
    pub fn log_state(&self) -> &LogState {
        &self.log
    }

    /// One overlay per table, in table order.
    #[must_use]
    pub fn overlays(&self) -> &[TableOverlay] {
        &self.overlays
    }

    /// Replace overlays and log state after replaying a newer log.
    pub fn refresh_log(&mut self, log: &ParsedWal) -> Result<(), StoreError> {
        let (overlays, state) = classify_log(&self.egg_path, self.base(), &self.tables, log)?;
        self.overlays = overlays;
        self.log = state;
        Ok(())
    }

    /// Adopt overlays a writer just made durable, without re-reading the log.
    /// `overlays` must have one entry per table with matching shapes.
    pub fn replace_overlays(
        &mut self,
        overlays: Vec<TableOverlay>,
        log: LogState,
    ) -> Result<(), StoreError> {
        let shapes_match = overlays.len() == self.tables.len()
            && overlays
                .iter()
                .zip(&self.tables)
                .all(|(o, t)| o.base_rows() == t.row_count);
        if !shapes_match {
            return Err(self.format_err("overlays do not match the main file's tables".into()));
        }
        self.overlays = overlays;
        self.log = log;
        Ok(())
    }

    /// Record a new log position whose frames do not change visible rows
    /// (for example a commit followed by its revert).
    pub fn set_log_state(&mut self, log: LogState) {
        self.log = log;
    }

    /// True when the log changes at least one visible row.
    #[must_use]
    pub fn has_pending_changes(&self) -> bool {
        self.overlays.iter().any(|o| !o.is_empty())
    }

    /// Rows a query sees in table `t` (main-file rows plus logged changes).
    #[must_use]
    pub fn live_row_count(&self, t: usize) -> u64 {
        self.overlays.get(t).map_or(0, TableOverlay::live_rows)
    }

    /// Decode main-file values (no logged changes) for the given columns.
    pub fn decode_base_columns(
        &self,
        t: usize,
        cols: &[usize],
    ) -> Result<Vec<Vec<Value>>, StoreError> {
        let table = self.table(t)?;
        let metas = cols
            .iter()
            .map(|&c| {
                table.columns.get(c).cloned().ok_or_else(|| {
                    self.format_err(format!("table '{}' has no column {c}", table.table_name))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        decode_columns_parallel(
            &self.data,
            self.header.yolk_offset,
            &metas,
            table.row_count,
            self.encryption_key(),
        )
        .map_err(|e| StoreError::format(&self.egg_path, e))
    }

    /// Live rows of the given columns, in rowid order, with changes applied.
    pub fn visible_columns(&self, t: usize, cols: &[usize]) -> Result<Vec<Vec<Value>>, StoreError> {
        let overlay = &self.overlays[self.check_table(t)?];
        self.decode_base_columns(t, cols)?
            .into_iter()
            .zip(cols)
            .map(|(base, &c)| {
                overlay
                    .visible_column(c, base)
                    .map_err(|e| StoreError::format(&self.egg_path, e))
            })
            .collect()
    }

    /// Every column of table `t`, live rows only, changes applied.
    pub fn visible_table(&self, t: usize) -> Result<Vec<Vec<Value>>, StoreError> {
        let all: Vec<usize> = (0..self.table(t)?.columns.len()).collect();
        self.visible_columns(t, &all)
    }

    fn table(&self, t: usize) -> Result<&TableMeta, StoreError> {
        self.tables
            .get(t)
            .ok_or_else(|| self.format_err(format!("no table at index {t}")))
    }

    fn check_table(&self, t: usize) -> Result<usize, StoreError> {
        self.table(t).map(|_| t)
    }

    fn format_err(&self, msg: String) -> StoreError {
        StoreError::format(&self.egg_path, eggdb_core::FormatError::ColumnData(msg))
    }
}

/// Parse write log bytes. `Ok(None)` means an interrupted log creation: the
/// file is no longer than a header and the header is invalid, so no frame was
/// ever written (frames are only appended after the header is durable).
pub fn parse_log_bytes(egg: &Path, bytes: &[u8]) -> Result<Option<ParsedWal>, StoreError> {
    match parse_wal(bytes) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(_) if bytes.len() <= WAL_HEADER_SIZE => Ok(None),
        Err(e) => Err(StoreError::format(wal_path(egg), e)),
    }
}

/// Decide what a parsed log means for the main file with identity `base`.
fn classify_log(
    egg: &Path,
    base: WalHeader,
    tables: &[TableMeta],
    parsed: &ParsedWal,
) -> Result<(Vec<TableOverlay>, LogState), StoreError> {
    if parsed.header == base {
        let overlays =
            replay(tables, parsed.commits()).map_err(|e| StoreError::format(wal_path(egg), e))?;
        let state = LogState::Active {
            next_seq: parsed.next_seq,
            valid_length: parsed.valid_length,
            torn_tail_bytes: parsed.torn_tail_bytes,
        };
        return Ok((overlays, state));
    }
    let absorbed = matches!(
        parsed.last_compacted(),
        Some((checksum, length, false))
            if checksum == base.base_checksum && length == base.base_length
    );
    if absorbed {
        Ok((empty_overlays(tables), LogState::Absorbed))
    } else {
        Err(StoreError::ForeignLog {
            egg: egg.to_path_buf(),
            wal: wal_path(egg),
        })
    }
}

type MainParts = (
    Header,
    Vec<TableMeta>,
    Membrane,
    Option<Zeroizing<[u8; 32]>>,
);

fn parse_main(egg: &Path, data: &[u8], passphrase: Option<&str>) -> Result<MainParts, StoreError> {
    let reader = match passphrase {
        Some(pass) => EggReader::open_with_passphrase(data, pass),
        None => EggReader::open(data),
    }
    .map_err(|e| StoreError::format(egg, e))?;
    Ok((
        reader.header().clone(),
        reader.tables().to_vec(),
        reader.membrane().clone(),
        reader.encryption_key().map(|k| Zeroizing::new(*k)),
    ))
}

fn empty_overlays(tables: &[TableMeta]) -> Vec<TableOverlay> {
    tables
        .iter()
        .map(|t| TableOverlay::new(t.row_count, t.columns.len()))
        .collect()
}

/// Read the main file and its log such that both belong together: if the main
/// file is replaced while reading (compaction renames a new one in), retry.
fn read_consistent(egg: &Path) -> Result<(Vec<u8>, Option<Vec<u8>>), StoreError> {
    let wal = wal_path(egg);
    for _ in 0..MAX_READ_ATTEMPTS {
        let before = FileIdentity::of_path(egg)?
            .ok_or_else(|| StoreError::io("opening", egg, std::io::ErrorKind::NotFound.into()))?;
        let data = std::fs::read(egg).map_err(|e| StoreError::io("reading", egg, e))?;
        let log = read_optional(&wal)?;
        let after = FileIdentity::of_path(egg)?;
        if after.as_ref() == Some(&before) && before.size() == data.len() as u64 {
            return Ok((data, log));
        }
    }
    Err(StoreError::Unstable {
        egg: egg.to_path_buf(),
    })
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io("reading", path, e)),
    }
}
