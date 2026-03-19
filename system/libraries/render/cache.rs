//! Per-node render cache for incremental rendering.
//!
//! Stores rendered bitmaps keyed by (node_id, content_hash). Each node ID
//! indexes directly into a fixed-size `Vec<CacheEntry>` of length `MAX_NODES`
//! for O(1) lookup. Entries are invalidated when the stored `content_hash`
//! differs from the current hash (content changed).
//!
//! Nodes with `Content::None` (pure containers) should NOT be cached ---
//! `fill_rect` is cheaper than a bitmap blit. Only cache `Glyphs`, `Image`,
//! `Path`. This is enforced by convention in the caller (render walk), not
//! by this module.

use alloc::vec::Vec;

use scene::MAX_NODES;

// ── CacheEntry ──────────────────────────────────────────────────────

/// A single cached rendered bitmap for one node.
struct CacheEntry {
    /// `content_hash` at the time of caching. If the node's current hash
    /// differs, this entry is stale.
    content_hash: u32,
    /// Cached rendered output (BGRA pixel data).
    data: Vec<u8>,
    /// Width of the cached bitmap in pixels.
    width: u32,
    /// Height of the cached bitmap in pixels.
    height: u32,
    /// Whether this entry contains valid data.
    valid: bool,
}

impl CacheEntry {
    const fn empty() -> Self {
        Self {
            content_hash: 0,
            data: Vec::new(),
            width: 0,
            height: 0,
            valid: false,
        }
    }
}

// ── NodeCache ───────────────────────────────────────────────────────

/// Per-node render cache. Stores rendered bitmaps keyed by
/// `(node_id, content_hash)`. Invalidated when `content_hash` changes.
/// Cleared on compaction (full rebuild).
///
/// Indexed by `node_id` (0..`MAX_NODES`). Node IDs beyond `MAX_NODES`
/// are silently ignored (no-op on store, miss on get).
pub struct NodeCache {
    entries: Vec<CacheEntry>,
}

impl NodeCache {
    /// Create a new cache with all entries invalid.
    ///
    /// Allocates a `Vec` of `MAX_NODES` empty entries. No pixel data is
    /// allocated until the first `store()` call for each slot.
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(MAX_NODES);
        for _ in 0..MAX_NODES {
            entries.push(CacheEntry::empty());
        }
        Self { entries }
    }

    /// Look up a cached bitmap for the given node.
    ///
    /// Returns `Some((width, height, &[u8]))` if the cache is valid AND
    /// the stored `content_hash` matches the provided hash. Returns `None`
    /// on cache miss (invalid entry, hash mismatch, or out-of-bounds ID).
    pub fn get(&self, node_id: u16, content_hash: u32) -> Option<(u32, u32, &[u8])> {
        let idx = node_id as usize;
        if idx >= self.entries.len() {
            return None;
        }
        let entry = &self.entries[idx];
        if entry.valid && entry.content_hash == content_hash {
            Some((entry.width, entry.height, &entry.data))
        } else {
            None
        }
    }

    /// Store a rendered bitmap for a node.
    ///
    /// Overwrites any previous entry for this `node_id`. If the new data
    /// is the same size as the existing allocation, the `Vec` is reused
    /// (no reallocation). Out-of-bounds `node_id` is a no-op.
    pub fn store(&mut self, node_id: u16, content_hash: u32, width: u32, height: u32, data: &[u8]) {
        let idx = node_id as usize;
        if idx >= self.entries.len() {
            return;
        }
        let entry = &mut self.entries[idx];
        entry.content_hash = content_hash;
        entry.width = width;
        entry.height = height;
        entry.valid = true;

        // Reuse existing allocation when size matches.
        if entry.data.len() == data.len() {
            entry.data.copy_from_slice(data);
        } else {
            entry.data = Vec::from(data);
        }
    }

    /// Invalidate a single entry. Out-of-bounds `node_id` is a no-op.
    pub fn evict(&mut self, node_id: u16) {
        let idx = node_id as usize;
        if idx >= self.entries.len() {
            return;
        }
        self.entries[idx].valid = false;
    }

    /// Invalidate all entries (e.g., on compaction or full rebuild).
    pub fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            entry.valid = false;
        }
    }

    /// Number of valid entries currently in the cache.
    pub fn valid_count(&self) -> usize {
        self.entries.iter().filter(|e| e.valid).count()
    }

    /// Total bytes used by cached bitmaps (only counts valid entries).
    pub fn total_bytes(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.valid)
            .map(|e| e.data.len())
            .sum()
    }
}
