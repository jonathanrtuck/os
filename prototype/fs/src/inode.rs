//! Inode — one 16 KiB block per file.
//!
//! Layout:
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │  HEADER             (bytes 0..64)            │
//! ├──────────────────────────────────────────────┤
//! │  EXTENT LIST        (bytes 64..256)          │
//! │  Up to 16 entries × 12 bytes                 │
//! ├──────────────────────────────────────────────┤
//! │  INLINE DATA        (bytes 256..16384)       │
//! │  Up to 16128 bytes of file content           │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! Small files (≤ 16128 bytes) store content directly in the inode block.
//! Larger files use the extent list to reference external data blocks.
//! The transition from inline to extent-based is handled by A5 (COW writes).

use crate::alloc::Allocator;
use crate::block::BlockDevice;
use crate::{FsError, BLOCK_SIZE};

// ── Layout constants ───────────────────────────────────────────────

const HEADER_SIZE: usize = 64;
/// Maximum extents stored directly in the inode.
pub const MAX_EXTENTS: usize = 16;
const EXTENT_SIZE: usize = 12;
const EXTENT_REGION: usize = MAX_EXTENTS * EXTENT_SIZE; // 192
const INLINE_OFFSET: usize = HEADER_SIZE + EXTENT_REGION; // 256

/// Maximum bytes storable inline in the inode block.
pub const INLINE_CAPACITY: usize = BLOCK_SIZE as usize - INLINE_OFFSET; // 16128

const FLAG_INLINE: u32 = 1;

// Header field offsets
const H_FILE_ID: usize = 0; //       u64
const H_SIZE: usize = 8; //          u64
const H_CREATED: usize = 16; //      u64 (nanos since epoch)
const H_MODIFIED: usize = 24; //     u64 (nanos since epoch)
const H_FLAGS: usize = 32; //        u32
const H_EXTENT_CT: usize = 36; //    u16
#[allow(dead_code)] // reserved for A6
const H_SNAP_CT: usize = 38;
#[allow(dead_code)]
const H_INDIRECT: usize = 40;
#[allow(dead_code)] // reserved for A6
const H_SNAP_BLK: usize = 44;
                                      // 48..64: reserved (zeros)

// ── Types ──────────────────────────────────────────────────────────

/// A contiguous run of data blocks belonging to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeExtent {
    /// First block of the run.
    pub start_block: u32,
    /// Number of blocks in the run (max 65535 = 1 GiB at 16 KiB blocks).
    pub count: u16,
    /// Transaction group when this extent was written. Used by snapshot
    /// deletion (Model D birth-time logic) to determine which snapshots
    /// reference these blocks.
    pub birth_txg: u64, // lower 48 bits persisted
}

/// A file's metadata, extent list, and inline data. One 16 KiB block.
///
/// Created with `Inode::create`, loaded with `Inode::load`. The inode
/// tracks its own block number for save-back. Inline data is held in
/// memory; extent-based data lives on separate blocks (read via device).
pub struct Inode {
    /// Globally unique file identifier.
    pub file_id: u64,
    /// File size in bytes.
    pub size: u64,
    /// Creation time (nanos since UNIX epoch).
    pub created: u64,
    /// Last modification time (nanos since UNIX epoch).
    pub modified: u64,
    flags: u32,
    extents: Vec<InodeExtent>,
    block: u32,
    inline_data: Vec<u8>,
}

impl Inode {
    /// Create a new empty inline file. Allocates one inode block.
    pub fn create(
        device: &mut impl BlockDevice,
        allocator: &mut Allocator,
        file_id: u64,
        now: u64,
    ) -> Result<Self, FsError> {
        let block = allocator.alloc(1).ok_or(FsError::NoSpace)?;
        let inode = Self {
            file_id,
            size: 0,
            created: now,
            modified: now,
            flags: FLAG_INLINE,
            extents: Vec::new(),
            block,
            inline_data: Vec::new(),
        };
        inode.save(device)?;
        Ok(inode)
    }

