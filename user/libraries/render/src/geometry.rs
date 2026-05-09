//! Backend-independent geometry primitives for scene graph rendering.
//!
//! Pure math: clip rectangles, vertex emission (NDC quads from point-space
//! coordinates), atlas rectangle packing, shader parameter packing, and
//! pointer coordinate scaling. No GPU API, no allocation, no syscalls.
//!
//! Every display driver needs these operations identically — extracting them
//! here prevents reimplementation when a second backend arrives.

use alloc::vec::Vec;

// ── Clip rectangle ──────────────────────────────────────────────────────

/// Clip rectangle in points (pre-scale).
#[derive(Clone, Copy)]
pub struct ClipRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl ClipRect {
    pub fn intersect(&self, other: &ClipRect) -> ClipRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = (self.x + self.w).min(other.x + other.w);
        let b = (self.y + self.h).min(other.y + other.h);

        ClipRect {
            x,
            y,
            w: (r - x).max(0.0),
            h: (b - y).max(0.0),
        }
    }

    /// Convert to integer pixel-space scissor rect.
    pub fn to_pixel_scissor(&self, scale: f32) -> (u16, u16, u16, u16) {
        let w_px = self.w * scale;
        let h_px = self.h * scale;

        (
            (self.x * scale) as u16,
            (self.y * scale) as u16,
            // Manual ceil: if fractional part > 0, round up.
            (w_px as u16) + if w_px > w_px as u16 as f32 { 1 } else { 0 },
            (h_px as u16) + if h_px > h_px as u16 as f32 { 1 } else { 0 },
        )
    }
}

// ── Image atlas ─────────────────────────────────────────────────────────

/// Per-frame image atlas packer. Each image uploads to the next available
/// sub-rectangle within a shared texture. Simple row-based packing: images
/// fill left-to-right in the current row. When an image doesn't fit
/// horizontally, advance to a new row (height = tallest image in the
/// previous row). Reset at frame start.
pub struct ImageAtlas {
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    dimension: u32,
}

impl ImageAtlas {
    pub fn new(dimension: u32) -> Self {
        Self {
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            dimension,
        }
    }

    /// Reset for a new frame.
    pub fn reset(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_height = 0;
    }

    /// Reserve a sub-rectangle for an image. Returns (x, y) atlas offset
    /// in pixels, or None if the image doesn't fit.
    pub fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let dim = self.dimension;

        if w > dim || h > dim {
            return None;
        }

        // Doesn't fit in current row — start a new one.
        if self.cursor_x + w > dim {
            self.cursor_y += self.row_height;
            self.cursor_x = 0;
            self.row_height = 0;
        }

        // Doesn't fit vertically — atlas is full.
        if self.cursor_y + h > dim {
            return None;
        }

        let x = self.cursor_x;
        let y = self.cursor_y;

        self.cursor_x += w;

        if h > self.row_height {
            self.row_height = h;
        }

        Some((x, y))
    }
}

// ── Vertex emission ─────────────────────────────────────────────────────
//
// All emit_* functions produce 6 vertices (two triangles) in the standard
// vertex layout: [ndc_x, ndc_y, texcoord_u, texcoord_v, r, g, b, a] — 8
// floats per vertex, 32 bytes. This layout is the natural GPU vertex for
// any backend that draws textured/colored triangles.

/// Bytes per vertex in the standard layout (8 × f32 = 32).
pub const VERTEX_BYTES: usize = 8 * 4;

/// Convert a point-space coordinate to NDC given viewport size in pixels
/// and scale factor.
#[inline]
fn to_ndc_x(x_pt: f32, scale: f32, viewport_w: f32) -> f32 {
    (x_pt * scale / viewport_w) * 2.0 - 1.0
}

/// Convert a point-space Y coordinate to NDC (Y-down → Y-up flip).
#[inline]
fn to_ndc_y(y_pt: f32, scale: f32, viewport_h: f32) -> f32 {
    1.0 - (y_pt * scale / viewport_h) * 2.0
}

