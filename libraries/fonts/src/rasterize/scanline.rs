//! Glyph rasterizer — bezier flattening + analytic area coverage.
//!
//! Converts a `GlyphOutline` (font-unit contours) into a grayscale coverage
//! bitmap. The pipeline: scale to fixed-point pixel coords, flatten quadratic
//! bezier curves into line segments, compute exact signed-area trapezoid
//! coverage per pixel (continuous, not quantized).
//!
//! All math is integer/fixed-point. No floating point.

use super::{
    outline::GlyphOutline,
    scale::{scale_fu, scale_fu_ceil, scale_fu_floor, FP_ONE, FP_SHIFT},
};

// ---------------------------------------------------------------------------
// Scanline rasterizer constants and types
// ---------------------------------------------------------------------------

/// Maximum line segments after bezier flattening.
const MAX_SEGMENTS: usize = 2048;

/// Maximum glyph dimensions for buffer sizing.
/// Sized for 2x Retina rasterization (glyphs up to ~36pt * 2 = 72px).
pub(crate) const GLYPH_MAX_W: usize = 100;

/// A line segment in pixel-space fixed-point coordinates.
#[derive(Clone, Copy, Default)]
pub(crate) struct Segment {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

/// Scratch space for rasterization. Caller allocates (typically in BSS).
pub struct RasterScratch {
    pub outline: GlyphOutline,
    pub(crate) segments: [Segment; MAX_SEGMENTS],
    pub(crate) num_segments: usize,
}

impl RasterScratch {
    pub const fn zeroed() -> Self {
        RasterScratch {
            outline: GlyphOutline::zeroed(),
            segments: [Segment {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            }; MAX_SEGMENTS],
            num_segments: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinate conversion: font units -> pixel-space fixed-point
// ---------------------------------------------------------------------------

fn fu_to_fp(val: i32, size_px: u32, upem: u16, origin: i32) -> i32 {
    let px = (val as i64 * size_px as i64 * FP_ONE as i64) / upem as i64;
    px as i32 - origin * FP_ONE
}

// ---------------------------------------------------------------------------
// Bezier flattening
// ---------------------------------------------------------------------------

fn emit_segment(x0: i32, y0: i32, x1: i32, y1: i32, scratch: &mut RasterScratch) {
    if scratch.num_segments < MAX_SEGMENTS && y0 != y1 {
        scratch.segments[scratch.num_segments] = Segment { x0, y0, x1, y1 };
        scratch.num_segments += 1;
    }
}

fn flatten_contour_from_scratch(
    scratch: &mut RasterScratch,
    start: usize,
    end: usize,
    size_px: u32,
    upem: u16,
    x_origin: i32,
    y_origin: i32,
) {
    let mut i = start;
    while i <= end {
        let i_next = if i == end { start } else { i + 1 };
        let cur_on = scratch.outline.points[i].on_curve;
        if cur_on {
            let next_on = scratch.outline.points[i_next].on_curve;
            let (x0, y0) = outline_point_to_fp(scratch, i, size_px, upem, x_origin, y_origin);
            if next_on {
                let (x1, y1) =
                    outline_point_to_fp(scratch, i_next, size_px, upem, x_origin, y_origin);
                emit_segment(x0, y0, x1, y1, scratch);
                i += 1;
            } else {
                let i_after = if i_next == end { start } else { i_next + 1 };
                let after_on = scratch.outline.points[i_after].on_curve;
                let (cx, cy) =
                    outline_point_to_fp(scratch, i_next, size_px, upem, x_origin, y_origin);
                if after_on {
                    let (x2, y2) =
                        outline_point_to_fp(scratch, i_after, size_px, upem, x_origin, y_origin);
                    flatten_quadratic(x0, y0, cx, cy, x2, y2, scratch, 0);
                    i += 2;
                } else {
                    let (cx2, cy2) =
                        outline_point_to_fp(scratch, i_after, size_px, upem, x_origin, y_origin);
                    let mid_x = (cx + cx2) >> 1;
                    let mid_y = (cy + cy2) >> 1;
                    flatten_quadratic(x0, y0, cx, cy, mid_x, mid_y, scratch, 0);
                    i += 1;
                }
            }
        } else {
            let i_prev = if i == start { end } else { i - 1 };
            let prev_on = scratch.outline.points[i_prev].on_curve;
            if !prev_on {
                let (px_prev, py_prev) =
                    outline_point_to_fp(scratch, i_prev, size_px, upem, x_origin, y_origin);
                let (cx, cy) = outline_point_to_fp(scratch, i, size_px, upem, x_origin, y_origin);
                let mid_x = (px_prev + cx) >> 1;
                let mid_y = (py_prev + cy) >> 1;
                let i_next2 = if i == end { start } else { i + 1 };
                let next2_on = scratch.outline.points[i_next2].on_curve;
                if next2_on {
                    let (x2, y2) =
                        outline_point_to_fp(scratch, i_next2, size_px, upem, x_origin, y_origin);
                    flatten_quadratic(mid_x, mid_y, cx, cy, x2, y2, scratch, 0);
                } else {
                    let (cx2, cy2) =
                        outline_point_to_fp(scratch, i_next2, size_px, upem, x_origin, y_origin);
                    let end_x = (cx + cx2) >> 1;
                    let end_y = (cy + cy2) >> 1;
                    flatten_quadratic(mid_x, mid_y, cx, cy, end_x, end_y, scratch, 0);
                }
            }
            i += 1;
        }
    }
}

pub(crate) fn flatten_outline_from_scratch(
    scratch: &mut RasterScratch,
    size_px: u32,
    upem: u16,
    x_origin: i32,
    y_origin: i32,
) {
    let nc = scratch.outline.num_contours as usize;
    let mut start = 0usize;
    for c in 0..nc {
        let end = scratch.outline.contour_ends[c] as usize;
        if end < start + 1 {
            start = end + 1;
            continue;
        }
        flatten_contour_from_scratch(scratch, start, end, size_px, upem, x_origin, y_origin);
        start = end + 1;
    }
}

fn flatten_quadratic(
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    scratch: &mut RasterScratch,
    depth: u32,
) {
    if scratch.num_segments >= MAX_SEGMENTS {
        return;
    }
    let mx = (x0 + x2) >> 1;
    let my = (y0 + y2) >> 1;
    let dx = x1 - mx;
    let dy = y1 - my;
    let flatness = (FP_ONE / 2) as i64 * (FP_ONE / 2) as i64;
    let dist_sq = dx as i64 * dx as i64 + dy as i64 * dy as i64;
    if depth >= 8 || dist_sq <= flatness {
        emit_segment(x0, y0, x2, y2, scratch);
        return;
    }
    let q0x = (x0 + x1) >> 1;
    let q0y = (y0 + y1) >> 1;
    let q1x = (x1 + x2) >> 1;
    let q1y = (y1 + y2) >> 1;
    let rx = (q0x + q1x) >> 1;
    let ry = (q0y + q1y) >> 1;
    flatten_quadratic(x0, y0, q0x, q0y, rx, ry, scratch, depth + 1);
    flatten_quadratic(rx, ry, q1x, q1y, x2, y2, scratch, depth + 1);
}

fn outline_point_to_fp(
    scratch: &RasterScratch,
    i: usize,
    size_px: u32,
    upem: u16,
    x_origin: i32,
    y_origin: i32,
) -> (i32, i32) {
    let p = &scratch.outline.points[i];
    let fx = fu_to_fp(p.x, size_px, upem, x_origin);
    let fy = y_origin * FP_ONE - fu_to_fp(p.y, size_px, upem, 0);
    (fx, fy)
}

// ---------------------------------------------------------------------------
// Analytic area coverage rasterizer
// ---------------------------------------------------------------------------
//
// Replaces the old 8x vertical oversampling approach with exact signed-area
// computation (FreeType ftgrays.c / stb_truetype v2 model). For each line
// segment crossing a pixel, computes the exact trapezoid area. Coverage is
// a continuous value, not quantized to 1/N levels.
//
// Algorithm:
//   For each pixel row:
//     For each segment crossing the row:
//       Clip segment to the row's y-extent
//       For each pixel column the clipped segment crosses:
//         Compute signed area contribution (fill propagates rightward)
//     Sweep left-to-right: coverage = |area + running_fill| * 255 / FP_ONE

/// Process a single edge segment within one pixel row.
///
/// The segment goes from (x0, y0) to (x1, y1) in fixed-point coordinates,
/// where y0 and y1 are within [row_y_top, row_y_top + FP_ONE].
///
/// `area`: per-pixel partial coverage from edges within each pixel.
/// `cover`: per-pixel fill that propagates rightward.
/// `dir`: +1 or -1 (winding direction).
fn process_edge(
    area: &mut [i32],
    cover: &mut [i32],
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    dir: i32,
    _row_y_top: i32,
    width: i32,
) {
    // Signed height of this edge segment within the pixel row.
    let dy = y1 - y0; // In FP units, max FP_ONE.
    if dy == 0 {
        return; // Horizontal edge — no coverage contribution.
    }

    let dy_signed = dy * dir;

    // Determine pixel column range.
    let (x_left, x_right) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    let px_min = x_left >> FP_SHIFT;
    let px_max = x_right >> FP_SHIFT;

    if px_min == px_max {
        // Entire edge within a single pixel column.
        let px = px_min;
        if px >= 0 && px < width {
            let pxi = px as usize;
            // Average fractional x-position within the pixel.
            let frac0 = x0 - (px << FP_SHIFT);
            let frac1 = x1 - (px << FP_SHIFT);
            let avg_frac = (frac0 + frac1) / 2;
            // Coverage = dy * (1 - avg_frac / FP_ONE) = dy * (FP_ONE - avg_frac) / FP_ONE
            let coverage_contrib =
                (dy_signed as i64 * (FP_ONE - avg_frac) as i64 / FP_ONE as i64) as i32;
            area[pxi] += coverage_contrib;
            // Fill propagates to the right.
            if pxi + 1 < cover.len() {
                cover[pxi + 1] += dy_signed;
            }
        } else if px >= width {
            // Edge is entirely to the right of the bitmap — no visible effect.
        } else {
            // Edge is entirely to the left of the bitmap — contributes full fill.
            cover[0] += dy_signed;
        }
        return;
    }

    // Edge crosses multiple pixel columns. Process first, interior, and last.
    let dx = x1 - x0; // Total x-delta (can be negative).
    if dx == 0 {
        // Vertical edge — impossible to cross multiple columns.
        // (This case is handled by px_min == px_max above.)
        return;
    }

    // We iterate from left to right, computing y-intercepts at pixel boundaries.
    // Determine the leftmost and rightmost pixel columns to process.
    let step = if dx > 0 { 1 } else { -1 };
    let mut cx = x0;
    let mut cy = y0;
    let mut px = x0 >> FP_SHIFT;

    // Process pixels from the starting pixel to the ending pixel.
    let end_px = x1 >> FP_SHIFT;

    loop {
        let next_px = px + step;
        // The x-coordinate of the boundary we're crossing.
        let boundary_x = if step > 0 {
            next_px << FP_SHIFT // Right edge of current pixel.
        } else {
            (px) << FP_SHIFT // Left edge of current pixel.
        };

        // Is this the last pixel?
        let is_last = (step > 0 && next_px > end_px) || (step < 0 && next_px < end_px);

        let (next_x, next_y) = if is_last {
            // Last pixel: end at (x1, y1).
            (x1, y1)
        } else {
            // Compute y at the boundary using linear interpolation.
            let t_num = (boundary_x - x0) as i64;
            let t_den = dx as i64;
            let by = y0 + ((y1 - y0) as i64 * t_num / t_den) as i32;
            (boundary_x, by)
        };

        // Process the sub-segment (cx, cy) → (next_x, next_y) within pixel column px.
        let seg_dy = next_y - cy;
        if seg_dy != 0 && px >= 0 && px < width {
            let pxi = px as usize;
            let seg_dy_signed = seg_dy * dir;
            let frac0 = cx - (px << FP_SHIFT);
            let frac1 = next_x - (px << FP_SHIFT);
            // Clamp fractions to [0, FP_ONE] to handle boundary values.
            let f0 = frac0.max(0).min(FP_ONE);
            let f1 = frac1.max(0).min(FP_ONE);
            let avg_frac = (f0 + f1) / 2;
            let coverage_contrib =
                (seg_dy_signed as i64 * (FP_ONE - avg_frac) as i64 / FP_ONE as i64) as i32;
            area[pxi] += coverage_contrib;
            if pxi + 1 < cover.len() {
                cover[pxi + 1] += seg_dy_signed;
            }
        } else if seg_dy != 0 && px < 0 {
            // Edge to the left of bitmap — full fill contribution.
            cover[0] += seg_dy * dir;
        }

        if is_last {
            break;
        }

        cx = next_x;
        cy = next_y;
        px = next_px;
    }
}

pub(crate) fn rasterize_segments(
    scratch: &RasterScratch,
    coverage: &mut [u8],
    width: u32,
    height: u32,
) {
    let nseg = scratch.num_segments;
    if nseg == 0 {
        return;
    }
    let w = width as usize;

    // Per-row working buffers (stack-allocated, reused each row).
    let mut area = [0i32; GLYPH_MAX_W];
    let mut cover = [0i32; GLYPH_MAX_W + 1];

    for row in 0..height {
        let y_top = row as i32 * FP_ONE;
        let y_bot = y_top + FP_ONE;

        // Clear working buffers for this row.
        for i in 0..w {
            area[i] = 0;
        }
        for i in 0..=w {
            cover[i] = 0;
        }

        // Process each segment that crosses this pixel row.
        for si in 0..nseg {
            let seg = &scratch.segments[si];

            // Orient segment top-to-bottom.
            let (sy0, sy1, sx0, sx1, dir) = if seg.y0 <= seg.y1 {
                (seg.y0, seg.y1, seg.x0, seg.x1, 1i32)
            } else {
                (seg.y1, seg.y0, seg.x1, seg.x0, -1i32)
            };

            // Skip if segment doesn't overlap this row.
            if sy1 <= y_top || sy0 >= y_bot {
                continue;
            }

            // Clip segment to row's y-extent via linear interpolation.
            let (cx0, cy0, cx1, cy1);
            if sy0 >= y_top && sy1 <= y_bot {
                // Entirely within the row — no clipping needed.
                cx0 = sx0;
                cy0 = sy0;
                cx1 = sx1;
                cy1 = sy1;
            } else {
                let dx = (sx1 - sx0) as i64;
                let dy = (sy1 - sy0) as i64;

                // Clip top.
                if sy0 < y_top {
                    let t = (y_top - sy0) as i64;
                    cx0 = sx0 + (dx * t / dy) as i32;
                    cy0 = y_top;
                } else {
                    cx0 = sx0;
                    cy0 = sy0;
                }

                // Clip bottom.
                if sy1 > y_bot {
                    let t = (y_bot - sy0) as i64;
                    cx1 = sx0 + (dx * t / dy) as i32;
                    cy1 = y_bot;
                } else {
                    cx1 = sx1;
                    cy1 = sy1;
                }
            }

            // Convert y values to row-relative (0 to FP_ONE).
            process_edge(
                &mut area[..w],
                &mut cover[..w + 1],
                cx0,
                cy0 - y_top,
                cx1,
                cy1 - y_top,
                dir,
                0,
                width as i32,
            );
        }

        // Sweep left-to-right: convert area/cover to final coverage values.
        let row_start = (row * width) as usize;
        let mut running_cover: i32 = 0;
        for x in 0..w {
            running_cover += cover[x];
            // Coverage = |running_cover + area[x]| scaled to 0-255.
            // running_cover and area are in FP_ONE units (max FP_ONE per full-height edge).
            let val = (running_cover + area[x]).abs();
            let cov = val * 255 / FP_ONE;
            let cov = if cov > 255 { 255 } else { cov };
            let idx = row_start + x;
            if idx < coverage.len() {
                coverage[idx] = cov as u8;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public API: rasterize a glyph by ID
// ---------------------------------------------------------------------------

use read_fonts::{FontRef, TableProvider};

use super::{
    embolden::{compute_dilation, embolden_outline},
    metrics::{GlyphMetrics, RasterBuffer},
    outline::extract_outline,
};

/// Rasterize a glyph by its ID from font data into a coverage buffer.
///
/// Uses read-fonts for glyph outline extraction and analytic area coverage
/// for grayscale anti-aliasing.
///
/// Returns `None` if the glyph ID is invalid, has no outline (e.g. space
/// returns `Some` with zero-size bitmap), or exceeds the buffer dimensions.
pub fn rasterize(
    font_data: &[u8],
    glyph_id: u16,
    size_px: u16,
    buffer: &mut RasterBuffer,
    scratch: &mut RasterScratch,
    scale_factor: u16,
) -> Option<GlyphMetrics> {
    let size_px = size_px as u32;

    let (advance_fu, _lsb_fu, upem) =
        match extract_outline(font_data, glyph_id, &mut scratch.outline) {
            Some(v) => v,
            None => {
                // Try to get metrics even if outline is empty (space-like glyphs)
                let font = FontRef::new(font_data).ok()?;
                let head = font.head().ok()?;
                let upem = head.units_per_em();
                let hmtx = font.hmtx().ok()?;
                let hhea = font.hhea().ok()?;
                let num_h_metrics = hhea.number_of_h_metrics();

                if glyph_id >= font.maxp().ok()?.num_glyphs() {
                    return None;
                }

                let advance_fu = if (glyph_id as u16) < num_h_metrics {
                    let metrics = hmtx.h_metrics();
                    metrics.get(glyph_id as usize)?.advance.get()
                } else {
                    let metrics = hmtx.h_metrics();
                    metrics.get(num_h_metrics as usize - 1)?.advance.get()
                };

                // Check if glyph exists but has no outline (like space)
                let loca = font.loca(None).ok()?;
                let glyf = font.glyf().ok()?;
                let gid = read_fonts::types::GlyphId::new(glyph_id as u32);
                match loca.get_glyf(gid, &glyf) {
                    Ok(None) => {
                        // Empty glyph (space) — return metrics with zero bitmap
                        let advance = scale_fu(advance_fu as i32, size_px, upem) as u32;
                        return Some(GlyphMetrics {
                            width: 0,
                            height: 0,
                            bearing_x: 0,
                            bearing_y: 0,
                            advance,
                        });
                    }
                    Ok(Some(_)) => {
                        // Has glyph data but extract_outline failed (too complex?)
                        return None;
                    }
                    Err(_) => return None,
                }
            }
        };

    // Apply outline dilation for stem darkening (macOS Core Text formula).
    let (dil_x, dil_y) = compute_dilation(size_px as u16, upem, scale_factor);
    if dil_x != 0 || dil_y != 0 {
        embolden_outline(&mut scratch.outline, dil_x, dil_y);
    }

    // Read bounding box values (after dilation — bbox was updated).
    let x_min_fu = scratch.outline.x_min;
    let y_min_fu = scratch.outline.y_min;
    let x_max_fu = scratch.outline.x_max;
    let y_max_fu = scratch.outline.y_max;

    // Scale bounding box to pixels, then expand by 1px on each side to
    // prevent AA overshoot clipping at glyph edges.
    let x_min_px = scale_fu_floor(x_min_fu as i32, size_px, upem) - 1;
    let y_min_px = scale_fu_floor(y_min_fu as i32, size_px, upem);
    let x_max_px = scale_fu_ceil(x_max_fu as i32, size_px, upem) + 1;
    let y_max_px = scale_fu_ceil(y_max_fu as i32, size_px, upem) + 1;
    if x_max_px < x_min_px || y_max_px < y_min_px {
        return None;
    }
    let bmp_w = (x_max_px - x_min_px) as u32;
    let bmp_h = (y_max_px - y_min_px) as u32;

    if bmp_w == 0 || bmp_h == 0 {
        let advance = scale_fu(advance_fu as i32, size_px, upem) as u32;
        return Some(GlyphMetrics {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            advance,
        });
    }

    if bmp_w > buffer.width || bmp_h > buffer.height {
        return None;
    }

    // Analytic area coverage: exact signed-area trapezoid computation.
    // Output is 1 byte per pixel (grayscale coverage).
    let out_total = (bmp_w * bmp_h) as usize;

    if out_total > buffer.data.len() {
        return None;
    }

    // Clear the coverage region
    for b in buffer.data[..out_total].iter_mut() {
        *b = 0;
    }

    // Flatten outline into line segments
    scratch.num_segments = 0;
    flatten_outline_from_scratch(scratch, size_px, upem, x_min_px, y_max_px);

    // Rasterize at native width (no horizontal oversampling)
    rasterize_segments(scratch, &mut buffer.data[..out_total], bmp_w, bmp_h);

    let advance = scale_fu(advance_fu as i32, size_px, upem) as u32;
    let bearing_x = x_min_px;
    let bearing_y = y_max_px;

    Some(GlyphMetrics {
        width: bmp_w,
        height: bmp_h,
        bearing_x,
        bearing_y,
        advance,
    })
}
