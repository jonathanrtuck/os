//! Scene graph tree walk for GPU rendering.
//!
//! Walks the scene graph depth-first and accumulates two types of geometry:
//! - **QuadBatch** (colored): solid rectangles from node backgrounds
//! - **TexturedBatch** (textured): glyph quads from Content::Glyphs nodes
//!
//! Each batch is uploaded to its own VBO and drawn with the appropriate
//! shader pipeline (COLOR_VS/COLOR_FS vs TEXTURED_VS/GLYPH_FS).

use scene::{
    Content, FillRule, Node, NodeFlags, NodeId, ShapedGlyph, NULL, PATH_CLOSE, PATH_CUBIC_TO,
    PATH_LINE_TO, PATH_MOVE_TO,
};

use crate::atlas::{self, GlyphAtlas};

/// Maximum number of colored quads per frame.
const MAX_QUADS: usize = 256;

/// Bytes per color vertex: x(f32) + y(f32) + r(f32) + g(f32) + b(f32) + a(f32) = 24.
pub const VERTEX_STRIDE: u32 = 24;

/// Maximum color vertex data in bytes.
pub const MAX_VERTEX_BYTES: usize = MAX_QUADS * 6 * VERTEX_STRIDE as usize;

/// Maximum number of textured quads (glyphs) per frame.
/// 4096 glyphs covers a full 1024×768 screen at ~80 chars × 48 lines
/// with headroom for title, clock, and other UI text.
const MAX_TEXT_QUADS: usize = 4096;

/// Bytes per textured vertex: x(f32) + y(f32) + u(f32) + v(f32) + r + g + b + a = 32.
pub const TEXTURED_VERTEX_STRIDE: u32 = 32;

/// Maximum textured vertex data in bytes (glyphs only).
pub const MAX_TEXTURED_VERTEX_BYTES: usize = MAX_TEXT_QUADS * 6 * TEXTURED_VERTEX_STRIDE as usize;

/// Dwords per image quad: 6 vertices x 8 floats = 48.
pub const DWORDS_PER_IMAGE_QUAD: usize = 48;

/// Total textured VBO size: image quads (MAX_IMAGES x 192 bytes) + glyph quads.
/// Image vertices occupy offset 0; glyphs start after all image data.
pub const TOTAL_TEXTURED_VBO_BYTES: usize =
    MAX_TEXTURED_VERTEX_BYTES + MAX_IMAGES * DWORDS_PER_IMAGE_QUAD * 4;

/// Total color VBO size: background quads + path fan triangles + path cover quads.
/// All three regions are packed sequentially in the same VBO.
pub const TOTAL_COLOR_VBO_BYTES: usize =
    MAX_VERTEX_BYTES + MAX_PATH_FAN_DWORDS * 4 + MAX_PATH_COVER_DWORDS * 4;

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
    /// Number of vertices silently dropped due to batch overflow.
    dropped: u32,
}

impl QuadBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_VERTEX_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
            dropped: 0,
        }
    }

    pub fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
        self.dropped = 0;
    }

    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    pub fn dropped_count(&self) -> u32 {
        self.dropped
    }

    fn push_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 6 > MAX_VERTEX_DWORDS {
            self.dropped += 1;
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
    /// Number of vertices silently dropped due to batch overflow.
    dropped: u32,
}

