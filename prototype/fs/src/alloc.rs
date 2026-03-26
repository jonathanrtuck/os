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

use crate::block::BlockDevice;
use crate::superblock::DATA_START;
use crate::{FsError, BLOCK_SIZE};

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
                return Err(FsError::Corrupt(format!(
                    "free extent {i} has zero count"
                )));
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
        if pos > 0
            && self.free[pos - 1].start + self.free[pos - 1].count == self.free[pos].start
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;
    use proptest::prelude::*;

    const TOTAL: u32 = 256;
    const FREE_AT_START: u32 = TOTAL - DATA_START; // 239

    fn fresh() -> Allocator {
        Allocator::new(TOTAL)
    }

    // ── basic operations ───────────────────────────────────────────

    #[test]
    fn new_has_one_extent() {
        let a = fresh();
        assert_eq!(a.extent_count(), 1);
        assert_eq!(a.free_blocks(), FREE_AT_START);
    }

    #[test]
    fn new_empty_device() {
        let a = Allocator::new(DATA_START);
        assert_eq!(a.extent_count(), 0);
        assert_eq!(a.free_blocks(), 0);
    }

    #[test]
    fn alloc_returns_first_block() {
        let mut a = fresh();
        assert_eq!(a.alloc(1), Some(DATA_START));
    }

    #[test]
    fn alloc_sequential() {
        let mut a = fresh();
        assert_eq!(a.alloc(3), Some(DATA_START));
        assert_eq!(a.alloc(2), Some(DATA_START + 3));
        assert_eq!(a.alloc(1), Some(DATA_START + 5));
        assert_eq!(a.free_blocks(), FREE_AT_START - 6);
    }

    #[test]
    fn alloc_exact_fit() {
        let mut a = fresh();
        let result = a.alloc(FREE_AT_START);
        assert_eq!(result, Some(DATA_START));
        assert_eq!(a.extent_count(), 0);
        assert_eq!(a.free_blocks(), 0);
    }

    #[test]
    fn alloc_too_large() {
        let mut a = fresh();
        assert_eq!(a.alloc(FREE_AT_START + 1), None);
        // State unchanged.
        assert_eq!(a.free_blocks(), FREE_AT_START);
    }

    #[test]
    fn alloc_zero() {
        let mut a = fresh();
        assert_eq!(a.alloc(0), None);
    }

    #[test]
    fn alloc_when_empty() {
        let mut a = Allocator::new(DATA_START);
        assert_eq!(a.alloc(1), None);
    }

    // ── free + coalesce ────────────────────────────────────────────

    #[test]
    fn free_creates_new_extent() {
        let mut a = fresh();
        let b1 = a.alloc(1).unwrap();
        let _b2 = a.alloc(1).unwrap();
        let b3 = a.alloc(1).unwrap();
        // Free middle block — no neighbors to coalesce with.
        // b1 is allocated, b2 freed, b3 is allocated, rest is one extent.
        // Actually: b1=17, b2=18, b3=19, free=[20..256].
        // Freeing b2=18 creates [18..19] between two allocated blocks.
        a.free(b1, 1); // free 17 → [17..18, 20..256]
        a.free(b3, 1); // free 19 → [17..18, 19..20, 20..256] → coalesces → [17..18, 19..256]
        assert_eq!(a.extent_count(), 2); // [17..18] and [19..256]
    }

    #[test]
    fn free_coalesce_right() {
        let mut a = fresh();
        let b = a.alloc(1).unwrap(); // 17
        // Free list: [18..256]
        a.free(b, 1); // 17 is adjacent to 18 → coalesce → [17..256]
        assert_eq!(a.extent_count(), 1);
        assert_eq!(a.free_blocks(), FREE_AT_START);
    }

    #[test]
    fn free_coalesce_left() {
        let mut a = fresh();
        let _b1 = a.alloc(1).unwrap(); // 17
        let b2 = a.alloc(1).unwrap(); // 18
        // Free list: [19..256]
        a.free(b2, 1); // 18 is adjacent to 19 → coalesce → [19..256] becomes [18..256]
        assert_eq!(a.extent_count(), 1);
    }

    #[test]
    fn free_coalesce_both() {
        let mut a = fresh();
        let b1 = a.alloc(1).unwrap(); // 17
        let b2 = a.alloc(1).unwrap(); // 18
        let b3 = a.alloc(1).unwrap(); // 19
        // Free list: [20..256]
        a.free(b1, 1); // → [17..18, 20..256]
        a.free(b3, 1); // → [17..18, 19..256]  (coalesce right)
        a.free(b2, 1); // → [17..256]  (coalesce both sides)
        assert_eq!(a.extent_count(), 1);
        assert_eq!(a.free_blocks(), FREE_AT_START);
    }

    #[test]
    fn free_multi_block_coalesce() {
        let mut a = fresh();
        let b1 = a.alloc(4).unwrap(); // 17..21
        let b2 = a.alloc(4).unwrap(); // 21..25
        // Free list: [25..256]
        a.free(b2, 4); // coalesce right → [21..256]
        a.free(b1, 4); // coalesce right → [17..256]
        assert_eq!(a.extent_count(), 1);
        assert_eq!(a.free_blocks(), FREE_AT_START);
    }

    // ── persist + load ─────────────────────────────────────────────

    #[test]
    fn persist_and_load_roundtrip() {
        let mut dev = MemoryBlockDevice::new(TOTAL);
        let mut a = fresh();

        // Create some fragmentation.
        let b1 = a.alloc(10).unwrap();
        let _b2 = a.alloc(5).unwrap();
        let b3 = a.alloc(10).unwrap();
        a.free(b1, 10);
        a.free(b3, 10);
        // Now we have a fragmented free list.
        let extents_before = a.extent_count();
        let free_before = a.free_blocks();

        let block = a.persist(&mut dev).unwrap();

        let loaded = Allocator::load(&dev, block).unwrap();
        assert_eq!(loaded.extent_count(), extents_before);
        // persist() allocated 1 block, so free_blocks is 1 less.
        assert_eq!(loaded.free_blocks(), free_before - 1);
    }

    #[test]
    fn persist_allocs_block_for_itself() {
        let mut dev = MemoryBlockDevice::new(TOTAL);
        let mut a = fresh();
        let free_before = a.free_blocks();

        let block = a.persist(&mut dev).unwrap();

        assert!(block >= DATA_START);
        assert_eq!(a.free_blocks(), free_before - 1);
    }

    #[test]
    fn persist_fails_when_full() {
        let mut dev = MemoryBlockDevice::new(TOTAL);
        let mut a = fresh();
        a.alloc(FREE_AT_START).unwrap(); // allocate everything
        assert!(matches!(a.persist(&mut dev), Err(FsError::NoSpace)));
    }

    #[test]
    fn load_detects_corrupt_count() {
        let mut dev = MemoryBlockDevice::new(TOTAL);
        let mut a = fresh();
        let block = a.persist(&mut dev).unwrap();

        // Corrupt: set entry_count to absurd value.
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        dev.read_block(block, &mut buf).unwrap();
        put_u32(&mut buf, OFF_COUNT, MAX_EXTENTS as u32 + 1);
        dev.write_block(block, &buf).unwrap();

        assert!(matches!(Allocator::load(&dev, block), Err(FsError::Corrupt(_))));
    }

    // ── proptest ───────────────────────────────────────────────────

    /// Deterministic shuffle using xorshift64.
    fn shuffle(items: &mut Vec<(u32, u32)>, mut seed: u64) {
        if seed == 0 {
            seed = 1;
        }
        for i in (1..items.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let j = (seed as usize) % (i + 1);
            items.swap(i, j);
        }
    }

    proptest! {
        /// Allocate random-sized blocks, free them in order, verify total restored.
        #[test]
        fn alloc_free_sequential(sizes in prop::collection::vec(1u32..=20, 1..50)) {
            let mut a = Allocator::new(512);
            let initial = a.free_blocks();

            let mut allocated = Vec::new();
            for &size in &sizes {
                if let Some(start) = a.alloc(size) {
                    allocated.push((start, size));
                }
            }
            let alloc_total: u32 = allocated.iter().map(|&(_, s)| s).sum();
            prop_assert_eq!(a.free_blocks(), initial - alloc_total);

            for &(start, size) in &allocated {
                a.free(start, size);
            }
            prop_assert_eq!(a.free_blocks(), initial);
            prop_assert_eq!(a.extent_count(), 1);
        }

        /// Allocate random-sized blocks, free them in shuffled order.
        #[test]
        fn alloc_free_shuffled(
            sizes in prop::collection::vec(1u32..=20, 1..30),
            seed in 1u64..=u64::MAX,
        ) {
            let mut a = Allocator::new(512);
            let initial = a.free_blocks();

            let mut allocated = Vec::new();
            for &size in &sizes {
                if let Some(start) = a.alloc(size) {
                    allocated.push((start, size));
                }
            }

            shuffle(&mut allocated, seed);

            for &(start, size) in &allocated {
                a.free(start, size);
            }
            prop_assert_eq!(a.free_blocks(), initial);
            prop_assert_eq!(a.extent_count(), 1);
        }

        /// Interleaved alloc/free: allocate some, free some, repeat.
        #[test]
        fn interleaved_alloc_free(
            sizes in prop::collection::vec(1u32..=10, 10..40),
        ) {
            let mut a = Allocator::new(1024);
            let initial = a.free_blocks();
            let mut live: Vec<(u32, u32)> = Vec::new();

            for (i, &size) in sizes.iter().enumerate() {
                if i % 3 == 0 && !live.is_empty() {
                    // Free the oldest allocation.
                    let (start, count) = live.remove(0);
                    a.free(start, count);
                }
                if let Some(start) = a.alloc(size) {
                    live.push((start, size));
                }
            }

            // Free everything remaining.
            for (start, count) in live {
                a.free(start, count);
            }
            prop_assert_eq!(a.free_blocks(), initial);
            prop_assert_eq!(a.extent_count(), 1);
        }

        /// Persist + load roundtrip preserves free list state.
        #[test]
        fn persist_load_roundtrip(
            sizes in prop::collection::vec(1u32..=10, 1..20),
        ) {
            let mut dev = MemoryBlockDevice::new(512);
            let mut a = Allocator::new(512);

            // Create some fragmentation.
            let mut allocated = Vec::new();
            for &size in &sizes {
                if let Some(start) = a.alloc(size) {
                    allocated.push((start, size));
                }
            }
            // Free every other one.
            for i in (0..allocated.len()).step_by(2) {
                let (start, count) = allocated[i];
                a.free(start, count);
            }

            let free_before = a.free_blocks();
            let block = a.persist(&mut dev).unwrap();
            let loaded = Allocator::load(&dev, block).unwrap();

            // persist() allocates 1 block from the free list.
            prop_assert_eq!(loaded.free_blocks(), free_before - 1);
        }
    }
}
