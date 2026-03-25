//! Stencil-then-cover path rendering for GPU scene walk.
//!
//! `PathBatch` accumulates triangle fan vertices (stencil write pass)
//! and covering quads (stencil test pass) for vector path rendering.
//! Includes cubic Bezier flattening and path command parsing.

use scene::{FillRule, PATH_CLOSE, PATH_CUBIC_TO, PATH_LINE_TO, PATH_MOVE_TO};

/// Maximum triangle fan vertices per frame (for stencil-then-cover).
/// 1024 fan triangles = 3072 vertices should handle complex paths.
const MAX_PATH_FAN_VERTS: usize = 3072;

/// Max fan vertex data in u32 DWORDs (6 floats per vertex = x, y, r, g, b, a).
/// Uses the same color vertex layout (VERTEX_STRIDE=24) for VE reuse.
pub(crate) const MAX_PATH_FAN_DWORDS: usize = MAX_PATH_FAN_VERTS * 6;

/// Maximum covering quads per frame (one per path node).
const MAX_PATH_COVERS: usize = 16;

/// Max cover vertex data (6 verts x 6 floats per covering quad x MAX_PATH_COVERS).
pub(crate) const MAX_PATH_COVER_DWORDS: usize = MAX_PATH_COVERS * 6 * 6;

/// Maximum triangle fan vertices for clip paths per frame.
/// Clip paths tend to be simple shapes (rounded rects, circles): 512 vertices is ample.
const MAX_CLIP_FAN_VERTS: usize = 512;

/// Max clip fan vertex data in u32 DWORDs (same 6-float layout as path fan).
pub(crate) const MAX_CLIP_FAN_DWORDS: usize = MAX_CLIP_FAN_VERTS * 6;

/// Accumulated path rendering data from a scene walk.
///
/// The triangle fan vertices go into the stencil write pass (position only,
/// drawn with PIPE_PRIM_TRIANGLES -- we decompose the fan ourselves).
/// The cover quads go into the stencil test pass (position + color).
/// Clip fan vertices go into a separate stencil write pass before all content
/// rendering (text, images, paths) to establish a clip region.
pub struct PathBatch {
    /// Fan triangle vertices: x, y per vertex (f32 pairs).
    fan_data: [u32; MAX_PATH_FAN_DWORDS],
    fan_len: usize,
    pub fan_vertex_count: u32,
    /// Cover quad vertices: x, y, r, g, b, a per vertex (6 floats).
    cover_data: [u32; MAX_PATH_COVER_DWORDS],
    cover_len: usize,
    pub cover_vertex_count: u32,
    /// Clip path fan vertices: rendered to stencil before all content,
    /// enabling stencil-test clipping for child nodes.
    clip_fan_data: [u32; MAX_CLIP_FAN_DWORDS],
    clip_fan_len: usize,
    pub clip_fan_vertex_count: u32,
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
            clip_fan_data: [0; MAX_CLIP_FAN_DWORDS],
            clip_fan_len: 0,
            clip_fan_vertex_count: 0,
            dropped: 0,
        }
    }

    pub fn clear(&mut self) {
        self.fan_len = 0;
        self.fan_vertex_count = 0;
        self.cover_len = 0;
        self.cover_vertex_count = 0;
        self.clip_fan_len = 0;
        self.clip_fan_vertex_count = 0;
        self.dropped = 0;
    }

    pub fn as_fan_data(&self) -> &[u32] {
        &self.fan_data[..self.fan_len]
    }

    pub fn as_cover_data(&self) -> &[u32] {
        &self.cover_data[..self.cover_len]
    }

    pub fn as_clip_fan_data(&self) -> &[u32] {
        &self.clip_fan_data[..self.clip_fan_len]
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
        self.fan_data[self.fan_len + 2] = 0; // r (unused -- colormask=0 in stencil pass)
        self.fan_data[self.fan_len + 3] = 0; // g
        self.fan_data[self.fan_len + 4] = 0; // b
        self.fan_data[self.fan_len + 5] = 1.0f32.to_bits(); // a = 1.0 (non-zero for ANGLE)
        self.fan_len += 6;
        self.fan_vertex_count += 1;
    }

    /// Push a clip fan vertex (same layout as fan vertex, but into the clip
    /// fan buffer for pre-content stencil write).
    fn push_clip_fan_vertex(&mut self, x: f32, y: f32) {
        if self.clip_fan_len + 6 > MAX_CLIP_FAN_DWORDS {
            self.dropped += 1;
            return;
        }
        self.clip_fan_data[self.clip_fan_len] = x.to_bits();
        self.clip_fan_data[self.clip_fan_len + 1] = y.to_bits();
        self.clip_fan_data[self.clip_fan_len + 2] = 0; // r (unused)
        self.clip_fan_data[self.clip_fan_len + 3] = 0; // g
        self.clip_fan_data[self.clip_fan_len + 4] = 0; // b
        self.clip_fan_data[self.clip_fan_len + 5] = 1.0f32.to_bits(); // a = 1.0
        self.clip_fan_len += 6;
        self.clip_fan_vertex_count += 1;
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

// -- Path emission helpers ------------------------------------------------

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
/// Appends to `points` (x, y pairs in points).
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
    // threshold: 0.5 points
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

// -- Path parsing ---------------------------------------------------------

/// Maximum flattened points per path (256 points × 8 bytes = 2 KiB, safe for 16 KiB stack).
const MAX_POINTS: usize = 256;

/// Parse path commands from a raw byte slice into a flattened list of (x, y) points.
///
/// Fills `out_points` in-place. Returns the number of valid points written.
/// Points are in the path's local coordinate space (before `node_x/node_y` translation
/// and `scale` factor — callers apply those when converting to NDC).
fn parse_path_to_points(data: &[u8], out_points: &mut [(f32, f32); MAX_POINTS]) -> usize {
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
                    out_points[point_count] = (cur_x, cur_y);
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
                    out_points[point_count] = (cur_x, cur_y);
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
                    out_points,
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
                    out_points[point_count] = (contour_start_x, contour_start_y);
                    point_count += 1;
                }
                cur_x = contour_start_x;
                cur_y = contour_start_y;
                pos += 4;
            }
            _ => break, // Unknown command.
        }
    }

    point_count
}

