//! Superblock ring and disk header.
//!
//! On-disk layout:
//! - Block 0: disk header (magic, version, geometry, CRC32)
//! - Blocks 1..=RING_SIZE: superblock ring entries
//! - Blocks DATA_START+: data and metadata
//!
//! The superblock ring provides crash-safe state persistence. Each commit
//! writes to the next ring slot. Mount scans all slots and picks the
//! highest valid txg (transaction group number). Torn writes are detected
//! via CRC32 on every entry.

use alloc::vec;

use crate::{block::BlockDevice, crc32::crc32, now_nanos, FsError, BLOCK_SIZE};

/// Number of slots in the superblock ring.
pub const RING_SIZE: u32 = 16;

/// First block available for data and metadata.
pub const DATA_START: u32 = 1 + RING_SIZE; // 17

const DISK_MAGIC: u64 = u64::from_le_bytes(*b"docOScow");
const RING_MAGIC: u64 = u64::from_le_bytes(*b"SUPRblok");
const DISK_VERSION: u32 = 1;

// ── Disk header layout (block 0) ───────────────────────────────────

const HDR_MAGIC: usize = 0; //     u64
const HDR_VERSION: usize = 8; //   u32
const HDR_RING_SIZE: usize = 12; // u32
const HDR_TOTAL: usize = 16; //    u32
const HDR_CKSUM: usize = 20; //    u32, CRC32 of bytes 0..20

// ── Ring entry layout ──────────────────────────────────────────────

const ENT_MAGIC: usize = 0; //     u64
const ENT_TXG: usize = 8; //       u64
const ENT_TS: usize = 16; //       u64, nanos since UNIX epoch
const ENT_FILE_ID: usize = 24; //  u64, next_file_id
const ENT_INODES: usize = 32; //   u32, root_inode_table block
const ENT_FREE: usize = 36; //     u32, root_free_list block
const ENT_SNAPS: usize = 40; //    u32, root_snapshot_index block
const ENT_TOTAL: usize = 44; //    u32, total_blocks
const ENT_USED: usize = 48; //     u32, used_blocks
const ENT_ROOT_FILE: usize = 52; // u64, root file id (0 = none)
const ENT_CKSUM: usize = 60; //    u32, CRC32 of bytes 0..60

/// Live filesystem state, loaded from the superblock ring on mount.
///
/// Upper layers modify the `pub` fields (root pointers, used_blocks,
/// next_file_id), then call `commit()` to persist. `txg` and `timestamp`
/// are managed by `commit()`.
#[derive(Debug)]
pub struct Superblock {
    /// Transaction group number. Incremented on each commit.
    pub txg: u64,
    /// Nanoseconds since UNIX epoch of last commit.
    pub timestamp: u64,
    /// Next file ID to allocate (monotonic counter).
    pub next_file_id: u64,
    /// Block of the inode table root (0 = none).
    pub root_inode_table: u32,
    /// Block of the persisted free-extent list (0 = none).
    pub root_free_list: u32,
    /// Block of the global snapshot index (0 = none).
    pub root_snapshot_index: u32,
    /// Total blocks on the device.
    pub total_blocks: u32,
    /// Blocks currently in use.
    pub used_blocks: u32,
    /// Root file ID (0 = none). Persists across commits.
    pub root_file: Option<u64>,
}

impl Superblock {
    /// Format a new filesystem on `device`.
    ///
    /// Writes the disk header and an initial superblock entry (txg=1).
    /// All existing data on the device is lost. Device must have at
    /// least `DATA_START` (17) blocks.
    pub fn format(device: &mut impl BlockDevice) -> Result<Self, FsError> {
        let total = device.block_count();
        if total < DATA_START {
            return Err(FsError::DeviceTooSmall {
                blocks: total,
                minimum: DATA_START,
            });
        }

        // Zero header + all ring slots to invalidate any stale entries.
        let zero = vec![0u8; BLOCK_SIZE as usize];
        for i in 0..=RING_SIZE {
            device.write_block(i, &zero)?;
        }

        // Disk header (block 0).
        let mut hdr = vec![0u8; BLOCK_SIZE as usize];
        put_u64(&mut hdr, HDR_MAGIC, DISK_MAGIC);
        put_u32(&mut hdr, HDR_VERSION, DISK_VERSION);
        put_u32(&mut hdr, HDR_RING_SIZE, RING_SIZE);
        put_u32(&mut hdr, HDR_TOTAL, total);
        let cksum = crc32(&hdr[..HDR_CKSUM]);
        put_u32(&mut hdr, HDR_CKSUM, cksum);
        device.write_block(0, &hdr)?;

        // Initial superblock entry.
        let sb = Self {
            txg: 1,
            timestamp: now_nanos(),
            next_file_id: 1,
            root_inode_table: 0,
            root_free_list: 0,
            root_snapshot_index: 0,
            total_blocks: total,
            used_blocks: DATA_START,
            root_file: None,
        };
        device.write_block(ring_block(sb.txg), &sb.encode())?;
        device.flush()?;

        Ok(sb)
    }

