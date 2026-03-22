//! Render library: scene graph rendering, compositing, and offscreen
//! buffer management.
//!
//! This library is the render backend for the compositor. It has NO
//! dependency on `sys` or `ipc` crates — it is a pure rendering library.
//! Dependencies: `drawing`, `scene`, `protocol`, `fonts`.
//!
//! Supports both full repaints and incremental rendering. The `damage`
//! module tracks dirty rectangles, and `incremental` provides per-node
//! state tracking and dirty rect computation from scene graph diffs.

#![no_std]

extern crate alloc;

pub mod cache;
pub mod clip_mask;
pub mod damage;
pub mod frame_scheduler;
pub mod incremental;
pub mod scene_render;
pub mod surface_pool;

use alloc::{boxed::Box, vec, vec::Vec};

pub use clip_mask::ClipMaskCache;
use drawing::Surface;
// Re-export helper functions at the crate root for external use.
pub use scene_render::{round_f32, scale_coord, scale_size};

/// Compute gap-free physical pixel size from point position and size.
///
/// Returns the result as `u16`, clamped to non-negative.
#[inline]
pub fn scale_size_u16(pt_pos: i32, pt_size: u32, scale: f32) -> u16 {
    let phys_start = round_f32(pt_pos as f32 * scale);
    let phys_end = round_f32((pt_pos as f32 + pt_size as f32) * scale);
    (phys_end - phys_start).max(0) as u16
}

/// Trait abstracting the full rendering pipeline: tree walk, content
/// rendering, compositing.
///
/// Implementations own all rendering state (glyph caches, surface pool,
/// scale factor) and accept a scene graph + target surface.
pub trait RenderBackend {
    /// Render the scene graph into the target surface (full repaint).
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface);
}

/// Maximum glyph dimensions for on-demand rasterization scratch buffer.
const GLYPH_MAX_W: usize = 50;
const GLYPH_MAX_H: usize = 50;

/// LRU glyph cache capacity (number of non-ASCII glyphs cached).
const LRU_CACHE_CAPACITY: usize = 256;

/// Mutable state for on-demand glyph rasterization (LRU fallback).
///
/// Grouped separately from the immutable glyph caches to allow split
/// borrows in `CpuBackend::render` — immutable caches go into `RenderCtx`,
/// while this struct is passed mutably through the tree walk for non-ASCII
/// glyph rasterization.
pub struct LruRasterizer {
    /// LRU cache for non-ASCII glyphs. Keyed by (glyph_id, font_size,
    /// axis_hash). Populated on-demand during rendering when the fixed
    /// ASCII cache misses.
    pub cache: fonts::cache::LruGlyphCache,
    /// Mono font data (owned copy for on-demand rasterization).
    font_data: Vec<u8>,
    /// Font axis values used for rasterization.
    axes: Vec<fonts::rasterize::AxisValue>,
    /// Scratch space for on-demand glyph rasterization (~39 KiB).
    scratch: Box<fonts::rasterize::RasterScratch>,
    /// Pixel buffer for on-demand glyph rasterization (GLYPH_MAX_W * GLYPH_MAX_H).
    raster_buf: Vec<u8>,
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
        axis_hash: u32,
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
            .insert_with_axes(glyph_id, font_size, axis_hash, cached);
        self.cache.get_with_axes(glyph_id, font_size, axis_hash)
    }
}

/// CPU-based software renderer implementing `RenderBackend`.
///
/// Encapsulates all rendering state: glyph caches (fixed ASCII + LRU
/// for non-ASCII), font data for on-demand rasterization, scale factor,
/// offscreen buffer pool, and per-node content cache.
pub struct CpuBackend {
    pub mono_cache: Box<fonts::cache::GlyphCache>,
    pub prop_cache: Box<fonts::cache::GlyphCache>,
    pub scale: f32,
    pub pool: surface_pool::SurfacePool,
    /// On-demand LRU rasterizer for non-ASCII glyphs. Separated from
    /// the immutable caches to allow split borrows.
    pub lru: LruRasterizer,
    /// Per-node content cache for incremental rendering. Stores
    /// rendered bitmaps keyed by (node_id, content_hash). On cache
    /// hit, the cached bitmap is blitted instead of re-rasterizing.
    pub node_cache: cache::NodeCache,
    /// LRU cache of rasterized clip masks. Keyed by (path DataRef +
    /// node dimensions). Masks are rasterized once and reused across
    /// frames as long as the path data and dimensions are unchanged.
    pub clip_cache: ClipMaskCache,
    /// Physical font size in pixels (after scale).
    font_size_px: u32,
}

