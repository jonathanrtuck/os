//! Scene graph tree walk for GPU rendering.
//!
//! Walks the scene graph depth-first and accumulates two types of geometry:
//! - **QuadBatch** (colored): solid rectangles from node backgrounds
//! - **TexturedBatch** (textured): glyph quads from Content::Glyphs nodes
//!
//! Each batch is uploaded to its own VBO and drawn with the appropriate
//! shader pipeline (COLOR_VS/COLOR_FS vs TEXTURED_VS/GLYPH_FS).

use scene::{Content, Node, NodeFlags, NodeId, ShapedGlyph, NULL};

use crate::atlas::{self, GlyphAtlas};

/// Maximum number of colored quads per frame.
const MAX_QUADS: usize = 256;

/// Bytes per color vertex: x(f32) + y(f32) + r(f32) + g(f32) + b(f32) + a(f32) = 24.
pub const VERTEX_STRIDE: u32 = 24;

/// Maximum color vertex data in bytes.
pub const MAX_VERTEX_BYTES: usize = MAX_QUADS * 6 * VERTEX_STRIDE as usize;

/// Maximum number of textured quads (glyphs) per frame.
/// 512 glyphs covers ~32 lines × 16 chars. If more are needed per frame,
/// a multi-pass approach would be required.
const MAX_TEXT_QUADS: usize = 512;

/// Bytes per textured vertex: x(f32) + y(f32) + u(f32) + v(f32) + r + g + b + a = 32.
pub const TEXTURED_VERTEX_STRIDE: u32 = 32;

/// Maximum textured vertex data in bytes.
pub const MAX_TEXTURED_VERTEX_BYTES: usize = MAX_TEXT_QUADS * 6 * TEXTURED_VERTEX_STRIDE as usize;

/// Maximum vertex data in u32 DWORDs (6 floats per vertex, 6 vertices per quad).
const MAX_VERTEX_DWORDS: usize = MAX_QUADS * 6 * 6;

/// Maximum textured vertex data in u32 DWORDs (8 floats per vertex, 6 vertices per quad).
const MAX_TEXTURED_DWORDS: usize = MAX_TEXT_QUADS * 6 * 8;

/// Accumulated colored quads from a scene walk.
pub struct QuadBatch {
    /// Vertex data as f32 bit representations (x, y, r, g, b, a per vertex).
    vertex_data: [u32; MAX_VERTEX_DWORDS],
    /// Current write offset in u32 DWORDs.
    vertex_len: usize,
    /// Number of vertices accumulated.
    pub vertex_count: u32,
}

impl QuadBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_VERTEX_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
    }

    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    fn push_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 6 > MAX_VERTEX_DWORDS {
            return;
        }
        self.vertex_data[self.vertex_len] = x.to_bits();
        self.vertex_data[self.vertex_len + 1] = y.to_bits();
        self.vertex_data[self.vertex_len + 2] = r.to_bits();
        self.vertex_data[self.vertex_len + 3] = g.to_bits();
        self.vertex_data[self.vertex_len + 4] = b.to_bits();
        self.vertex_data[self.vertex_len + 5] = a.to_bits();
        self.vertex_len += 6;
        self.vertex_count += 1;
    }

    /// Emit a colored quad as two CCW triangles (6 vertices) in NDC.
    fn push_quad(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        vw: f32,
        vh: f32,
        r: f32,
        g: f32,
        b: f32,
        a: f32,
    ) {
        let x0 = px / vw * 2.0 - 1.0;
        let y0 = 1.0 - py / vh * 2.0;
        let x1 = (px + pw) / vw * 2.0 - 1.0;
        let y1 = 1.0 - (py + ph) / vh * 2.0;

        // Triangle 1 (CCW in NDC Y-up): top-left, bottom-left, top-right
        self.push_vertex(x0, y0, r, g, b, a);
        self.push_vertex(x0, y1, r, g, b, a);
        self.push_vertex(x1, y0, r, g, b, a);

        // Triangle 2 (CCW in NDC Y-up): top-right, bottom-left, bottom-right
        self.push_vertex(x1, y0, r, g, b, a);
        self.push_vertex(x0, y1, r, g, b, a);
        self.push_vertex(x1, y1, r, g, b, a);
    }
}

// ── Textured batch (for glyph quads) ─────────────────────────────────────

/// Accumulated textured quads from glyph rendering.
pub struct TexturedBatch {
    /// Vertex data: x, y, u, v, r, g, b, a per vertex (8 floats = 32 bytes).
    vertex_data: [u32; MAX_TEXTURED_DWORDS],
    vertex_len: usize,
    pub vertex_count: u32,
}

