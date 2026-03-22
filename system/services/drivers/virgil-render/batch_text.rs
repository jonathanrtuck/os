//! Textured quad rendering for glyph atlas text.
//!
//! `TexturedBatch` accumulates glyph quads with texture coordinates
//! and per-vertex color, ready for upload to a textured vertex buffer.
//!
//! Supports dual-batch mode for clip path rendering: non-clipped glyphs
//! use the lower half, clipped glyphs use the upper half.

/// Maximum number of textured quads (glyphs) per frame.
/// 4096 glyphs covers a full 1024x768 screen at ~80 chars x 48 lines
/// with headroom for title, clock, and other UI text.
const MAX_TEXT_QUADS: usize = 4096;

/// Bytes per textured vertex: x(f32) + y(f32) + u(f32) + v(f32) + r + g + b + a = 32.
pub const TEXTURED_VERTEX_STRIDE: u32 = 32;

/// Maximum textured vertex data in bytes (glyphs only).
pub const MAX_TEXTURED_VERTEX_BYTES: usize = MAX_TEXT_QUADS * 6 * TEXTURED_VERTEX_STRIDE as usize;

/// Maximum textured vertex data in u32 DWORDs (8 floats per vertex, 6 vertices per quad).
const MAX_TEXTURED_DWORDS: usize = MAX_TEXT_QUADS * 6 * 8;

/// Split point: non-clipped uses [0..CLIP_BASE), clipped uses [CLIP_BASE..MAX).
const CLIP_BASE: usize = MAX_TEXTURED_DWORDS / 2;

/// Accumulated textured quads from glyph rendering.
pub struct TexturedBatch {
    /// Vertex data: x, y, u, v, r, g, b, a per vertex (8 floats = 32 bytes).
    vertex_data: [u32; MAX_TEXTURED_DWORDS],
    /// Non-clipped write offset (grows up from 0).
    vertex_len: usize,
    /// Number of non-clipped vertices.
    pub vertex_count: u32,
    /// Clipped write offset (grows up from CLIP_BASE).
    clip_len: usize,
    /// Number of clipped vertices.
    pub clip_vertex_count: u32,
    /// Number of vertices silently dropped due to batch overflow.
    dropped: u32,
}

impl TexturedBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_TEXTURED_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
            clip_len: CLIP_BASE,
            clip_vertex_count: 0,
            dropped: 0,
        }
    }

    pub fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
        self.clip_len = CLIP_BASE;
        self.clip_vertex_count = 0;
        self.dropped = 0;
    }

    /// Non-clipped glyph vertex data.
    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    /// Clipped glyph vertex data.
    pub fn as_clip_vertex_data(&self) -> &[u32] {
        &self.vertex_data[CLIP_BASE..self.clip_len]
    }

    pub fn dropped_count(&self) -> u32 {
        self.dropped
    }

    /// Push a textured vertex: position(f32x2) + texcoord(f32x2) + color(f32x4).
    fn push_vertex(&mut self, x: f32, y: f32, u: f32, v: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 8 > CLIP_BASE {
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

    fn push_clip_vertex(&mut self, x: f32, y: f32, u: f32, v: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.clip_len + 8 > MAX_TEXTURED_DWORDS {
            self.dropped += 1;
            return;
        }
        self.vertex_data[self.clip_len] = x.to_bits();
        self.vertex_data[self.clip_len + 1] = y.to_bits();
        self.vertex_data[self.clip_len + 2] = u.to_bits();
        self.vertex_data[self.clip_len + 3] = v.to_bits();
        self.vertex_data[self.clip_len + 4] = r.to_bits();
        self.vertex_data[self.clip_len + 5] = g.to_bits();
        self.vertex_data[self.clip_len + 6] = b.to_bits();
        self.vertex_data[self.clip_len + 7] = a.to_bits();
        self.clip_len += 8;
        self.clip_vertex_count += 1;
    }

    /// Emit a textured quad as two CCW triangles (6 vertices) in NDC.
    /// `u0,v0,u1,v1` are atlas texcoords (0.0-1.0).
    pub fn push_textured_quad(
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

    /// Emit a textured quad into the clipped region.
    pub fn push_clip_textured_quad(
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

        self.push_clip_vertex(x0, y0, u0, v0, r, g, b, a);
        self.push_clip_vertex(x0, y1, u0, v1, r, g, b, a);
        self.push_clip_vertex(x1, y0, u1, v0, r, g, b, a);
        self.push_clip_vertex(x1, y0, u1, v0, r, g, b, a);
        self.push_clip_vertex(x0, y1, u0, v1, r, g, b, a);
        self.push_clip_vertex(x1, y1, u1, v1, r, g, b, a);
    }
}
