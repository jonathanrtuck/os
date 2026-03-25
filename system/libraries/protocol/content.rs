//! Content Region shared memory layout.
//!
//! The Content Region is a persistent shared memory region containing decoded
//! rendering data (font TTF bytes, decoded image pixels). Allocated by init,
//! managed by core (sole writer), read-only for render services.
//!
//! Layout: `[ContentRegionHeader | entries[MAX] | padding to CONTENT_HEADER_SIZE | data area]`

/// Magic number for Content Region validation ("CONT" in ASCII).
pub const CONTENT_REGION_MAGIC: u32 = 0x434F_4E54;
/// Current Content Region format version.
pub const CONTENT_REGION_VERSION: u32 = 1;
/// Maximum number of content entries in the registry.
pub const MAX_CONTENT_ENTRIES: usize = 64;
/// Total header size in bytes (header struct + padding for alignment).
/// Data area starts at this offset from the Content Region base.
pub const CONTENT_HEADER_SIZE: usize = 2048;

// ── Well-known content IDs ──────────────────────────────────────────

/// Unused/invalid content ID.
pub const CONTENT_ID_NONE: u32 = 0;
/// Monospace font (JetBrains Mono) — rendering data for glyph rasterization.
pub const CONTENT_ID_FONT_MONO: u32 = 1;
/// Sans-serif font (Inter) — rendering data for chrome text.
pub const CONTENT_ID_FONT_SANS: u32 = 2;
/// Serif font (Source Serif 4) — rendering data for body text.
pub const CONTENT_ID_FONT_SERIF: u32 = 3;
/// First dynamically assigned content ID (for decoded images, etc.).
pub const CONTENT_ID_DYNAMIC_START: u32 = 16;

// ── Content class ───────────────────────────────────────────────────

/// Classification of content stored in a Content Region entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ContentClass {
    /// Font rendering data (TTF bytes). Read by render services for
    /// glyph rasterization.
    Font = 0,
    /// Decoded pixel data (BGRA8888). Referenced by Content::Image
    /// nodes in the scene graph via content_id.
    Pixels = 1,
}

// ── Content entry ───────────────────────────────────────────────────

/// A single entry in the Content Region registry.
///
/// Each entry describes one block of data in the Content Region's data area.
/// Entries are immutable once written (write-once semantics for lock-free
/// concurrent reads by the compositor).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ContentEntry {
    /// Unique content ID. 0 = unused slot. Well-known IDs (1-15) for
    /// fonts; dynamic IDs (≥16) for decoded images.
    pub content_id: u32,
    /// Byte offset from the Content Region base address.
    pub offset: u32,
    /// Byte length of the content data.
    pub length: u32,
    /// Content class (Font, Pixels). Stored as u8 for repr(C) stability.
    pub class: u8,
    pub _pad: [u8; 3],
    /// For Pixels: source image width in pixels. 0 for Font.
    pub width: u16,
    /// For Pixels: source image height in pixels. 0 for Font.
    pub height: u16,
    /// Scene graph generation when this entry was created (for future
    /// generation-based GC). 0 for entries created at boot.
    pub generation: u32,
}

const _: () = assert!(core::mem::size_of::<ContentEntry>() == 24);

impl ContentEntry {
    /// An empty/unused entry.
    pub const EMPTY: Self = Self {
        content_id: CONTENT_ID_NONE,
        offset: 0,
        length: 0,
        class: 0,
        _pad: [0; 3],
        width: 0,
        height: 0,
        generation: 0,
    };
}

// ── Content Region header ───────────────────────────────────────────