/// Write 6 vertex structs (two triangles) to a byte buffer.
#[inline]
fn push_quad(buf: &mut Vec<u8>, verts: &[[f32; 8]; 6]) {
    for v in verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a solid-color quad (6 vertices) into the vertex buffer.
pub fn emit_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = to_ndc_x(x, scale, vw);
    let r_ndc = to_ndc_x(x + w, scale, vw);
    let t = to_ndc_y(y, scale, vh);
    let b_ndc = to_ndc_y(y + h, scale, vh);

    push_quad(
        buf,
        &[
            [l, t, 0.0, 0.0, r, g, b, a],
            [r_ndc, t, 1.0, 0.0, r, g, b, a],
            [l, b_ndc, 0.0, 1.0, r, g, b, a],
            [r_ndc, t, 1.0, 0.0, r, g, b, a],
            [r_ndc, b_ndc, 1.0, 1.0, r, g, b, a],
            [l, b_ndc, 0.0, 1.0, r, g, b, a],
        ],
    );
}

/// Push a textured quad (6 vertices) with custom UV coordinates.
pub fn emit_textured_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = to_ndc_x(x, scale, vw);
    let r_ndc = to_ndc_x(x + w, scale, vw);
    let t = to_ndc_y(y, scale, vh);
    let b_ndc = to_ndc_y(y + h, scale, vh);

    push_quad(
        buf,
        &[
            [l, t, u0, v0, r, g, b, a],
            [r_ndc, t, u1, v0, r, g, b, a],
            [l, b_ndc, u0, v1, r, g, b, a],
            [r_ndc, t, u1, v0, r, g, b, a],
            [r_ndc, b_ndc, u1, v1, r, g, b, a],
            [l, b_ndc, u0, v1, r, g, b, a],
        ],
    );
}

/// Push a solid-color quad with an affine transform applied to each vertex.
/// The transform maps local coordinates (0,0)->(w,h) to parent space at
/// (ox, oy).
pub fn emit_transformed_quad(
    buf: &mut Vec<u8>,
    ox: f32,
    oy: f32,
    w: f32,
    h: f32,
    t: &scene::AffineTransform,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let corners = [(0.0f32, 0.0f32), (w, 0.0), (0.0, h), (w, h)];
    let mut tc = [(0.0f32, 0.0f32); 4];

    for (i, &(lx, ly)) in corners.iter().enumerate() {
        let (tx, ty) = t.transform_point(lx, ly);
        tc[i] = (ox + tx, oy + ty);
    }

    let ndc =
        |px: f32, py: f32| -> (f32, f32) { (to_ndc_x(px, scale, vw), to_ndc_y(py, scale, vh)) };
    let (x0, y0) = ndc(tc[0].0, tc[0].1);
    let (x1, y1) = ndc(tc[1].0, tc[1].1);
    let (x2, y2) = ndc(tc[2].0, tc[2].1);
    let (x3, y3) = ndc(tc[3].0, tc[3].1);

    push_quad(
        buf,
        &[
            [x0, y0, 0.0, 0.0, r, g, b, a],
            [x1, y1, 1.0, 0.0, r, g, b, a],
            [x2, y2, 0.0, 1.0, r, g, b, a],
            [x1, y1, 1.0, 0.0, r, g, b, a],
            [x3, y3, 1.0, 1.0, r, g, b, a],
            [x2, y2, 0.0, 1.0, r, g, b, a],
        ],
    );
}

/// Push a rounded-rect quad. texCoord carries local pixel coordinates
/// relative to the rect center (the SDF evaluates per-pixel in local space).
pub fn emit_rounded_rect_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = to_ndc_x(x, scale, vw);
    let r_ndc = to_ndc_x(x + w, scale, vw);
    let t = to_ndc_y(y, scale, vh);
    let b_ndc = to_ndc_y(y + h, scale, vh);
    let half_w_px = w * scale * 0.5;
    let half_h_px = h * scale * 0.5;

    push_quad(
        buf,
        &[
            [l, t, -half_w_px, -half_h_px, r, g, b, a],
            [r_ndc, t, half_w_px, -half_h_px, r, g, b, a],
            [l, b_ndc, -half_w_px, half_h_px, r, g, b, a],
            [r_ndc, t, half_w_px, -half_h_px, r, g, b, a],
            [r_ndc, b_ndc, half_w_px, half_h_px, r, g, b, a],
            [l, b_ndc, -half_w_px, half_h_px, r, g, b, a],
        ],
    );
}

