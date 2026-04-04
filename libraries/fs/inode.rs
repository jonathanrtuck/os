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

use alloc::{format, vec, vec::Vec};

use crate::{alloc_mod::Allocator, block::BlockDevice, FsError, BLOCK_SIZE};

// ── Layout constants ───────────────────────────────────────────────

const HEADER_SIZE: usize = 64;
/// Maximum extents stored directly in the inode.
pub const MAX_EXTENTS: usize = 16;
const EXTENT_SIZE: usize = 12;
const EXTENT_REGION: usize = MAX_EXTENTS * EXTENT_SIZE; // 192
const INLINE_OFFSET: usize = HEADER_SIZE + EXTENT_REGION; // 256

/// Maximum bytes storable inline in the inode block.
pub const INLINE_CAPACITY: usize = BLOCK_SIZE as usize - INLINE_OFFSET; // 16128

/// Overflow block layout: [count: u32] [reserved: u32] [extents...]
const OVERFLOW_HEADER: usize = 8;
/// Maximum extents in the overflow block.
const MAX_OVERFLOW_EXTENTS: usize = (BLOCK_SIZE as usize - OVERFLOW_HEADER) / EXTENT_SIZE; // 1364

/// Total maximum extents per file (inline + overflow).
pub const MAX_TOTAL_EXTENTS: usize = MAX_EXTENTS + MAX_OVERFLOW_EXTENTS; // 1380

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
const H_INDIRECT: usize = 40; //     u32 (overflow extent block, 0 = none)
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
    /// Extents stored directly in the inode (up to MAX_EXTENTS = 16).
    extents: Vec<InodeExtent>,
    /// Overflow extents stored in a separate block (up to MAX_OVERFLOW_EXTENTS).
    overflow_extents: Vec<InodeExtent>,
    /// Block number of the overflow extent block (0 = none).
    indirect_block: u32,
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
            overflow_extents: Vec::new(),
            indirect_block: 0,
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
        let indirect_block = get_u32(&buf, H_INDIRECT);

        if extent_count > MAX_EXTENTS {
            return Err(FsError::Corrupt(format!(
                "inode {file_id}: {extent_count} inline extents, max {MAX_EXTENTS}"
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

        // Load overflow extents from indirect block, if present.
        let overflow_extents = if indirect_block != 0 {
            load_overflow_extents(device, file_id, indirect_block)?
        } else {
            Vec::new()
        };

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
            overflow_extents,
            indirect_block,
            block,
            inline_data,
        })
    }

    /// Write the inode back to its block (and overflow block if present).
    pub fn save(&self, device: &mut impl BlockDevice) -> Result<(), FsError> {
        // Write overflow extents to indirect block first.
        if self.indirect_block != 0 && !self.overflow_extents.is_empty() {
            save_overflow_extents(device, self.indirect_block, &self.overflow_extents)?;
        }

        let mut buf = vec![0u8; BLOCK_SIZE as usize];

        put_u64(&mut buf, H_FILE_ID, self.file_id);
        put_u64(&mut buf, H_SIZE, self.size);
        put_u64(&mut buf, H_CREATED, self.created);
        put_u64(&mut buf, H_MODIFIED, self.modified);
        put_u32(&mut buf, H_FLAGS, self.flags);
        put_u16(&mut buf, H_EXTENT_CT, self.extents.len() as u16);
        put_u32(&mut buf, H_INDIRECT, self.indirect_block);
        // H_SNAP_CT, H_SNAP_BLK: remain 0 (reserved for A6)

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
        debug_assert!(
            self.is_inline(),
            "write_inline called on extent-based inode"
        );
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

    /// All extents for this file (inline + overflow, in order).
    /// Empty for inline files.
    pub fn extents(&self) -> Vec<InodeExtent> {
        if self.overflow_extents.is_empty() {
            self.extents.clone()
        } else {
            let mut all = self.extents.clone();
            all.extend_from_slice(&self.overflow_extents);
            all
        }
    }

    /// Number of extents (inline + overflow).
    pub fn extent_count(&self) -> usize {
        self.extents.len() + self.overflow_extents.len()
    }

    /// The inline extents only (for snapshot serialization).
    pub fn inline_extents(&self) -> &[InodeExtent] {
        &self.extents
    }

    /// The overflow extents only (for snapshot serialization).
    pub fn overflow_extents_ref(&self) -> &[InodeExtent] {
        &self.overflow_extents
    }

    /// Add an extent. Fills the inline list first (up to MAX_EXTENTS),
    /// then spills to the overflow list (up to MAX_OVERFLOW_EXTENTS).
    pub fn add_extent(&mut self, ext: InodeExtent) -> Result<(), FsError> {
        if self.extents.len() < MAX_EXTENTS {
            self.extents.push(ext);
        } else if self.overflow_extents.len() < MAX_OVERFLOW_EXTENTS {
            self.overflow_extents.push(ext);
        } else {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// Clear all extents (inline + overflow). Resets indirect_block to 0.
    pub fn clear_extents(&mut self) {
        self.extents.clear();
        self.overflow_extents.clear();
        self.indirect_block = 0;
    }

    /// Transition from inline to extent-based storage.
    /// Clears the inline flag and data. The caller is responsible for
    /// writing the inline content to data blocks first.
    pub fn transition_to_extents(&mut self) -> Vec<u8> {
        self.flags &= !FLAG_INLINE;
        core::mem::take(&mut self.inline_data)
    }

    /// The overflow block number (0 = none). Exposed for snapshot
    /// serialization and COW lifecycle management.
    pub fn indirect_block(&self) -> u32 {
        self.indirect_block
    }

    /// Allocate an overflow block if needed (extents exceed inline capacity).
    /// Called before save/save_cow when overflow extents exist but no
    /// indirect block has been allocated yet.
    pub fn ensure_overflow_block(
        &mut self,
        allocator: &mut Allocator,
    ) -> Result<(), FsError> {
        if !self.overflow_extents.is_empty() && self.indirect_block == 0 {
            self.indirect_block = allocator.alloc(1).ok_or(FsError::NoSpace)?;
        }
        Ok(())
    }

    /// Write the inode to a newly allocated block (COW). Returns the old
    /// inode block number and the old overflow block number (0 if none)
    /// for deferred freeing.
    pub fn save_cow(
        &mut self,
        device: &mut impl BlockDevice,
        allocator: &mut Allocator,
    ) -> Result<OldBlocks, FsError> {
        let old_block = self.block;
        let old_indirect = self.indirect_block;

        // Allocate new inode block.
        let new_block = allocator.alloc(1).ok_or(FsError::NoSpace)?;
        self.block = new_block;

        // Allocate new overflow block if needed.
        if !self.overflow_extents.is_empty() {
            self.indirect_block = allocator.alloc(1).ok_or(FsError::NoSpace)?;
        } else {
            self.indirect_block = 0;
        }

        self.save(device)?;
        Ok(OldBlocks {
            inode: old_block,
            indirect: old_indirect,
        })
    }

    /// Transition back to inline (empty). Used when truncating to zero.
    pub fn transition_to_inline(&mut self) {
        self.flags |= FLAG_INLINE;
        self.inline_data.clear();
        self.extents.clear();
        self.overflow_extents.clear();
        self.indirect_block = 0;
        self.size = 0;
    }

    /// Delete this inode, freeing its block, all data extent blocks,
    /// and the overflow block (if any). Consumes the inode.
    pub fn delete(self, allocator: &mut Allocator) {
        for ext in &self.extents {
            allocator.free(ext.start_block, ext.count as u32);
        }
        for ext in &self.overflow_extents {
            allocator.free(ext.start_block, ext.count as u32);
        }
        if self.indirect_block != 0 {
            allocator.free(self.indirect_block, 1);
        }
        allocator.free(self.block, 1);
    }
}

/// Block numbers returned by `save_cow` for deferred freeing.
pub struct OldBlocks {
    /// The old inode block.
    pub inode: u32,
    /// The old overflow extent block (0 = none).
    pub indirect: u32,
}

// ── Overflow extent I/O ───────────────────────────────────────────

/// Load overflow extents from an indirect block.
fn load_overflow_extents(
    device: &impl BlockDevice,
    file_id: u64,
    block: u32,
) -> Result<Vec<InodeExtent>, FsError> {
    let mut buf = vec![0u8; BLOCK_SIZE as usize];
    device.read_block(block, &mut buf)?;

    let count = get_u32(&buf, 0) as usize;
    if count > MAX_OVERFLOW_EXTENTS {
        return Err(FsError::Corrupt(format!(
            "inode {file_id}: overflow block has {count} extents, max {MAX_OVERFLOW_EXTENTS}"
        )));
    }

    let mut extents = Vec::with_capacity(count);
    for i in 0..count {
        let off = OVERFLOW_HEADER + i * EXTENT_SIZE;
        extents.push(InodeExtent {
            start_block: get_u32(&buf, off),
            count: get_u16(&buf, off + 4),
            birth_txg: get_u48(&buf, off + 6),
        });
    }

    Ok(extents)
}

/// Save overflow extents to an indirect block.
fn save_overflow_extents(
    device: &mut impl BlockDevice,
    block: u32,
    extents: &[InodeExtent],
) -> Result<(), FsError> {
    debug_assert!(
        extents.len() <= MAX_OVERFLOW_EXTENTS,
        "overflow extents exceed capacity: {} > {MAX_OVERFLOW_EXTENTS}",
        extents.len()
    );

    let mut buf = vec![0u8; BLOCK_SIZE as usize];
    put_u32(&mut buf, 0, extents.len() as u32);
    // bytes 4..8: reserved (zero)

    for (i, ext) in extents.iter().enumerate() {
        let off = OVERFLOW_HEADER + i * EXTENT_SIZE;
        put_u32(&mut buf, off, ext.start_block);
        put_u16(&mut buf, off + 4, ext.count);
        put_u48(&mut buf, off + 6, ext.birth_txg);
    }

    device.write_block(block, &buf)
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
