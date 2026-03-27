//! Glyph texture atlas with row-based packing.

use crate::{ATLAS_HEIGHT, ATLAS_WIDTH};

/// Maximum glyph ID per font in the atlas lookup table.
pub(crate) const GLYPH_STRIDE: usize = 2048;
/// Number of font slots in the atlas (0 = mono, 1 = sans).
pub(crate) const MAX_FONTS: usize = 2;
/// Total atlas entry capacity (GLYPH_STRIDE * MAX_FONTS).
const MAX_GLYPH_ENTRIES: usize = GLYPH_STRIDE * MAX_FONTS;

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

/// Glyph texture atlas with row-based packing.
/// Supports multiple fonts via `font_id` offset into the entries array:
/// font 0 (mono) uses entries[0..GLYPH_STRIDE), font 1 (sans) uses
/// entries[GLYPH_STRIDE..2*GLYPH_STRIDE), etc.
pub(crate) struct GlyphAtlas {
    entries: [AtlasEntry; MAX_GLYPH_ENTRIES],
    pub(crate) pixels: [u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
    pub(crate) row_y: u16,
    pub(crate) row_x: u16,
    pub(crate) row_h: u16,
}

impl GlyphAtlas {
    /// Flat index for a (glyph_id, font_id) pair.
    fn effective_id(glyph_id: u16, font_id: u16) -> usize {
        font_id as usize * GLYPH_STRIDE + glyph_id as usize
    }

    pub(crate) fn lookup(&self, glyph_id: u16, font_id: u16) -> Option<&AtlasEntry> {
        let id = Self::effective_id(glyph_id, font_id);
        if id < MAX_GLYPH_ENTRIES && self.entries[id].width > 0 {
            Some(&self.entries[id])
        } else {
            None
        }
    }

    pub(crate) fn pack(
        &mut self,
        glyph_id: u16,
        font_id: u16,
        w: u16,
        h: u16,
        bearing_x: i16,
        bearing_y: i16,
        data: &[u8],
    ) -> bool {
        let id = Self::effective_id(glyph_id, font_id);
        if id >= MAX_GLYPH_ENTRIES {
            return false;
        }
        // Check if we need a new row.
        if self.row_x + w > ATLAS_WIDTH as u16 {
            self.row_y += self.row_h;
            self.row_x = 0;
            self.row_h = 0;
        }
        if self.row_y + h > ATLAS_HEIGHT as u16 {
            return false; // Atlas full.
        }

        let u = self.row_x;
        let v = self.row_y;

        // Copy glyph bitmap into atlas pixel buffer.
        for row in 0..h as usize {
            let src_start = row * w as usize;
            let dst_start = (v as usize + row) * ATLAS_WIDTH as usize + u as usize;
            let src_end = src_start + w as usize;
            let dst_end = dst_start + w as usize;
            if src_end <= data.len() && dst_end <= self.pixels.len() {
                self.pixels[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
            }
        }

        self.entries[id] = AtlasEntry {
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
        true
    }
}