impl CpuBackend {
    /// Construct a `CpuBackend` with pre-populated glyph caches.
    ///
    /// `mono_font_data` — raw font file bytes for the monospace face (JetBrains Mono).
    /// `prop_font_data` — optional raw font file bytes for the proportional
    ///   face (Inter). When `None`, the monospace font is used as fallback.
    /// `font_size` — font size in points (before scale).
    /// `dpi` — display DPI for optical sizing.
    /// `scale` — fractional display scale factor (1.0, 1.5, 2.0, etc.).
    /// `fb_width`, `fb_height` — physical framebuffer dimensions (unused
    ///   currently; retained for API compatibility).
    ///
    /// Returns `None` if allocation fails or the monospace font is invalid.
    pub fn new(
        mono_font_data: &[u8],
        prop_font_data: Option<&[u8]>,
        font_size: u32,
        dpi: u16,
        scale: f32,
        _fb_width: u16,
        _fb_height: u16,
    ) -> Option<Box<Self>> {
        // Validate mono font before allocating.
        if fonts::rasterize::font_metrics(mono_font_data).is_none() {
            return None;
        }

        // Physical pixel size: font_size (points) × scale.
        let physical_size = round_f32(font_size as f32 * scale).max(1) as u32;

        // Allocate and populate monospace glyph cache (JetBrains Mono).
        // SAFETY: Layout::new::<GlyphCache>() produces a correctly sized and
        // aligned layout for the type. alloc_zeroed returns a valid, zeroed
        // allocation (or null, which we check). All GlyphCache fields are
        // integer/array types where all-zeroes is a valid bit pattern (no
        // Drop-bearing fields requiring ptr::write). Box::from_raw takes
        // ownership with the matching global allocator layout.
        let mut mono_cache: Box<fonts::cache::GlyphCache> = unsafe {
            let layout = alloc::alloc::Layout::new::<fonts::cache::GlyphCache>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;
            if ptr.is_null() {
                return None;
            }
            Box::from_raw(ptr)
        };
        // No extra axes needed — automatic opsz/wght applied by populate_with_axes.
        mono_cache.populate_with_axes(mono_font_data, physical_size, dpi, &[]);

        // Allocate and populate proportional glyph cache (Inter or fallback to mono).
        // SAFETY: Same rationale as mono_cache above — Layout::new produces
        // correct size/alignment for GlyphCache, alloc_zeroed returns valid
        // zeroed memory (null-checked), all-zeroes is a valid GlyphCache,
        // and Box::from_raw takes ownership with matching layout.
        let mut prop_cache: Box<fonts::cache::GlyphCache> = unsafe {
            let layout = alloc::alloc::Layout::new::<fonts::cache::GlyphCache>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;
            if ptr.is_null() {
                return None;
            }
            Box::from_raw(ptr)
        };
        let prop_data_slice = prop_font_data.unwrap_or(mono_font_data);
        if fonts::rasterize::font_metrics(prop_data_slice).is_some() {
            prop_cache.populate_with_axes(prop_data_slice, physical_size, dpi, &[]);
        } else {
            // Fallback: use mono font.
            prop_cache.populate_with_axes(mono_font_data, physical_size, dpi, &[]);
        }

        // Own copy of font data for on-demand LRU rasterization.
        let font_data_owned = Vec::from(mono_font_data);

        // Allocate rasterization scratch space (~39 KiB).
        // SAFETY: Layout::new::<RasterScratch>() produces correct size and
        // alignment. alloc_zeroed returns valid zeroed memory (null-checked).
        // RasterScratch::zeroed() is all-zeros, so the zero-initialized
        // memory is a valid instance. Box::from_raw takes ownership.
        let raster_scratch: Box<fonts::rasterize::RasterScratch> = unsafe {
            let layout = alloc::alloc::Layout::new::<fonts::rasterize::RasterScratch>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::rasterize::RasterScratch;
            if ptr.is_null() {
                return None;
            }
            Box::from_raw(ptr)
        };

        let raster_buf = vec![0u8; GLYPH_MAX_W * GLYPH_MAX_H];

        let lru = LruRasterizer {
            cache: fonts::cache::LruGlyphCache::new(LRU_CACHE_CAPACITY),
            font_data: font_data_owned,
            axes: vec![],
            scratch: raster_scratch,
            raster_buf,
        };

        // Heap-allocate the CpuBackend.
        //
        // SAFETY: Layout::new::<CpuBackend>() produces correct size and
        // alignment. alloc_zeroed returns valid zeroed memory (null-checked).
        // ptr::write is used for Drop-bearing fields (Box, Vec, LruRasterizer,
        // SurfacePool, NodeCache, ClipMaskCache) — these types whose drop glue
        // must not run on the zeroed memory, so ptr::write overwrites them
        // without dropping the destination. Primitive fields (scale,
        // font_size_px) are safe to assign directly (no Drop). Box::from_raw
        // takes ownership of the fully-initialized CpuBackend with matching
        // layout.
        unsafe {
            let layout = alloc::alloc::Layout::new::<CpuBackend>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut CpuBackend;
            if ptr.is_null() {
                return None;
            }
            core::ptr::write(&mut (*ptr).mono_cache, mono_cache);
            core::ptr::write(&mut (*ptr).prop_cache, prop_cache);
            (*ptr).scale = scale;
            core::ptr::write(
                &mut (*ptr).pool,
                surface_pool::SurfacePool::new(surface_pool::DEFAULT_BUDGET),
            );
            core::ptr::write(&mut (*ptr).lru, lru);
            core::ptr::write(&mut (*ptr).node_cache, cache::NodeCache::new());
            core::ptr::write(&mut (*ptr).clip_cache, ClipMaskCache::new());
            (*ptr).font_size_px = physical_size;
            Some(Box::from_raw(ptr))
        }
    }