    /// Mount an existing filesystem from `device`.
    ///
    /// Validates the disk header, then scans the superblock ring for the
    /// highest-txg entry with valid magic and CRC32.
    pub fn mount(device: &impl BlockDevice) -> Result<Self, FsError> {
        let mut buf = vec![0u8; BLOCK_SIZE as usize];

        // Validate disk header.
        device.read_block(0, &mut buf)?;
        if get_u64(&buf, HDR_MAGIC) != DISK_MAGIC {
            return Err(FsError::BadMagic);
        }
        let stored = get_u32(&buf, HDR_CKSUM);
        let computed = crc32(&buf[..HDR_CKSUM]);
        if stored != computed {
            return Err(FsError::ChecksumMismatch {
                expected: stored,
                actual: computed,
            });
        }

        // Scan ring for highest valid txg.
        let mut best: Option<Self> = None;
        for slot in 0..RING_SIZE {
            device.read_block(slot + 1, &mut buf)?;

            if get_u64(&buf, ENT_MAGIC) != RING_MAGIC {
                continue;
            }
            if get_u32(&buf, ENT_CKSUM) != crc32(&buf[..ENT_CKSUM]) {
                continue;
            }

            let txg = get_u64(&buf, ENT_TXG);
            if best.as_ref().map_or(true, |b| txg > b.txg) {
                best = Some(Self::decode(&buf));
            }
        }

        best.ok_or(FsError::NoValidSuperblock)
    }

    /// Commit current state to the next ring slot.
    ///
    /// Increments `txg`, sets `timestamp`, writes the entry, and flushes.
    /// The caller must write and flush all data/metadata blocks BEFORE
    /// calling this (first flush in the two-flush protocol). This method
    /// performs the second flush.
    pub fn commit(&mut self, device: &mut impl BlockDevice) -> Result<(), FsError> {
        self.txg += 1;
        self.timestamp = now_nanos();
        device.write_block(ring_block(self.txg), &self.encode())?;
        device.flush()
    }

    fn encode(&self) -> alloc::vec::Vec<u8> {
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        put_u64(&mut buf, ENT_MAGIC, RING_MAGIC);
        put_u64(&mut buf, ENT_TXG, self.txg);
        put_u64(&mut buf, ENT_TS, self.timestamp);
        put_u64(&mut buf, ENT_FILE_ID, self.next_file_id);
        put_u32(&mut buf, ENT_INODES, self.root_inode_table);
        put_u32(&mut buf, ENT_FREE, self.root_free_list);
        put_u32(&mut buf, ENT_SNAPS, self.root_snapshot_index);
        put_u32(&mut buf, ENT_TOTAL, self.total_blocks);
        put_u32(&mut buf, ENT_USED, self.used_blocks);
        put_u64(&mut buf, ENT_ROOT_FILE, self.root_file.unwrap_or(0));
        let cksum = crc32(&buf[..ENT_CKSUM]);
        put_u32(&mut buf, ENT_CKSUM, cksum);
        buf
    }

    fn decode(buf: &[u8]) -> Self {
        let raw_root = get_u64(buf, ENT_ROOT_FILE);
        Self {
            txg: get_u64(buf, ENT_TXG),
            timestamp: get_u64(buf, ENT_TS),
            next_file_id: get_u64(buf, ENT_FILE_ID),
            root_inode_table: get_u32(buf, ENT_INODES),
            root_free_list: get_u32(buf, ENT_FREE),
            root_snapshot_index: get_u32(buf, ENT_SNAPS),
            total_blocks: get_u32(buf, ENT_TOTAL),
            used_blocks: get_u32(buf, ENT_USED),
            root_file: if raw_root == 0 { None } else { Some(raw_root) },
        }
    }
}

/// Ring slot block number for a given txg.
/// txg=1 → block 1, txg=16 → block 16, txg=17 → block 1 (wrap).
fn ring_block(txg: u64) -> u32 {
    ((txg - 1) % RING_SIZE as u64) as u32 + 1
}

fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn get_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
