//! Glyph texture atlas with open-addressed hash table lookup.
//!
//! Keyed by `(glyph_id, font_size_px, style_id)`. Shelf-based bin packing
//! into a 2048x2048 R8 texture. LRU eviction when full, partial dirty
//! rect tracking for minimal upload bandwidth.

extern crate alloc;

use alloc::boxed::Box;

pub const ATLAS_WIDTH: u32 = 2048;
pub const ATLAS_HEIGHT: u32 = 2048;

const CAPACITY: usize = 16384;
const EMPTY: u64 = u64::MAX;
const MAX_SHELVES: usize = 128;

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

#[derive(Clone, Copy)]
struct ShelfInfo {
    y: u16,
    height: u16,
    last_used_frame: u32,
}

#[derive(Clone, Copy)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
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
    shelves: [ShelfInfo; MAX_SHELVES],
    shelf_count: usize,
    frame_counter: u32,
    pub dirty: Option<DirtyRect>,
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

    pub fn begin_frame(&mut self) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
    }

    pub fn lookup(
        &mut self,
        glyph_id: u16,
        font_size_px: u16,
        style_id: u32,
    ) -> Option<AtlasEntry> {
        let key = pack_key(glyph_id, font_size_px, style_id);
        let mut idx = hash_key(key);
        let mut probes = 0usize;

        while probes < CAPACITY {
            if self.slots[idx].key == EMPTY {
                return None;
            }
            if self.slots[idx].key == key {
                let entry = self.slots[idx].entry;

                self.touch_shelf(entry.v);

                return Some(entry);
            }

            idx = (idx + 1) & (CAPACITY - 1);
            probes += 1;
        }

        None
    }

    fn touch_shelf(&mut self, glyph_v: u16) {
        let mut i = 0;

        while i < self.shelf_count {
            let s = &self.shelves[i];

            if glyph_v >= s.y && glyph_v < s.y + s.height {
                self.shelves[i].last_used_frame = self.frame_counter;

                return;
            }

            i += 1;
        }
    }

    pub fn insert_zero(
        &mut self,
        glyph_id: u16,
        font_size_px: u16,
        style_id: u32,
        entry: AtlasEntry,
    ) {
        let key = pack_key(glyph_id, font_size_px, style_id);

        self.insert(key, entry);
    }

    fn insert(&mut self, key: u64, entry: AtlasEntry) -> bool {
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

    fn record_shelf(&mut self) {
        if self.shelf_count < MAX_SHELVES {
            self.shelves[self.shelf_count] = ShelfInfo {
                y: self.row_y,
                height: 0,
                last_used_frame: self.frame_counter,
            };
            self.shelf_count += 1;
        }
    }

    fn update_shelf_height(&mut self, h: u16) {
        if self.shelf_count > 0 && h > self.shelves[self.shelf_count - 1].height {
            self.shelves[self.shelf_count - 1].height = h;
        }
    }

    fn expand_dirty(&mut self, x: u16, y: u16, w: u16, h: u16) {
        match self.dirty {
            None => {
                self.dirty = Some(DirtyRect { x, y, w, h });
            }
            Some(ref mut r) => {
                let x2 = (r.x + r.w).max(x + w);
                let y2 = (r.y + r.h).max(y + h);

                r.x = r.x.min(x);
                r.y = r.y.min(y);
                r.w = x2 - r.x;
                r.h = y2 - r.y;
            }
        }
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
            if self.shelf_count > 0 {
                self.shelves[self.shelf_count - 1].height = self.row_h;
            }

            self.row_y += self.row_h;
            self.row_x = 0;
            self.row_h = 0;

            self.record_shelf();
        }

        if self.row_y + h > ATLAS_HEIGHT as u16 && !self.evict_shelf(h) {
            return false;
        }

        self.blit_and_insert(
            glyph_id,
            font_size_px,
            style_id,
            w,
            h,
            bearing_x,
            bearing_y,
            data,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_and_insert(
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

        self.update_shelf_height(self.row_h);
        self.expand_dirty(u, v, w, h);

        let key = pack_key(glyph_id, font_size_px, style_id);

        self.insert(key, entry)
    }

    fn evict_shelf(&mut self, min_height: u16) -> bool {
        if self.shelf_count == 0 {
            return false;
        }

        let mut lru_idx = 0usize;
        let mut lru_frame = self.shelves[0].last_used_frame;
        let mut i = 1;

        while i < self.shelf_count {
            if self.shelves[i].last_used_frame < lru_frame && self.shelves[i].height >= min_height {
                lru_frame = self.shelves[i].last_used_frame;
                lru_idx = i;
            }

            i += 1;
        }

        if self.shelves[lru_idx].height < min_height {
            return false;
        }

        let evict_y = self.shelves[lru_idx].y;
        let evict_h = self.shelves[lru_idx].height;
        let pixel_start = evict_y as usize * ATLAS_WIDTH as usize;
        let pixel_end = (evict_y + evict_h) as usize * ATLAS_WIDTH as usize;

        if pixel_end <= self.pixels.len() {
            let mut p = pixel_start;

            while p < pixel_end {
                self.pixels[p] = 0;

                p += 1;
            }
        }

        self.rebuild_table_without(evict_y, evict_h);

        if self.shelf_count > 0 {
            self.shelves[self.shelf_count - 1].height = self.row_h;
        }

        self.row_y = evict_y;
        self.row_x = 0;
        self.row_h = 0;
        self.shelves[lru_idx] = ShelfInfo {
            y: evict_y,
            height: 0,
            last_used_frame: self.frame_counter,
        };

        self.expand_dirty(0, evict_y, ATLAS_WIDTH as u16, evict_h);

        true
    }

    fn rebuild_table_without(&mut self, evict_y: u16, evict_h: u16) {
        let evict_end = evict_y + evict_h;
        let mut kept_count = 0usize;
        let mut kept = [(
            0u64,
            AtlasEntry {
                u: 0,
                v: 0,
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
            },
        ); CAPACITY];
        let mut i = 0;

        while i < CAPACITY {
            let slot = &self.slots[i];

            if slot.key != EMPTY {
                let gy = slot.entry.v;

                if gy < evict_y || gy >= evict_end {
                    kept[kept_count] = (slot.key, slot.entry);
                    kept_count += 1;
                }
            }

            i += 1;
        }

        i = 0;

        while i < CAPACITY {
            self.slots[i].key = EMPTY;

            i += 1;
        }

        i = 0;

        while i < kept_count {
            self.insert(kept[i].0, kept[i].1);

            i += 1;
        }
    }

    pub fn reset(&mut self) {
        let mut i = 0;

        while i < CAPACITY {
            self.slots[i].key = EMPTY;

            i += 1;
        }

        let mut p = 0;

        while p < self.pixels.len() {
            self.pixels[p] = 0;

            p += 1;
        }

        self.row_y = 0;
        self.row_x = 0;
        self.row_h = 0;
        self.shelf_count = 0;
        self.frame_counter = 0;
        self.dirty = None;

        self.record_shelf();
    }
}
