//! Glyph texture atlas for GPU-accelerated text rendering.
//!
//! Packs pre-rasterized glyph coverage bitmaps into a single R8_UNORM
//! GPU texture. Row-based packing: glyphs are placed left-to-right,
//! starting a new row when the current row is full.
//!
//! Pre-populated from the fonts library's `GlyphCache` at startup.
//! On lookup miss, returns None (glyph not rendered).

/// Atlas texture dimensions (pixels).
pub const ATLAS_WIDTH: u32 = 512;
pub const ATLAS_HEIGHT: u32 = 512;

/// Atlas size in bytes (R8_UNORM = 1 byte per pixel).
pub const ATLAS_BYTES: usize = (ATLAS_WIDTH * ATLAS_HEIGHT) as usize;

/// Maximum glyph ID supported by the atlas lookup table.
const MAX_GLYPH_ID: usize = 2048;

/// Atlas entry for a single rasterized glyph.
#[derive(Clone, Copy)]
pub struct AtlasEntry {
    /// X position in atlas texture (pixels).
    pub u: u16,
    /// Y position in atlas texture (pixels).
    pub v: u16,
    /// Glyph bitmap width (pixels).
    pub width: u16,
    /// Glyph bitmap height (pixels).
    pub height: u16,
    /// Horizontal bearing from glyph origin to left edge of bitmap.
    pub bearing_x: i16,
    /// Vertical bearing from baseline to top edge of bitmap.
    pub bearing_y: i16,
}

/// Glyph texture atlas with row-based packing.
///
/// ~16 KiB for the entries table. The pixel data lives in DMA backing
/// memory pointed to by `dma_va` — this struct does NOT hold the pixel
/// buffer.
pub struct GlyphAtlas {
    /// Lookup table: glyph_id → AtlasEntry. `width == 0` means empty.
    entries: [AtlasEntry; MAX_GLYPH_ID],
    /// DMA backing memory VA (atlas pixel data is written here directly).
    dma_va: usize,
    /// Current row Y position.
    row_y: u16,
    /// Current row X cursor.
    row_x: u16,
    /// Current row height (tallest glyph in this row).
    row_h: u16,
}

impl GlyphAtlas {
    /// Set the DMA backing VA. Used after `alloc_zeroed` construction
    /// (all other fields are valid when zero-initialized).
    pub fn set_dma_va(&mut self, va: usize) {
        self.dma_va = va;
    }

    /// Look up a glyph in the atlas by font glyph ID.
    /// Returns None if the glyph is not cached.
    pub fn lookup(&self, glyph_id: u16) -> Option<&AtlasEntry> {
        let id = glyph_id as usize;
        if id >= MAX_GLYPH_ID {
            return None;
        }
        let entry = &self.entries[id];
        if entry.width == 0 {
            return None;
        }
        Some(entry)
    }

    /// Pack a glyph into the atlas and write its coverage into DMA backing.
    /// `coverage` is grayscale coverage (1 byte/pixel, row-major, w×h bytes).
    /// Returns the entry if successful, None if atlas is full.
    pub fn pack_glyph(
        &mut self,
        glyph_id: u16,
        width: u32,
        height: u32,
        bearing_x: i32,
        bearing_y: i32,
        coverage: &[u8],
    ) -> Option<AtlasEntry> {
        let id = glyph_id as usize;
        if id >= MAX_GLYPH_ID || width == 0 || height == 0 {
            return None;
        }
        // Already cached?
        if self.entries[id].width > 0 {
            return Some(self.entries[id]);
        }

        let w = width as u16;
        let h = height as u16;

        // Wrap to next row if current glyph doesn't fit horizontally.
        if self.row_x + w > ATLAS_WIDTH as u16 {
            self.row_y += self.row_h + 1;
            self.row_x = 0;
            self.row_h = 0;
        }

        // Check vertical overflow.
        if self.row_y + h > ATLAS_HEIGHT as u16 {
            return None;
        }

        let entry = AtlasEntry {
            u: self.row_x,
            v: self.row_y,
            width: w,
            height: h,
            bearing_x: bearing_x as i16,
            bearing_y: bearing_y as i16,
        };

        // Copy coverage data into DMA backing at the correct atlas position.
        let atlas_stride = ATLAS_WIDTH as usize;
        for row in 0..height as usize {
            let src_start = row * width as usize;
            let src_end = src_start + width as usize;
            if src_end > coverage.len() {
                break;
            }
            let dst_y = entry.v as usize + row;
            let dst_x = entry.u as usize;
            let dst_offset = dst_y * atlas_stride + dst_x;

            // SAFETY: dma_va points to valid DMA memory of ATLAS_BYTES size,
            // allocated and zeroed by the caller. dst_offset + width is within
            // bounds because we checked atlas width/height overflow above.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    coverage[src_start..src_end].as_ptr(),
                    (self.dma_va + dst_offset) as *mut u8,
                    width as usize,
                );
            }
        }

        // Advance packing cursor.
        self.row_x += w + 1; // +1 pixel gap between glyphs
        if h > self.row_h {
            self.row_h = h;
        }

        self.entries[id] = entry;
        Some(entry)
    }

    /// Populate the atlas from a fonts::cache::GlyphCache.
    /// Iterates over all cached glyph IDs and packs their coverage data.
    pub fn populate_from_cache(&mut self, cache: &fonts::cache::GlyphCache) {
        for glyph_id in 0..MAX_GLYPH_ID as u16 {
            if let Some((glyph, coverage)) = cache.get(glyph_id) {
                if glyph.width > 0 && glyph.height > 0 {
                    self.pack_glyph(
                        glyph_id,
                        glyph.width,
                        glyph.height,
                        glyph.bearing_x,
                        glyph.bearing_y,
                        coverage,
                    );
                }
            }
        }
    }
}