/// Header at the start of the Content Region shared memory.
///
/// Written by init (font entries) and core (decoded image entries).
/// Read by render services to locate font data and image pixels.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ContentRegionHeader {
    /// Magic number for validation (CONTENT_REGION_MAGIC).
    pub magic: u32,
    /// Format version (CONTENT_REGION_VERSION).
    pub version: u32,
    /// Number of active entries in the registry.
    pub entry_count: u32,
    /// Maximum entries (MAX_CONTENT_ENTRIES).
    pub max_entries: u32,
    /// Byte offset where the data area starts (CONTENT_HEADER_SIZE).
    pub data_offset: u32,
    /// Bump allocator: next free byte offset in the data area
    /// (relative to Content Region base, not data area start).
    pub next_alloc: u32,
    /// Reserved for future use.
    pub _reserved: [u32; 2],
    /// Registry entries.
    pub entries: [ContentEntry; MAX_CONTENT_ENTRIES],
}

// Header struct must fit within CONTENT_HEADER_SIZE.
const _: () = assert!(core::mem::size_of::<ContentRegionHeader>() <= CONTENT_HEADER_SIZE);

// ── Lookup ──────────────────────────────────────────────────────────

/// Find a content entry by ID. Returns the first entry with a matching
/// `content_id`, or `None` if not found. Linear scan — fine for ≤64 entries.
pub fn find_entry(header: &ContentRegionHeader, content_id: u32) -> Option<&ContentEntry> {
    let count = header.entry_count as usize;
    if count > MAX_CONTENT_ENTRIES {
        return None;
    }
    for i in 0..count {
        if header.entries[i].content_id == content_id {
            return Some(&header.entries[i]);
        }
    }
    None
}

/// Remove a content entry by ID from the registry. Returns the entry's
/// data location `(offset, length)` so the caller can free the backing
/// allocation via [`ContentAllocator::free`]. Returns `None` if not found.
pub fn remove_entry(header: &mut ContentRegionHeader, content_id: u32) -> Option<(u32, u32)> {
    let count = header.entry_count as usize;
    if count == 0 || count > MAX_CONTENT_ENTRIES {
        return None;
    }
    for i in 0..count {
        if header.entries[i].content_id == content_id {
            let offset = header.entries[i].offset;
            let length = header.entries[i].length;
            // Compact: shift remaining entries left.
            for j in i..count - 1 {
                header.entries[j] = header.entries[j + 1];
            }
            header.entries[count - 1] = ContentEntry::EMPTY;
            header.entry_count -= 1;
            return Some((offset, length));
        }
    }
    None
}

// ── Content Region allocator ──────────────────────────────────────

/// Minimum allocation alignment (16 bytes for BGRA rows and NEON).
pub const CONTENT_ALLOC_ALIGN: u32 = 16;

/// Maximum number of free blocks the allocator can track.
///
/// With 64 content entries and coalescing, the worst case is 33 free
/// blocks (every other slot freed). 64 is generous headroom.
pub const MAX_FREE_BLOCKS: usize = 64;

/// Round `value` up to the next multiple of `align` (must be a power of two).
const fn align_up_u32(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

/// Maximum number of entries awaiting deferred free (one per content entry).
pub const MAX_PENDING_FREE: usize = 64;

/// A contiguous free region in the Content Region data area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeBlock {
    /// Byte offset from the Content Region base.
    pub offset: u32,
    /// Size in bytes.
    pub length: u32,
}

/// An entry awaiting safe reclamation. Core retires a content entry at
/// generation N; the backing memory cannot be freed until the compositor
/// has finished reading all scene buffers that might reference it
/// (`reader_done_gen >= death_gen`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingFree {
    /// Content ID to remove from the registry on reclaim.
    pub content_id: u32,
    /// Scene graph generation when this entry was removed from the scene.
    pub death_gen: u32,
}