    /// Render only the region within `dirty` (absolute pixel coordinates).
    ///
    /// Nodes outside the dirty rect are clipped and skipped. Used for
    /// incremental rendering where the caller has already copied the
    /// presented buffer into the render target (unchanged pixels are
    /// correct) and only needs to repaint dirty regions.
    ///
    /// The per-node content cache is passed through the tree walk. On
    /// cache hit (same content_hash), the cached bitmap is blitted
    /// instead of re-rasterizing. On miss, content is rendered to an
    /// offscreen buffer, blitted to the target, and stored in the cache.
    pub fn render_clipped(
        &mut self,
        scene: &scene_render::SceneGraph,
        target: &mut Surface,
        dirty: &protocol::DirtyRect,
    ) {
        // Split borrow: immutable caches for RenderCtx, mutable lru + pool
        // + node_cache for the tree walk. These are disjoint fields — no aliasing.
        let ctx = scene_render::RenderCtx {
            mono_cache: &self.mono_cache,
            prop_cache: &self.prop_cache,
            scale: self.scale,
            font_size_px: self.font_size_px as u16,
        };
        scene_render::render_scene_clipped_full(
            target,
            scene,
            &ctx,
            dirty,
            &mut self.pool,
            &mut self.lru,
            Some(&mut self.node_cache),
            &mut self.clip_cache,
        );
    }

    /// Invalidate all cached content bitmaps.
    ///
    /// Called on full repaint (all content is re-rendered, so any cached
    /// bitmaps are stale). Pixel buffer allocations are retained for reuse.
    pub fn clear_cache(&mut self) {
        self.node_cache.clear();
    }
}

impl RenderBackend for CpuBackend {
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface) {
        // Full repaint: clear the node cache (all content is re-rendered).
        self.node_cache.clear();
        // Split borrow: immutable caches for RenderCtx, mutable lru + pool
        // for the tree walk. These are disjoint fields — no aliasing.
        let ctx = scene_render::RenderCtx {
            mono_cache: &self.mono_cache,
            prop_cache: &self.prop_cache,
            scale: self.scale,
            font_size_px: self.font_size_px as u16,
        };
        scene_render::render_scene_full(
            target,
            scene,
            &ctx,
            &mut self.pool,
            &mut self.lru,
            &mut self.clip_cache,
        );
    }
}