    /// Load an inode from `block`.
    pub fn load(device: &impl BlockDevice, block: u32) -> Result<Self, FsError> {
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        device.read_block(block, &mut buf)?;

        let file_id = get_u64(&buf, H_FILE_ID);
        let size = get_u64(&buf, H_SIZE);
        let flags = get_u32(&buf, H_FLAGS);
        let extent_count = get_u16(&buf, H_EXTENT_CT) as usize;

        if extent_count > MAX_EXTENTS {
            return Err(FsError::Corrupt(format!(
                "inode {file_id}: {extent_count} extents, max {MAX_EXTENTS}"
            )));
        }

        let is_inline = flags & FLAG_INLINE != 0;
        if is_inline && size > INLINE_CAPACITY as u64 {
            return Err(FsError::Corrupt(format!(
                "inode {file_id}: inline but size {size} > capacity {INLINE_CAPACITY}"
            )));
        }

        let mut extents = Vec::with_capacity(extent_count);
        for i in 0..extent_count {
            let off = HEADER_SIZE + i * EXTENT_SIZE;
            extents.push(InodeExtent {
                start_block: get_u32(&buf, off),
                count: get_u16(&buf, off + 4),
                birth_txg: get_u48(&buf, off + 6),
            });
        }

        let inline_data = if is_inline && size > 0 {
            buf[INLINE_OFFSET..INLINE_OFFSET + size as usize].to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            file_id,
            size,
            created: get_u64(&buf, H_CREATED),
            modified: get_u64(&buf, H_MODIFIED),
            flags,
            extents,
            block,
            inline_data,
        })
    }

    /// Write the inode back to its block.
    pub fn save(&self, device: &mut impl BlockDevice) -> Result<(), FsError> {
        let mut buf = vec![0u8; BLOCK_SIZE as usize];

        put_u64(&mut buf, H_FILE_ID, self.file_id);
        put_u64(&mut buf, H_SIZE, self.size);
        put_u64(&mut buf, H_CREATED, self.created);
        put_u64(&mut buf, H_MODIFIED, self.modified);
        put_u32(&mut buf, H_FLAGS, self.flags);
        put_u16(&mut buf, H_EXTENT_CT, self.extents.len() as u16);
        // H_SNAP_CT, H_INDIRECT, H_SNAP_BLK: remain 0 (reserved for A6)

        for (i, ext) in self.extents.iter().enumerate() {
            let off = HEADER_SIZE + i * EXTENT_SIZE;
            put_u32(&mut buf, off, ext.start_block);
            put_u16(&mut buf, off + 4, ext.count);
            put_u48(&mut buf, off + 6, ext.birth_txg);
        }

        if self.is_inline() {
            buf[INLINE_OFFSET..INLINE_OFFSET + self.inline_data.len()]
                .copy_from_slice(&self.inline_data);
        }

        device.write_block(self.block, &buf)
    }

    /// Read up to `buf.len()` bytes from file content starting at `offset`.
    /// Returns the number of bytes actually read. For inline files, reads
    /// from the in-memory buffer. For extent-based files, returns 0 (A5
    /// adds device-backed reads).
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> usize {
        if !self.is_inline() || buf.is_empty() {
            return 0;
        }
        let offset = offset as usize;
        if offset >= self.inline_data.len() {
            return 0;
        }
        let available = self.inline_data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.inline_data[offset..offset + n]);
        n
    }

    /// Read the entire inline content. Convenience for small files.
    pub fn read_all(&self) -> Vec<u8> {
        if self.is_inline() {
            self.inline_data.clone()
        } else {
            Vec::new()
        }
    }

    /// Write `data` at `offset` into inline storage.
    /// Fails with `NoSpace` if the result would exceed `INLINE_CAPACITY`.
    /// Does NOT update `modified` — caller sets timestamps.
    pub fn write_inline(&mut self, offset: u64, data: &[u8]) -> Result<(), FsError> {
        debug_assert!(self.is_inline(), "write_inline called on extent-based inode");
        if data.is_empty() {
            return Ok(());
        }
        let offset = offset as usize;
        let end = offset + data.len();
        if end > INLINE_CAPACITY {
            return Err(FsError::NoSpace);
        }
        if end > self.inline_data.len() {
            self.inline_data.resize(end, 0);
        }
        self.inline_data[offset..end].copy_from_slice(data);
        self.size = self.inline_data.len() as u64;
        Ok(())
    }

    /// Resize the file. For inline files, truncates or zero-extends the
    /// in-memory buffer. For extent-based files, only updates the size
    /// field (block-level truncation is handled by A5).
    pub fn truncate(&mut self, new_size: u64) -> Result<(), FsError> {
        if self.is_inline() {
            if new_size > INLINE_CAPACITY as u64 {
                return Err(FsError::NoSpace);
            }
            self.inline_data.resize(new_size as usize, 0);
        }
        self.size = new_size;
        Ok(())
    }

    /// Whether the file stores data inline (vs extent-based).
    pub fn is_inline(&self) -> bool {
        self.flags & FLAG_INLINE != 0
    }

    /// The block number where this inode is stored.
    pub fn inode_block(&self) -> u32 {
        self.block
    }

    /// The file's extent list (empty for inline files).
    pub fn extents(&self) -> &[InodeExtent] {
        &self.extents
    }

    /// Add an extent. Used by A5 when writing extent-based data.
    pub fn add_extent(&mut self, ext: InodeExtent) -> Result<(), FsError> {
        if self.extents.len() >= MAX_EXTENTS {
            return Err(FsError::NoSpace);
        }
        self.extents.push(ext);
        Ok(())
    }

    /// Clear all extents. Used by A5/A6 during restore or rewrite.
    pub fn clear_extents(&mut self) {
        self.extents.clear();
    }

    /// Transition from inline to extent-based storage.
    /// Clears the inline flag and data. The caller is responsible for
    /// writing the inline content to data blocks first.
    pub fn transition_to_extents(&mut self) -> Vec<u8> {
        self.flags &= !FLAG_INLINE;
        std::mem::take(&mut self.inline_data)
    }

    /// Write the inode to a newly allocated block (COW). Returns the old
    /// block number for deferred freeing.
    pub fn save_cow(
        &mut self,
        device: &mut impl BlockDevice,
        allocator: &mut Allocator,
    ) -> Result<u32, FsError> {
        let new_block = allocator.alloc(1).ok_or(FsError::NoSpace)?;
        let old_block = self.block;
        self.block = new_block;
        self.save(device)?;
        Ok(old_block)
    }

    /// Transition back to inline (empty). Used when truncating to zero.
    pub fn transition_to_inline(&mut self) {
        self.flags |= FLAG_INLINE;
        self.inline_data.clear();
        self.extents.clear();
        self.size = 0;
    }

    /// Delete this inode, freeing its block and all data extent blocks.
    /// Consumes the inode — it cannot be used after deletion.
    pub fn delete(self, allocator: &mut Allocator) {
        for ext in &self.extents {
            allocator.free(ext.start_block, ext.count as u32);
        }
        allocator.free(self.block, 1);
    }
}