/// Free-list allocator for the Content Region data area.
///
/// Manages variable-size allocations with first-fit strategy, automatic
/// coalescing on free, and generation-based deferred reclamation for
/// safe concurrent access.
///
/// The free-list and pending-free queue are private to core's memory —
/// render services never see them.
///
/// Invariants:
/// - Free blocks are sorted by offset (ascending).
/// - No two free blocks overlap or are adjacent (coalesced on free).
/// - All offsets and lengths are aligned to [`CONTENT_ALLOC_ALIGN`].
/// - Pending entries are freed only when `reader_done_gen >= death_gen`.
pub struct ContentAllocator {
    blocks: [FreeBlock; MAX_FREE_BLOCKS],
    count: usize,
    pending: [PendingFree; MAX_PENDING_FREE],
    pending_count: usize,
}

impl ContentAllocator {
    /// An empty allocator with no free space. Use as a default before
    /// the Content Region is available, then replace with [`Self::new`].
    pub const fn empty() -> Self {
        Self {
            blocks: [FreeBlock {
                offset: 0,
                length: 0,
            }; MAX_FREE_BLOCKS],
            count: 0,
            pending: [PendingFree {
                content_id: 0,
                death_gen: 0,
            }; MAX_PENDING_FREE],
            pending_count: 0,
        }
    }

    /// Create an allocator with a single free region spanning
    /// `[free_start, free_end)`. Both are byte offsets from the Content
    /// Region base. `free_start` is aligned up; `free_end` is used as-is.
    ///
    /// Typical use: `ContentAllocator::new(header.next_alloc, content_size as u32)`
    /// where `header.next_alloc` is the first byte after init's boot-time
    /// font data.
    pub fn new(free_start: u32, free_end: u32) -> Self {
        let aligned_start = align_up_u32(free_start, CONTENT_ALLOC_ALIGN);
        let mut alloc = Self::empty();
        if aligned_start < free_end {
            alloc.blocks[0] = FreeBlock {
                offset: aligned_start,
                length: free_end - aligned_start,
            };
            alloc.count = 1;
        }
        alloc
    }

    /// Allocate `size` bytes using first-fit. Returns the byte offset
    /// from the Content Region base, or `None` if no block is large enough.
    /// The actual allocation is rounded up to [`CONTENT_ALLOC_ALIGN`].
    pub fn allocate(&mut self, size: u32) -> Option<u32> {
        if size == 0 {
            return None;
        }
        let aligned_size = align_up_u32(size, CONTENT_ALLOC_ALIGN);
        for i in 0..self.count {
            if self.blocks[i].length >= aligned_size {
                let offset = self.blocks[i].offset;
                let remainder = self.blocks[i].length - aligned_size;
                if remainder > 0 {
                    // Shrink: advance the block's start past the allocation.
                    self.blocks[i].offset = offset + aligned_size;
                    self.blocks[i].length = remainder;
                } else {
                    // Exact fit: remove the block entirely.
                    self.remove_block(i);
                }
                return Some(offset);
            }
        }
        None
    }

    /// Return a previously allocated region to the free-list.
    ///
    /// `offset` and `length` should match a prior allocation. The length
    /// is aligned up to [`CONTENT_ALLOC_ALIGN`] (matching what `allocate`
    /// consumed). Coalesces with adjacent free blocks automatically.
    pub fn free(&mut self, offset: u32, length: u32) {
        if length == 0 {
            return;
        }
        let length = align_up_u32(length, CONTENT_ALLOC_ALIGN);
        let end = offset + length;

        // Find insertion point (maintain sorted order by offset).
        let pos = self.insert_position(offset);

        // Check adjacency with left and right neighbors.
        let merge_left = pos > 0 && {
            let left = self.blocks[pos - 1];
            left.offset + left.length == offset
        };
        let merge_right = pos < self.count && self.blocks[pos].offset == end;

        match (merge_left, merge_right) {
            (true, true) => {
                // Three-way merge: left + freed + right → one block.
                self.blocks[pos - 1].length += length + self.blocks[pos].length;
                self.remove_block(pos);
            }
            (true, false) => {
                // Extend left block rightward.
                self.blocks[pos - 1].length += length;
            }
            (false, true) => {
                // Extend right block leftward.
                self.blocks[pos].offset = offset;
                self.blocks[pos].length += length;
            }
            (false, false) => {
                // No neighbors to merge — insert a new free block.
                self.insert_block(pos, FreeBlock { offset, length });
            }
        }
    }

