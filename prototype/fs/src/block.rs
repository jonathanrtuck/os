//! Block device abstraction and implementations.
//!
//! The `BlockDevice` trait is the filesystem's foundation. Every layer above
//! (superblock, allocator, inodes, snapshots) operates through this trait.
//!
//! Three implementations:
//! - `FileBlockDevice` — file-backed, for the host prototype
//! - `MemoryBlockDevice` — in-memory, for unit tests
//! - `LoggingBlockDevice` — wraps any device, logs writes for crash testing

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::{FsError, BLOCK_SIZE};

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

fn check_bounds(index: u32, count: u32) -> Result<(), FsError> {
    if index >= count {
        return Err(FsError::OutOfBounds {
            block: index,
            count,
        });
    }
    Ok(())
}

fn check_buf(buf: &[u8]) -> Result<(), FsError> {
    if buf.len() != BLOCK_SIZE as usize {
        return Err(FsError::BadBufferSize {
            expected: BLOCK_SIZE,
            actual: buf.len(),
        });
    }
    Ok(())
}

// ── FileBlockDevice ────────────────────────────────────────────────────

/// A block device backed by a file on the host filesystem.
///
/// Uses `pread`/`pwrite` (position-independent I/O) so reads don't require
/// `&mut self`. Flush uses `sync_all`. Note: on macOS, true durability
/// requires `fcntl(F_FULLFSYNC)` — the hypervisor's virtio-blk backend
/// handles this; the host prototype uses `sync_all` which is sufficient
/// for correctness testing (crash testing uses `LoggingBlockDevice`, not
/// actual power cuts).
pub struct FileBlockDevice {
    file: File,
    blocks: u32,
}

impl FileBlockDevice {
    /// Create a new block device file with `blocks` zero-filled blocks.
    /// Fails if the file already exists.
    pub fn create(path: impl AsRef<Path>, blocks: u32) -> Result<Self, FsError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.set_len(u64::from(blocks) * u64::from(BLOCK_SIZE))?;
        Ok(Self { file, blocks })
    }

    /// Open an existing block device file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FsError> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len();
        let blocks = (len / u64::from(BLOCK_SIZE)) as u32;
        Ok(Self { file, blocks })
    }
}

impl BlockDevice for FileBlockDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        check_bounds(index, self.blocks)?;
        check_buf(buf)?;
        self.file
            .read_exact_at(buf, u64::from(index) * u64::from(BLOCK_SIZE))?;
        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        check_bounds(index, self.blocks)?;
        check_buf(data)?;
        self.file
            .write_all_at(data, u64::from(index) * u64::from(BLOCK_SIZE))?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), FsError> {
        self.file.sync_all()?;
        Ok(())
    }

    fn block_count(&self) -> u32 {
        self.blocks
    }
}

// ── MemoryBlockDevice ──────────────────────────────────────────────────

/// An in-memory block device for unit testing.
///
/// All blocks are zero-initialized. No persistence.
pub struct MemoryBlockDevice {
    blocks: Vec<Vec<u8>>,
}

impl MemoryBlockDevice {
    /// Create a device with `count` zero-filled blocks.
    pub fn new(count: u32) -> Self {
        Self {
            blocks: (0..count).map(|_| vec![0u8; BLOCK_SIZE as usize]).collect(),
        }
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        check_bounds(index, self.blocks.len() as u32)?;
        check_buf(buf)?;
        buf.copy_from_slice(&self.blocks[index as usize]);
        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        check_bounds(index, self.blocks.len() as u32)?;
        check_buf(data)?;
        self.blocks[index as usize].copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), FsError> {
        Ok(())
    }

    fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }
}

// ── LoggingBlockDevice ─────────────────────────────────────────────────

/// A record of a single block write.
#[derive(Debug, Clone)]
pub struct WriteRecord {
    /// Block that was written.
    pub block: u32,
    /// Data written (`BLOCK_SIZE` bytes).
    pub data: Vec<u8>,
    /// Flush epoch at the time of write. Writes within the same epoch
    /// have no ordering guarantee on real hardware — they may land in
    /// any order on crash.
    pub epoch: u64,
}

