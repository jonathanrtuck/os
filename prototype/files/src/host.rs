//! macOS-backed prototype of `Files`.
//!
//! Files live at `{base}/files/{id}`, snapshots at `{base}/snapshots/{id}`.
//! An atomic counter generates unique IDs. Snapshots are plain file copies —
//! the real implementation will use COW block sharing for O(1) snapshots.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::{FileId, Files, SnapshotId, SnapshotInfo};

/// Snapshot bookkeeping stored in memory.
struct SnapshotEntry {
    file_id: FileId,
    path: PathBuf,
    timestamp: SystemTime,
}

/// A `Files` implementation backed by the macOS (host) filesystem.
///
/// Intended for prototyping only — validates the interface before building
/// the real COW filesystem. Files and snapshots are regular files in a
/// directory tree; snapshots are full copies.
#[derive(Debug)]
pub struct HostFiles {
    base: PathBuf,
    files_dir: PathBuf,
    snapshots_dir: PathBuf,
    next_id: AtomicU64,
    /// FileId → path on disk.
    files: HashMap<FileId, PathBuf>,
    /// SnapshotId → bookkeeping.
    snapshots: HashMap<SnapshotId, SnapshotEntry>,
}

impl HostFiles {
    /// Create a new `HostFiles` rooted at `base`.
    ///
    /// Creates the `files/` and `snapshots/` subdirectories if they don't
    /// already exist.
    pub fn new(base: impl AsRef<Path>) -> io::Result<Self> {
        let base = base.as_ref().to_path_buf();
        let files_dir = base.join("files");
        let snapshots_dir = base.join("snapshots");

        fs::create_dir_all(&files_dir)?;
        fs::create_dir_all(&snapshots_dir)?;

        Ok(Self {
            base,
            files_dir,
            snapshots_dir,
            next_id: AtomicU64::new(1),
            files: HashMap::new(),
            snapshots: HashMap::new(),
        })
    }

    /// The base directory this store operates in.
    pub fn base(&self) -> &Path {
        &self.base
    }
    /// Resolve a `FileId` to its on-disk path, or return `NotFound`.
    fn file_path(&self, file: FileId) -> io::Result<&PathBuf> {
        self.files.get(&file).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("file {:?} not found", file),
            )
        })
    }
    /// Allocate the next unique ID (used for both files and snapshots).
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
    /// Resolve a `SnapshotId` to its entry, or return `NotFound`.
    fn snap_entry(&self, snap: SnapshotId) -> io::Result<&SnapshotEntry> {
        self.snapshots.get(&snap).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {:?} not found", snap),
            )
        })
    }
}
impl Drop for HostFiles {
    /// Best-effort cleanup of the base directory on drop.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
impl Files for HostFiles {
    fn clone_file(&mut self, source: FileId) -> io::Result<FileId> {
        let src_path = self.file_path(source)?.clone();
        let id = FileId(self.next_id());
        let dst_path = self.files_dir.join(id.0.to_string());

        fs::copy(&src_path, &dst_path)?;

        self.files.insert(id, dst_path);

        Ok(id)
    }
    fn create(&mut self) -> io::Result<FileId> {
        let id = FileId(self.next_id());
        let path = self.files_dir.join(id.0.to_string());

        fs::File::create(&path)?;

        self.files.insert(id, path);

        Ok(id)
    }
    fn delete(&mut self, file: FileId) -> io::Result<()> {
        let path = self.file_path(file)?.clone();

        fs::remove_file(&path)?;

        self.files.remove(&file);

        // Remove all snapshots belonging to this file.
        let snap_ids: Vec<SnapshotId> = self
            .snapshots
            .iter()
            .filter(|(_, entry)| entry.file_id == file)
            .map(|(id, _)| *id)
            .collect();

        for snap_id in snap_ids {
            if let Some(entry) = self.snapshots.remove(&snap_id) {
                let _ = fs::remove_file(&entry.path);
            }
        }

        Ok(())
    }
    fn delete_snapshot(&mut self, snap: SnapshotId) -> io::Result<()> {
        let entry = self.snapshots.remove(&snap).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {:?} not found", snap),
            )
        })?;

        fs::remove_file(&entry.path)
    }
    fn read(&self, file: FileId) -> io::Result<Vec<u8>> {
        let path = self.file_path(file)?;

        fs::read(path)
    }
    fn read_snapshot(&self, snap: SnapshotId) -> io::Result<Vec<u8>> {
        let entry = self.snap_entry(snap)?;

        fs::read(&entry.path)
    }
    fn resize(&mut self, file: FileId, new_size: u64) -> io::Result<()> {
        let path = self.file_path(file)?;
        let f = fs::OpenOptions::new().write(true).open(path)?;

        f.set_len(new_size)?;

        Ok(())
    }
    fn restore(&mut self, file: FileId, snap: SnapshotId) -> io::Result<()> {
        let entry = self.snap_entry(snap)?;

        if entry.file_id != file {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("snapshot {:?} does not belong to file {:?}", snap, file),
            ));
        }

        let snap_path = entry.path.clone();
        let file_path = self.file_path(file)?.clone();

        fs::copy(&snap_path, &file_path)?;

        Ok(())
    }
    fn size(&self, file: FileId) -> io::Result<u64> {
        let path = self.file_path(file)?;

        Ok(fs::metadata(path)?.len())
    }
    fn snapshot(&mut self, file: FileId) -> io::Result<SnapshotId> {
        let src_path = self.file_path(file)?.clone();
        let snap_id = SnapshotId(self.next_id());
        let snap_path = self.snapshots_dir.join(snap_id.0.to_string());

        fs::copy(&src_path, &snap_path)?;

        self.snapshots.insert(
            snap_id,
            SnapshotEntry {
                file_id: file,
                path: snap_path,
                timestamp: SystemTime::now(),
            },
        );

        Ok(snap_id)
    }
    fn snapshots(&self, file: FileId) -> io::Result<Vec<SnapshotInfo>> {
        // Verify the file exists.
        let _ = self.file_path(file)?;
        let mut infos: Vec<SnapshotInfo> = self
            .snapshots
            .iter()
            .filter(|(_, entry)| entry.file_id == file)
            .map(|(id, entry)| SnapshotInfo {
                id: *id,
                timestamp: entry.timestamp,
                file_size: fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(0),
            })
            .collect();

        infos.sort_by_key(|info| info.timestamp);

        Ok(infos)
    }
    fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};

        let path = self.file_path(file)?.clone();
        let mut f = fs::OpenOptions::new().write(true).open(&path)?;
        // Extend the file if the write would go past the current end.
        let end = offset + data.len() as u64;
        let current_len = f.metadata()?.len();

        if end > current_len {
            f.set_len(end)?;
        }

        f.seek(SeekFrom::Start(offset))?;
        f.write_all(data)?;

        Ok(())
    }
}

impl std::fmt::Debug for SnapshotEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotEntry")
            .field("file_id", &self.file_id)
            .field("path", &self.path)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}
