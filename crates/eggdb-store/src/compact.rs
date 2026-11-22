//! Compaction: fold the write log into a new main file.
//!
//! Crash safety comes from ordering (see `docs/v0.3_plan.md`, M14):
//! new file durable → `Compacted` marker durable → rename → remove log.
//! A crash at any point leaves either the old main file with its log, or the
//! new main file with a log that readers recognize as already absorbed.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use eggdb_core::{
    encode_table, validate, write_egg, ColumnInput, EggReader, Header, PendingCounts,
    PreCompressedBlock, TableInput, WriteOptions,
};

use crate::fsutil::{compact_tmp_path, sync_parent_dir, wal_path, FileIdentity};
use crate::snapshot::EggSnapshot;
use crate::writer::WalWriter;
use crate::StoreError;

/// Options for [`compact`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactOptions {
    /// zstd level for re-encoded tables. Unchanged tables keep their blocks.
    pub zstd_level: i32,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self { zstd_level: 3 }
    }
}

/// What compaction did to one table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCompaction {
    /// Table name.
    pub table: String,
    /// Rows in the old main file.
    pub rows_before: u64,
    /// Rows in the new main file.
    pub rows_after: u64,
    /// Changes that were folded in.
    pub changes: PendingCounts,
    /// False when the table had no changes and its blocks were copied as-is.
    pub rewritten: bool,
}

/// Summary of a completed compaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactReport {
    /// Per-table results, in table order.
    pub tables: Vec<TableCompaction>,
    /// Old main file size in bytes.
    pub size_before: u64,
    /// New main file size in bytes.
    pub size_after: u64,
}

/// Result of [`compact`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    /// Nothing to fold in. Any leftover (empty or absorbed) log was removed.
    NothingPending,
    /// A new main file replaced the old one and the log was removed.
    Compacted(CompactReport),
}

/// Fold `egg`'s write log into a new main file and remove the log.
pub fn compact(egg: &Path, options: &CompactOptions) -> Result<CompactOutcome, StoreError> {
    if FileIdentity::of_path(&wal_path(egg))?.is_none() {
        return Ok(CompactOutcome::NothingPending);
    }
    let (mut writer, log) = WalWriter::acquire(egg, None)?;
    // Holding the lock, so no other writer or compaction can change either file.
    let data = std::fs::read(egg).map_err(|e| StoreError::io("reading", egg, e))?;
    let snapshot = EggSnapshot::from_parts(egg, data, Some(&log), None)?;
    if snapshot.base() != writer.base() {
        return Err(StoreError::Stale {
            egg: egg.to_path_buf(),
        });
    }
    if !snapshot.has_pending_changes() {
        writer.remove_log()?;
        return Ok(CompactOutcome::NothingPending);
    }

    check_integrity(egg, snapshot.data())?;
    let (inputs, mut report) = build_tables(&snapshot)?;
    let new_bytes = write_egg(&inputs, &write_options(snapshot.header(), options))
        .map_err(|e| StoreError::format(egg, e))?;
    verify_output(egg, &new_bytes, &report)?;

    install(egg, &mut writer, &new_bytes)?;
    writer.remove_log()?;
    report.size_after = new_bytes.len() as u64;
    Ok(CompactOutcome::Compacted(report))
}

/// Refuse to rewrite a main file that fails any integrity check: reused
/// blocks are copied without decoding, so corruption would be carried forward.
fn check_integrity(egg: &Path, data: &[u8]) -> Result<(), StoreError> {
    let results = validate(data).map_err(|e| StoreError::format(egg, e))?;
    match results.into_iter().find(|r| !r.passed) {
        Some(failed) => Err(StoreError::Integrity {
            egg: egg.to_path_buf(),
            check: failed.check,
            detail: failed.detail,
        }),
        None => Ok(()),
    }
}

fn build_tables(snapshot: &EggSnapshot) -> Result<(Vec<TableInput>, CompactReport), StoreError> {
    let mut inputs = Vec::with_capacity(snapshot.tables().len());
    let mut tables = Vec::with_capacity(snapshot.tables().len());
    for (t, (meta, overlay)) in snapshot
        .tables()
        .iter()
        .zip(snapshot.overlays())
        .enumerate()
    {
        let rewritten = !overlay.is_empty();
        let input = if rewritten {
            let columns = snapshot.visible_table(t)?;
            encode_table(meta, overlay.live_rows(), columns)
                .map_err(|e| StoreError::format(snapshot.egg_path(), e))?
        } else {
            reuse_table(snapshot, t)?
        };
        tables.push(TableCompaction {
            table: meta.table_name.clone(),
            rows_before: meta.row_count,
            rows_after: overlay.live_rows(),
            changes: overlay.pending_counts(),
            rewritten,
        });
        inputs.push(input);
    }
    let report = CompactReport {
        tables,
        size_before: snapshot.data().len() as u64,
        size_after: 0,
    };
    Ok((inputs, report))
}