/// Wraps any `BlockDevice`, logging all writes for crash consistency testing.
///
/// The write log records every block write with its flush epoch. After a
/// test workload, `replay_prefix` applies any subset of writes to a fresh
/// device to simulate crash states.
///
/// Flush epochs model hardware write barriers: writes before a flush are
/// durable; writes after the last flush may be lost or reordered.
pub struct LoggingBlockDevice<D> {
    inner: D,
    log: Vec<WriteRecord>,
    epoch: u64,
}

impl<D: BlockDevice> LoggingBlockDevice<D> {
    /// Wrap a device with write logging.
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            log: Vec::new(),
            epoch: 0,
        }
    }

    /// All recorded writes.
    pub fn log(&self) -> &[WriteRecord] {
        &self.log
    }

    /// Current flush epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Number of recorded writes.
    pub fn write_count(&self) -> usize {
        self.log.len()
    }

    /// Consume this wrapper, returning the inner device.
    pub fn into_inner(self) -> D {
        self.inner
    }

    /// Replay writes onto `target`, applying all writes with index `< end`.
    /// Simulates a crash where exactly those writes completed.
    ///
    /// Writes are applied in log order (optimistic: epoch-internal writes
    /// land in issue order). For epoch-internal reordering tests, the log
    /// and epoch fields are public — build custom replay strategies on top.
    pub fn replay_prefix(&self, target: &mut impl BlockDevice, end: usize) -> Result<(), FsError> {
        let end = end.min(self.log.len());
        for record in &self.log[..end] {
            target.write_block(record.block, &record.data)?;
        }
        Ok(())
    }

    /// Replay only writes whose flush epoch is strictly less than `epoch`.
    /// These writes are guaranteed durable (they precede a completed flush).
    pub fn replay_durable(&self, target: &mut impl BlockDevice, epoch: u64) -> Result<(), FsError> {
        for record in &self.log {
            if record.epoch >= epoch {
                break;
            }
            target.write_block(record.block, &record.data)?;
        }
        Ok(())
    }
}