/// Push a rounded-rect quad with an affine transform applied to vertex
/// positions. NDC positions are transformed; texCoords stay in local
/// (pre-transform) pixel space. Because the affine is linear and GPU
/// barycentric interpolation is linear, each fragment receives the correct
/// local-space coordinate — no shader changes needed.
pub fn emit_transformed_rounded_rect_quad(
    buf: &mut Vec<u8>,
    ox: f32,
    oy: f32,
    w: f32,
    h: f32,
    t: &scene::AffineTransform,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let corners = [
        (0.0f32, 0.0f32), // top-left
        (w, 0.0),         // top-right
        (0.0, h),         // bottom-left
        (w, h),           // bottom-right
    ];
    let mut ndc = [(0.0f32, 0.0f32); 4];

    for (i, &(lx, ly)) in corners.iter().enumerate() {
        let (tx, ty) = t.transform_point(lx, ly);
        let px = ox + tx;
        let py = oy + ty;
        ndc[i] = (to_ndc_x(px, scale, vw), to_ndc_y(py, scale, vh));
    }

    let half_w_px = w * scale * 0.5;
    let half_h_px = h * scale * 0.5;

    push_quad(
        buf,
        &[
            [ndc[0].0, ndc[0].1, -half_w_px, -half_h_px, r, g, b, a],
            [ndc[1].0, ndc[1].1, half_w_px, -half_h_px, r, g, b, a],
            [ndc[2].0, ndc[2].1, -half_w_px, half_h_px, r, g, b, a],
            [ndc[1].0, ndc[1].1, half_w_px, -half_h_px, r, g, b, a],
            [ndc[3].0, ndc[3].1, half_w_px, half_h_px, r, g, b, a],
            [ndc[2].0, ndc[2].1, -half_w_px, half_h_px, r, g, b, a],
        ],
    );
}

/// Emit a shadow quad (6 vertices) covering the shadow rect plus blur
/// padding. texCoord carries absolute pixel-space coordinates for the
/// fragment shader's Gaussian evaluation.
pub fn emit_shadow_quad(
    buf: &mut Vec<u8>,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    pad: f32,
    vw: f32,
    vh: f32,
    scale: f32,
) {
    // Quad extends beyond shadow rect by pad on all sides.
    let qx = sx - pad;
    let qy = sy - pad;
    let qw = sw + 2.0 * pad;
    let qh = sh + 2.0 * pad;
    let l = to_ndc_x(qx, scale, vw);
    let r = to_ndc_x(qx + qw, scale, vw);
    let t = to_ndc_y(qy, scale, vh);
    let b = to_ndc_y(qy + qh, scale, vh);
    // Pixel-space coordinates for the fragment shader's Gaussian evaluation.
    let px_l = qx * scale;
    let px_r = (qx + qw) * scale;
    let px_t = qy * scale;
    let px_b = (qy + qh) * scale;

    // Color fields unused by the shadow shader (reads from uniform buffer).
    push_quad(
        buf,
        &[
            [l, t, px_l, px_t, 0.0, 0.0, 0.0, 0.0],
            [r, t, px_r, px_t, 0.0, 0.0, 0.0, 0.0],
            [l, b, px_l, px_b, 0.0, 0.0, 0.0, 0.0],
            [r, t, px_r, px_t, 0.0, 0.0, 0.0, 0.0],
            [r, b, px_r, px_b, 0.0, 0.0, 0.0, 0.0],
            [l, b, px_l, px_b, 0.0, 0.0, 0.0, 0.0],
        ],
    );
}

// ── Shader parameter packing ────────────────────────────────────────────

