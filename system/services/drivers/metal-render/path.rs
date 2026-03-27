//! Path parsing, cubic flattening, and stencil-cover rendering.

use alloc::vec::Vec;

use protocol::metal;

use crate::{
    scene_walk::{emit_quad, flush_solid_vertices},
    DSS_NONE, DSS_STENCIL_INVERT, DSS_STENCIL_TEST, DSS_STENCIL_WINDING, MAX_INLINE_BYTES,
    PIPE_SOLID, PIPE_STENCIL_WRITE, VERTEX_BYTES,
};

pub(crate) const MAX_PATH_POINTS: usize = 512;

/// Maximum number of contour boundaries tracked in a single path.
pub(crate) const MAX_CONTOURS: usize = 32;

/// Reusable heap buffer for path flattening. Shared across `walk_scene` and
/// `draw_path_stencil_cover` to keep the recursive `walk_scene` stack frame small
/// (~300 bytes per level instead of ~4400).
pub(crate) type PathPointsBuf = [(f32, f32); MAX_PATH_POINTS];

/// One-time warning for path truncation.
static mut PATH_TRUNCATION_WARNED: bool = false;

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

pub(crate) fn flatten_cubic(
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
    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x0) * dy - (c1y - y0) * dx).abs();
    let d2 = ((c2x - x0) * dy - (c2y - y0) * dx).abs();
    let max_d = if d1 > d2 { d1 } else { d2 };
    let chord_sq = dx * dx + dy * dy;
    if max_d * max_d <= 0.25 * chord_sq || chord_sq < 0.001 {
        points[*count] = (x3, y3);
        *count += 1;
        return;
    }
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

/// Parsed path result: flat point array plus contour boundary indices.
pub(crate) struct ParsedPath {
    /// Total number of points.
    pub(crate) n: usize,
    /// Start index of each contour in the points array.
    /// `contour_starts[0..num_contours]` are valid.
    pub(crate) contour_starts: [usize; MAX_CONTOURS],
    /// Number of contours.
    pub(crate) num_contours: usize,
}

pub(crate) fn parse_path_to_points(
    data: &[u8],
    out: &mut [(f32, f32); MAX_PATH_POINTS],
) -> ParsedPath {
    let mut n: usize = 0;
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    let mut sx: f32 = 0.0;
    let mut sy: f32 = 0.0;
    let mut pos: usize = 0;
    let mut contour_starts = [0usize; MAX_CONTOURS];
    let mut num_contours: usize = 0;
    while pos + 4 <= data.len() {
        let tag = read_u32_le(data, pos);
        match tag {
            scene::PATH_MOVE_TO => {
                if pos + 12 > data.len() {
                    break;
                }
                // Record the start of a new contour.
                if num_contours < MAX_CONTOURS {
                    contour_starts[num_contours] = n;
                    num_contours += 1;
                }
                cx = read_f32_le(data, pos + 4);
                cy = read_f32_le(data, pos + 8);
                sx = cx;
                sy = cy;
                if n < MAX_PATH_POINTS {
                    out[n] = (cx, cy);
                    n += 1;
                }
                pos += 12;
            }
            scene::PATH_LINE_TO => {
                if pos + 12 > data.len() {
                    break;
                }
                cx = read_f32_le(data, pos + 4);
                cy = read_f32_le(data, pos + 8);
                if n < MAX_PATH_POINTS {
                    out[n] = (cx, cy);
                    n += 1;
                }
                pos += 12;
            }
            scene::PATH_CUBIC_TO => {
                if pos + 28 > data.len() {
                    break;
                }
                let c1x = read_f32_le(data, pos + 4);
                let c1y = read_f32_le(data, pos + 8);
                let c2x = read_f32_le(data, pos + 12);
                let c2y = read_f32_le(data, pos + 16);
                let x3 = read_f32_le(data, pos + 20);
                let y3 = read_f32_le(data, pos + 24);
                flatten_cubic(cx, cy, c1x, c1y, c2x, c2y, x3, y3, out, &mut n, 0);
                cx = x3;
                cy = y3;
                pos += 28;
            }
            scene::PATH_CLOSE => {
                if n < MAX_PATH_POINTS && (cx != sx || cy != sy) {
                    out[n] = (sx, sy);
                    n += 1;
                }
                cx = sx;
                cy = sy;
                pos += 4;
            }
            _ => break,
        }
    }
    if n >= MAX_PATH_POINTS {
        // SAFETY: single-threaded driver, no data race possible.
        unsafe {
            if !PATH_TRUNCATION_WARNED {
                PATH_TRUNCATION_WARNED = true;
                sys::print(b"WARNING: path flattening hit MAX_PATH_POINTS (");
                crate::print_u32(MAX_PATH_POINTS as u32);
                sys::print(b") - complex paths may lose detail\n");
            }
        }
    }
    // If no MoveTo was encountered, treat the whole thing as one contour.
    if num_contours == 0 && n > 0 {
        contour_starts[0] = 0;
        num_contours = 1;
    }
    ParsedPath {
        n,
        contour_starts,
        num_contours,
    }
}

