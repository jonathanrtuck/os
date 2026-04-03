//! Clip mask cache: rasterized 8bpp alpha masks for path clipping.
//!
//! Clips are rasterized once and cached. The cache key is a u64 hash of the
//! clip path DataRef, node bounds, and transform. 16 slots, LRU eviction when
//! full.

use alloc::vec::Vec;

use scene::FillRule;

use crate::scene_render::path_raster::rasterize_path_to_coverage;

// ── CachedMask ───────────────────────────────────────────────────────────────

/// One cached clip mask: 8bpp alpha buffer at a specific size.
struct CachedMask {
    /// Cache lookup key (hash of path data, dimensions, and fill rule).
    key: u64,
    /// Coverage buffer: `width * height` bytes, one per pixel.
    data: Vec<u8>,
    width: u32,
    height: u32,
    /// Monotonic generation counter set when this slot was last accessed.
    last_used: u32,
}

// ── ClipMaskCache ────────────────────────────────────────────────────────────

/// LRU cache of rasterized clip masks. Fixed 16-slot capacity.
///
/// On cache miss the path is rasterized via `rasterize_path_to_coverage` and
/// stored in the least-recently-used slot. On cache hit `last_used` is
/// refreshed and a slice of the coverage buffer is returned.
pub struct ClipMaskCache {
    masks: [Option<CachedMask>; 16],
    /// Monotonically increasing counter; incremented on every access.
    generation: u32,
}

impl ClipMaskCache {
    /// Create an empty cache. All 16 slots are vacant.
    pub fn new() -> Self {
        ClipMaskCache {
            masks: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
            generation: 0,
        }
    }

    /// Return a reference to the 8bpp coverage buffer for the given path,
    /// rasterizing it on first use.
    ///
    /// Returns `None` if rasterization fails (empty path, zero dimensions,
    /// oversized buffer).
    pub fn get_or_rasterize(
        &mut self,
        path_data: &[u8],
        width: u32,
        height: u32,
        fill_rule: FillRule,
        cache_key: u64,
    ) -> Option<&[u8]> {
        self.generation = self.generation.wrapping_add(1);
        let current_gen = self.generation;

        // Look for a matching slot by index (avoids holding a borrow across
        // the mutable store below).
        let hit_idx = self.find_hit(cache_key, width, height);

        if let Some(idx) = hit_idx {
            // Cache hit: refresh last_used and return the coverage slice.
            if let Some(ref mut m) = self.masks[idx] {
                m.last_used = current_gen;
            }
            return self.masks[idx].as_ref().map(|m| m.data.as_slice());
        }

        // Cache miss: rasterize.
        let coverage = rasterize_path_to_coverage(path_data, width, height, fill_rule);
        if coverage.is_empty() {
            return None;
        }

        // Find an empty slot or the LRU slot.
        let target_idx = self.find_eviction_slot();
        self.masks[target_idx] = Some(CachedMask {
            key: cache_key,
            data: coverage,
            width,
            height,
            last_used: current_gen,
        });

        self.masks[target_idx].as_ref().map(|m| m.data.as_slice())
    }

    /// Return the index of the slot matching the given key and dimensions,
    /// or `None` if not found.
    fn find_hit(&self, cache_key: u64, width: u32, height: u32) -> Option<usize> {
        for (i, slot) in self.masks.iter().enumerate() {
            if let Some(ref m) = slot {
                if m.key == cache_key && m.width == width && m.height == height {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Return the index of the slot to evict: prefer an empty slot, otherwise
    /// the slot with the smallest `last_used` counter.
    fn find_eviction_slot(&self) -> usize {
        // Prefer any empty slot.
        for (i, slot) in self.masks.iter().enumerate() {
            if slot.is_none() {
                return i;
            }
        }

        // All full: evict LRU.
        let mut lru_idx = 0;
        let mut lru_gen = u32::MAX;
        for (i, slot) in self.masks.iter().enumerate() {
            if let Some(ref m) = slot {
                if m.last_used < lru_gen {
                    lru_gen = m.last_used;
                    lru_idx = i;
                }
            }
        }
        lru_idx
    }
}

impl Default for ClipMaskCache {
    fn default() -> Self {
        Self::new()
    }
}
