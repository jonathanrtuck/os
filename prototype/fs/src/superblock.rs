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

use crate::block::BlockDevice;
use crate::crc32::crc32;
use crate::{FsError, BLOCK_SIZE};

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
const ENT_CKSUM: usize = 52; //    u32, CRC32 of bytes 0..52

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

    fn encode(&self) -> Vec<u8> {
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
        let cksum = crc32(&buf[..ENT_CKSUM]);
        put_u32(&mut buf, ENT_CKSUM, cksum);
        buf
    }

    fn decode(buf: &[u8]) -> Self {
        Self {
            txg: get_u64(buf, ENT_TXG),
            timestamp: get_u64(buf, ENT_TS),
            next_file_id: get_u64(buf, ENT_FILE_ID),
            root_inode_table: get_u32(buf, ENT_INODES),
            root_free_list: get_u32(buf, ENT_FREE),
            root_snapshot_index: get_u32(buf, ENT_SNAPS),
            total_blocks: get_u32(buf, ENT_TOTAL),
            used_blocks: get_u32(buf, ENT_USED),
        }
    }
}

/// Ring slot block number for a given txg.
/// txg=1 → block 1, txg=16 → block 16, txg=17 → block 1 (wrap).
fn ring_block(txg: u64) -> u32 {
    ((txg - 1) % RING_SIZE as u64) as u32 + 1
}

