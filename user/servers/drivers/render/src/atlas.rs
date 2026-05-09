//! Glyph texture atlas with open-addressed hash table lookup.
//!
//! Keyed by `(glyph_id, font_size_px, style_id)`. Row-based bin packing
//! into a 2048x2048 R8 texture.

extern crate alloc;

use alloc::boxed::Box;

pub const ATLAS_WIDTH: u32 = 2048;
pub const ATLAS_HEIGHT: u32 = 2048;

const CAPACITY: usize = 16384;
const EMPTY: u64 = u64::MAX;

#[derive(Clone, Copy)]
pub struct AtlasEntry {
    pub u: u16,
    pub v: u16,
    pub width: u16,
    pub height: u16,
    pub bearing_x: i16,
    pub bearing_y: i16,
}

#[derive(Clone, Copy)]
struct AtlasSlot {
    key: u64,
    entry: AtlasEntry,
}

fn pack_key(glyph_id: u16, font_size_px: u16, style_id: u32) -> u64 {
    glyph_id as u64 | ((font_size_px as u64) << 16) | ((style_id as u64) << 32)
}

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

pub struct GlyphAtlas {
    slots: [AtlasSlot; CAPACITY],
    pub pixels: [u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
    pub row_y: u16,
    pub row_x: u16,
    pub row_h: u16,
}

impl GlyphAtlas {
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: Layout is valid. alloc_zeroed returns aligned, zeroed memory.
        // reset() writes EMPTY sentinels into the hash table.
        unsafe {
            let layout = alloc::alloc::Layout::new::<Self>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut Self;

            if ptr.is_null() {
                alloc::alloc::handle_alloc_error(layout);
            }

            let mut b = Box::from_raw(ptr);

            b.reset();

            b
        }
    }

    pub fn lookup(&self, glyph_id: u16, font_size_px: u16, style_id: u32) -> Option<&AtlasEntry> {
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

    pub fn insert(
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

    #[allow(clippy::too_many_arguments)]
    pub fn pack(
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
        if self.row_x + w > ATLAS_WIDTH as u16 {
            self.row_y += self.row_h;
            self.row_x = 0;
            self.row_h = 0;
        }
        if self.row_y + h > ATLAS_HEIGHT as u16 {
            return false;
        }

        let u = self.row_x;
        let v = self.row_y;
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

    pub fn reset(&mut self) {
        let mut i = 0;

        while i < CAPACITY {
            self.slots[i].key = EMPTY;
            i += 1;
        }

        self.row_y = 0;
        self.row_x = 0;
        self.row_h = 0;
    }
}
