//! Render library: scene graph rendering, compositing, and offscreen
//! buffer management.
//!
//! This library is the render backend for the compositor. It has NO
//! dependency on `sys` or `ipc` crates — it is a pure rendering library.
//! Dependencies: `drawing`, `scene`, `protocol`, `fonts`.
//!
//! Always performs full repaints. Damage tracking was removed from this
//! layer — see journal entry 2026-03-17. The `damage` module is retained
//! as a building block for future backend-internal optimizations.

#![no_std]

extern crate alloc;

pub mod compositing;
pub mod cursor;
pub mod damage;
pub mod scene_render;
pub mod surface_pool;

use drawing::Surface;

// Re-export helper functions at the crate root for external use.
pub use scene_render::{round_f32, scale_coord, scale_size};

/// Compute gap-free physical size from logical position and size.
///
/// Returns the result as `u16`, clamped to non-negative.
#[inline]
pub fn scale_size_u16(logical_pos: i32, logical_size: u32, scale: f32) -> u16 {
    let phys_start = round_f32(logical_pos as f32 * scale);
    let phys_end = round_f32((logical_pos as f32 + logical_size as f32) * scale);
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

/// CPU-based software renderer implementing `RenderBackend`.
///
/// Encapsulates all rendering state: glyph caches, scale factor,
/// and offscreen buffer pool.
pub struct CpuBackend {
    pub mono_cache: alloc::boxed::Box<fonts::cache::GlyphCache>,
    pub prop_cache: alloc::boxed::Box<fonts::cache::GlyphCache>,
    pub scale: f32,
    pub pool: surface_pool::SurfacePool,
}

impl CpuBackend {
    /// Construct a `CpuBackend` with pre-populated glyph caches.
    ///
    /// `mono_font_data` — raw font file bytes for the monospace face.
    /// `prop_font_data` — optional raw font file bytes for the proportional
    ///   face. When `None`, the monospace font is reused with `MONO=0`.
    /// `font_size` — logical font size in pixels (before scale).
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
    ) -> Option<alloc::boxed::Box<Self>> {
        use alloc::boxed::Box;

        // Validate mono font before allocating.
        if fonts::rasterize::font_metrics(mono_font_data).is_none() {
            return None;
        }

        // Physical pixel size: logical font_size × scale.
        let physical_size = round_f32(font_size as f32 * scale).max(1) as u32;

        // Allocate and populate monospace glyph cache (MONO=1).
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
        let mono_axes = [fonts::rasterize::AxisValue {
            tag: *b"MONO",
            value: 1.0,
        }];
        mono_cache.populate_with_axes(mono_font_data, physical_size, dpi, &mono_axes);

        // Allocate and populate proportional glyph cache (MONO=0).
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
        let prop_data = prop_font_data.unwrap_or(mono_font_data);
        if fonts::rasterize::font_metrics(prop_data).is_some() {
            let prop_axes = [fonts::rasterize::AxisValue {
                tag: *b"MONO",
                value: 0.0,
            }];
            prop_cache.populate_with_axes(prop_data, physical_size, dpi, &prop_axes);
        } else {
            // Fallback: use mono font with MONO=1 axes.
            prop_cache.populate_with_axes(mono_font_data, physical_size, dpi, &mono_axes);
        }

        // Heap-allocate the CpuBackend.
        //
        // SAFETY: Layout::new::<CpuBackend>() produces correct size and
        // alignment. alloc_zeroed returns valid zeroed memory (null-checked).
        // ptr::write is used for Drop-bearing fields (mono_cache, prop_cache,
        // pool) — these are Box/Vec/struct types whose drop glue must not
        // run on the zeroed memory, so ptr::write overwrites them without
        // dropping the destination. Primitive fields (scale) are safe to
        // assign directly (no Drop). Box::from_raw takes ownership of the
        // fully-initialized CpuBackend with matching layout.
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
            Some(Box::from_raw(ptr))
        }
    }
}

impl RenderBackend for CpuBackend {
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface) {
        let ctx = scene_render::RenderCtx {
            mono_cache: &self.mono_cache,
            prop_cache: &self.prop_cache,
            scale: self.scale,
        };
        scene_render::render_scene_with_pool(target, scene, &ctx, &mut self.pool);
    }
}