/// Pack ShadowParams for an analytical Gaussian shadow shader.
///
/// Layout: { rect_min_x, rect_min_y, rect_max_x, rect_max_y,
///           color_r, color_g, color_b, color_a,
///           sigma, corner_radius, _pad0, _pad1 } (12 × f32 = 48 bytes).
pub fn pack_shadow_params(
    rect_min_x: f32,
    rect_min_y: f32,
    rect_max_x: f32,
    rect_max_y: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
    sigma: f32,
    corner_radius: f32,
) -> [u8; 48] {
    let mut buf = [0u8; 48];

    buf[0..4].copy_from_slice(&rect_min_x.to_le_bytes());
    buf[4..8].copy_from_slice(&rect_min_y.to_le_bytes());
    buf[8..12].copy_from_slice(&rect_max_x.to_le_bytes());
    buf[12..16].copy_from_slice(&rect_max_y.to_le_bytes());
    buf[16..20].copy_from_slice(&color_r.to_le_bytes());
    buf[20..24].copy_from_slice(&color_g.to_le_bytes());
    buf[24..28].copy_from_slice(&color_b.to_le_bytes());
    buf[28..32].copy_from_slice(&color_a.to_le_bytes());
    buf[32..36].copy_from_slice(&sigma.to_le_bytes());
    buf[36..40].copy_from_slice(&corner_radius.to_le_bytes());
    buf
}

/// Pack RoundedRectParams for an SDF rounded-rect shader.
///
/// Layout: { half_w, half_h, radius, border_w, border_r, border_g,
///           border_b, border_a } (8 × f32 = 32 bytes).
pub fn pack_rounded_rect_params(
    half_w: f32,
    half_h: f32,
    radius: f32,
    border_w: f32,
    border_r: f32,
    border_g: f32,
    border_b: f32,
    border_a: f32,
) -> [u8; 32] {
    let mut buf = [0u8; 32];

    buf[0..4].copy_from_slice(&half_w.to_le_bytes());
    buf[4..8].copy_from_slice(&half_h.to_le_bytes());
    buf[8..12].copy_from_slice(&radius.to_le_bytes());
    buf[12..16].copy_from_slice(&border_w.to_le_bytes());
    buf[16..20].copy_from_slice(&border_r.to_le_bytes());
    buf[20..24].copy_from_slice(&border_g.to_le_bytes());
    buf[24..28].copy_from_slice(&border_b.to_le_bytes());
    buf[28..32].copy_from_slice(&border_a.to_le_bytes());
    buf
}

/// Pack BlurParams for separable box-blur compute shaders.
///
/// Layout: { half_width: i32, region_w: i32, region_h: i32, _pad: i32 }
/// (4 × i32 = 16 bytes).
pub fn pack_blur_params(half_width: i32, region_w: i32, region_h: i32) -> [u8; 16] {
    let mut buf = [0u8; 16];

    buf[0..4].copy_from_slice(&half_width.to_le_bytes());
    buf[4..8].copy_from_slice(&region_w.to_le_bytes());
    buf[8..12].copy_from_slice(&region_h.to_le_bytes());
    buf
}

/// Pack CopyParams for color-space conversion compute shaders.
///
/// Layout: { src_x, src_y, dst_x, dst_y, width, height } (6 × i32 = 24
/// bytes).
pub fn pack_copy_params(
    src_x: i32,
    src_y: i32,
    dst_x: i32,
    dst_y: i32,
    width: i32,
    height: i32,
) -> [u8; 24] {
    let mut buf = [0u8; 24];

    buf[0..4].copy_from_slice(&src_x.to_le_bytes());
    buf[4..8].copy_from_slice(&src_y.to_le_bytes());
    buf[8..12].copy_from_slice(&dst_x.to_le_bytes());
    buf[12..16].copy_from_slice(&dst_y.to_le_bytes());
    buf[16..20].copy_from_slice(&width.to_le_bytes());
    buf[20..24].copy_from_slice(&height.to_le_bytes());
    buf
}

// ── Pointer coordinate scaling ──────────────────────────────────────────

/// Scale a raw pointer coordinate [0, 32767] to framebuffer pixels.
pub fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;

    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}
