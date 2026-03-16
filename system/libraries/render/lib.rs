//! Render library: scene graph rendering, compositing, damage tracking,
//! and offscreen buffer management.
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

/// Result of `CpuBackend::prepare_frame()` indicating what the compositor
/// should do with this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    /// No changes detected — skip rendering entirely.
    Skip,
    /// Full repaint required (node count changed, full rebuild, etc.).
    Full,
    /// Partial update — only damaged regions need rendering.
    Partial,
}

/// Trait abstracting the full rendering pipeline: tree walk, damage
/// computation, content rendering, compositing.
///
/// Implementations own all rendering state (glyph caches, damage tracker,
/// surface pool, scale factor) and accept a scene graph + target surface.
pub trait RenderBackend {
    /// Render the scene graph into the target surface.
    ///
    /// Uses the damage state from the most recent `prepare_frame()` call
    /// to decide between full and clipped rendering.
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface);

    /// Return dirty rectangles from the most recent render pass.
    fn dirty_rects(&self) -> &[DirtyRect];
}

/// CPU-based software renderer implementing `RenderBackend`.
///
/// Encapsulates all rendering state: glyph caches, scale factor,
/// offscreen buffer pool, damage tracker, and previous-frame bounds for
/// move-based damage detection.
pub struct CpuBackend {
    pub mono_cache: alloc::boxed::Box<fonts::cache::GlyphCache>,
    pub prop_cache: alloc::boxed::Box<fonts::cache::GlyphCache>,
    pub scale: f32,
    pub pool: surface_pool::SurfacePool,
    pub damage: damage::DamageTracker,
    pub prev_bounds: [(i32, i32, u16, u16); scene::MAX_NODES],
    /// Previous frame's node count for structural change detection.
    pub prev_node_count: u16,
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
    /// `fb_width`, `fb_height` — physical framebuffer dimensions (for
    ///   damage tracker initialization).
    ///
    /// Returns `None` if allocation fails or the monospace font is invalid.
    /// The returned `Box` avoids placing the large `prev_bounds` array on
    /// the caller's stack.
    pub fn new(
        mono_font_data: &[u8],
        prop_font_data: Option<&[u8]>,
        font_size: u32,
        dpi: u16,
        scale: f32,
        fb_width: u16,
        fb_height: u16,
    ) -> Option<alloc::boxed::Box<Self>> {
        use alloc::boxed::Box;

        // Validate mono font before allocating.
        if fonts::rasterize::font_metrics(mono_font_data).is_none() {
            return None;
        }

        // Physical pixel size: logical font_size × scale.
        let physical_size = round_f32(font_size as f32 * scale).max(1) as u32;

        // Allocate and populate monospace glyph cache (MONO=1).
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

        // Heap-allocate the CpuBackend to avoid placing the 6 KiB
        // prev_bounds array on the caller's stack.
        unsafe {
            let layout = alloc::alloc::Layout::new::<CpuBackend>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut CpuBackend;
            if ptr.is_null() {
                return None;
            }
            // Write fields into the heap allocation in-place.
            core::ptr::write(&mut (*ptr).mono_cache, mono_cache);
            core::ptr::write(&mut (*ptr).prop_cache, prop_cache);
            (*ptr).scale = scale;
            core::ptr::write(
                &mut (*ptr).pool,
                surface_pool::SurfacePool::new(surface_pool::DEFAULT_BUDGET),
            );
            core::ptr::write(
                &mut (*ptr).damage,
                damage::DamageTracker::new(fb_width, fb_height),
            );
            // prev_bounds is already zeroed by alloc_zeroed — (0,0,0,0) for all entries.
            (*ptr).prev_node_count = 0;
            Some(Box::from_raw(ptr))
        }
    }

