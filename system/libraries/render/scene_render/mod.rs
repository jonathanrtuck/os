//! Scene graph renderer: walks a tree of `scene::Node` and draws to a Surface.
//!
//! Text rendering uses shaped glyph arrays from the scene graph. Each Glyphs
//! node stores an array of `ShapedGlyph` in the data buffer. The renderer
//! reads glyph IDs and advances, rasterizes via the glyph cache (pre-populated
//! for monospace ASCII, on-demand via LRU for other glyphs), and composites
//! via `draw_coverage`.

mod content;
mod coords;
pub mod path_raster;
mod walk;

pub use coords::{round_f32, scale_coord, scale_size};
use fonts::cache::GlyphCache;
use scene::Node;
pub use walk::{
    render_scene, render_scene_clipped, render_scene_clipped_full, render_scene_clipped_with_pool,
    render_scene_full, render_scene_with_pool,
};

/// Rendering context passed through the recursive tree walk.
pub struct RenderCtx<'a> {
    pub mono_cache: &'a GlyphCache,
    pub prop_cache: &'a GlyphCache,
    /// Fractional display scale factor (1.0, 1.25, 1.5, 2.0, etc.).
    /// Scene graph is in point coordinates; multiply by this to get
    /// physical pixel positions and sizes. Borders snap to whole physical
    /// pixels (round to nearest).
    pub scale: f32,
    /// Physical font size in pixels (after scale). Used as the LRU cache
    /// key's font_size component for on-demand rasterization.
    pub font_size_px: u16,
}

/// Immutable scene graph data referenced during rendering.
pub struct SceneGraph<'a> {
    pub nodes: &'a [Node],
    pub data: &'a [u8],
    /// Content Region shared memory (header + data area). Empty if no
    /// Content Region is available. Used to resolve Content::Image
    /// content_ids to decoded pixel data.
    pub content_region: &'a [u8],
}
