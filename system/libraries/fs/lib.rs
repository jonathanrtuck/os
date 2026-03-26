//! COW filesystem library (`#![no_std]`).
//!
//! Implements the filesystem designed in `design/journal.md`. The
//! `BlockDevice` trait abstracts storage, allowing the same filesystem
//! code to run against any block-level backend.
//!
//! ## Public API
//!
//! The primary interface is the `Files` trait — what the OS service calls.
//! `Filesystem<D>` implements it over any `BlockDevice`.
//!
//! ```text
//! Core (documents) ──Files trait──→ Filesystem<D> ──BlockDevice──→ disk
//! ```

#![no_std]
extern crate alloc;

use alloc::{string::String, vec::Vec};

mod alloc_mod;
mod block;
mod crc32;
mod filesystem;
mod inode;
mod snapshot;
mod superblock;

pub use alloc_mod::Allocator;
pub use block::BlockDevice;
pub use filesystem::Filesystem;
pub use inode::{Inode, InodeExtent, INLINE_CAPACITY};
pub use superblock::{Superblock, DATA_START, RING_SIZE};

/// Block size in bytes. Matches the kernel's 16 KiB page size and the
/// on-disk format designed in the filesystem journal entry.
pub const BLOCK_SIZE: u32 = 16_384;

// ── Public API types ───────────────────────────────────────────────

/// Opaque, globally unique file identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u64);

/// Opaque, globally unique snapshot identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub u64);

/// File metadata (filesystem-level only — mimetype etc. is core's concern).
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub file_id: FileId,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
}

/// The filesystem interface — what the OS service calls.
///
/// Sole consumer: the OS service (core). Editors never interact with this
/// directly. The filesystem knows nothing about documents, undo ordering,
/// compound structures, or mimetype. It stores files, provides read/write
/// access, manages snapshots, and commits transactions.
///
/// `commit()` is the transaction boundary. Between commits, writes
/// accumulate in memory. A crash loses uncommitted writes — correct,
/// because the operation wasn't complete.
pub trait Files {
    /// Create a new empty file.
    fn create(&mut self) -> Result<FileId, FsError>;
    /// Delete a file and all its snapshots.
    fn delete(&mut self, file: FileId) -> Result<(), FsError>;
    /// Read file content into `buf` starting at `offset`. Returns bytes read.
    fn read(&self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    /// Write `data` at `offset`.
    fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), FsError>;
    /// Resize a file.
    fn truncate(&mut self, file: FileId, len: u64) -> Result<(), FsError>;
    /// Current file size in bytes.
    fn size(&self, file: FileId) -> Result<u64, FsError>;
    /// Snapshot the given files atomically. Returns the snapshot ID.
    fn snapshot(&mut self, files: &[FileId]) -> Result<SnapshotId, FsError>;
    /// Restore all files in a snapshot to their snapshotted state.
    fn restore(&mut self, snapshot: SnapshotId) -> Result<(), FsError>;
    /// Delete a snapshot, freeing blocks unique to it.
    fn delete_snapshot(&mut self, snapshot: SnapshotId) -> Result<(), FsError>;
    /// List snapshot IDs that include a given file.
    fn list_snapshots(&self, file: FileId) -> Result<Vec<SnapshotId>, FsError>;
    /// File metadata.
    fn metadata(&self, file: FileId) -> Result<FileMetadata, FsError>;
    /// Commit all pending changes. Two-flush protocol.
    fn commit(&mut self) -> Result<(), FsError>;
}

// ── Errors ─────────────────────────────────────────────────────────

/// Filesystem error.
#[derive(Debug)]
pub enum FsError {
    /// Block index out of range.
    OutOfBounds { block: u32, count: u32 },
    /// I/O error from underlying storage.
    Io,
    /// Buffer has wrong size (expected `BLOCK_SIZE` bytes).
    BadBufferSize { expected: u32, actual: usize },
    /// Wrong magic number in header or ring entry.
    BadMagic,
    /// CRC32 checksum mismatch.
    ChecksumMismatch { expected: u32, actual: u32 },
    /// No valid superblock entry found in ring.
    NoValidSuperblock,
    /// Device too small for the filesystem.
    DeviceTooSmall { blocks: u32, minimum: u32 },
    /// No free space available.
    NoSpace,
    /// On-disk data is corrupt or inconsistent.
    Corrupt(String),
    /// File or snapshot not found.
    NotFound(u64),
}

pub(crate) fn now_nanos() -> u64 {
    0
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds { block, count } => {
                write!(f, "block {block} out of range (device has {count} blocks)")
            }
            Self::Io => write!(f, "I/O error"),
            Self::BadBufferSize { expected, actual } => {
                write!(f, "buffer size {actual} != expected {expected}")
            }
            Self::BadMagic => write!(f, "bad magic number"),
            Self::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: stored {expected:#010x}, computed {actual:#010x}"
                )
            }
            Self::NoValidSuperblock => write!(f, "no valid superblock entry in ring"),
            Self::DeviceTooSmall { blocks, minimum } => {
                write!(f, "device has {blocks} blocks, need at least {minimum}")
            }
            Self::NoSpace => write!(f, "no free space available"),
            Self::Corrupt(msg) => write!(f, "corrupt: {msg}"),
            Self::NotFound(id) => write!(f, "file/snapshot {id} not found"),
        }
    }
}