impl TexturedBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_TEXTURED_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
    }

    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    /// Push a textured vertex: position(f32×2) + texcoord(f32×2) + color(f32×4).
    fn push_vertex(&mut self, x: f32, y: f32, u: f32, v: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 8 > MAX_TEXTURED_DWORDS {
            return;
        }
        self.vertex_data[self.vertex_len] = x.to_bits();
        self.vertex_data[self.vertex_len + 1] = y.to_bits();
        self.vertex_data[self.vertex_len + 2] = u.to_bits();
        self.vertex_data[self.vertex_len + 3] = v.to_bits();
        self.vertex_data[self.vertex_len + 4] = r.to_bits();
        self.vertex_data[self.vertex_len + 5] = g.to_bits();
        self.vertex_data[self.vertex_len + 6] = b.to_bits();
        self.vertex_data[self.vertex_len + 7] = a.to_bits();
        self.vertex_len += 8;
        self.vertex_count += 1;
    }

    /// Emit a textured quad as two CCW triangles (6 vertices) in NDC.
    /// `u0,v0,u1,v1` are atlas texcoords (0.0–1.0).
    fn push_textured_quad(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        vw: f32,
        vh: f32,
        u0: f32,
        v0: f32,
        u1: f32,
        v1: f32,
        r: f32,
        g: f32,
        b: f32,
        a: f32,
    ) {
        let x0 = px / vw * 2.0 - 1.0;
        let y0 = 1.0 - py / vh * 2.0;
        let x1 = (px + pw) / vw * 2.0 - 1.0;
        let y1 = 1.0 - (py + ph) / vh * 2.0;

        // Triangle 1 (CCW): top-left, bottom-left, top-right
        self.push_vertex(x0, y0, u0, v0, r, g, b, a);
        self.push_vertex(x0, y1, u0, v1, r, g, b, a);
        self.push_vertex(x1, y0, u1, v0, r, g, b, a);

        // Triangle 2 (CCW): top-right, bottom-left, bottom-right
        self.push_vertex(x1, y0, u1, v0, r, g, b, a);
        self.push_vertex(x0, y1, u0, v1, r, g, b, a);
        self.push_vertex(x1, y1, u1, v1, r, g, b, a);
    }
}

// ── Clip rectangle ───────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct ClipRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl ClipRect {
    fn intersect(self, other: ClipRect) -> ClipRect {
        let x0 = if self.x > other.x { self.x } else { other.x };
        let y0 = if self.y > other.y { self.y } else { other.y };
        let x1_a = self.x + self.w;
        let x1_b = other.x + other.w;
        let y1_a = self.y + self.h;
        let y1_b = other.y + other.h;
        let x1 = if x1_a < x1_b { x1_a } else { x1_b };
        let y1 = if y1_a < y1_b { y1_a } else { y1_b };
        let w = if x1 > x0 { x1 - x0 } else { 0.0 };
        let h = if y1 > y0 { y1 - y0 } else { 0.0 };
        ClipRect { x: x0, y: y0, w, h }
    }

    fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

// ── Scene walk ───────────────────────────────────────────────────────────

/// Walk the scene graph, accumulating colored quads and textured glyph quads.
///
/// `glyphs_data` is the scene graph's data buffer (for resolving DataRef).
/// `atlas` provides glyph→texture-coordinate lookups.
/// `ascent` is the font ascent in logical pixels (for baseline positioning).
pub fn walk_scene(
    nodes: &[Node],
    root: NodeId,
    scale: f32,
    viewport_w: u32,
    viewport_h: u32,
    batch: &mut QuadBatch,
    text_batch: &mut TexturedBatch,
    glyphs_data: &[u8],
    atlas: &GlyphAtlas,
    ascent: u32,
) {
    batch.clear();
    text_batch.clear();

    if root == NULL || nodes.is_empty() {
        return;
    }

    let clip = ClipRect {
        x: 0.0,
        y: 0.0,
        w: viewport_w as f32,
        h: viewport_h as f32,
    };

    let vw = viewport_w as f32;
    let vh = viewport_h as f32;
    walk_node(
        nodes,
        root,
        0.0,
        0.0,
        scale,
        clip,
        vw,
        vh,
        batch,
        text_batch,
        glyphs_data,
        atlas,
        ascent,
    );
}

