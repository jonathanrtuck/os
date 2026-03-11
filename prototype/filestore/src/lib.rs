//! FileStore — the filesystem interface for the OS service.
//!
//! This crate defines the `FileStore` trait (12 operations) and provides a
//! macOS-backed prototype implementation (`HostFileStore`) that stores files
//! as regular files on the host filesystem. The real implementation will use
//! a COW filesystem with memory-mapped access; this prototype substitutes
//! explicit read/write for mmap and file copies for snapshots.

use std::io;

mod host;

#[cfg(test)]
mod tests;

pub use host::HostFileStore;

/// Opaque, globally unique file identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u64);
/// Opaque, globally unique snapshot identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub u64);
/// Metadata about a snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub id: SnapshotId,
    pub timestamp: std::time::SystemTime,
    pub file_size: u64,
}

/// The filesystem interface — what the OS service calls.
///
/// Sole consumer: the OS service. Editors never interact with this directly.
/// The filesystem knows nothing about documents, undo ordering, compound
/// structures, or metadata. It stores files, provides mapped access, and
/// takes snapshots.
pub trait FileStore {
    /// Create a new independent file with the same content as source.
    /// Implementation may share physical blocks (COW clone) for efficiency.
    fn clone_file(&mut self, source: FileId) -> io::Result<FileId>;
    /// Create an empty file.
    fn create(&mut self) -> io::Result<FileId>;
    /// Delete a file and all its snapshots.
    fn delete(&mut self, file: FileId) -> io::Result<()>;
    /// Delete a snapshot, freeing storage unique to it.
    fn delete_snapshot(&mut self, snap: SnapshotId) -> io::Result<()>;
    /// Read the entire file content. (Prototype substitute for memory mapping.)
    fn read(&self, file: FileId) -> io::Result<Vec<u8>>;
    /// Read a snapshot's content without affecting current state.
    fn read_snapshot(&self, snap: SnapshotId) -> io::Result<Vec<u8>>;
    /// Grow or shrink a file.
    fn resize(&mut self, file: FileId, new_size: u64) -> io::Result<()>;
    /// Restore a file to a previous snapshot's state.
    fn restore(&mut self, file: FileId, snap: SnapshotId) -> io::Result<()>;
    /// Current size in bytes.
    fn size(&self, file: FileId) -> io::Result<u64>;
    /// Record the current state as a snapshot. Should be O(1) in the real
    /// implementation; the prototype uses file copies.
    fn snapshot(&mut self, file: FileId) -> io::Result<SnapshotId>;
    /// List snapshots for a file, ordered by creation time.
    fn snapshots(&self, file: FileId) -> io::Result<Vec<SnapshotInfo>>;
    /// Write content at the given offset. (Prototype substitute for mapped writes.)
    fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> io::Result<()>;
}
