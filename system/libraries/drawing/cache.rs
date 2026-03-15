// LRU glyph cache — bounded, glyph-ID-keyed cache for pre-rasterized glyphs.
//
// Replaces the fixed 95-slot ASCII GlyphCache with a configurable-capacity
// LRU cache keyed by (glyph_id, font_size, axis_hash). Supports arbitrary
// glyph IDs (including > 127) and evicts the least-recently-used entry when
// full. The axis_hash component ensures that different variable font axis
// settings (e.g., wght=400 vs wght=700) are cached as separate entries.
//
// Implementation: entries live in a Vec with an intrusive doubly-linked list
// for LRU ordering (head = MRU, tail = LRU). A BTreeMap provides O(log n)
// key → index lookup. All operations are O(log n) in the number of cached
// entries — acceptable for typical cache sizes (64–512).

use alloc::{collections::BTreeMap, vec, vec::Vec};

/// Cache key: (glyph_id, font_size, axis_hash).
/// The axis_hash is 0 for default axis values (no variation).
type CacheKey = (u16, u16, u32);

/// Sentinel value meaning "no linked-list neighbor."
const NONE: usize = usize::MAX;

/// Pre-rasterized glyph data stored in the LRU cache.
///
/// Contains the same metrics as `CachedGlyph` plus an owned coverage buffer.
/// The coverage buffer holds 3-channel (RGB) subpixel data: `width * height * 3`
/// bytes, matching the format produced by the scanline rasterizer.
#[derive(Clone, Debug)]
pub struct LruCachedGlyph {
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: u32,
    /// 3-channel (RGB) subpixel coverage, row-major. Length = width * height * 3.
    pub coverage: Vec<u8>,
}

/// Internal slot in the cache's entry array.
struct Slot {
    key: CacheKey,
    glyph: LruCachedGlyph,
    /// Index of the more-recently-used entry (toward head), or NONE.
    prev: usize,
    /// Index of the less-recently-used entry (toward tail), or NONE.
    next: usize,
}

/// An LRU cache for pre-rasterized glyphs, keyed by `(glyph_id, font_size)`.
///
/// Bounded: `len()` never exceeds `max_capacity`. When full, inserting a new
/// entry evicts the least-recently-used one. Accessing an entry via `get()`
/// promotes it to most-recently-used.
pub struct LruGlyphCache {
    /// Maximum number of entries before eviction.
    max_capacity: usize,
    /// All cache entries (may contain gaps after eviction — but we compact by
    /// reusing evicted slots via `free_list`).
    entries: Vec<Slot>,
    /// Key → index into `entries`.
    index: BTreeMap<CacheKey, usize>,
    /// Indices of freed slots available for reuse.
    free_list: Vec<usize>,
    /// Index of the most-recently-used entry, or NONE.
    head: usize,
    /// Index of the least-recently-used entry, or NONE.
    tail: usize,
}

impl LruGlyphCache {
    /// Create a new LRU glyph cache with the given maximum entry count.
    ///
    /// `max_capacity` must be at least 1.
    pub fn new(max_capacity: usize) -> Self {
        let cap = if max_capacity == 0 { 1 } else { max_capacity };
        LruGlyphCache {
            max_capacity: cap,
            entries: Vec::with_capacity(cap),
            index: BTreeMap::new(),
            free_list: Vec::new(),
            head: NONE,
            tail: NONE,
        }
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Look up a cached glyph by `(glyph_id, font_size)` at default axis values.
    ///
    /// Returns a reference to the cached glyph data if present, and promotes
    /// the entry to most-recently-used. Returns `None` on cache miss.
    pub fn get(&mut self, glyph_id: u16, font_size: u16) -> Option<&LruCachedGlyph> {
        self.get_with_axes(glyph_id, font_size, 0)
    }

    /// Look up a cached glyph by `(glyph_id, font_size, axis_hash)`.
    ///
    /// The `axis_hash` distinguishes glyphs rasterized at different variable
    /// font axis positions. Use 0 for default axis values.
    pub fn get_with_axes(
        &mut self,
        glyph_id: u16,
        font_size: u16,
        axis_hash: u32,
    ) -> Option<&LruCachedGlyph> {
        let key = (glyph_id, font_size, axis_hash);
        let &idx = self.index.get(&key)?;
        self.move_to_head(idx);
        Some(&self.entries[idx].glyph)
    }

    /// Insert a glyph into the cache at default axis values.
    ///
    /// If an entry with the same `(glyph_id, font_size)` already exists, it is
    /// updated with the new data and promoted to most-recently-used. If the
    /// cache is at capacity and the key is new, the least-recently-used entry
    /// is evicted first.
    pub fn insert(&mut self, glyph_id: u16, font_size: u16, glyph: LruCachedGlyph) {
        self.insert_with_axes(glyph_id, font_size, 0, glyph);
    }

    /// Insert a glyph into the cache with axis value hash.
    ///
    /// The `axis_hash` distinguishes glyphs rasterized at different variable
    /// font axis positions. Use 0 for default axis values.
    pub fn insert_with_axes(
        &mut self,
        glyph_id: u16,
        font_size: u16,
        axis_hash: u32,
        glyph: LruCachedGlyph,
    ) {
        let key = (glyph_id, font_size, axis_hash);

        // Update existing entry.
        if let Some(&idx) = self.index.get(&key) {
            self.entries[idx].glyph = glyph;
            self.move_to_head(idx);
            return;
        }

        // Evict LRU if at capacity.
        if self.index.len() >= self.max_capacity {
            self.evict_tail();
        }

        // Allocate a slot (reuse freed slot or push new).
        let idx = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx] = Slot {
                key,
                glyph,
                prev: NONE,
                next: NONE,
            };
            free_idx
        } else {
            let idx = self.entries.len();
            self.entries.push(Slot {
                key,
                glyph,
                prev: NONE,
                next: NONE,
            });
            idx
        };

        self.index.insert(key, idx);
        self.push_head(idx);
    }

    // -----------------------------------------------------------------------
    // Internal linked-list operations
    // -----------------------------------------------------------------------

    /// Unlink an entry from its current position in the LRU list.
    fn unlink(&mut self, idx: usize) {
        let prev = self.entries[idx].prev;
        let next = self.entries[idx].next;

        if prev != NONE {
            self.entries[prev].next = next;
        } else {
            // This was the head.
            self.head = next;
        }

        if next != NONE {
            self.entries[next].prev = prev;
        } else {
            // This was the tail.
            self.tail = prev;
        }

        self.entries[idx].prev = NONE;
        self.entries[idx].next = NONE;
    }

    /// Push an entry to the head (most-recently-used) of the LRU list.
    /// The entry must not currently be in the list.
    fn push_head(&mut self, idx: usize) {
        self.entries[idx].prev = NONE;
        self.entries[idx].next = self.head;

        if self.head != NONE {
            self.entries[self.head].prev = idx;
        }

        self.head = idx;

        if self.tail == NONE {
            self.tail = idx;
        }
    }

    /// Move an existing entry to the head (most-recently-used).
    fn move_to_head(&mut self, idx: usize) {
        if self.head == idx {
            return; // Already at head.
        }
        self.unlink(idx);
        self.push_head(idx);
    }

    /// Evict the tail (least-recently-used) entry.
    fn evict_tail(&mut self) {
        if self.tail == NONE {
            return;
        }
        let idx = self.tail;
        let key = self.entries[idx].key;

        self.unlink(idx);
        self.index.remove(&key);

        // Clear the slot's coverage to free memory, then add to free list.
        self.entries[idx].glyph.coverage = vec![];
        self.free_list.push(idx);
    }
}