impl<D: BlockDevice> BlockDevice for LoggingBlockDevice<D> {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        self.inner.read_block(index, buf)
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        self.log.push(WriteRecord {
            block: index,
            data: data.to_vec(),
            epoch: self.epoch,
        });
        self.inner.write_block(index, data)
    }

    fn flush(&mut self) -> Result<(), FsError> {
        self.epoch += 1;
        self.inner.flush()
    }

    fn block_count(&self) -> u32 {
        self.inner.block_count()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn block(byte: u8) -> Vec<u8> {
        vec![byte; BLOCK_SIZE as usize]
    }

    fn read(dev: &impl BlockDevice, index: u32) -> Vec<u8> {
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        dev.read_block(index, &mut buf).unwrap();
        buf
    }

    fn tempdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "fs-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── MemoryBlockDevice ──────────────────────────────────────────

    #[test]
    fn memory_zeroed_on_creation() {
        let dev = MemoryBlockDevice::new(4);
        assert_eq!(read(&dev, 0), block(0));
        assert_eq!(read(&dev, 3), block(0));
    }

    #[test]
    fn memory_write_read_roundtrip() {
        let mut dev = MemoryBlockDevice::new(4);
        dev.write_block(2, &block(0xAB)).unwrap();
        assert_eq!(read(&dev, 2), block(0xAB));
    }

    #[test]
    fn memory_write_isolates_blocks() {
        let mut dev = MemoryBlockDevice::new(4);
        dev.write_block(1, &block(0xFF)).unwrap();
        assert_eq!(read(&dev, 0), block(0));
        assert_eq!(read(&dev, 2), block(0));
    }

    #[test]
    fn memory_overwrite() {
        let mut dev = MemoryBlockDevice::new(4);
        dev.write_block(0, &block(0x11)).unwrap();
        dev.write_block(0, &block(0x22)).unwrap();
        assert_eq!(read(&dev, 0), block(0x22));
    }

    #[test]
    fn memory_block_count() {
        assert_eq!(MemoryBlockDevice::new(8).block_count(), 8);
        assert_eq!(MemoryBlockDevice::new(0).block_count(), 0);
    }

    #[test]
    fn memory_out_of_bounds_read() {
        let dev = MemoryBlockDevice::new(4);
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        assert!(matches!(
            dev.read_block(4, &mut buf),
            Err(FsError::OutOfBounds { block: 4, count: 4 })
        ));
    }

    #[test]
    fn memory_out_of_bounds_write() {
        let mut dev = MemoryBlockDevice::new(4);
        assert!(matches!(
            dev.write_block(4, &block(0)),
            Err(FsError::OutOfBounds { block: 4, count: 4 })
        ));
    }

    #[test]
    fn memory_bad_buffer_read() {
        let dev = MemoryBlockDevice::new(4);
        let mut buf = vec![0u8; 100];
        assert!(matches!(
            dev.read_block(0, &mut buf),
            Err(FsError::BadBufferSize { .. })
        ));
    }

    #[test]
    fn memory_bad_buffer_write() {
        let mut dev = MemoryBlockDevice::new(4);
        assert!(matches!(
            dev.write_block(0, &[0u8; 100]),
            Err(FsError::BadBufferSize { .. })
        ));
    }

    // ── FileBlockDevice ────────────────────────────────────────────

    #[test]
    fn file_create_and_block_count() {
        let dir = tempdir();
        let dev = FileBlockDevice::create(dir.join("test.img"), 8).unwrap();
        assert_eq!(dev.block_count(), 8);
    }

    #[test]
    fn file_write_read_roundtrip() {
        let dir = tempdir();
        let mut dev = FileBlockDevice::create(dir.join("test.img"), 4).unwrap();
        dev.write_block(1, &block(0xCD)).unwrap();
        assert_eq!(read(&dev, 1), block(0xCD));
    }

    #[test]
    fn file_zeroed_on_creation() {
        let dir = tempdir();
        let dev = FileBlockDevice::create(dir.join("test.img"), 4).unwrap();
        assert_eq!(read(&dev, 0), block(0));
        assert_eq!(read(&dev, 3), block(0));
    }

    #[test]
    fn file_persists_across_open() {
        let dir = tempdir();
        let path = dir.join("test.img");
        {
            let mut dev = FileBlockDevice::create(&path, 8).unwrap();
            dev.write_block(3, &block(0xEF)).unwrap();
            dev.flush().unwrap();
        }
        let dev = FileBlockDevice::open(&path).unwrap();
        assert_eq!(dev.block_count(), 8);
        assert_eq!(read(&dev, 3), block(0xEF));
    }

    #[test]
    fn file_create_existing_fails() {
        let dir = tempdir();
        let path = dir.join("test.img");
        FileBlockDevice::create(&path, 4).unwrap();
        assert!(FileBlockDevice::create(&path, 4).is_err());
    }

    #[test]
    fn file_flush_succeeds() {
        let dir = tempdir();
        let mut dev = FileBlockDevice::create(dir.join("test.img"), 4).unwrap();
        dev.write_block(0, &block(0x42)).unwrap();
        dev.flush().unwrap();
    }

    #[test]
    fn file_out_of_bounds() {
        let dir = tempdir();
        let mut dev = FileBlockDevice::create(dir.join("test.img"), 4).unwrap();
        assert!(dev.write_block(4, &block(0)).is_err());
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        assert!(dev.read_block(4, &mut buf).is_err());
    }

    // ── LoggingBlockDevice ─────────────────────────────────────────

    #[test]
    fn logging_passthrough_read_write() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0xAA)).unwrap();
        assert_eq!(read(&dev, 0), block(0xAA));
    }

    #[test]
    fn logging_records_writes() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();
        dev.write_block(2, &block(0x22)).unwrap();
        assert_eq!(dev.write_count(), 2);
        assert_eq!(dev.log()[0].block, 0);
        assert_eq!(dev.log()[1].block, 2);
    }

    #[test]
    fn logging_does_not_record_reads() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();
        let _ = read(&dev, 0);
        let _ = read(&dev, 0);
        assert_eq!(dev.write_count(), 1);
    }

    #[test]
    fn logging_epoch_starts_at_zero() {
        let dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        assert_eq!(dev.epoch(), 0);
    }

    #[test]
    fn logging_epoch_increments_on_flush() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();
        assert_eq!(dev.log()[0].epoch, 0);

        dev.flush().unwrap();
        assert_eq!(dev.epoch(), 1);

        dev.write_block(1, &block(0x22)).unwrap();
        assert_eq!(dev.log()[1].epoch, 1);

        dev.flush().unwrap();
        assert_eq!(dev.epoch(), 2);
    }

    #[test]
    fn logging_writes_within_epoch_share_epoch() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();
        dev.write_block(1, &block(0x22)).unwrap();
        dev.write_block(2, &block(0x33)).unwrap();
        assert!(dev.log().iter().all(|r| r.epoch == 0));
    }

    #[test]
    fn logging_replay_prefix() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();
        dev.write_block(1, &block(0x22)).unwrap();
        dev.write_block(2, &block(0x33)).unwrap();

        // Replay first 2 writes (indices 0, 1) onto a fresh device.
        let mut target = MemoryBlockDevice::new(4);
        dev.replay_prefix(&mut target, 2).unwrap();

        assert_eq!(read(&target, 0), block(0x11));
        assert_eq!(read(&target, 1), block(0x22));
        assert_eq!(read(&target, 2), block(0)); // not replayed
    }

    #[test]
    fn logging_replay_prefix_empty_log() {
        let dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        let mut target = MemoryBlockDevice::new(4);
        dev.replay_prefix(&mut target, 0).unwrap(); // no-op
        assert_eq!(read(&target, 0), block(0));
    }

    #[test]
    fn logging_replay_prefix_clamps() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();

        let mut target = MemoryBlockDevice::new(4);
        dev.replay_prefix(&mut target, 999).unwrap(); // clamps to 1 write
        assert_eq!(read(&target, 0), block(0x11));
    }

    #[test]
    fn logging_replay_durable() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));

        // Epoch 0: two writes.
        dev.write_block(0, &block(0x11)).unwrap();
        dev.write_block(1, &block(0x22)).unwrap();
        dev.flush().unwrap(); // epoch becomes 1

        // Epoch 1: one write.
        dev.write_block(2, &block(0x33)).unwrap();
        dev.flush().unwrap(); // epoch becomes 2

        // Epoch 2: one unflushed write.
        dev.write_block(3, &block(0x44)).unwrap();

        // Replay only writes durable before epoch 2 (epochs 0 and 1).
        let mut target = MemoryBlockDevice::new(4);
        dev.replay_durable(&mut target, 2).unwrap();

        assert_eq!(read(&target, 0), block(0x11)); // epoch 0
        assert_eq!(read(&target, 1), block(0x22)); // epoch 0
        assert_eq!(read(&target, 2), block(0x33)); // epoch 1
        assert_eq!(read(&target, 3), block(0)); // epoch 2 — not durable
    }

    #[test]
    fn logging_replay_durable_nothing() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0x11)).unwrap();
        // No flush — nothing is durable.

        let mut target = MemoryBlockDevice::new(4);
        dev.replay_durable(&mut target, 0).unwrap();
        assert_eq!(read(&target, 0), block(0)); // nothing replayed
    }

    #[test]
    fn logging_block_count_passthrough() {
        let dev = LoggingBlockDevice::new(MemoryBlockDevice::new(16));
        assert_eq!(dev.block_count(), 16);
    }

    #[test]
    fn logging_into_inner() {
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0xBB)).unwrap();
        let inner = dev.into_inner();
        assert_eq!(read(&inner, 0), block(0xBB));
    }

    // ── Cross-device replay (logging → file) ───────────────────────

    #[test]
    fn logging_replay_to_file_device() {
        let dir = tempdir();
        let mut dev = LoggingBlockDevice::new(MemoryBlockDevice::new(4));
        dev.write_block(0, &block(0xAA)).unwrap();
        dev.write_block(1, &block(0xBB)).unwrap();

        let mut file_dev = FileBlockDevice::create(dir.join("replay.img"), 4).unwrap();
        dev.replay_prefix(&mut file_dev, 2).unwrap();

        assert_eq!(read(&file_dev, 0), block(0xAA));
        assert_eq!(read(&file_dev, 1), block(0xBB));
    }
}
