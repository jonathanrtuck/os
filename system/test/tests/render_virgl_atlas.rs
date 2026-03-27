//! Host-side tests for glyph atlas packing logic.
//!
//! The atlas module's `pack_glyph` does DMA writes via raw pointer, which
//! cannot be tested directly on the host. We copy the pure packing logic
//! (row cursor, overflow detection, UV calculation) into a test-friendly
//! struct that writes to a Vec instead.

// ── Atlas constants (from virgil-render/atlas.rs) ───────────────────

const ATLAS_WIDTH: u32 = 512;
const ATLAS_HEIGHT: u32 = 512;
const MAX_GLYPH_ID: usize = 2048;

// ── Test-friendly atlas entry ───────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
struct AtlasEntry {
    u: u16,
    v: u16,
    width: u16,
    height: u16,
    bearing_x: i16,
    bearing_y: i16,
}

// ── Test-friendly atlas (writes to Vec, not DMA) ────────────────────

struct TestGlyphAtlas {
    entries: Vec<AtlasEntry>,
    /// Pixel data buffer (substitute for DMA memory).
    pixels: Vec<u8>,
    row_y: u16,
    row_x: u16,
    row_h: u16,
}

impl TestGlyphAtlas {
    fn new() -> Self {
        Self {
            entries: vec![
                AtlasEntry {
                    u: 0,
                    v: 0,
                    width: 0,
                    height: 0,
                    bearing_x: 0,
                    bearing_y: 0,
                };
                MAX_GLYPH_ID
            ],
            pixels: vec![0u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
            row_y: 0,
            row_x: 0,
            row_h: 0,
        }
    }

    fn lookup(&self, glyph_id: u16) -> Option<&AtlasEntry> {
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

    /// Pack a glyph, copying the packing logic from atlas.rs but writing
    /// to self.pixels instead of DMA memory.
    fn pack_glyph(
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

        // Copy coverage data into pixel buffer.
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
            self.pixels[dst_offset..dst_offset + width as usize]
                .copy_from_slice(&coverage[src_start..src_end]);
        }

        // Advance packing cursor.
        self.row_x += w + 1; // +1 pixel gap between glyphs
        if h > self.row_h {
            self.row_h = h;
        }

        self.entries[id] = entry;
        Some(entry)
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests: pack_glyph basics
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pack_first_glyph_at_origin() {
    let mut atlas = TestGlyphAtlas::new();
    let coverage = vec![255u8; 10 * 12]; // 10x12 glyph
    let entry = atlas.pack_glyph(65, 10, 12, 1, 10, &coverage).unwrap();

    assert_eq!(entry.u, 0, "first glyph should be at u=0");
    assert_eq!(entry.v, 0, "first glyph should be at v=0");
    assert_eq!(entry.width, 10);
    assert_eq!(entry.height, 12);
    assert_eq!(entry.bearing_x, 1);
    assert_eq!(entry.bearing_y, 10);
}

#[test]
fn pack_second_glyph_adjacent() {
    let mut atlas = TestGlyphAtlas::new();
    let cov1 = vec![128u8; 10 * 12];
    let cov2 = vec![200u8; 8 * 14];

    let e1 = atlas.pack_glyph(65, 10, 12, 0, 0, &cov1).unwrap();
    let e2 = atlas.pack_glyph(66, 8, 14, 0, 0, &cov2).unwrap();

    // Second glyph should be placed after first + 1 pixel gap.
    assert_eq!(e2.u, e1.width + 1);
    assert_eq!(e2.v, 0, "still on first row");
}

#[test]
fn pack_row_wrap() {
    let mut atlas = TestGlyphAtlas::new();

    // Place a glyph nearly at the right edge.
    let wide = vec![0u8; 500 * 10]; // 500 pixels wide
    let e1 = atlas.pack_glyph(1, 500, 10, 0, 0, &wide).unwrap();
    assert_eq!(e1.u, 0);
    assert_eq!(e1.v, 0);

    // Next glyph (20px wide) won't fit on this row (500 + 1 + 20 > 512).
    let small = vec![0u8; 20 * 8];
    let e2 = atlas.pack_glyph(2, 20, 8, 0, 0, &small).unwrap();
    assert_eq!(e2.u, 0, "should wrap to start of new row");
    assert_eq!(e2.v, 10 + 1, "new row = prev row_y + row_h + 1 gap");
}

#[test]
fn pack_row_height_is_max_glyph_height() {
    let mut atlas = TestGlyphAtlas::new();

    // Pack two glyphs of different heights on the same row.
    let tall = vec![0u8; 10 * 20]; // 10x20
    let short = vec![0u8; 10 * 8]; // 10x8

    atlas.pack_glyph(1, 10, 20, 0, 0, &tall).unwrap();
    atlas.pack_glyph(2, 10, 8, 0, 0, &short).unwrap();

    // Force a row wrap.
    let wide = vec![0u8; 500 * 5];
    let e3 = atlas.pack_glyph(3, 500, 5, 0, 0, &wide).unwrap();

    // New row should start at y = 20 + 1 (height of tallest glyph + gap).
    assert_eq!(e3.v, 21);
}

#[test]
fn pack_duplicate_returns_cached() {
    let mut atlas = TestGlyphAtlas::new();
    let cov = vec![100u8; 10 * 12];

    let e1 = atlas.pack_glyph(65, 10, 12, 1, 10, &cov).unwrap();
    let e2 = atlas.pack_glyph(65, 10, 12, 1, 10, &cov).unwrap();

    assert_eq!(e1, e2, "duplicate pack should return cached entry");
}

#[test]
fn pack_zero_width_returns_none() {
    let mut atlas = TestGlyphAtlas::new();
    assert!(atlas.pack_glyph(1, 0, 10, 0, 0, &[]).is_none());
}

#[test]
fn pack_zero_height_returns_none() {
    let mut atlas = TestGlyphAtlas::new();
    assert!(atlas.pack_glyph(1, 10, 0, 0, 0, &[]).is_none());
}

#[test]
fn pack_glyph_id_out_of_range() {
    let mut atlas = TestGlyphAtlas::new();
    let cov = vec![0u8; 10];
    assert!(atlas
        .pack_glyph(MAX_GLYPH_ID as u16, 10, 1, 0, 0, &cov)
        .is_none());
}

#[test]
fn pack_vertical_overflow_returns_none() {
    let mut atlas = TestGlyphAtlas::new();

    // Fill the atlas vertically: pack tall glyphs that force row wraps.
    let mut glyph_id = 0u16;
    loop {
        let h = 50u32;
        let cov = vec![0u8; (ATLAS_WIDTH * h) as usize];
        let result = atlas.pack_glyph(glyph_id, ATLAS_WIDTH, h, 0, 0, &cov);
        if result.is_none() {
            break; // Vertical overflow reached.
        }
        glyph_id += 1;
        if glyph_id > 100 {
            panic!("expected vertical overflow within 100 glyphs");
        }
    }
    // We should have packed some glyphs before overflow.
    assert!(
        glyph_id > 0,
        "should pack at least one glyph before overflow"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Tests: UV coordinate calculation
// ═══════════════════════════════════════════════════════════════════

#[test]
fn uv_coordinates_first_glyph() {
    let mut atlas = TestGlyphAtlas::new();
    let cov = vec![255u8; 10 * 12];
    let entry = atlas.pack_glyph(65, 10, 12, 1, 10, &cov).unwrap();

    let atlas_w = ATLAS_WIDTH as f32;
    let atlas_h = ATLAS_HEIGHT as f32;

    let u0 = entry.u as f32 / atlas_w;
    let v0 = entry.v as f32 / atlas_h;
    let u1 = (entry.u as f32 + entry.width as f32) / atlas_w;
    let v1 = (entry.v as f32 + entry.height as f32) / atlas_h;

    assert!((u0 - 0.0).abs() < 0.001, "u0 should be 0.0, got {u0}");
    assert!((v0 - 0.0).abs() < 0.001, "v0 should be 0.0, got {v0}");
    assert!(
        (u1 - 10.0 / 512.0).abs() < 0.001,
        "u1 should be 10/512, got {u1}"
    );
    assert!(
        (v1 - 12.0 / 512.0).abs() < 0.001,
        "v1 should be 12/512, got {v1}"
    );
}

#[test]
fn uv_coordinates_second_row() {
    let mut atlas = TestGlyphAtlas::new();

    // Force a glyph onto the second row.
    let wide = vec![0u8; 500 * 10];
    atlas.pack_glyph(1, 500, 10, 0, 0, &wide).unwrap();

    let small_cov = vec![0u8; 20 * 15];
    let entry = atlas.pack_glyph(2, 20, 15, 0, 0, &small_cov).unwrap();

    let atlas_w = ATLAS_WIDTH as f32;
    let atlas_h = ATLAS_HEIGHT as f32;

    let u0 = entry.u as f32 / atlas_w;
    let v0 = entry.v as f32 / atlas_h;
    let u1 = (entry.u as f32 + entry.width as f32) / atlas_w;
    let v1 = (entry.v as f32 + entry.height as f32) / atlas_h;

    // Entry should be at (0, 11) — row 0 height was 10, +1 gap.
    assert!((u0 - 0.0).abs() < 0.001);
    assert!(
        (v0 - 11.0 / 512.0).abs() < 0.001,
        "v0 should be 11/512, got {v0}"
    );
    assert!((u1 - 20.0 / 512.0).abs() < 0.001);
    assert!(
        (v1 - 26.0 / 512.0).abs() < 0.001,
        "v1 should be (11+15)/512, got {v1}"
    );
}

#[test]
fn uv_coordinates_never_exceed_1() {
    let mut atlas = TestGlyphAtlas::new();

    // Pack a full-width glyph.
    let cov = vec![0u8; (ATLAS_WIDTH * 10) as usize];
    let entry = atlas.pack_glyph(1, ATLAS_WIDTH, 10, 0, 0, &cov).unwrap();

    let u1 = (entry.u as f32 + entry.width as f32) / ATLAS_WIDTH as f32;
    let v1 = (entry.v as f32 + entry.height as f32) / ATLAS_HEIGHT as f32;

    assert!(u1 <= 1.0, "u1 should not exceed 1.0, got {u1}");
    assert!(v1 <= 1.0, "v1 should not exceed 1.0, got {v1}");
}

// ═══════════════════════════════════════════════════════════════════
// Tests: Coverage data integrity
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pack_glyph_writes_coverage_data() {
    let mut atlas = TestGlyphAtlas::new();

    // 3x2 glyph with identifiable pixel values.
    let coverage = vec![10, 20, 30, 40, 50, 60];
    let entry = atlas.pack_glyph(65, 3, 2, 0, 0, &coverage).unwrap();

    // Verify coverage was written to the correct position in the pixel buffer.
    let stride = ATLAS_WIDTH as usize;
    let base = entry.v as usize * stride + entry.u as usize;

    assert_eq!(atlas.pixels[base], 10);
    assert_eq!(atlas.pixels[base + 1], 20);
    assert_eq!(atlas.pixels[base + 2], 30);
    assert_eq!(atlas.pixels[base + stride], 40);
    assert_eq!(atlas.pixels[base + stride + 1], 50);
    assert_eq!(atlas.pixels[base + stride + 2], 60);
}

#[test]
fn lookup_returns_none_for_missing_glyph() {
    let atlas = TestGlyphAtlas::new();
    assert!(atlas.lookup(99).is_none());
}

#[test]
fn lookup_returns_entry_after_pack() {
    let mut atlas = TestGlyphAtlas::new();
    let cov = vec![0u8; 10 * 12];
    atlas.pack_glyph(42, 10, 12, 2, 8, &cov).unwrap();

    let entry = atlas.lookup(42).unwrap();
    assert_eq!(entry.width, 10);
    assert_eq!(entry.height, 12);
    assert_eq!(entry.bearing_x, 2);
    assert_eq!(entry.bearing_y, 8);
}

#[test]
fn lookup_out_of_range_returns_none() {
    let atlas = TestGlyphAtlas::new();
    assert!(atlas.lookup(MAX_GLYPH_ID as u16).is_none());
    assert!(atlas.lookup(u16::MAX).is_none());
}
