//! Block allocator — sorted free-extent list.
//!
//! Tracks free space as a sorted, coalesced list of `(start, count)` extents.
//! First-fit allocation. O(n) operations where n = number of free extents
//! (typically a few hundred for document workloads).
//!
//! Persisted as a single 16 KiB block: 8-byte header + up to 2047 entries.
//! The block is itself allocated from the free list (self-referential
//! allocation — the "turtles" problem is solved by allocating from the
//! current generation and serializing the post-allocation state).

use alloc::{format, vec, vec::Vec};

use crate::{BLOCK_SIZE, FsError, block::BlockDevice, superblock::DATA_START};

/// Maximum free extents that fit in one persistence block.
/// (BLOCK_SIZE - 8 byte header) / 8 bytes per entry.
const MAX_EXTENTS: usize = (BLOCK_SIZE as usize - 8) / 8; // 2047

// ── Persistence layout ─────────────────────────────────────────────
// Bytes 0..4:   entry_count: u32
// Bytes 4..8:   free_blocks: u32
// Bytes 8..:    entries × 8 bytes (start: u32, count: u32)

const OFF_COUNT: usize = 0;
const OFF_FREE: usize = 4;
const OFF_ENTRIES: usize = 8;

/// A contiguous range of free blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    start: u32,
    count: u32,
}

/// Block allocator backed by a sorted free-extent list.
///
/// Invariants (enforced by debug assertions):
/// - `free` is sorted by `start`, non-overlapping, non-adjacent (coalesced)
/// - `free_blocks` equals the sum of all extent counts
/// - All extents have `count > 0`
pub struct Allocator {
    free: Vec<Extent>,
    free_blocks: u32,
}

impl Allocator {
    /// Create an allocator for a freshly formatted filesystem.
    /// All blocks from `DATA_START` to `total_blocks` are free.
    pub fn new(total_blocks: u32) -> Self {
        let free_count = total_blocks.saturating_sub(DATA_START);
        let free = if free_count > 0 {
            vec![Extent {
                start: DATA_START,
                count: free_count,
            }]
        } else {
            Vec::new()
        };
        Self {
            free,
            free_blocks: free_count,
        }
    }

    /// Load from a persisted free-list block.
    pub fn load(device: &impl BlockDevice, block: u32) -> Result<Self, FsError> {
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        device.read_block(block, &mut buf)?;

        let entry_count = get_u32(&buf, OFF_COUNT) as usize;
        let free_blocks = get_u32(&buf, OFF_FREE);

        if entry_count > MAX_EXTENTS {
            return Err(FsError::Corrupt(format!(
                "free list has {entry_count} entries, max {MAX_EXTENTS}"
            )));
        }

        let mut free = Vec::with_capacity(entry_count);
        let mut sum = 0u32;
        let mut prev_end = 0u32;

        for i in 0..entry_count {
            let off = OFF_ENTRIES + i * 8;
            let start = get_u32(&buf, off);
            let count = get_u32(&buf, off + 4);

            if count == 0 {
                return Err(FsError::Corrupt(format!("free extent {i} has zero count")));
            }
            if start < prev_end {
                return Err(FsError::Corrupt(format!(
                    "free extent {i} at {start} overlaps previous ending at {prev_end}"
                )));
            }
            if start == prev_end && i > 0 {
                return Err(FsError::Corrupt(format!(
                    "free extent {i} at {start} is adjacent to previous (should be coalesced)"
                )));
            }

            free.push(Extent { start, count });
            sum += count;
            prev_end = start + count;
        }

        if sum != free_blocks {
            return Err(FsError::Corrupt(format!(
                "free_blocks header ({free_blocks}) != sum of extents ({sum})"
            )));
        }

        Ok(Self { free, free_blocks })
    }

    /// Allocate `count` contiguous blocks. Returns the start block, or
    /// `None` if no extent is large enough.
    pub fn alloc(&mut self, count: u32) -> Option<u32> {
        if count == 0 {
            return None;
        }

        let idx = self.free.iter().position(|e| e.count >= count)?;
        let start = self.free[idx].start;

        if self.free[idx].count == count {
            self.free.remove(idx);
        } else {
            self.free[idx].start += count;
            self.free[idx].count -= count;
        }

        self.free_blocks -= count;
        self.debug_check();
        Some(start)
    }

