//! Block device abstraction.
//!
//! The `BlockDevice` trait is the filesystem's foundation. Every layer above
//! (superblock, allocator, inodes, snapshots) operates through this trait.

use crate::FsError;

/// Abstract block device.
///
/// All reads and writes operate on fixed-size blocks of `BLOCK_SIZE` bytes.
/// `flush` ensures durability: all previously written blocks reach stable
/// storage. The commit protocol depends on flush ordering.
pub trait BlockDevice {
    /// Read block at `index` into `buf` (must be exactly `BLOCK_SIZE` bytes).
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError>;
    /// Write `data` (must be exactly `BLOCK_SIZE` bytes) to block at `index`.
    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError>;
    /// Flush all writes to stable storage.
    fn flush(&mut self) -> Result<(), FsError>;
    /// Total number of blocks on this device.
    fn block_count(&self) -> u32;
}