/// Draw a Content::Path using stencil-then-cover within the current render pass.
pub(crate) fn draw_path_stencil_cover(
    cmdbuf: &mut metal::CommandBuffer,
    solid_verts: &mut Vec<u8>,
    data_buf: &[u8],
    contours: scene::DataRef,
    color: scene::Color,
    fill_rule: scene::FillRule,
    node_x: f32,
    node_y: f32,
    node_w: f32,
    node_h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    opacity: f32,
    path_buf: &mut PathPointsBuf,
) {
    let offset = contours.offset as usize;
    let end = offset + contours.length as usize;
    if end > data_buf.len() {
        return;
    }

    let parsed = parse_path_to_points(&data_buf[offset..end], path_buf);
    if parsed.n < 3 {
        return;
    }

    // Flush any pending solid geometry before changing pipeline.
    flush_solid_vertices(cmdbuf, solid_verts);

    // Build fan triangle vertices from a single arbitrary point (origin = 0,0).
    // With two-sided stencil (front INCR_WRAP, back DECR_WRAP), any fan origin
    // produces correct stencil winding for any polygon — convex, concave, or
    // multi-contour. This is the standard GPU path fill algorithm.
    let n = parsed.n;
    let mut fan_verts: Vec<u8> = Vec::with_capacity(n * 3 * VERTEX_BYTES);
    let to_ndc_x = |px: f32| -> f32 { ((node_x + px) * scale / vw) * 2.0 - 1.0 };
    let to_ndc_y = |py: f32| -> f32 { 1.0 - ((node_y + py) * scale / vh) * 2.0 };

    // Use (0, 0) as the fan origin — outside the path, which is fine.
    // Fan each contour separately to avoid spurious triangles spanning
    // contour boundaries (MoveTo discontinuities).
    let fan_ox = 0.0f32;
    let fan_oy = 0.0f32;

    for ci in 0..parsed.num_contours {
        let start = parsed.contour_starts[ci];
        let end_idx = if ci + 1 < parsed.num_contours {
            parsed.contour_starts[ci + 1]
        } else {
            parsed.n
        };
        if end_idx - start < 2 {
            continue;
        }
        for i in start..end_idx - 1 {
            let (ax, ay) = path_buf[i];
            let (bx, by) = path_buf[i + 1];
            for &(px, py) in &[(fan_ox, fan_oy), (ax, ay), (bx, by)] {
                let ndc_x = to_ndc_x(px);
                let ndc_y = to_ndc_y(py);
                fan_verts.extend_from_slice(&ndc_x.to_le_bytes());
                fan_verts.extend_from_slice(&ndc_y.to_le_bytes());
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // u
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // v
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // r
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // g
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // b
                fan_verts.extend_from_slice(&1.0f32.to_le_bytes()); // a=1
            }
        }
    }

    // Pass 1: Stencil write (fan triangles, no color).
    // Winding rule: two-sided INCR_WRAP/DECR_WRAP — correct for any polygon.
    //   Front-facing triangles increment, back-facing decrement.
    //   Stencil != 0 means inside (non-zero winding number).
    // Even-odd rule: INVERT flips stencil bit on each triangle overlap,
    //   so odd overlap count = 1 (inside), even = 0 (outside/hole).
    cmdbuf.set_render_pipeline(PIPE_STENCIL_WRITE);
    match fill_rule {
        scene::FillRule::Winding => {
            cmdbuf.set_depth_stencil_state(DSS_STENCIL_WINDING);
            cmdbuf.set_stencil_ref(0);
        }
        scene::FillRule::EvenOdd => {
            cmdbuf.set_depth_stencil_state(DSS_STENCIL_INVERT);
            cmdbuf.set_stencil_ref(0); // ref unused for INVERT, but set for clarity
        }
    }

    // Flush fan in 4KB chunks.
    let mut sent = 0;
    while sent < fan_verts.len() {
        let chunk_end = core::cmp::min(sent + MAX_INLINE_BYTES, fan_verts.len());
        let chunk = &fan_verts[sent..chunk_end];
        let vc = chunk.len() / VERTEX_BYTES;
        cmdbuf.set_vertex_bytes(0, chunk);
        cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vc as u32);
        sent = chunk_end;
    }

    // Pass 2: Stencil test + cover (colored quad where stencil != 0).
    // ref=0: NOT_EQUAL passes where stencil != 0 (i.e., inside the path).
    cmdbuf.set_render_pipeline(PIPE_SOLID);
    cmdbuf.set_depth_stencil_state(DSS_STENCIL_TEST);
    cmdbuf.set_stencil_ref(0);

    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = (color.a as f32 / 255.0) * opacity;
    emit_quad(
        solid_verts,
        node_x,
        node_y,
        node_w,
        node_h,
        vw,
        vh,
        scale,
        r,
        g,
        b,
        a,
    );
    flush_solid_vertices(cmdbuf, solid_verts);

    // Restore normal state.
    cmdbuf.set_depth_stencil_state(DSS_NONE);
}