impl TexturedBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_TEXTURED_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
            dropped: 0,
        }
    }

    pub fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
        self.dropped = 0;
    }

    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    pub fn dropped_count(&self) -> u32 {
        self.dropped
    }

    /// Push a textured vertex: position(f32×2) + texcoord(f32×2) + color(f32×4).
    fn push_vertex(&mut self, x: f32, y: f32, u: f32, v: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 8 > MAX_TEXTURED_DWORDS {
            self.dropped += 1;
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

// ── Image batch ──────────────────────────────────────────────────────────

/// Maximum images per frame.
pub const MAX_IMAGES: usize = 4;

/// A single image to render as a textured quad.
#[derive(Clone, Copy)]
pub struct ImageQuad {
    /// Screen-space position (physical pixels).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Offset into the scene graph data buffer (BGRA pixel data).
    pub data_offset: u32,
    pub data_length: u32,
    /// Source image dimensions (pixels).
    pub src_width: u16,
    pub src_height: u16,
}

/// Collected image draw requests from a scene walk.
pub struct ImageBatch {
    images: [ImageQuad; MAX_IMAGES],
    pub count: usize,
}

impl ImageBatch {
    pub const fn new() -> Self {
        Self {
            images: [ImageQuad {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                data_offset: 0,
                data_length: 0,
                src_width: 0,
                src_height: 0,
            }; MAX_IMAGES],
            count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    fn push(&mut self, img: ImageQuad) {
        if self.count < MAX_IMAGES {
            self.images[self.count] = img;
            self.count += 1;
        }
    }

    pub fn get(&self, i: usize) -> Option<&ImageQuad> {
        if i < self.count {
            Some(&self.images[i])
        } else {
            None
        }
    }
}

// ── Path batch ──────────────────────────────────────────────────────────

/// Maximum triangle fan vertices per frame (for stencil-then-cover).
/// 1024 fan triangles = 3072 vertices should handle complex paths.
const MAX_PATH_FAN_VERTS: usize = 3072;

/// Max fan vertex data in u32 DWORDs (6 floats per vertex = x, y, r, g, b, a).
/// Uses the same color vertex layout (VERTEX_STRIDE=24) for VE reuse.
const MAX_PATH_FAN_DWORDS: usize = MAX_PATH_FAN_VERTS * 6;

/// Maximum covering quads per frame (one per path node).
const MAX_PATH_COVERS: usize = 16;

/// Max cover vertex data (6 verts × 6 floats per covering quad × MAX_PATH_COVERS).
const MAX_PATH_COVER_DWORDS: usize = MAX_PATH_COVERS * 6 * 6;

/// Accumulated path rendering data from a scene walk.
///
/// The triangle fan vertices go into the stencil write pass (position only,
/// drawn with PIPE_PRIM_TRIANGLES — we decompose the fan ourselves).
/// The cover quads go into the stencil test pass (position + color).
pub struct PathBatch {
    /// Fan triangle vertices: x, y per vertex (f32 pairs).
    fan_data: [u32; MAX_PATH_FAN_DWORDS],
    fan_len: usize,
    pub fan_vertex_count: u32,
    /// Cover quad vertices: x, y, r, g, b, a per vertex (6 floats).
    cover_data: [u32; MAX_PATH_COVER_DWORDS],
    cover_len: usize,
    pub cover_vertex_count: u32,
    /// Number of vertices silently dropped due to batch overflow.
    dropped: u32,
}

impl PathBatch {
    pub const fn new() -> Self {
        Self {
            fan_data: [0; MAX_PATH_FAN_DWORDS],
            fan_len: 0,
            fan_vertex_count: 0,
            cover_data: [0; MAX_PATH_COVER_DWORDS],
            cover_len: 0,
            cover_vertex_count: 0,
            dropped: 0,
        }
    }

    pub fn clear(&mut self) {
        self.fan_len = 0;
        self.fan_vertex_count = 0;
        self.cover_len = 0;
        self.cover_vertex_count = 0;
        self.dropped = 0;
    }

    pub fn as_fan_data(&self) -> &[u32] {
        &self.fan_data[..self.fan_len]
    }

    pub fn as_cover_data(&self) -> &[u32] {
        &self.cover_data[..self.cover_len]
    }

    pub fn dropped_count(&self) -> u32 {
        self.dropped
    }

    /// Push a fan vertex (position + dummy color, for stencil write).
    /// Color is not written (colormask=0 in stencil pass), but alpha MUST be
    /// non-zero: ANGLE/Metal's early fragment discard optimization skips
    /// per-fragment operations (including stencil writes) for alpha=0 fragments.
    fn push_fan_vertex(&mut self, x: f32, y: f32) {
        if self.fan_len + 6 > MAX_PATH_FAN_DWORDS {
            self.dropped += 1;
            return;
        }
        self.fan_data[self.fan_len] = x.to_bits();
        self.fan_data[self.fan_len + 1] = y.to_bits();
        self.fan_data[self.fan_len + 2] = 0; // r (unused — colormask=0 in stencil pass)
        self.fan_data[self.fan_len + 3] = 0; // g
        self.fan_data[self.fan_len + 4] = 0; // b
        self.fan_data[self.fan_len + 5] = 1.0f32.to_bits(); // a = 1.0 (non-zero for ANGLE)
        self.fan_len += 6;
        self.fan_vertex_count += 1;
    }

    /// Push a cover vertex (position + color, for stencil test + fill).
    fn push_cover_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.cover_len + 6 > MAX_PATH_COVER_DWORDS {
            self.dropped += 1;
            return;
        }
        self.cover_data[self.cover_len] = x.to_bits();
        self.cover_data[self.cover_len + 1] = y.to_bits();
        self.cover_data[self.cover_len + 2] = r.to_bits();
        self.cover_data[self.cover_len + 3] = g.to_bits();
        self.cover_data[self.cover_len + 4] = b.to_bits();
        self.cover_data[self.cover_len + 5] = a.to_bits();
        self.cover_len += 6;
        self.cover_vertex_count += 1;
    }

    /// Emit a covering quad (two CCW triangles) for the path bounding box.
    fn push_cover_quad(
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

        self.push_cover_vertex(x0, y0, r, g, b, a);
        self.push_cover_vertex(x0, y1, r, g, b, a);
        self.push_cover_vertex(x1, y0, r, g, b, a);
        self.push_cover_vertex(x1, y0, r, g, b, a);
        self.push_cover_vertex(x0, y1, r, g, b, a);
        self.push_cover_vertex(x1, y1, r, g, b, a);
    }
}

// ── Clip rectangle (f32, NDC-space) ─────────────────────────────────────
// render/scene_render.rs has an independent i32 variant for physical pixel
// clipping — intentionally separate coordinate systems.

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
    image_batch: &mut ImageBatch,
    path_batch: &mut PathBatch,
    glyphs_data: &[u8],
    atlas: &GlyphAtlas,
    ascent: u32,
) {
    batch.clear();
    text_batch.clear();
    image_batch.clear();
    path_batch.clear();

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
        image_batch,
        path_batch,
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
    image_batch: &mut ImageBatch,
    path_batch: &mut PathBatch,
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
    match node.content {
        Content::Glyphs {
            color,
            glyphs: glyph_ref,
            glyph_count,
            ..
        } => {
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
        Content::Image {
            data,
            src_width,
            src_height,
        } => {
            if data.length > 0 && src_width > 0 && src_height > 0 {
                image_batch.push(ImageQuad {
                    x: abs_x,
                    y: abs_y,
                    w,
                    h,
                    data_offset: data.offset,
                    data_length: data.length,
                    src_width,
                    src_height,
                });
            }
        }
        Content::Path {
            color,
            fill_rule,
            contours,
        } => {
            if contours.length > 0 {
                emit_path(
                    path_batch,
                    glyphs_data,
                    contours,
                    color,
                    fill_rule,
                    abs_x,
                    abs_y,
                    w,
                    h,
                    scale,
                    vw,
                    vh,
                );
            }
        }
        Content::None => {}
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
            image_batch,
            path_batch,
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
    clip: ClipRect,
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

            // Cull glyphs entirely outside the clip rect.
            if gx + gw <= clip.x
                || gy + gh <= clip.y
                || gx >= clip.x + clip.w
                || gy >= clip.y + clip.h
            {
                pen_x += (sg.x_advance as f32) * scale;
                continue;
            }

            let u0 = entry.u as f32 / atlas_w;
            let v0 = entry.v as f32 / atlas_h;
            let u1 = (entry.u as f32 + entry.width as f32) / atlas_w;
            let v1 = (entry.v as f32 + entry.height as f32) / atlas_h;

            text_batch.push_textured_quad(gx, gy, gw, gh, vw, vh, u0, v0, u1, v1, r, g, b, a);
        }

        pen_x += (sg.x_advance as f32) * scale;
    }
}

// ── Path emission (stencil-then-cover geometry) ─────────────────────

/// Read an f32 from a byte slice at the given offset (little-endian).
#[inline]
fn read_f32_le(data: &[u8], offset: usize) -> f32 {
    if offset + 4 > data.len() {
        return 0.0;
    }
    f32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a u32 from a byte slice at the given offset (little-endian).
#[inline]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() {
        return u32::MAX;
    }
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Flatten a cubic Bezier to line segments via de Casteljau subdivision.
/// Appends to `points` (x, y pairs in logical pixels).
fn flatten_cubic(
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    points: &mut [(f32, f32)],
    count: &mut usize,
    depth: u32,
) {
    if *count >= points.len() || depth >= 10 {
        if *count < points.len() {
            points[*count] = (x3, y3);
            *count += 1;
        }
        return;
    }

    // Flatness test: max deviation of control points from chord.
    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x0) * dy - (c1y - y0) * dx).abs();
    let d2 = ((c2x - x0) * dy - (c2y - y0) * dx).abs();
    let max_d = if d1 > d2 { d1 } else { d2 };
    let chord_sq = dx * dx + dy * dy;
    // threshold: 0.5 logical pixels
    let threshold = 0.5;

    if max_d * max_d <= threshold * threshold * chord_sq || chord_sq < 0.001 {
        points[*count] = (x3, y3);
        *count += 1;
        return;
    }

    // De Casteljau at t=0.5.
    let m01x = (x0 + c1x) * 0.5;
    let m01y = (y0 + c1y) * 0.5;
    let m12x = (c1x + c2x) * 0.5;
    let m12y = (c1y + c2y) * 0.5;
    let m23x = (c2x + x3) * 0.5;
    let m23y = (c2y + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;

    flatten_cubic(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        points,
        count,
        depth + 1,
    );
    flatten_cubic(
        mx,
        my,
        m123x,
        m123y,
        m23x,
        m23y,
        x3,
        y3,
        points,
        count,
        depth + 1,
    );
}

/// Parse path commands from the data buffer, flatten to line segments,
/// then generate triangle fan vertices for stencil-then-cover rendering.
///
/// The triangle fan radiates from the centroid of the path's bounding box.
/// Each edge segment becomes a triangle: (centroid, p[i], p[i+1]).
/// Self-intersections are handled correctly by the stencil winding count.
#[allow(clippy::too_many_arguments)]
fn emit_path(
    path_batch: &mut PathBatch,
    data_buf: &[u8],
    contours: scene::DataRef,
    color: scene::Color,
    _fill_rule: FillRule,
    node_x: f32,
    node_y: f32,
    node_w: f32,
    node_h: f32,
    scale: f32,
    vw: f32,
    vh: f32,
) {
    let offset = contours.offset as usize;
    let end = offset + contours.length as usize;
    if end > data_buf.len() {
        return;
    }
    let data = &data_buf[offset..end];

    // Pass 1: Parse path commands into flattened line segments.
    // 256 points × 8 bytes = 2 KiB — safe for 16 KiB stack.
    const MAX_POINTS: usize = 256;
    let mut points = [(0.0f32, 0.0f32); MAX_POINTS];
    let mut point_count: usize = 0;
    let mut cur_x: f32 = 0.0;
    let mut cur_y: f32 = 0.0;
    let mut contour_start_x: f32 = 0.0;
    let mut contour_start_y: f32 = 0.0;

    let mut pos: usize = 0;
    while pos + 4 <= data.len() {
        let tag = read_u32_le(data, pos);
        match tag {
            PATH_MOVE_TO => {
                if pos + 12 > data.len() {
                    break;
                }
                cur_x = read_f32_le(data, pos + 4);
                cur_y = read_f32_le(data, pos + 8);
                contour_start_x = cur_x;
                contour_start_y = cur_y;
                if point_count < MAX_POINTS {
                    points[point_count] = (cur_x, cur_y);
                    point_count += 1;
                }
                pos += 12;
            }
            PATH_LINE_TO => {
                if pos + 12 > data.len() {
                    break;
                }
                cur_x = read_f32_le(data, pos + 4);
                cur_y = read_f32_le(data, pos + 8);
                if point_count < MAX_POINTS {
                    points[point_count] = (cur_x, cur_y);
                    point_count += 1;
                }
                pos += 12;
            }
            PATH_CUBIC_TO => {
                if pos + 28 > data.len() {
                    break;
                }
                let c1x = read_f32_le(data, pos + 4);
                let c1y = read_f32_le(data, pos + 8);
                let c2x = read_f32_le(data, pos + 12);
                let c2y = read_f32_le(data, pos + 16);
                let x3 = read_f32_le(data, pos + 20);
                let y3 = read_f32_le(data, pos + 24);
                flatten_cubic(
                    cur_x,
                    cur_y,
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x3,
                    y3,
                    &mut points,
                    &mut point_count,
                    0,
                );
                cur_x = x3;
                cur_y = y3;
                pos += 28;
            }
            PATH_CLOSE => {
                // Close back to contour start.
                if point_count < MAX_POINTS
                    && (cur_x != contour_start_x || cur_y != contour_start_y)
                {
                    points[point_count] = (contour_start_x, contour_start_y);
                    point_count += 1;
                }
                cur_x = contour_start_x;
                cur_y = contour_start_y;
                pos += 4;
            }
            _ => break, // Unknown command.
        }
    }

    if point_count < 3 {
        return; // Need at least 3 points for a triangle.
    }

    // Compute centroid (average of all points).
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    for i in 0..point_count {
        cx += points[i].0;
        cy += points[i].1;
    }
    cx /= point_count as f32;
    cy /= point_count as f32;

    // Convert centroid to NDC (physical pixels via scale, then NDC).
    let cx_px = node_x + cx * scale;
    let cy_px = node_y + cy * scale;
    let cx_ndc = cx_px / vw * 2.0 - 1.0;
    let cy_ndc = 1.0 - cy_px / vh * 2.0;

    // Emit triangle fan: for each consecutive pair (p[i], p[i+1]),
    // emit triangle (centroid, p[i], p[i+1]).
    for i in 0..point_count - 1 {
        let (ax, ay) = points[i];
        let (bx, by) = points[i + 1];

        let ax_px = node_x + ax * scale;
        let ay_px = node_y + ay * scale;
        let bx_px = node_x + bx * scale;
        let by_px = node_y + by * scale;

        let ax_ndc = ax_px / vw * 2.0 - 1.0;
        let ay_ndc = 1.0 - ay_px / vh * 2.0;
        let bx_ndc = bx_px / vw * 2.0 - 1.0;
        let by_ndc = 1.0 - by_px / vh * 2.0;

        // Emit CCW triangle: (centroid, p[i+1], p[i]).
        // Reversed from natural order because path points go CW on screen,
        // and ANGLE/Metal culls CW triangles in NDC despite cull_face=NONE.
        path_batch.push_fan_vertex(cx_ndc, cy_ndc);
        path_batch.push_fan_vertex(bx_ndc, by_ndc);
        path_batch.push_fan_vertex(ax_ndc, ay_ndc);
    }

    // Emit covering quad (bounding box of the path node).
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = color.a as f32 / 255.0;
    path_batch.push_cover_quad(node_x, node_y, node_w, node_h, vw, vh, r, g, b, a);
}