use crate::now_nanos;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    fn dev(blocks: u32) -> MemoryBlockDevice {
        MemoryBlockDevice::new(blocks)
    }

    // ── format + mount ─────────────────────────────────────────────

    #[test]
    fn format_and_mount_roundtrip() {
        let mut d = dev(64);
        let sb = Superblock::format(&mut d).unwrap();
        assert_eq!(sb.txg, 1);
        assert_eq!(sb.total_blocks, 64);
        assert_eq!(sb.used_blocks, DATA_START);
        assert_eq!(sb.next_file_id, 1);
        assert_eq!(sb.root_inode_table, 0);
        assert_eq!(sb.root_free_list, 0);
        assert_eq!(sb.root_snapshot_index, 0);

        let m = Superblock::mount(&d).unwrap();
        assert_eq!(m.txg, 1);
        assert_eq!(m.total_blocks, 64);
        assert_eq!(m.used_blocks, DATA_START);
        assert_eq!(m.next_file_id, 1);
    }

    #[test]
    fn format_minimum_device() {
        let mut d = dev(DATA_START);
        let sb = Superblock::format(&mut d).unwrap();
        assert_eq!(sb.total_blocks, DATA_START);
    }

    #[test]
    fn format_too_small() {
        let mut d = dev(10);
        assert!(matches!(
            Superblock::format(&mut d),
            Err(FsError::DeviceTooSmall {
                blocks: 10,
                minimum: 17
            })
        ));
    }

    #[test]
    fn format_timestamp_nonzero() {
        let mut d = dev(64);
        let sb = Superblock::format(&mut d).unwrap();
        assert!(sb.timestamp > 0);
    }

    // ── commit ─────────────────────────────────────────────────────

    #[test]
    fn commit_increments_txg() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();
        assert_eq!(sb.txg, 1);
        sb.commit(&mut d).unwrap();
        assert_eq!(sb.txg, 2);
        sb.commit(&mut d).unwrap();
        assert_eq!(sb.txg, 3);
    }

    #[test]
    fn commit_persists_fields() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();
        sb.root_inode_table = 20;
        sb.root_free_list = 21;
        sb.root_snapshot_index = 22;
        sb.used_blocks = 30;
        sb.next_file_id = 100;
        sb.commit(&mut d).unwrap();

        let m = Superblock::mount(&d).unwrap();
        assert_eq!(m.txg, 2);
        assert_eq!(m.root_inode_table, 20);
        assert_eq!(m.root_free_list, 21);
        assert_eq!(m.root_snapshot_index, 22);
        assert_eq!(m.used_blocks, 30);
        assert_eq!(m.next_file_id, 100);
    }

    #[test]
    fn commit_advances_timestamp() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();
        let t1 = sb.timestamp;
        sb.commit(&mut d).unwrap();
        assert!(sb.timestamp >= t1);
    }

    // ── mount finds latest ─────────────────────────────────────────

    #[test]
    fn mount_finds_latest() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();
        sb.root_free_list = 42;
        sb.commit(&mut d).unwrap(); // txg=2
        sb.root_free_list = 99;
        sb.commit(&mut d).unwrap(); // txg=3

        let m = Superblock::mount(&d).unwrap();
        assert_eq!(m.txg, 3);
        assert_eq!(m.root_free_list, 99);
    }

    // ── ring wrap ──────────────────────────────────────────────────

    #[test]
    fn ring_wraps_around() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();

        for i in 0..RING_SIZE {
            sb.used_blocks = DATA_START + i + 1;
            sb.commit(&mut d).unwrap();
        }
        // format wrote txg=1, then 16 commits → txg=17
        assert_eq!(sb.txg, 1 + RING_SIZE as u64);

        let m = Superblock::mount(&d).unwrap();
        assert_eq!(m.txg, 1 + RING_SIZE as u64);
        assert_eq!(m.used_blocks, DATA_START + RING_SIZE);
    }

    #[test]
    fn many_commits() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();
        for i in 0..100u64 {
            sb.next_file_id = i + 10;
            sb.commit(&mut d).unwrap();
        }
        assert_eq!(sb.txg, 101);

        let m = Superblock::mount(&d).unwrap();
        assert_eq!(m.txg, 101);
        assert_eq!(m.next_file_id, 109); // last: 99 + 10
    }

    // ── corruption handling ────────────────────────────────────────

    #[test]
    fn mount_survives_corrupted_entry() {
        let mut d = dev(64);
        let mut sb = Superblock::format(&mut d).unwrap();
        sb.commit(&mut d).unwrap(); // txg=2
        sb.commit(&mut d).unwrap(); // txg=3, ring slot 2 → block 3

        // Corrupt the latest entry.
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        d.read_block(3, &mut buf).unwrap();
        buf[0] ^= 0xFF;
        d.write_block(3, &buf).unwrap();

        // Mount falls back to txg=2.
        let m = Superblock::mount(&d).unwrap();
        assert_eq!(m.txg, 2);
    }

    #[test]
    fn mount_fails_unformatted() {
        let d = dev(64);
        assert!(matches!(Superblock::mount(&d), Err(FsError::BadMagic)));
    }

    #[test]
    fn mount_fails_header_checksum_corrupt() {
        let mut d = dev(64);
        Superblock::format(&mut d).unwrap();

        // Corrupt header checksum (flip a byte in the version field).
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        d.read_block(0, &mut buf).unwrap();
        buf[HDR_VERSION] ^= 0xFF;
        d.write_block(0, &buf).unwrap();

        assert!(matches!(
            Superblock::mount(&d),
            Err(FsError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn mount_fails_all_entries_corrupt() {
        let mut d = dev(64);
        Superblock::format(&mut d).unwrap();

        // Corrupt the one valid ring entry (txg=1, slot 0, block 1).
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        d.read_block(1, &mut buf).unwrap();
        buf[ENT_CKSUM] ^= 0xFF;
        d.write_block(1, &buf).unwrap();

        assert!(matches!(
            Superblock::mount(&d),
            Err(FsError::NoValidSuperblock)
        ));
    }

    // ── ring_block mapping ─────────────────────────────────────────

    #[test]
    fn ring_block_mapping() {
        assert_eq!(ring_block(1), 1); //  txg=1  → block 1
        assert_eq!(ring_block(16), 16); // txg=16 → block 16
        assert_eq!(ring_block(17), 1); //  txg=17 → block 1 (wrap)
        assert_eq!(ring_block(32), 16); // txg=32 → block 16
        assert_eq!(ring_block(33), 1); //  txg=33 → block 1
    }
}