/// Recursive depth-first walk of the scene tree.
#[allow(clippy::too_many_arguments)]
fn walk_node(
    nodes: &[Node],
    id: NodeId,
    parent_x: f32,
    parent_y: f32,
    scale: f32,
    clip: ClipRect,
    vw: f32,
    vh: f32,
    batch: &mut QuadBatch,
    text_batch: &mut TexturedBatch,
    glyphs_data: &[u8],
    atlas: &GlyphAtlas,
    ascent: u32,
) {
    let idx = id as usize;
    if idx >= nodes.len() {
        return;
    }

    let node = &nodes[idx];

    if !node.flags.contains(NodeFlags::VISIBLE) {
        return;
    }

    let abs_x = parent_x + (node.x as f32) * scale;
    let abs_y = parent_y + (node.y as f32) * scale;
    let w = (node.width as f32) * scale;
    let h = (node.height as f32) * scale;

    // Draw background if non-transparent.
    let bg = node.background;
    if bg.a > 0 {
        let quad_clip = ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        }
        .intersect(clip);

        if !quad_clip.is_empty() {
            let r = bg.r as f32 / 255.0;
            let g = bg.g as f32 / 255.0;
            let b = bg.b as f32 / 255.0;
            let a = bg.a as f32 / 255.0;
            batch.push_quad(
                quad_clip.x,
                quad_clip.y,
                quad_clip.w,
                quad_clip.h,
                vw,
                vh,
                r,
                g,
                b,
                a,
            );
        }
    }

    // Render content.
    if let Content::Glyphs {
        color,
        glyphs: glyph_ref,
        glyph_count,
        ..
    } = node.content
    {
        emit_glyphs(
            text_batch,
            glyphs_data,
            glyph_ref,
            glyph_count,
            atlas,
            abs_x,
            abs_y,
            scale,
            ascent,
            clip,
            vw,
            vh,
            color,
        );
    }

    // Compute clip rect for children.
    let child_clip = if node.flags.contains(NodeFlags::CLIPS_CHILDREN) {
        clip.intersect(ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        })
    } else {
        clip
    };

    if child_clip.is_empty() {
        return;
    }

    let scroll_y = (node.scroll_y as f32) * scale;

    let mut child = node.first_child;
    while child != NULL {
        let child_idx = child as usize;
        if child_idx >= nodes.len() {
            break;
        }
        walk_node(
            nodes,
            child,
            abs_x,
            abs_y - scroll_y,
            scale,
            child_clip,
            vw,
            vh,
            batch,
            text_batch,
            glyphs_data,
            atlas,
            ascent,
        );
        child = nodes[child_idx].next_sibling;
    }
}

// ── Glyph emission ──────────────────────────────────────────────────────

/// Emit textured quads for a Glyphs content node.
///
/// Resolves the ShapedGlyph array from the data buffer, looks up each
/// glyph in the atlas, and pushes a textured quad per visible glyph.
#[allow(clippy::too_many_arguments)]
fn emit_glyphs(
    text_batch: &mut TexturedBatch,
    glyphs_data: &[u8],
    glyph_ref: scene::DataRef,
    glyph_count: u16,
    atlas: &GlyphAtlas,
    node_x: f32,
    node_y: f32,
    scale: f32,
    ascent: u32,
    _clip: ClipRect,
    vw: f32,
    vh: f32,
    color: scene::Color,
) {
    let glyph_size = core::mem::size_of::<ShapedGlyph>();
    let offset = glyph_ref.offset as usize;
    let byte_len = glyph_count as usize * glyph_size;
    let end = offset + byte_len;

    if end > glyphs_data.len() || glyph_count == 0 {
        return;
    }

    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = color.a as f32 / 255.0;

    // Atlas texcoord scale (pixels → 0.0–1.0).
    let atlas_w = atlas::ATLAS_WIDTH as f32;
    let atlas_h = atlas::ATLAS_HEIGHT as f32;

    // Baseline position: node_y + ascent (logical pixels, scaled).
    let baseline_y = node_y + (ascent as f32) * scale;
    let mut pen_x = node_x;

    for i in 0..glyph_count as usize {
        let glyph_offset = offset + i * glyph_size;
        // SAFETY: We verified end <= glyphs_data.len() above, and ShapedGlyph
        // is repr(C) with 8 bytes. The data buffer is 4-byte aligned.
        let sg: ShapedGlyph = unsafe {
            core::ptr::read_unaligned(glyphs_data.as_ptr().add(glyph_offset) as *const _)
        };

        if let Some(entry) = atlas.lookup(sg.glyph_id) {
            let gx = pen_x + (entry.bearing_x as f32) * scale + (sg.x_offset as f32) * scale;
            let gy = baseline_y - (entry.bearing_y as f32) * scale + (sg.y_offset as f32) * scale;
            let gw = (entry.width as f32) * scale;
            let gh = (entry.height as f32) * scale;

            let u0 = entry.u as f32 / atlas_w;
            let v0 = entry.v as f32 / atlas_h;
            let u1 = (entry.u as f32 + entry.width as f32) / atlas_w;
            let v1 = (entry.v as f32 + entry.height as f32) / atlas_h;

            text_batch.push_textured_quad(gx, gy, gw, gh, vw, vh, u0, v0, u1, v1, r, g, b, a);
        }

        pen_x += (sg.x_advance as f32) * scale;
    }
}