// ── Encoding helpers ───────────────────────────────────────────────

fn get_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn get_u48(buf: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes[..6].copy_from_slice(&buf[off..off + 6]);
    u64::from_le_bytes(bytes)
}

fn get_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u48(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 6].copy_from_slice(&v.to_le_bytes()[..6]);
}

fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::superblock::DATA_START;
    use crate::MemoryBlockDevice;

    const TOTAL: u32 = 256;

    fn setup() -> (MemoryBlockDevice, Allocator) {
        (MemoryBlockDevice::new(TOTAL), Allocator::new(TOTAL))
    }

    // ── create + load roundtrip ────────────────────────────────────

    #[test]
    fn create_empty_file() {
        let (mut dev, mut alloc) = setup();
        let inode = Inode::create(&mut dev, &mut alloc, 1, 1000).unwrap();

        assert_eq!(inode.file_id, 1);
        assert_eq!(inode.size, 0);
        assert_eq!(inode.created, 1000);
        assert_eq!(inode.modified, 1000);
        assert!(inode.is_inline());
        assert!(inode.extents().is_empty());
        assert!(inode.inode_block() >= DATA_START);
    }

    #[test]
    fn create_and_load_roundtrip() {
        let (mut dev, mut alloc) = setup();
        let inode = Inode::create(&mut dev, &mut alloc, 42, 5000).unwrap();
        let block = inode.inode_block();

        let loaded = Inode::load(&dev, block).unwrap();
        assert_eq!(loaded.file_id, 42);
        assert_eq!(loaded.size, 0);
        assert_eq!(loaded.created, 5000);
        assert_eq!(loaded.modified, 5000);
        assert!(loaded.is_inline());
    }

    // ── inline write + read ────────────────────────────────────────

    #[test]
    fn write_and_read_inline() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        inode.write_inline(0, b"hello world").unwrap();
        assert_eq!(inode.size, 11);

        let mut buf = vec![0u8; 11];
        assert_eq!(inode.read(0, &mut buf), 11);
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn write_at_offset() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        inode.write_inline(0, b"hello world").unwrap();
        inode.write_inline(6, b"rust!").unwrap();
        assert_eq!(inode.read_all(), b"hello rust!");
    }

    #[test]
    fn write_at_offset_extends() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        inode.write_inline(5, b"abc").unwrap();
        assert_eq!(inode.size, 8);
        let data = inode.read_all();
        assert_eq!(&data[..5], &[0, 0, 0, 0, 0]);
        assert_eq!(&data[5..], b"abc");
    }

    #[test]
    fn write_empty_data_is_noop() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"").unwrap();
        assert_eq!(inode.size, 0);
    }

    #[test]
    fn write_full_inline_capacity() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        let data = vec![0xAB; INLINE_CAPACITY];
        inode.write_inline(0, &data).unwrap();
        assert_eq!(inode.size as usize, INLINE_CAPACITY);
        assert_eq!(inode.read_all(), data);
    }

    #[test]
    fn write_exceeds_inline_capacity() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        let data = vec![0xAB; INLINE_CAPACITY + 1];
        assert!(matches!(inode.write_inline(0, &data), Err(FsError::NoSpace)));
    }

    // ── read edge cases ────────────────────────────────────────────

    #[test]
    fn read_empty_file() {
        let (mut dev, mut alloc) = setup();
        let inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        let mut buf = vec![0u8; 10];
        assert_eq!(inode.read(0, &mut buf), 0);
    }

    #[test]
    fn read_past_end() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"short").unwrap();

        let mut buf = vec![0u8; 100];
        assert_eq!(inode.read(0, &mut buf), 5);
        assert_eq!(&buf[..5], b"short");
    }

    #[test]
    fn read_at_offset() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"hello world").unwrap();

        let mut buf = vec![0u8; 5];
        assert_eq!(inode.read(6, &mut buf), 5);
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn read_at_end_returns_zero() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"abc").unwrap();
        let mut buf = vec![0u8; 10];
        assert_eq!(inode.read(3, &mut buf), 0);
    }

    // ── save + load roundtrip with data ────────────────────────────

    #[test]
    fn save_load_preserves_inline_data() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"persistent content").unwrap();
        inode.modified = 9999;
        inode.save(&mut dev).unwrap();

        let loaded = Inode::load(&dev, inode.inode_block()).unwrap();
        assert_eq!(loaded.read_all(), b"persistent content");
        assert_eq!(loaded.modified, 9999);
        assert_eq!(loaded.size, 18);
    }

    #[test]
    fn save_load_preserves_extents() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        // Manually add extents (normally done by A5).
        inode
            .add_extent(InodeExtent {
                start_block: 100,
                count: 5,
                birth_txg: 42,
            })
            .unwrap();
        inode
            .add_extent(InodeExtent {
                start_block: 200,
                count: 3,
                birth_txg: 0x0000_FFFF_FFFF_FFFF, // max u48
            })
            .unwrap();
        inode.save(&mut dev).unwrap();

        let loaded = Inode::load(&dev, inode.inode_block()).unwrap();
        assert_eq!(loaded.extents().len(), 2);
        assert_eq!(loaded.extents()[0].start_block, 100);
        assert_eq!(loaded.extents()[0].count, 5);
        assert_eq!(loaded.extents()[0].birth_txg, 42);
        assert_eq!(loaded.extents()[1].start_block, 200);
        assert_eq!(loaded.extents()[1].count, 3);
        assert_eq!(loaded.extents()[1].birth_txg, 0x0000_FFFF_FFFF_FFFF);
    }

    // ── truncate ───────────────────────────────────────────────────

    #[test]
    fn truncate_shrink() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"hello world").unwrap();

        inode.truncate(5).unwrap();
        assert_eq!(inode.size, 5);
        assert_eq!(inode.read_all(), b"hello");
    }

    #[test]
    fn truncate_grow() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"hi").unwrap();

        inode.truncate(10).unwrap();
        assert_eq!(inode.size, 10);
        let data = inode.read_all();
        assert_eq!(&data[..2], b"hi");
        assert!(data[2..].iter().all(|&b| b == 0));
    }

    #[test]
    fn truncate_to_zero() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"data").unwrap();

        inode.truncate(0).unwrap();
        assert_eq!(inode.size, 0);
        assert_eq!(inode.read_all(), b"");
    }

    #[test]
    fn truncate_exceeds_inline_capacity() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        assert!(matches!(
            inode.truncate(INLINE_CAPACITY as u64 + 1),
            Err(FsError::NoSpace)
        ));
    }

    // ── transition to extents ──────────────────────────────────────

    #[test]
    fn transition_to_extents() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode.write_inline(0, b"inline data").unwrap();
        assert!(inode.is_inline());

        let old_data = inode.transition_to_extents();
        assert_eq!(old_data, b"inline data");
        assert!(!inode.is_inline());
    }

    // ── extent list ────────────────────────────────────────────────

    #[test]
    fn add_max_extents() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        for i in 0..MAX_EXTENTS {
            inode
                .add_extent(InodeExtent {
                    start_block: (i * 10) as u32,
                    count: 5,
                    birth_txg: i as u64,
                })
                .unwrap();
        }
        assert_eq!(inode.extents().len(), MAX_EXTENTS);

        // One more should fail.
        assert!(matches!(
            inode.add_extent(InodeExtent {
                start_block: 999,
                count: 1,
                birth_txg: 0
            }),
            Err(FsError::NoSpace)
        ));
    }

    #[test]
    fn clear_extents() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        inode
            .add_extent(InodeExtent {
                start_block: 50,
                count: 10,
                birth_txg: 1,
            })
            .unwrap();
        inode.clear_extents();
        assert!(inode.extents().is_empty());
    }

    // ── delete ─────────────────────────────────────────────────────

    #[test]
    fn delete_frees_inode_block() {
        let (mut dev, mut alloc) = setup();
        let free_before = alloc.free_blocks();
        let inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        assert_eq!(alloc.free_blocks(), free_before - 1);

        inode.delete(&mut alloc);
        assert_eq!(alloc.free_blocks(), free_before);
    }

    #[test]
    fn delete_frees_extent_blocks() {
        let (mut dev, mut alloc) = setup();
        let mut inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();

        // Simulate extent-based file: allocate data blocks and add extents.
        let data_block = alloc.alloc(5).unwrap();
        inode
            .add_extent(InodeExtent {
                start_block: data_block,
                count: 5,
                birth_txg: 1,
            })
            .unwrap();
        let free_before = alloc.free_blocks();

        inode.delete(&mut alloc);
        assert_eq!(alloc.free_blocks(), free_before + 6); // 5 data + 1 inode
    }

    // ── create fails when full ─────────────────────────────────────

    #[test]
    fn create_fails_no_space() {
        let (mut dev, mut alloc) = setup();
        // Exhaust all free blocks.
        let free = alloc.free_blocks();
        alloc.alloc(free).unwrap();
        assert!(matches!(
            Inode::create(&mut dev, &mut alloc, 1, 0),
            Err(FsError::NoSpace)
        ));
    }

    // ── corrupt inode detection ────────────────────────────────────

    #[test]
    fn load_detects_excess_extents() {
        let (mut dev, mut alloc) = setup();
        let inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        let block = inode.inode_block();

        // Corrupt: set extent_count to 255.
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        dev.read_block(block, &mut buf).unwrap();
        put_u16(&mut buf, H_EXTENT_CT, 255);
        dev.write_block(block, &buf).unwrap();

        assert!(matches!(Inode::load(&dev, block), Err(FsError::Corrupt(_))));
    }

    #[test]
    fn load_detects_inline_size_overflow() {
        let (mut dev, mut alloc) = setup();
        let inode = Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
        let block = inode.inode_block();

        // Corrupt: set inline flag with size > capacity.
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        dev.read_block(block, &mut buf).unwrap();
        put_u32(&mut buf, H_FLAGS, FLAG_INLINE);
        put_u64(&mut buf, H_SIZE, INLINE_CAPACITY as u64 + 1);
        dev.write_block(block, &buf).unwrap();

        assert!(matches!(Inode::load(&dev, block), Err(FsError::Corrupt(_))));
    }
}
