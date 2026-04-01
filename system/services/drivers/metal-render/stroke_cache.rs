//! Stroke expansion cache: memoizes `expand_stroke` results by content hash.
//!
//! Keyed by `(content_hash, stroke_width_fixed)` packed into a `u64`.
//! Open-addressed hash table with linear probing (same pattern as
//! `GlyphAtlas`). Entries store the expanded stroke bytes in a `Vec<u8>`
//! that retains capacity across re-expansions.
//!
//! In steady state (unchanged scene), every lookup is a cache hit —
//! zero allocations, zero CPU stroke expansion work per frame.

use alloc::vec::Vec;

/// Hash table capacity (must be a power of 2).
const CAPACITY: usize = 64;
/// Sentinel value for empty slots.
const EMPTY: u64 = u64::MAX;

/// Pack `(content_hash, stroke_width_fixed)` into a single `u64` key.
/// `stroke_width_fixed` is the 8.8 fixed-point u16 from the scene node.
fn pack_key(content_hash: u32, stroke_width: u16) -> u64 {
    content_hash as u64 | ((stroke_width as u64) << 32)
}

/// FNV-1a hash of a `u64` key, masked to the table size.
fn hash_key(key: u64) -> usize {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;

    let bytes = key.to_le_bytes();
    let mut h = FNV_OFFSET;
    let mut i = 0;
    while i < 8 {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    (h as usize) & (CAPACITY - 1)
}

/// A single slot in the hash table.
struct StrokeCacheSlot {
    key: u64,
    data: Vec<u8>,
}

/// Stroke expansion cache.
///
/// Stores expanded stroke bytes keyed by `(content_hash, stroke_width)`.
/// Lookup is O(1) average. On miss, expands the stroke and stores the
/// result. The stored `Vec<u8>` retains capacity across overwrites,
/// so even replacement entries avoid allocation when the new expansion
/// fits within the previous capacity.
pub(crate) struct StrokeCache {
    slots: [StrokeCacheSlot; CAPACITY],
}

impl StrokeCache {
    /// Create a new empty cache.
    pub(crate) fn new() -> Self {
        // Initialize all slots with EMPTY keys and empty Vecs.
        // Using array::from_fn to avoid Copy requirement on Vec.
        let slots = core::array::from_fn(|_| StrokeCacheSlot {
            key: EMPTY,
            data: Vec::new(),
        });
        StrokeCache { slots }
    }

    /// Look up cached expanded stroke data, or expand and cache on miss.
    ///
    /// `content_hash` and `stroke_width` form the cache key.
    /// `path_data` is the raw path command bytes (only read on miss).
    /// `stroke_width_pt` is the float stroke width in points (only used on miss).
    ///
    /// Returns a slice of the expanded stroke bytes (may be empty if the
    /// path is empty or stroke_width is zero).
    pub(crate) fn get_or_expand(
        &mut self,
        content_hash: u32,
        stroke_width: u16,
        path_data: &[u8],
        stroke_width_pt: f32,
    ) -> &[u8] {
        let key = pack_key(content_hash, stroke_width);
        let mut idx = hash_key(key);
        let mut probes = 0usize;

        // Probe for existing entry.
        while probes < CAPACITY {
            if self.slots[idx].key == key {
                return &self.slots[idx].data;
            }
            if self.slots[idx].key == EMPTY {
                break;
            }
            idx = (idx + 1) & (CAPACITY - 1);
            probes += 1;
        }

        // Cache miss — expand and store.
        // If we ran out of probes (table full), overwrite the last probed slot.
        // This is a simple eviction policy: the displaced entry will be
        // re-expanded on its next access. At 64 slots this is unlikely.
        let slot = &mut self.slots[idx];
        slot.key = key;
        scene::stroke::expand_stroke_into(path_data, stroke_width_pt, &mut slot.data);
        &self.slots[idx].data
    }
}
