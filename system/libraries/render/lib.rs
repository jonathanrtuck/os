//! Render library: scene graph rendering, compositing, SVG rasterization,
//! damage tracking, and offscreen buffer management.
//!
//! This library is the render backend for the compositor. It has NO
//! dependency on `sys` or `ipc` crates — it is a pure rendering library.
//! Dependencies: `drawing`, `scene`, `protocol`, `fonts`.

#![no_std]

extern crate alloc;

pub mod compositing;
pub mod cursor;
pub mod damage;
pub mod scene_render;
pub mod surface_pool;
pub mod svg;

use drawing::Surface;
use protocol::DirtyRect;

// Re-export helper functions at the crate root for external use.
pub use scene_render::{round_f32, scale_coord, scale_size};

/// Compute gap-free physical size from logical position and size.
///
/// Returns the result as `u16`, clamped to non-negative. This variant is
/// used by the compositor's damage tracking where `u16` dimensions are needed
/// for the `DirtyRect` payload.
#[inline]
pub fn scale_size_u16(logical_pos: i32, logical_size: u32, scale: f32) -> u16 {
    let phys_start = round_f32(logical_pos as f32 * scale);
    let phys_end = round_f32((logical_pos as f32 + logical_size as f32) * scale);
    (phys_end - phys_start).max(0) as u16
}

/// Trait abstracting the full rendering pipeline: tree walk, damage
/// computation, content rendering, compositing.
///
/// Implementations own all rendering state (glyph caches, damage tracker,
/// surface pool, scale factor) and accept a scene graph + target surface.
pub trait RenderBackend {
    /// Render the scene graph into the target surface.
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface);

    /// Return dirty rectangles from the most recent render pass.
    fn dirty_rects(&self) -> &[DirtyRect];
}

/// CPU-based software renderer implementing `RenderBackend`.
///
/// Encapsulates all rendering state: glyph caches, icon data, scale factor,
/// offscreen buffer pool, damage tracker, and previous-frame bounds for
/// move-based damage detection.
pub struct CpuBackend {
    pub mono_cache: alloc::boxed::Box<fonts::cache::GlyphCache>,
    pub prop_cache: alloc::boxed::Box<fonts::cache::GlyphCache>,
    pub icon_coverage: alloc::vec::Vec<u8>,
    pub icon_w: u32,
    pub icon_h: u32,
    pub icon_color: drawing::Color,
    pub icon_node: scene::NodeId,
    pub scale: f32,
    pub pool: surface_pool::SurfacePool,
    pub damage: damage::DamageTracker,
    pub prev_bounds: [(i32, i32, u16, u16); scene::MAX_NODES],
}

impl RenderBackend for CpuBackend {
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface) {
        let ctx = scene_render::RenderCtx {
            mono_cache: &self.mono_cache,
            prop_cache: &self.prop_cache,
            icon_coverage: &self.icon_coverage,
            icon_w: self.icon_w,
            icon_h: self.icon_h,
            icon_color: self.icon_color,
            icon_node: self.icon_node,
            scale: self.scale,
        };
        scene_render::render_scene_with_pool(target, scene, &ctx, &mut self.pool);
    }

    fn dirty_rects(&self) -> &[DirtyRect] {
        &self.damage.rects[..self.damage.count]
    }
}