    /// Allocate `count` blocks, potentially across multiple non-contiguous
    /// extents. Tries contiguous first (fast path). Falls back to greedy
    /// multi-extent allocation from available free space.
    ///
    /// `max_extents` caps the number of extents returned (e.g., how many
    /// the inode can hold). Each individual extent count is capped at
    /// `u16::MAX` to fit the `InodeExtent` on-disk format.
    ///
    /// Returns `None` if total free blocks < `count` or if the allocation
    /// would require more extents than `max_extents`.
    ///
    /// On failure, no blocks are consumed (all-or-nothing).
    pub fn alloc_multi(&mut self, count: u32, max_extents: usize) -> Option<Vec<(u32, u32)>> {
        if count == 0 || max_extents == 0 {
            return None;
        }
        if self.free_blocks < count {
            return None;
        }

        // Fast path: single contiguous allocation.
        if let Some(start) = self.alloc(count) {
            return Some(vec![(start, count)]);
        }

        // Greedy: take from free extents front-to-back until we have enough.
        let mut plan: Vec<(u32, u32)> = Vec::new();
        let mut remaining = count;

        for ext in &self.free {
            if remaining == 0 {
                break;
            }
            // Take as much as we can from this extent.
            let take = remaining.min(ext.count).min(u16::MAX as u32);
            plan.push((ext.start, take));
            remaining -= take;

            if plan.len() > max_extents {
                return None; // Would exceed extent limit.
            }
        }

        if remaining > 0 || plan.len() > max_extents {
            return None; // Not enough space or too many extents.
        }

        // Execute the plan — allocate each chunk.
        // We must be careful: each `alloc` modifies the free list, which
        // invalidates our plan indices. But since we planned from front to
        // back and each chunk starts at a free extent's start, the first-fit
        // `alloc` will find each one at the expected position.
        let result: Vec<(u32, u32)> = plan.clone();
        for &(_, chunk_count) in &result {
            let _start = self
                .alloc(chunk_count)
                .expect("alloc_multi: planned block should be available");
        }

        Some(result)
    }

    /// Free `count` blocks starting at `start`. Coalesces with neighbors.
    pub fn free(&mut self, start: u32, count: u32) {
        debug_assert!(count > 0, "freeing zero blocks");

        let end = start + count;

        // Insertion point: first extent starting after `start`.
        let pos = self.free.partition_point(|e| e.start < start);

        // Debug: verify no overlap with neighbors.
        debug_assert!(
            pos == 0 || self.free[pos - 1].start + self.free[pos - 1].count <= start,
            "freed range [{start}..{end}) overlaps previous extent {:?}",
            self.free.get(pos - 1)
        );
        debug_assert!(
            pos >= self.free.len() || end <= self.free[pos].start,
            "freed range [{start}..{end}) overlaps next extent {:?}",
            self.free.get(pos)
        );

        self.free.insert(pos, Extent { start, count });
        self.free_blocks += count;

        // Coalesce with right neighbor (now at pos+1).
        if pos + 1 < self.free.len()
            && self.free[pos].start + self.free[pos].count == self.free[pos + 1].start
        {
            self.free[pos].count += self.free[pos + 1].count;
            self.free.remove(pos + 1);
        }

        // Coalesce with left neighbor (at pos-1).
        if pos > 0 && self.free[pos - 1].start + self.free[pos - 1].count == self.free[pos].start {
            self.free[pos - 1].count += self.free[pos].count;
            self.free.remove(pos);
        }

        self.debug_check();
    }

    /// Persist the free list to a new block on `device`.
    ///
    /// Allocates one block for itself, serializes the (post-allocation)
    /// free list into it, and writes it. Returns the block number.
    /// The caller stores this in `superblock.root_free_list`.
    pub fn persist(&mut self, device: &mut impl BlockDevice) -> Result<u32, FsError> {
        if self.free.len() > MAX_EXTENTS {
            return Err(FsError::Corrupt(format!(
                "free list has {} extents, max {MAX_EXTENTS}",
                self.free.len()
            )));
        }

        let block = self.alloc(1).ok_or(FsError::NoSpace)?;

        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        put_u32(&mut buf, OFF_COUNT, self.free.len() as u32);
        put_u32(&mut buf, OFF_FREE, self.free_blocks);

        for (i, ext) in self.free.iter().enumerate() {
            let off = OFF_ENTRIES + i * 8;
            put_u32(&mut buf, off, ext.start);
            put_u32(&mut buf, off + 4, ext.count);
        }

        device.write_block(block, &buf)?;
        Ok(block)
    }

    /// Number of free blocks.
    pub fn free_blocks(&self) -> u32 {
        self.free_blocks
    }

    /// Number of free extents (fragmentation indicator).
    pub fn extent_count(&self) -> usize {
        self.free.len()
    }

    /// Debug-only invariant check.
    fn debug_check(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        let mut sum = 0u32;
        for (i, ext) in self.free.iter().enumerate() {
            assert!(ext.count > 0, "extent {i} has zero count");
            if i > 0 {
                let prev = &self.free[i - 1];
                assert!(
                    prev.start + prev.count < ext.start,
                    "extents {}/{i} not sorted/coalesced: {:?} vs {:?}",
                    i - 1,
                    prev,
                    ext
                );
            }
            sum += ext.count;
        }
        assert_eq!(
            self.free_blocks, sum,
            "free_blocks ({}) != sum ({})",
            self.free_blocks, sum
        );
    }
}

fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
