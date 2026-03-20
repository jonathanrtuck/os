//! Path rasterization: flatten cubic Bezier curves, scanline fill with
//! anti-aliasing, and composite onto a framebuffer.
//!
//! Dependencies: `alloc`, `drawing`, `scene`, and the `coords` sibling module.

use alloc::{vec, vec::Vec};

use drawing::{Color, Surface};

use super::SceneGraph;

/// Maximum line segments after cubic Bezier flattening for a single Path node.
const PATH_MAX_SEGMENTS: usize = 4096;

/// Maximum active edges during scanline sweep.
const PATH_MAX_ACTIVE: usize = 256;

/// Fixed-point precision for path rasterization (20.12 format).
const PATH_FP_SHIFT: i32 = 12;
const PATH_FP_ONE: i32 = 1 << PATH_FP_SHIFT;

/// Vertical oversampling for anti-aliased path edges.
const PATH_OVERSAMPLE_Y: i32 = 8;

/// A line segment in physical pixel fixed-point coordinates.
#[derive(Clone, Copy)]
struct PathSegment {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

/// Read an f32 from a byte slice at the given byte offset (little-endian).
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

/// Read a u32 from a byte slice at the given byte offset (little-endian).
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

/// Convert a float coordinate to fixed-point.
#[inline]
fn f32_to_fp(v: f32) -> i32 {
    (v * PATH_FP_ONE as f32) as i32
}

/// Convert a `scene::Color` to a `drawing::Color`.
pub(super) fn scene_to_draw_color(c: scene::Color) -> Color {
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// Flatten a cubic Bezier curve to line segments using recursive subdivision.
/// Error threshold is ~0.25 physical pixels (in fixed-point).
fn flatten_cubic(
    x0: i32,
    y0: i32,
    c1x: i32,
    c1y: i32,
    c2x: i32,
    c2y: i32,
    x3: i32,
    y3: i32,
    segments: &mut [PathSegment],
    num_segments: &mut usize,
    depth: u32,
) {
    if *num_segments >= segments.len() {
        return;
    }
    // Flatness test: maximum deviation of control points from the chord.
    // Uses the distance from each control point to the line (x0,y0)->(x3,y3).
    let dx = x3 - x0;
    let dy = y3 - y0;
    // Compute perpendicular distances of control points from chord.
    let d1 = ((c1x - x0) as i64 * dy as i64 - (c1y - y0) as i64 * dx as i64).abs();
    let d2 = ((c2x - x0) as i64 * dy as i64 - (c2y - y0) as i64 * dx as i64).abs();
    let chord_len_sq = dx as i64 * dx as i64 + dy as i64 * dy as i64;
    // threshold: 0.25 pixels in fixed-point = PATH_FP_ONE / 4
    let threshold_fp = (PATH_FP_ONE / 4) as i128;
    let max_d = if d1 > d2 { d1 } else { d2 };
    // Flatness check: max_d^2 <= threshold^2 * chord_len  (avoid sqrt)
    // Use i128 to prevent overflow with large fixed-point coordinates.
    let flat = if chord_len_sq == 0 {
        max_d as i128 <= threshold_fp
    } else {
        depth >= 8
            || (max_d as i128 * max_d as i128) <= threshold_fp * threshold_fp * chord_len_sq as i128
    };

    if flat || depth >= 10 {
        if y0 != y3 {
            segments[*num_segments] = PathSegment {
                x0,
                y0,
                x1: x3,
                y1: y3,
            };
            *num_segments += 1;
        }
        return;
    }

    // De Casteljau subdivision at t=0.5.
    let m01x = (x0 + c1x) >> 1;
    let m01y = (y0 + c1y) >> 1;
    let m12x = (c1x + c2x) >> 1;
    let m12y = (c1y + c2y) >> 1;
    let m23x = (c2x + x3) >> 1;
    let m23y = (c2y + y3) >> 1;
    let m012x = (m01x + m12x) >> 1;
    let m012y = (m01y + m12y) >> 1;
    let m123x = (m12x + m23x) >> 1;
    let m123y = (m12y + m23y) >> 1;
    let mx = (m012x + m123x) >> 1;
    let my = (m012y + m123y) >> 1;

    flatten_cubic(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        segments,
        num_segments,
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
        segments,
        num_segments,
        depth + 1,
    );
}

/// Rasterize path contours to a coverage buffer using scanline sweep.
///
/// `segments` are in physical pixel fixed-point coordinates.
/// Coverage buffer is 1 byte per pixel, `width * height`.
fn path_scanline_fill(
    segments: &[PathSegment],
    num_segments: usize,
    coverage: &mut [u8],
    width: u32,
    height: u32,
    fill_rule: scene::FillRule,
) {
    if num_segments == 0 || width == 0 || height == 0 {
        return;
    }

    let mut active_x = vec![0i32; PATH_MAX_ACTIVE];
    let mut active_dir = vec![0i32; PATH_MAX_ACTIVE];

    for row in 0..height {
        let y_top_fp = row as i32 * PATH_FP_ONE;
        let sub_step = PATH_FP_ONE / PATH_OVERSAMPLE_Y;

        for sub in 0..PATH_OVERSAMPLE_Y {
            let scan_y = y_top_fp + sub * sub_step + sub_step / 2;
            let mut num_active = 0usize;

            // Find active edges for this scanline.
            for si in 0..num_segments {
                let seg = &segments[si];
                let (y_top, y_bot, x_top, x_bot, dir) = if seg.y0 < seg.y1 {
                    (seg.y0, seg.y1, seg.x0, seg.x1, 1i32)
                } else {
                    (seg.y1, seg.y0, seg.x1, seg.x0, -1i32)
                };
                if y_top > scan_y || y_bot <= scan_y {
                    continue;
                }
                if num_active >= PATH_MAX_ACTIVE {
                    break;
                }
                let dy = y_bot - y_top;
                let t = scan_y - y_top;
                let x = if dy == 0 {
                    x_top
                } else {
                    x_top + ((x_bot - x_top) as i64 * t as i64 / dy as i64) as i32
                };
                active_x[num_active] = x;
                active_dir[num_active] = dir;
                num_active += 1;
            }

            // Sort active edges by x (insertion sort -- small N).
            for i in 1..num_active {
                let key_x = active_x[i];
                let key_dir = active_dir[i];
                let mut j = i;
                while j > 0 && active_x[j - 1] > key_x {
                    active_x[j] = active_x[j - 1];
                    active_dir[j] = active_dir[j - 1];
                    j -= 1;
                }
                active_x[j] = key_x;
                active_dir[j] = key_dir;
            }

            // Apply fill rule.
            let contribution = (256 / PATH_OVERSAMPLE_Y) as u16;

            match fill_rule {
                scene::FillRule::Winding => {
                    // Winding rule: fill spans where winding != 0.
                    let mut winding: i32 = 0;
                    let mut edge_idx = 0;
                    while edge_idx < num_active {
                        let old_winding = winding;
                        winding += active_dir[edge_idx];
                        if old_winding == 0 && winding != 0 {
                            let x_start = active_x[edge_idx];
                            let mut ei = edge_idx + 1;
                            while ei < num_active {
                                winding += active_dir[ei];
                                if winding == 0 {
                                    let x_end = active_x[ei];
                                    path_fill_span(
                                        coverage,
                                        width,
                                        row,
                                        x_start,
                                        x_end,
                                        contribution,
                                    );
                                    edge_idx = ei + 1;
                                    break;
                                }
                                ei += 1;
                            }
                            if winding != 0 {
                                break;
                            }
                        } else {
                            edge_idx += 1;
                        }
                    }
                }
                scene::FillRule::EvenOdd => {
                    // Even-odd rule: toggle fill at each edge crossing.
                    let mut i = 0;
                    while i + 1 < num_active {
                        let x_start = active_x[i];
                        let x_end = active_x[i + 1];
                        path_fill_span(coverage, width, row, x_start, x_end, contribution);
                        i += 2;
                    }
                }
            }
        }
    }
}

/// Fill a horizontal span in the coverage buffer with anti-aliased edges.
fn path_fill_span(
    coverage: &mut [u8],
    width: u32,
    row: u32,
    x_start_fp: i32,
    x_end_fp: i32,
    contribution: u16,
) {
    let px_start = x_start_fp >> PATH_FP_SHIFT;
    let px_end_raw = x_end_fp + PATH_FP_ONE - 1;
    let px_end = px_end_raw >> PATH_FP_SHIFT;
    let px_start = if px_start < 0 { 0 } else { px_start as u32 };
    let px_end = if px_end < 0 {
        return;
    } else if (px_end as u32) > width {
        width
    } else {
        px_end as u32
    };
    let row_start = (row * width) as usize;
    for px in px_start..px_end {
        let idx = row_start + px as usize;
        if idx >= coverage.len() {
            break;
        }
        let cov = if px as i32 == (x_start_fp >> PATH_FP_SHIFT)
            && px as i32 == ((x_end_fp - 1) >> PATH_FP_SHIFT)
        {
            let frac = x_end_fp - x_start_fp;
            (contribution as i32 * frac / PATH_FP_ONE) as u16
        } else if px as i32 == (x_start_fp >> PATH_FP_SHIFT) {
            let right_edge = ((px + 1) as i32) << PATH_FP_SHIFT;
            let frac = right_edge - x_start_fp;
            (contribution as i32 * frac / PATH_FP_ONE) as u16
        } else if px as i32 == ((x_end_fp - 1) >> PATH_FP_SHIFT) {
            let left_edge = (px as i32) << PATH_FP_SHIFT;
            let frac = x_end_fp - left_edge;
            (contribution as i32 * frac / PATH_FP_ONE) as u16
        } else {
            contribution
        };
        let val = coverage[idx] as u16 + cov;
        coverage[idx] = if val > 255 { 255 } else { val as u8 };
    }
}

/// Parse path commands from `path_data`, flatten cubics, and fill into a
/// coverage buffer using the scanline sweep algorithm.
///
/// Returns a `Vec<u8>` of `width * height` bytes (one byte per pixel,
/// row-major). Returns an empty `Vec` if the path is empty, dimensions
/// are zero, or the allocation would exceed 4 MiB.
///
/// This is the low-level primitive used by both `render_path` (which
/// composites BGRA pixels) and `ClipMaskCache` (which keeps the 8bpp
/// alpha buffer for clip masking).
pub fn rasterize_path_to_coverage(
    path_data: &[u8],
    width: u32,
    height: u32,
    fill_rule: scene::FillRule,
) -> Vec<u8> {
    if path_data.is_empty() || width == 0 || height == 0 {
        return Vec::new();
    }

    // Parse path commands and build segment list in fixed-point coordinates.
    // Coordinates are treated as already in physical pixels (scale = 1.0).
    let mut segments = vec![
        PathSegment {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        };
        PATH_MAX_SEGMENTS
    ];
    let mut num_segments = 0usize;

    let mut cursor_x = 0i32;
    let mut cursor_y = 0i32;
    let mut contour_start_x = 0i32;
    let mut contour_start_y = 0i32;

    let mut offset = 0usize;
    while offset < path_data.len() {
        let tag = read_u32_le(path_data, offset);
        match tag {
            scene::PATH_MOVE_TO => {
                if offset + scene::PATH_MOVE_TO_SIZE > path_data.len() {
                    break;
                }
                let x = read_f32_le(path_data, offset + 4);
                let y = read_f32_le(path_data, offset + 8);
                cursor_x = f32_to_fp(x);
                cursor_y = f32_to_fp(y);
                contour_start_x = cursor_x;
                contour_start_y = cursor_y;
                offset += scene::PATH_MOVE_TO_SIZE;
            }
            scene::PATH_LINE_TO => {
                if offset + scene::PATH_LINE_TO_SIZE > path_data.len() {
                    break;
                }
                let x = read_f32_le(path_data, offset + 4);
                let y = read_f32_le(path_data, offset + 8);
                let nx = f32_to_fp(x);
                let ny = f32_to_fp(y);
                if cursor_y != ny && num_segments < PATH_MAX_SEGMENTS {
                    segments[num_segments] = PathSegment {
                        x0: cursor_x,
                        y0: cursor_y,
                        x1: nx,
                        y1: ny,
                    };
                    num_segments += 1;
                }
                cursor_x = nx;
                cursor_y = ny;
                offset += scene::PATH_LINE_TO_SIZE;
            }
            scene::PATH_CUBIC_TO => {
                if offset + scene::PATH_CUBIC_TO_SIZE > path_data.len() {
                    break;
                }
                let c1x = read_f32_le(path_data, offset + 4);
                let c1y = read_f32_le(path_data, offset + 8);
                let c2x = read_f32_le(path_data, offset + 12);
                let c2y = read_f32_le(path_data, offset + 16);
                let x = read_f32_le(path_data, offset + 20);
                let y = read_f32_le(path_data, offset + 24);
                flatten_cubic(
                    cursor_x,
                    cursor_y,
                    f32_to_fp(c1x),
                    f32_to_fp(c1y),
                    f32_to_fp(c2x),
                    f32_to_fp(c2y),
                    f32_to_fp(x),
                    f32_to_fp(y),
                    &mut segments,
                    &mut num_segments,
                    0,
                );
                cursor_x = f32_to_fp(x);
                cursor_y = f32_to_fp(y);
                offset += scene::PATH_CUBIC_TO_SIZE;
            }
            scene::PATH_CLOSE => {
                if cursor_y != contour_start_y && num_segments < PATH_MAX_SEGMENTS {
                    segments[num_segments] = PathSegment {
                        x0: cursor_x,
                        y0: cursor_y,
                        x1: contour_start_x,
                        y1: contour_start_y,
                    };
                    num_segments += 1;
                }
                cursor_x = contour_start_x;
                cursor_y = contour_start_y;
                offset += scene::PATH_CLOSE_SIZE;
            }
            _ => break,
        }
    }

    // Implicit close.
    if (cursor_x != contour_start_x || cursor_y != contour_start_y)
        && cursor_y != contour_start_y
        && num_segments < PATH_MAX_SEGMENTS
    {
        segments[num_segments] = PathSegment {
            x0: cursor_x,
            y0: cursor_y,
            x1: contour_start_x,
            y1: contour_start_y,
        };
        num_segments += 1;
    }

    if num_segments == 0 {
        return Vec::new();
    }

    // Clamp all segments to the requested [0, width) × [0, height) region.
    // Translate so that the coverage buffer origin is (0, 0).
    let cov_w = width;
    let cov_h = height;
    let cov_size = cov_w as usize * cov_h as usize;
    if cov_size > 4 * 1024 * 1024 {
        return Vec::new();
    }

    let mut coverage = vec![0u8; cov_size];
    path_scanline_fill(
        &segments,
        num_segments,
        &mut coverage,
        cov_w,
        cov_h,
        fill_rule,
    );
    coverage
}

/// Render a `Content::Path` node: read contour commands, flatten cubics,
/// scanline fill with AA, composite onto framebuffer.
pub(super) fn render_path(
    fb: &mut Surface,
    graph: &SceneGraph,
    scale: f32,
    contours: scene::DataRef,
    color: scene::Color,
    fill_rule: scene::FillRule,
    draw_x: i32,
    draw_y: i32,
    nw: i32,
    nh: i32,
) {
    if contours.length == 0 || nw <= 0 || nh <= 0 {
        return;
    }

    let data = if (contours.offset as usize + contours.length as usize) <= graph.data.len() {
        &graph.data[contours.offset as usize..][..contours.length as usize]
    } else {
        return;
    };

    let s = scale;

    // Parse path commands and build segment list in physical pixel fixed-point.
    let mut segments = vec![
        PathSegment {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0
        };
        PATH_MAX_SEGMENTS
    ];
    let mut num_segments = 0usize;

    let mut cursor_x = 0i32;
    let mut cursor_y = 0i32;
    let mut contour_start_x = 0i32;
    let mut contour_start_y = 0i32;

    let mut offset = 0usize;
    while offset < data.len() {
        let tag = read_u32_le(data, offset);
        match tag {
            scene::PATH_MOVE_TO => {
                if offset + scene::PATH_MOVE_TO_SIZE > data.len() {
                    break;
                }
                let x = read_f32_le(data, offset + 4);
                let y = read_f32_le(data, offset + 8);
                cursor_x = f32_to_fp(x * s);
                cursor_y = f32_to_fp(y * s);
                contour_start_x = cursor_x;
                contour_start_y = cursor_y;
                offset += scene::PATH_MOVE_TO_SIZE;
            }
            scene::PATH_LINE_TO => {
                if offset + scene::PATH_LINE_TO_SIZE > data.len() {
                    break;
                }
                let x = read_f32_le(data, offset + 4);
                let y = read_f32_le(data, offset + 8);
                let nx = f32_to_fp(x * s);
                let ny = f32_to_fp(y * s);
                if cursor_y != ny && num_segments < PATH_MAX_SEGMENTS {
                    segments[num_segments] = PathSegment {
                        x0: cursor_x,
                        y0: cursor_y,
                        x1: nx,
                        y1: ny,
                    };
                    num_segments += 1;
                }
                cursor_x = nx;
                cursor_y = ny;
                offset += scene::PATH_LINE_TO_SIZE;
            }
            scene::PATH_CUBIC_TO => {
                if offset + scene::PATH_CUBIC_TO_SIZE > data.len() {
                    break;
                }
                let c1x = read_f32_le(data, offset + 4);
                let c1y = read_f32_le(data, offset + 8);
                let c2x = read_f32_le(data, offset + 12);
                let c2y = read_f32_le(data, offset + 16);
                let x = read_f32_le(data, offset + 20);
                let y = read_f32_le(data, offset + 24);
                flatten_cubic(
                    cursor_x,
                    cursor_y,
                    f32_to_fp(c1x * s),
                    f32_to_fp(c1y * s),
                    f32_to_fp(c2x * s),
                    f32_to_fp(c2y * s),
                    f32_to_fp(x * s),
                    f32_to_fp(y * s),
                    &mut segments,
                    &mut num_segments,
                    0,
                );
                cursor_x = f32_to_fp(x * s);
                cursor_y = f32_to_fp(y * s);
                offset += scene::PATH_CUBIC_TO_SIZE;
            }
            scene::PATH_CLOSE => {
                // Close: line back to contour start.
                if cursor_y != contour_start_y && num_segments < PATH_MAX_SEGMENTS {
                    segments[num_segments] = PathSegment {
                        x0: cursor_x,
                        y0: cursor_y,
                        x1: contour_start_x,
                        y1: contour_start_y,
                    };
                    num_segments += 1;
                }
                cursor_x = contour_start_x;
                cursor_y = contour_start_y;
                offset += scene::PATH_CLOSE_SIZE;
            }
            _ => break, // Unknown command -- stop parsing.
        }
    }

    // Implicit close: if cursor is not at contour start, close it.
    if (cursor_x != contour_start_x || cursor_y != contour_start_y)
        && cursor_y != contour_start_y
        && num_segments < PATH_MAX_SEGMENTS
    {
        segments[num_segments] = PathSegment {
            x0: cursor_x,
            y0: cursor_y,
            x1: contour_start_x,
            y1: contour_start_y,
        };
        num_segments += 1;
    }

    if num_segments == 0 {
        return;
    }

    // Compute bounding box of all segments in physical pixels.
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for i in 0..num_segments {
        let seg = &segments[i];
        let sx0 = seg.x0 >> PATH_FP_SHIFT;
        let sy0 = seg.y0 >> PATH_FP_SHIFT;
        let sx1 = seg.x1 >> PATH_FP_SHIFT;
        let sy1 = seg.y1 >> PATH_FP_SHIFT;
        if sx0 < min_x {
            min_x = sx0;
        }
        if sx1 < min_x {
            min_x = sx1;
        }
        if sy0 < min_y {
            min_y = sy0;
        }
        if sy1 < min_y {
            min_y = sy1;
        }
        if sx0 > max_x {
            max_x = sx0;
        }
        if sx1 > max_x {
            max_x = sx1;
        }
        if sy0 > max_y {
            max_y = sy0;
        }
        if sy1 > max_y {
            max_y = sy1;
        }
    }
    // Add 1-pixel margin for AA.
    min_x -= 1;
    min_y -= 1;
    max_x += 2;
    max_y += 2;

    // min_x/min_y may be negative (e.g., glyphs with negative bearing_x).
    // The coverage buffer is sized to the full bbox; the translation on
    // lines below shifts all segments so the buffer origin is at (min_x, min_y).
    // draw_coverage handles negative blit coordinates via pre-clipping.

    let cov_w = (max_x - min_x) as u32;
    let cov_h = (max_y - min_y) as u32;
    if cov_w == 0 || cov_h == 0 {
        return;
    }

    // Cap allocation to prevent OOM.
    let cov_size = cov_w as usize * cov_h as usize;
    if cov_size > 4 * 1024 * 1024 {
        return;
    }

    // Translate segments so (min_x, min_y) becomes origin of coverage buffer.
    let off_x = min_x * PATH_FP_ONE;
    let off_y = min_y * PATH_FP_ONE;
    for i in 0..num_segments {
        segments[i].x0 -= off_x;
        segments[i].y0 -= off_y;
        segments[i].x1 -= off_x;
        segments[i].y1 -= off_y;
    }

    // Rasterize into coverage buffer.
    let mut coverage = vec![0u8; cov_size];
    path_scanline_fill(
        &segments,
        num_segments,
        &mut coverage,
        cov_w,
        cov_h,
        fill_rule,
    );

    // Composite coverage onto framebuffer.
    let path_color = scene_to_draw_color(color);
    let blit_x = draw_x + min_x;
    let blit_y = draw_y + min_y;
    fb.draw_coverage(blit_x, blit_y, &coverage, cov_w, cov_h, path_color);
}
