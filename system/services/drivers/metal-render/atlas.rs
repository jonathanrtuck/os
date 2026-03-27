//! Glyph texture atlas with open-addressed hash table lookup.
//!
//! Keyed by `(glyph_id, font_size_px, style_id)` — supports arbitrary
//! font/size/style combinations unlike the previous flat-array design.

/// Atlas texture width in pixels.
pub(crate) const ATLAS_WIDTH: u32 = 512;
/// Atlas texture height in pixels.
pub(crate) const ATLAS_HEIGHT: u32 = 512;

/// Hash table capacity (must be a power of 2).
const CAPACITY: usize = 16384;
/// Sentinel value for empty slots.
const EMPTY: u64 = u64::MAX;

/// Atlas entry for a single rasterized glyph.
#[derive(Clone, Copy)]
pub(crate) struct AtlasEntry {
    pub(crate) u: u16,
    pub(crate) v: u16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) bearing_x: i16,
    pub(crate) bearing_y: i16,
}

/// A single slot in the open-addressed hash table.
#[derive(Clone, Copy)]
struct AtlasSlot {
    key: u64,
    entry: AtlasEntry,
}

/// Pack a `(glyph_id, font_size_px, style_id)` triple into a single `u64` key.
///
/// Layout: `glyph_id` in bits 0..15, `font_size_px` in bits 16..31,
/// `style_id` in bits 32..63.
fn pack_key(glyph_id: u16, font_size_px: u16, style_id: u32) -> u64 {
    glyph_id as u64 | ((font_size_px as u64) << 16) | ((style_id as u64) << 32)
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

/// Glyph texture atlas with open-addressed hash table and row-based packing.
///
/// Supports arbitrary font/size/style combinations via a `(glyph_id,
/// font_size_px, style_id)` key. The hash table uses linear probing with
/// FNV-1a hashing. Eviction is full-reset only (`reset()`).
pub(crate) struct GlyphAtlas {
    slots: [AtlasSlot; CAPACITY],
    pub(crate) pixels: [u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
    pub(crate) row_y: u16,
    pub(crate) row_x: u16,
    pub(crate) row_h: u16,
}

impl GlyphAtlas {
    /// Create a new empty atlas with all slots cleared.
    pub(crate) fn new() -> Self {
        let empty_slot = AtlasSlot {
            key: EMPTY,
            entry: AtlasEntry {
                u: 0,
                v: 0,
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
            },
        };
        GlyphAtlas {
            slots: [empty_slot; CAPACITY],
            pixels: [0u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
            row_y: 0,
            row_x: 0,
            row_h: 0,
        }
    }

    /// Look up a glyph entry by `(glyph_id, font_size_px, style_id)`.
    ///
    /// Returns `None` if the glyph is not in the atlas.
    pub(crate) fn lookup(
        &self,
        glyph_id: u16,
        font_size_px: u16,
        style_id: u32,
    ) -> Option<&AtlasEntry> {
        let key = pack_key(glyph_id, font_size_px, style_id);
        let mut idx = hash_key(key);
        let mut probes = 0usize;
        while probes < CAPACITY {
            let slot = &self.slots[idx];
            if slot.key == EMPTY {
                return None;
            }
            if slot.key == key {
                return Some(&slot.entry);
            }
            idx = (idx + 1) & (CAPACITY - 1);
            probes += 1;
        }
        None
    }

    /// Insert a pre-built `AtlasEntry` into the hash table without touching
    /// the pixel buffer or row packer. Returns `false` if the table is full.
    pub(crate) fn insert(
        &mut self,
        glyph_id: u16,
        font_size_px: u16,
        style_id: u32,
        entry: AtlasEntry,
    ) -> bool {
        let key = pack_key(glyph_id, font_size_px, style_id);
        let mut idx = hash_key(key);
        let mut probes = 0usize;
        while probes < CAPACITY {
            let slot = &self.slots[idx];
            if slot.key == EMPTY || slot.key == key {
                self.slots[idx] = AtlasSlot { key, entry };
                return true;
            }
            idx = (idx + 1) & (CAPACITY - 1);
            probes += 1;
        }
        false
    }

    /// Rasterize and pack a glyph into the atlas texture, then insert the
    /// resulting entry into the hash table.
    ///
    /// Returns `false` if the atlas texture is full or the hash table is full.
    pub(crate) fn pack(
        &mut self,
        glyph_id: u16,
        font_size_px: u16,
        style_id: u32,
        w: u16,
        h: u16,
        bearing_x: i16,
        bearing_y: i16,
        data: &[u8],
    ) -> bool {
        // Check if we need a new row.
        if self.row_x + w > ATLAS_WIDTH as u16 {
            self.row_y += self.row_h;
            self.row_x = 0;
            self.row_h = 0;
        }
        if self.row_y + h > ATLAS_HEIGHT as u16 {
            return false; // Atlas texture full.
        }

        let u = self.row_x;
        let v = self.row_y;

        // Copy glyph bitmap into atlas pixel buffer.
        let mut row = 0u16;
        while row < h {
            let src_start = row as usize * w as usize;
            let dst_start = (v + row) as usize * ATLAS_WIDTH as usize + u as usize;
            let src_end = src_start + w as usize;
            let dst_end = dst_start + w as usize;
            if src_end <= data.len() && dst_end <= self.pixels.len() {
                self.pixels[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
            }
            row += 1;
        }

        let entry = AtlasEntry {
            u,
            v,
            width: w,
            height: h,
            bearing_x,
            bearing_y,
        };

        self.row_x += w;
        if h > self.row_h {
            self.row_h = h;
        }

        self.insert(glyph_id, font_size_px, style_id, entry)
    }

    /// Clear all hash table slots and reset the row packer.
    pub(crate) fn reset(&mut self) {
        let mut i = 0;
        while i < CAPACITY {
            self.slots[i].key = EMPTY;
            i += 1;
        }
        self.row_y = 0;
        self.row_x = 0;
        self.row_h = 0;
        // Note: pixel buffer is not cleared — stale data is harmless since
        // all lookups go through the hash table.
    }

    // ── COMPAT: remove in Task 7 when callers updated ─────────────────

    /// Compatibility wrapper: delegates to new API with `font_size_px=0,
    /// style_id=font_id as u32`.
    pub(crate) fn lookup_compat(&self, glyph_id: u16, font_id: u16) -> Option<&AtlasEntry> {
        // COMPAT: remove in Task 7 when callers updated
        self.lookup(glyph_id, 0, font_id as u32)
    }

    /// Compatibility wrapper: delegates to new API with `font_size_px=0,
    /// style_id=font_id as u32`.
    pub(crate) fn pack_compat(
        &mut self,
        glyph_id: u16,
        font_id: u16,
        w: u16,
        h: u16,
        bearing_x: i16,
        bearing_y: i16,
        data: &[u8],
    ) -> bool {
        // COMPAT: remove in Task 7 when callers updated
        self.pack(glyph_id, 0, font_id as u32, w, h, bearing_x, bearing_y, data)
    }
}