    /// Total free bytes across all blocks.
    pub fn free_bytes(&self) -> u32 {
        let mut total: u32 = 0;
        for i in 0..self.count {
            total += self.blocks[i].length;
        }
        total
    }

    /// Number of free blocks (fragmentation indicator).
    pub fn block_count(&self) -> usize {
        self.count
    }

    /// Largest contiguous free region, or 0 if exhausted.
    pub fn largest_free(&self) -> u32 {
        let mut max: u32 = 0;
        for i in 0..self.count {
            if self.blocks[i].length > max {
                max = self.blocks[i].length;
            }
        }
        max
    }

    // ── Deferred reclamation (GC) ─────────────────────────────────

    /// Schedule a content entry for deferred free. The entry remains in
    /// the registry until `sweep()` confirms the compositor has moved past
    /// `death_gen`. Returns `false` if the pending queue is full.
    ///
    /// Call this when a content_id is removed from the scene graph.
    /// `death_gen` should be the generation of the scene publish that
    /// no longer references this content_id.
    pub fn defer_free(&mut self, content_id: u32, death_gen: u32) -> bool {
        if self.pending_count >= MAX_PENDING_FREE {
            return false;
        }
        self.pending[self.pending_count] = PendingFree {
            content_id,
            death_gen,
        };
        self.pending_count += 1;
        true
    }

    /// Reclaim entries whose `death_gen` the compositor has moved past.
    ///
    /// For each pending entry where `reader_done_gen >= death_gen`, removes
    /// the registry entry and frees the backing allocation. Returns the
    /// number of entries reclaimed.
    ///
    /// Call after each scene publish with the current `reader_done_gen`.
    pub fn sweep(&mut self, reader_done_gen: u32, header: &mut ContentRegionHeader) -> u32 {
        let mut reclaimed: u32 = 0;
        let mut i = 0;
        while i < self.pending_count {
            if reader_done_gen >= self.pending[i].death_gen {
                let content_id = self.pending[i].content_id;
                if let Some((offset, length)) = remove_entry(header, content_id) {
                    self.free(offset, length);
                    reclaimed += 1;
                }
                // Swap-remove: move last element into this slot.
                self.pending_count -= 1;
                if i < self.pending_count {
                    self.pending[i] = self.pending[self.pending_count];
                }
                // Don't increment i — re-check the swapped element.
            } else {
                i += 1;
            }
        }
        reclaimed
    }

    /// Number of entries awaiting deferred reclamation.
    pub fn pending_count(&self) -> usize {
        self.pending_count
    }

    // ── Private helpers ───────────────────────────────────────────

    /// Find the sorted insertion index for `offset`.
    fn insert_position(&self, offset: u32) -> usize {
        for i in 0..self.count {
            if self.blocks[i].offset > offset {
                return i;
            }
        }
        self.count
    }

    /// Remove block at `index`, shifting subsequent blocks left.
    fn remove_block(&mut self, index: usize) {
        for i in index..self.count - 1 {
            self.blocks[i] = self.blocks[i + 1];
        }
        self.blocks[self.count - 1] = FreeBlock {
            offset: 0,
            length: 0,
        };
        self.count -= 1;
    }

    /// Insert a block at `index`, shifting subsequent blocks right.
    /// Silently drops the block if the free-list is full (should not
    /// happen with proper coalescing and ≤64 content entries).
    fn insert_block(&mut self, index: usize, block: FreeBlock) {
        if self.count >= MAX_FREE_BLOCKS {
            return;
        }
        let mut i = self.count;
        while i > index {
            self.blocks[i] = self.blocks[i - 1];
            i -= 1;
        }
        self.blocks[index] = block;
        self.count += 1;
    }
}
