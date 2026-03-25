//! Solid color quad geometry generation for GPU rendering.
//!
//! `QuadBatch` accumulates colored rectangles as pairs of CCW triangles
//! in NDC coordinates, ready for upload to a color vertex buffer.
//!
//! Supports dual-batch mode for clip path rendering: non-clipped quads
//! are stored in the lower half, clipped quads in the upper half.
//! The render loop draws non-clipped quads first, writes the clip
//! stencil, then draws clipped quads with stencil test enabled.

/// Maximum number of colored quads per frame (shared between clipped
/// and non-clipped). Each half gets MAX_QUADS/2.
const MAX_QUADS: usize = 256;

/// Bytes per color vertex: x(f32) + y(f32) + r(f32) + g(f32) + b(f32) + a(f32) = 24.
pub const VERTEX_STRIDE: u32 = 24;

/// Maximum color vertex data in bytes.
pub const MAX_VERTEX_BYTES: usize = MAX_QUADS * 6 * VERTEX_STRIDE as usize;

/// Maximum vertex data in u32 DWORDs (6 floats per vertex, 6 vertices per quad).
const MAX_VERTEX_DWORDS: usize = MAX_QUADS * 6 * 6;

/// Split point: non-clipped uses [0..CLIP_BASE), clipped uses [CLIP_BASE..MAX).
const CLIP_BASE: usize = MAX_VERTEX_DWORDS / 2;

/// Accumulated colored quads from a scene walk.
pub struct QuadBatch {
    /// Vertex data as f32 bit representations (x, y, r, g, b, a per vertex).
    vertex_data: [u32; MAX_VERTEX_DWORDS],
    /// Current write offset in u32 DWORDs (non-clipped region).
    vertex_len: usize,
    /// Number of non-clipped vertices accumulated.
    pub vertex_count: u32,
    /// Current write offset in u32 DWORDs (clipped region, starts at CLIP_BASE).
    clip_len: usize,
    /// Number of clipped vertices accumulated.
    pub clip_vertex_count: u32,
    /// Number of vertices silently dropped due to batch overflow.
    dropped: u32,
}

impl QuadBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_VERTEX_DWORDS],
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

    /// Non-clipped vertex data (lower region).
    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    /// Clipped vertex data (upper region).
    pub fn as_clip_vertex_data(&self) -> &[u32] {
        &self.vertex_data[CLIP_BASE..self.clip_len]
    }

    pub fn dropped_count(&self) -> u32 {
        self.dropped
    }

    fn push_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 6 > CLIP_BASE {
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

    fn push_clip_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.clip_len + 6 > MAX_VERTEX_DWORDS {
            self.dropped += 1;
            return;
        }
        self.vertex_data[self.clip_len] = x.to_bits();
        self.vertex_data[self.clip_len + 1] = y.to_bits();
        self.vertex_data[self.clip_len + 2] = r.to_bits();
        self.vertex_data[self.clip_len + 3] = g.to_bits();
        self.vertex_data[self.clip_len + 4] = b.to_bits();
        self.vertex_data[self.clip_len + 5] = a.to_bits();
        self.clip_len += 6;
        self.clip_vertex_count += 1;
    }

    /// Emit a colored quad as two CCW triangles (6 vertices) in NDC.
    pub fn push_quad(
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

    /// Emit a colored quad into the clipped region.
    pub fn push_clip_quad(
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

        self.push_clip_vertex(x0, y0, r, g, b, a);
        self.push_clip_vertex(x0, y1, r, g, b, a);
        self.push_clip_vertex(x1, y0, r, g, b, a);
        self.push_clip_vertex(x1, y0, r, g, b, a);
        self.push_clip_vertex(x0, y1, r, g, b, a);
        self.push_clip_vertex(x1, y1, r, g, b, a);
    }
}