    /// Analyse the scene graph's change information and compute damage.
    ///
    /// Call this before `render()` each frame. Returns a `FrameAction`
    /// telling the compositor whether to skip, do a full repaint, or a
    /// partial update.
    ///
    /// * `nodes` — current frame's scene nodes
    /// * `node_count` — number of live nodes
    /// * `change_list` — `Some(&[node_ids])` for incremental changes,
    ///   `None` for a full rebuild (change list overflow / sentinel)
    /// * `is_full_repaint` — set when the scene writer signals a full
    ///   rebuild (e.g. change list overflow)
    pub fn prepare_frame(
        &mut self,
        nodes: &[scene::Node],
        node_count: u16,
        change_list: Option<&[u16]>,
        is_full_repaint: bool,
    ) -> FrameAction {
        // Reset damage tracker.
        self.damage.reset();

        if node_count != self.prev_node_count || is_full_repaint {
            self.damage.mark_full_screen();
            return FrameAction::Full;
        }

        match change_list {
            Some(changed) if changed.is_empty() => {
                // No nodes changed — skip rendering entirely.
                self.prev_node_count = node_count;
                FrameAction::Skip
            }
            Some(changed) => {
                // Compute dirty rects from changed node positions.
                let parent_map = scene::build_parent_map(nodes, node_count as usize);
                let sf = self.scale;
                let fbw = self.damage.fb_width();
                let fbh = self.damage.fb_height();

                for &node_id in changed {
                    if (node_id as usize) >= nodes.len() {
                        continue;
                    }

                    // Damage the OLD position (previous frame's bounds).
                    let (ox, oy, ow, oh) = self.prev_bounds[node_id as usize];
                    if ow > 0 && oh > 0 && ox >= 0 && oy >= 0 {
                        let old_x = (ox as u32).min(fbw as u32) as u16;
                        let old_y = (oy as u32).min(fbh as u32) as u16;
                        let old_w = ow.min(fbw - old_x);
                        let old_h = oh.min(fbh - old_y);
                        self.damage.add(old_x, old_y, old_w, old_h);
                    }

                    // Damage the NEW position (current frame's bounds).
                    let (ax, ay, aw, ah) =
                        scene::abs_bounds(nodes, &parent_map, node_id as usize);
                    let px = (scale_coord(ax, sf).max(0) as u32).min(fbw as u32) as u16;
                    let py = (scale_coord(ay, sf).max(0) as u32).min(fbh as u32) as u16;
                    let w = scale_size_u16(ax, aw, sf).min(fbw - px);
                    let h = scale_size_u16(ay, ah, sf).min(fbh - py);
                    self.damage.add(px, py, w, h);
                }

                if self.damage.count == 0 && !self.damage.full_screen {
                    // All damage rects were zero-size — nothing to render.
                    self.prev_node_count = node_count;
                    FrameAction::Skip
                } else {
                    FrameAction::Partial
                }
            }
            None => {
                // No change list (sentinel or overflow) — full repaint.
                self.damage.mark_full_screen();
                FrameAction::Full
            }
        }
    }

    /// Whether the current frame requires a full repaint.
    pub fn is_full_repaint(&self) -> bool {
        self.damage.full_screen
    }

    /// Update previous-frame bounds after rendering.
    ///
    /// Call this after `render()`. For full repaints, updates all nodes.
    /// For partial updates, only updates the changed nodes.
    pub fn finish_frame(
        &mut self,
        nodes: &[scene::Node],
        node_count: u16,
        change_list: Option<&[u16]>,
    ) {
        let n = (node_count as usize).min(nodes.len()).min(scene::MAX_NODES);

        if self.damage.full_screen {
            // Full repaint: refresh all prev_bounds.
            let parent_map = scene::build_parent_map(nodes, n);
            let sf = self.scale;
            for i in 0..n {
                let (ax, ay, aw, ah) = scene::abs_bounds(nodes, &parent_map, i);
                let px = scale_coord(ax, sf).max(0);
                let py = scale_coord(ay, sf).max(0);
                let pw = scale_size_u16(ax, aw, sf);
                let ph = scale_size_u16(ay, ah, sf);
                self.prev_bounds[i] = (px, py, pw, ph);
            }
            // Zero out entries beyond live node count.
            for i in n..scene::MAX_NODES {
                self.prev_bounds[i] = (0, 0, 0, 0);
            }
        } else if let Some(changed) = change_list {
            // Partial update: refresh only changed nodes' prev_bounds.
            let parent_map = scene::build_parent_map(nodes, n);
            let sf = self.scale;
            for &node_id in changed {
                if (node_id as usize) >= n {
                    continue;
                }
                let (ax, ay, aw, ah) =
                    scene::abs_bounds(nodes, &parent_map, node_id as usize);
                let px = scale_coord(ax, sf).max(0);
                let py = scale_coord(ay, sf).max(0);
                let pw = scale_size_u16(ax, aw, sf);
                let ph = scale_size_u16(ay, ah, sf);
                self.prev_bounds[node_id as usize] = (px, py, pw, ph);
            }
        }

        self.prev_node_count = node_count;
    }

    /// Build a `RenderCtx` from the backend's cached state.
    fn make_ctx(&self) -> scene_render::RenderCtx<'_> {
        scene_render::RenderCtx {
            mono_cache: &self.mono_cache,
            prop_cache: &self.prop_cache,
            scale: self.scale,
        }
    }
}

impl RenderBackend for CpuBackend {
    fn render(&mut self, scene: &scene_render::SceneGraph, target: &mut Surface) {
        // Build RenderCtx inline to avoid borrowing `self` immutably
        // while also needing `&mut self.pool`.
        let ctx = scene_render::RenderCtx {
            mono_cache: &self.mono_cache,
            prop_cache: &self.prop_cache,
            scale: self.scale,
        };
        if self.damage.full_screen {
            scene_render::render_scene_with_pool(target, scene, &ctx, &mut self.pool);
        } else if self.damage.count > 0 {
            let bbox = DirtyRect::union_all(&self.damage.rects[..self.damage.count]);
            scene_render::render_scene_clipped_with_pool(
                target, scene, &ctx, &bbox, &mut self.pool,
            );
        }
    }

    fn dirty_rects(&self) -> &[DirtyRect] {
        &self.damage.rects[..self.damage.count]
    }
}