/// Emit triangle fan vertices from a point list into `path_batch` (Content::Path stencil write).
fn emit_fan_vertices(
    path_batch: &mut PathBatch,
    points: &[(f32, f32); MAX_POINTS],
    point_count: usize,
    node_x: f32,
    node_y: f32,
    scale: f32,
    vw: f32,
    vh: f32,
) {
    // Compute centroid (average of all points).
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    for i in 0..point_count {
        cx += points[i].0;
        cy += points[i].1;
    }
    cx /= point_count as f32;
    cy /= point_count as f32;

    let cx_px = node_x + cx * scale;
    let cy_px = node_y + cy * scale;
    let cx_ndc = cx_px / vw * 2.0 - 1.0;
    let cy_ndc = 1.0 - cy_px / vh * 2.0;

    for i in 0..point_count - 1 {
        let (ax, ay) = points[i];
        let (bx, by) = points[i + 1];

        let ax_ndc = (node_x + ax * scale) / vw * 2.0 - 1.0;
        let ay_ndc = 1.0 - (node_y + ay * scale) / vh * 2.0;
        let bx_ndc = (node_x + bx * scale) / vw * 2.0 - 1.0;
        let by_ndc = 1.0 - (node_y + by * scale) / vh * 2.0;

        // CCW triangle: (centroid, p[i+1], p[i]).
        // Reversed because path points go CW on screen, and ANGLE/Metal culls
        // CW triangles in NDC despite cull_face=NONE.
        path_batch.push_fan_vertex(cx_ndc, cy_ndc);
        path_batch.push_fan_vertex(bx_ndc, by_ndc);
        path_batch.push_fan_vertex(ax_ndc, ay_ndc);
    }
}

/// Emit clip fan vertices from a point list into `path_batch` (clip stencil write).
fn emit_clip_fan_vertices(
    path_batch: &mut PathBatch,
    points: &[(f32, f32); MAX_POINTS],
    point_count: usize,
    node_x: f32,
    node_y: f32,
    scale: f32,
    vw: f32,
    vh: f32,
) {
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    for i in 0..point_count {
        cx += points[i].0;
        cy += points[i].1;
    }
    cx /= point_count as f32;
    cy /= point_count as f32;

    let cx_ndc = (node_x + cx * scale) / vw * 2.0 - 1.0;
    let cy_ndc = 1.0 - (node_y + cy * scale) / vh * 2.0;

    for i in 0..point_count - 1 {
        let (ax, ay) = points[i];
        let (bx, by) = points[i + 1];

        let ax_ndc = (node_x + ax * scale) / vw * 2.0 - 1.0;
        let ay_ndc = 1.0 - (node_y + ay * scale) / vh * 2.0;
        let bx_ndc = (node_x + bx * scale) / vw * 2.0 - 1.0;
        let by_ndc = 1.0 - (node_y + by * scale) / vh * 2.0;

        path_batch.push_clip_fan_vertex(cx_ndc, cy_ndc);
        path_batch.push_clip_fan_vertex(bx_ndc, by_ndc);
        path_batch.push_clip_fan_vertex(ax_ndc, ay_ndc);
    }
}

// -- Public path emission -------------------------------------------------

/// Parse path commands from the data buffer, flatten to line segments,
/// then generate triangle fan vertices for stencil-then-cover rendering.
///
/// The triangle fan radiates from the centroid of the path's bounding box.
/// Each edge segment becomes a triangle: (centroid, p[i], p[i+1]).
/// Self-intersections are handled correctly by the stencil winding count.
#[allow(clippy::too_many_arguments)]
pub fn emit_path(
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

    let mut points = [(0.0f32, 0.0f32); MAX_POINTS];
    let point_count = parse_path_to_points(data, &mut points);

    if point_count < 3 {
        return; // Need at least 3 points for a triangle.
    }

    emit_fan_vertices(
        path_batch,
        &points,
        point_count,
        node_x,
        node_y,
        scale,
        vw,
        vh,
    );

    // Emit covering quad (bounding box of the path node).
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = color.a as f32 / 255.0;
    path_batch.push_cover_quad(node_x, node_y, node_w, node_h, vw, vh, r, g, b, a);
}

/// Parse clip path commands from the data buffer and generate clip fan vertices.
///
/// Clip fan vertices are rendered to the stencil buffer before all content
/// rendering, so that subsequent text/image/path draws are clipped to the
/// path shape (stencil test: pass if stencil != 0).
#[allow(clippy::too_many_arguments)]
pub fn emit_clip_fan(
    path_batch: &mut PathBatch,
    data_buf: &[u8],
    clip_path: scene::DataRef,
    node_x: f32,
    node_y: f32,
    scale: f32,
    vw: f32,
    vh: f32,
) {
    let offset = clip_path.offset as usize;
    let end = offset + clip_path.length as usize;
    if end > data_buf.len() {
        return;
    }
    let data = &data_buf[offset..end];

    let mut points = [(0.0f32, 0.0f32); MAX_POINTS];
    let point_count = parse_path_to_points(data, &mut points);

    if point_count < 3 {
        return;
    }

    emit_clip_fan_vertices(
        path_batch,
        &points,
        point_count,
        node_x,
        node_y,
        scale,
        vw,
        vh,
    );
}
