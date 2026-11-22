//! Path naming and small filesystem helpers (durability, file identity).

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::StoreError;

/// Path of the write log for `egg`: the file name with `.wal` appended
/// (`data.egg` → `data.egg.wal`).
#[must_use]
pub fn wal_path(egg: &Path) -> PathBuf {
    with_suffix(egg, ".wal")
}

/// Temporary file compaction writes before renaming over the main file.
#[must_use]
pub fn compact_tmp_path(egg: &Path) -> PathBuf {
    with_suffix(egg, ".compact-tmp")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Error if `egg` has a write log beside it. Used before overwriting an
/// output path, so pending changes are never orphaned.
pub fn ensure_no_pending_log(egg: &Path) -> Result<(), StoreError> {
    let wal = wal_path(egg);
    match std::fs::symlink_metadata(&wal) {
        Ok(_) => Err(StoreError::PendingLog {
            egg: egg.to_path_buf(),
            wal,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreError::io("checking for write log", wal, e)),
    }
}

/// Make a rename or unlink in `path`'s directory durable.
///
/// On unix, fsyncs the parent directory. Elsewhere directory handles cannot be
/// synced, and this is a no-op.
pub fn sync_parent_dir(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| StoreError::io("syncing directory", dir, e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Enough metadata to tell whether a path now names a different file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    #[cfg(unix)]
    dev_ino: (u64, u64),
    len: u64,
    modified: Option<SystemTime>,
}

impl FileIdentity {
    /// Identity of an open file handle.
    pub fn of_file(file: &File, path: &Path) -> Result<Self, StoreError> {
        let meta = file
            .metadata()
            .map_err(|e| StoreError::io("reading metadata of", path, e))?;
        Ok(Self::from_metadata(&meta))
    }

    /// Identity of whatever `path` names right now, or `None` if it is gone.
    pub fn of_path(path: &Path) -> Result<Option<Self>, StoreError> {
        match std::fs::metadata(path) {
            Ok(meta) => Ok(Some(Self::from_metadata(&meta))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::io("reading metadata of", path, e)),
        }
    }

    /// File size in bytes when the identity was taken.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.len
    }

    /// Same underlying file, ignoring size and timestamps. On non-unix
    /// platforms there is no inode, so length and mtime stand in.
    #[must_use]
    pub fn same_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.dev_ino == other.dev_ino
        }
        #[cfg(not(unix))]
        {
            self == other
        }
    }

    fn from_metadata(meta: &Metadata) -> Self {
        Self {
            #[cfg(unix)]
            dev_ino: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_paths_append_to_file_name() {
        let egg = Path::new("/tmp/dir/data.egg");
        assert_eq!(wal_path(egg), Path::new("/tmp/dir/data.egg.wal"));
        assert_eq!(
            compact_tmp_path(egg),
            Path::new("/tmp/dir/data.egg.compact-tmp")
        );
        assert_eq!(wal_path(Path::new("x")), Path::new("x.wal"));
    }

    #[test]
    fn pending_log_detection() {
        let dir = tempfile::tempdir().unwrap();
        let egg = dir.path().join("a.egg");
        ensure_no_pending_log(&egg).unwrap();
        std::fs::write(wal_path(&egg), b"x").unwrap();
        assert!(matches!(
            ensure_no_pending_log(&egg),
            Err(StoreError::PendingLog { .. })
        ));
    }

    #[test]
    fn identity_tracks_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"one").unwrap();
        let first = FileIdentity::of_path(&path).unwrap().unwrap();
        assert_eq!(first.size(), 3);

        let tmp = dir.path().join("g");
        std::fs::write(&tmp, b"two").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        let second = FileIdentity::of_path(&path).unwrap().unwrap();
        assert_eq!(second.size(), 3);
        #[cfg(unix)]
        {
            assert!(!first.same_file(&second), "rename must yield a new inode");
            assert_ne!(first, second);
        }
        assert!(FileIdentity::of_path(&dir.path().join("missing"))
            .unwrap()
            .is_none());
    }
}