/// Writer input that copies an unchanged table's compressed blocks verbatim.
fn reuse_table(snapshot: &EggSnapshot, t: usize) -> Result<TableInput, StoreError> {
    let meta = &snapshot.tables()[t];
    let integrity = |detail: String| StoreError::Integrity {
        egg: snapshot.egg_path().to_path_buf(),
        check: "block_reuse".into(),
        detail,
    };
    let mut columns = Vec::with_capacity(meta.columns.len());
    for (c, col) in meta.columns.iter().enumerate() {
        let start = snapshot.header().yolk_offset.checked_add(col.yolk_offset);
        let end = start.and_then(|s| s.checked_add(col.yolk_length));
        let block = match (start, end) {
            (Some(s), Some(e)) => snapshot.data().get(s as usize..e as usize),
            _ => None,
        }
        .ok_or_else(|| {
            integrity(format!(
                "{}.{}: block out of range",
                meta.table_name, col.name
            ))
        })?;
        let crc32c = snapshot
            .membrane()
            .block_checksums
            .iter()
            .find(|b| b.table_index as usize == t && b.column_index as usize == c)
            .map(|b| b.crc32c)
            .ok_or_else(|| {
                integrity(format!(
                    "{}.{}: no block checksum",
                    meta.table_name, col.name
                ))
            })?;
        columns.push(ColumnInput {
            name: col.name.clone(),
            sqlite_type: col.sqlite_type,
            encoding: col.encoding,
            cardinality: col.cardinality,
            null_count: col.null_count,
            null_bitmap: Default::default(),
            encoded_values: Vec::new(),
            dictionary: col.dictionary.clone(),
            min_value: col.min_value.clone(),
            max_value: col.max_value.clone(),
            pre_compressed: Some(PreCompressedBlock {
                compressed_data: block.to_vec(),
                uncompressed_size: col.uncompressed_size,
                crc32c,
            }),
        });
    }
    Ok(TableInput {
        table_name: meta.table_name.clone(),
        row_count: meta.row_count,
        original_ddl: meta.original_ddl.clone(),
        columns,
        index_definitions: meta.index_definitions.clone(),
    })
}

fn write_options(header: &Header, options: &CompactOptions) -> WriteOptions {
    WriteOptions {
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        original_size: header.original_size,
        flags: header.flags,
        zstd_level: options.zstd_level,
        ..WriteOptions::default()
    }
}

/// The new file must pass every integrity check and have exactly the
/// expected row counts before it may replace the old one.
fn verify_output(egg: &Path, bytes: &[u8], report: &CompactReport) -> Result<(), StoreError> {
    check_integrity(egg, bytes)?;
    let reader = EggReader::open(bytes).map_err(|e| StoreError::format(egg, e))?;
    let counts: Vec<u64> = reader.tables().iter().map(|t| t.row_count).collect();
    let expected: Vec<u64> = report.tables.iter().map(|t| t.rows_after).collect();
    if counts == expected {
        Ok(())
    } else {
        Err(StoreError::Integrity {
            egg: egg.to_path_buf(),
            check: "compacted_row_counts".into(),
            detail: format!("expected {expected:?}, new file has {counts:?}"),
        })
    }
}

/// Make the new file durable, record the marker, then atomically replace.
fn install(egg: &Path, writer: &mut WalWriter, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = compact_tmp_path(egg);
    let mut file = File::create(&tmp).map_err(|e| StoreError::io("creating", &tmp, e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| StoreError::io("writing", &tmp, e))?;
    drop(file);
    // Keep the original file's permissions (rename would otherwise reset them).
    let perms = std::fs::metadata(egg)
        .map_err(|e| StoreError::io("reading metadata of", egg, e))?
        .permissions();
    std::fs::set_permissions(&tmp, perms)
        .map_err(|e| StoreError::io("setting permissions on", &tmp, e))?;

    let header = Header::from_bytes(bytes).map_err(|e| StoreError::format(egg, e))?;
    writer.append_compacted(header.checksum, bytes.len() as u64)?;

    std::fs::rename(&tmp, egg).map_err(|e| StoreError::io("replacing", egg, e))?;
    sync_parent_dir(egg)
}
