//! Crash consistency tests for the COW filesystem.
//!
//! Validates that the two-flush commit protocol preserves data integrity:
//! (1) write all data blocks + metadata, flush. (2) write superblock, flush.
//! A crash before step 2 means the old superblock is still valid and the
//! previous committed state is recovered on mount.

extern crate alloc;

use alloc::{vec, vec::Vec};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use fs::{BlockDevice, Filesystem, FsError, BLOCK_SIZE};

// ── MemDevice ──────────────────────────────────────────────────────

/// In-memory block device for testing.
struct MemDevice {
    blocks: Vec<Vec<u8>>,
}

impl MemDevice {
    fn new(block_count: u32) -> Self {
        Self {
            blocks: vec![vec![0u8; BLOCK_SIZE as usize]; block_count as usize],
        }
    }
}

impl BlockDevice for MemDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if index as usize >= self.blocks.len() {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks.len() as u32,
            });
        }
        if buf.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: buf.len(),
            });
        }
        buf.copy_from_slice(&self.blocks[index as usize]);
        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        if index as usize >= self.blocks.len() {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks.len() as u32,
            });
        }
        if data.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: data.len(),
            });
        }
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

// ── CrashDevice ────────────────────────────────────────────────────

/// Sentinel value: no crash armed.
const NO_CRASH: usize = usize::MAX;

/// Shared crash trigger. The test holds one `Arc` clone and the
/// `CrashDevice` holds another. The test arms the trigger by storing
/// a write count; the device decrements it on each `write_block`.
/// Once it reaches zero, all subsequent writes and flushes are
/// silently dropped.
struct CrashTrigger {
    writes_remaining: AtomicUsize,
}

impl CrashTrigger {
    fn new() -> Self {
        Self {
            writes_remaining: AtomicUsize::new(NO_CRASH),
        }
    }

    /// Arm: after `n` more block writes, drop all subsequent I/O.
    fn arm(&self, n: usize) {
        self.writes_remaining.store(n, Ordering::SeqCst);
    }

    /// Returns `true` if the write should proceed, `false` if crashed.
    fn try_write(&self) -> bool {
        loop {
            let current = self.writes_remaining.load(Ordering::SeqCst);
            if current == NO_CRASH {
                return true; // not armed
            }
            if current == 0 {
                return false; // crashed — drop the write
            }
            // Decrement.
            if self
                .writes_remaining
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn is_crashed(&self) -> bool {
        self.writes_remaining.load(Ordering::SeqCst) == 0
    }
}

/// A block device that simulates power loss during writes.
///
/// The `CrashTrigger` is shared via `Arc` so the test can arm/disarm
/// it while the `Filesystem` owns the device.
struct CrashDevice {
    inner: MemDevice,
    trigger: Arc<CrashTrigger>,
}

impl CrashDevice {
    fn new(block_count: u32) -> (Self, Arc<CrashTrigger>) {
        let trigger = Arc::new(CrashTrigger::new());
        let dev = Self {
            inner: MemDevice::new(block_count),
            trigger: Arc::clone(&trigger),
        };
        (dev, trigger)
    }

    /// Clone the underlying block data into a fresh `MemDevice`.
    /// Represents the disk state at the moment of simulated power loss.
    fn clone_device(&self) -> MemDevice {
        MemDevice {
            blocks: self.inner.blocks.clone(),
        }
    }
}

impl BlockDevice for CrashDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        self.inner.read_block(index, buf)
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        if !self.trigger.try_write() {
            return Ok(()); // silently drop — power is gone
        }
        self.inner.write_block(index, data)
    }

    fn flush(&mut self) -> Result<(), FsError> {
        if self.trigger.is_crashed() {
            return Ok(()); // no-op after crash
        }
        self.inner.flush()
    }

    fn block_count(&self) -> u32 {
        self.inner.block_count()
    }
}

// ── Tests ──────────────────────────────────────────────────────────

/// Helper: read the full contents of a file into a Vec.
fn read_all(fs: &Filesystem<MemDevice>, file_id: u64, max_size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; max_size];
    let n = fs.read(file_id, 0, &mut buf).expect("read failed");
    buf.truncate(n);
    buf
}

#[test]
fn crash_before_first_flush_loses_uncommitted() {
    // Establish committed state: file contains "hello".
    let (dev, _trigger) = CrashDevice::new(256);
    let mut fs = Filesystem::format(dev).expect("format");
    let fid = fs.create_file().expect("create");
    fs.write(fid, 0, b"hello").expect("write hello");
    fs.commit().expect("commit hello");

    // Write "world" but do NOT commit — simulates crash before any
    // flush of the new transaction.
    fs.write(fid, 0, b"world").expect("write world");

    // Power lost. Recover: extract the device, clone its blocks, mount.
    let crash_dev = fs.into_device();
    let recovered = crash_dev.clone_device();
    let fs2 = Filesystem::mount(recovered).expect("mount after crash");

    // The uncommitted "world" write must be lost. File has "hello".
    let data = read_all(&fs2, fid, 16);
    assert_eq!(&data, b"hello", "uncommitted write must be lost");
}

#[test]
fn crash_during_commit_recovers_old_state() {
    // Establish committed state: file contains "before".
    let (dev, trigger) = CrashDevice::new(256);
    let mut fs = Filesystem::format(dev).expect("format");
    let fid = fs.create_file().expect("create");
    fs.write(fid, 0, b"before").expect("write before");
    fs.commit().expect("commit before");

    // Write new data and begin commit, but crash mid-way: allow only 1
    // block write during the commit sequence, then silently drop the
    // rest. The superblock (written last in the two-flush protocol)
    // will not reach disk, so the old committed state must survive.
    fs.write(fid, 0, b"after!").expect("write after");
    trigger.arm(1);
    let _commit_result = fs.commit(); // may succeed or error — irrelevant

    // Recover from whatever made it to disk.
    let crash_dev = fs.into_device();
    let recovered = crash_dev.clone_device();
    let fs2 = Filesystem::mount(recovered).expect("mount after mid-commit crash");

    // Old committed state must be intact.
    let data = read_all(&fs2, fid, 16);
    assert_eq!(
        &data, b"before",
        "old committed state must survive a crash during commit"
    );
}

#[test]
fn successful_commit_survives_remount() {
    // Format, create a file, write data, commit — the happy path.
    let dev = MemDevice::new(256);
    let mut fs = Filesystem::format(dev).expect("format");
    let fid = fs.create_file().expect("create");

    let payload = b"the quick brown fox jumps over the lazy dog";
    fs.write(fid, 0, payload).expect("write");
    fs.commit().expect("commit");

    // Remount from the same device.
    let dev = fs.into_device();
    let fs2 = Filesystem::mount(dev).expect("mount");

    // All data must be intact.
    let data = read_all(&fs2, fid, payload.len());
    assert_eq!(
        data.as_slice(),
        payload,
        "committed data must survive remount"
    );

    // File size must match.
    let size = fs2.file_size(fid).expect("file_size");
    assert_eq!(size, payload.len() as u64, "file size must match");
}
