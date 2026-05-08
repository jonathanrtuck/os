//! Render library: scene graph rendering, compositing, and offscreen
//! buffer management.
//!
//! This library is the render backend for the compositor. It has NO
//! dependency on `sys` or `ipc` crates — it is a pure rendering library.
//! Dependencies: `drawing`, `scene`, `fonts`.
//!
//! Supports both full repaints and incremental rendering. The `damage`
//! module tracks dirty rectangles, and `incremental` provides per-node
//! state tracking and dirty rect computation from scene graph diffs.

#![no_std]

extern crate alloc;

pub mod cache;
pub mod clip_mask;
pub mod content;
pub mod damage;
pub mod frame_scheduler;
pub mod geometry;
pub mod incremental;
pub mod scene_render;
pub mod surface_pool;

use alloc::{boxed::Box, vec, vec::Vec};

pub use clip_mask::ClipMaskCache;
pub use scene_render::{round_f32, scale_coord, scale_size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl DirtyRect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub fn union(self, other: DirtyRect) -> DirtyRect {
        if self.w == 0 || self.h == 0 {
            return other;
        }
        if other.w == 0 || other.h == 0 {
            return self;
        }
        let x0 = if self.x < other.x { self.x } else { other.x };
        let y0 = if self.y < other.y { self.y } else { other.y };
        let self_x1 = self.x as u32 + self.w as u32;
        let other_x1 = other.x as u32 + other.w as u32;
        let self_y1 = self.y as u32 + self.h as u32;
        let other_y1 = other.y as u32 + other.h as u32;
        let x1 = if self_x1 > other_x1 {
            self_x1
        } else {
            other_x1
        };
        let y1 = if self_y1 > other_y1 {
            self_y1
        } else {
            other_y1
        };
        DirtyRect {
            x: x0,
            y: y0,
            w: (x1 - x0 as u32).min(u16::MAX as u32) as u16,
            h: (y1 - y0 as u32).min(u16::MAX as u32) as u16,
        }
    }

    pub fn union_all(rects: &[DirtyRect]) -> DirtyRect {
        let mut result = DirtyRect::new(0, 0, 0, 0);
        for &r in rects {
            result = result.union(r);
        }
        result
    }
}

/// Compute gap-free physical pixel size from point position and size.
///
/// Returns the result as `u16`, clamped to non-negative.
#[inline]
pub fn scale_size_u16(pt_pos: i32, pt_size: u32, scale: f32) -> u16 {
    let phys_start = round_f32(pt_pos as f32 * scale);
    let phys_end = round_f32((pt_pos as f32 + pt_size as f32) * scale);
    (phys_end - phys_start).max(0) as u16
}

/// Maximum glyph dimensions for on-demand rasterization scratch buffer.
const GLYPH_MAX_W: usize = 50;
const GLYPH_MAX_H: usize = 50;

/// LRU glyph cache capacity (number of non-ASCII glyphs cached).
const LRU_CACHE_CAPACITY: usize = 256;

/// Mutable state for on-demand glyph rasterization (LRU fallback).
///
/// Grouped separately from the immutable glyph caches to allow split
/// borrows — immutable caches go into `RenderCtx`, while this struct
/// is passed mutably through the tree walk for non-ASCII glyph
/// rasterization.
pub struct LruRasterizer {
    /// LRU cache for non-ASCII glyphs. Keyed by (glyph_id, font_size,
    /// style_id). Populated on-demand during rendering when the fixed
    /// ASCII cache misses.
    pub cache: fonts::cache::LruGlyphCache,
    /// Mono font data (owned copy for on-demand rasterization).
    font_data: Vec<u8>,
    /// Font axis values used for rasterization.
    axes: Vec<fonts::metrics::AxisValue>,
    /// Scratch space for on-demand glyph rasterization (~39 KiB).
    scratch: Box<fonts::rasterize::RasterScratch>,
    /// Pixel buffer for on-demand glyph rasterization (GLYPH_MAX_W * GLYPH_MAX_H).
    raster_buf: Vec<u8>,
    /// Display scale factor (1 for standard, 2 for Retina). Used to
    /// compute stem darkening dilation during on-demand rasterization.
    scale_factor: u16,
}

impl LruRasterizer {
    /// Create an `LruRasterizer` with no font data (for testing cache
    /// operations without rasterization). On-demand rasterization will
    /// always return `None` since there is no font to rasterize from.
    pub fn new_test(capacity: usize) -> Self {
        // SAFETY: Layout::new::<RasterScratch>() produces correct size and
        // alignment. alloc_zeroed returns valid zeroed memory (null-checked).
        // RasterScratch::zeroed() is all-zeros. Box::from_raw takes ownership.
        let scratch: Box<fonts::rasterize::RasterScratch> = unsafe {
            let layout = alloc::alloc::Layout::new::<fonts::rasterize::RasterScratch>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::rasterize::RasterScratch;
            assert!(!ptr.is_null(), "RasterScratch allocation failed");
            Box::from_raw(ptr)
        };
        LruRasterizer {
            cache: fonts::cache::LruGlyphCache::new(capacity),
            font_data: Vec::new(),
            axes: Vec::new(),
            scratch,
            raster_buf: vec![0u8; GLYPH_MAX_W * GLYPH_MAX_H],
            scale_factor: 1,
        }
    }

    /// Rasterize a glyph on demand and insert it into the LRU cache.
    ///
    /// Returns a reference to the cached glyph if rasterization succeeded,
    /// `None` otherwise (invalid glyph ID, outline too complex, etc.).
    pub fn rasterize_and_get(
        &mut self,
        glyph_id: u16,
        font_size: u16,
        style_id: u32,
    ) -> Option<&fonts::cache::LruCachedGlyph> {
        // Clear the raster buffer.
        for b in self.raster_buf.iter_mut() {
            *b = 0;
        }
        let mut raster = fonts::rasterize::RasterBuffer {
            data: &mut self.raster_buf,
            width: GLYPH_MAX_W as u32,
            height: GLYPH_MAX_H as u32,
        };

        let metrics = fonts::rasterize::rasterize_with_axes(
            &self.font_data,
            glyph_id,
            font_size,
            &mut raster,
            &mut self.scratch,
            &self.axes,
            self.scale_factor,
        );

        let m = match metrics {
            Some(m) => m,
            None => return None,
        };

        // Copy coverage data into an owned Vec for the LRU cache entry.
        let pixel_count = (m.width * m.height) as usize;
        let coverage = Vec::from(&self.raster_buf[..pixel_count]);

        let cached = fonts::cache::LruCachedGlyph {
            width: m.width,
            height: m.height,
            bearing_x: m.bearing_x,
            bearing_y: m.bearing_y,
            advance: m.advance,
            coverage,
        };

        self.cache
            .insert_with_axes(glyph_id, font_size, style_id, cached);
        self.cache.get_with_axes(glyph_id, font_size, style_id)
    }
}
