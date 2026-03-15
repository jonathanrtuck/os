// Scanline rasterizer — converts glyph outlines to coverage maps.
//
// Pipeline: outline points → flatten beziers → line segments → scanline
// sweep with 4× vertical oversampling → coverage map (0–255 per pixel).
//
// All math is integer/fixed-point. No floating point, no allocations.
//
// NOTE: Rasterization now happens via shaping::rasterize which has its own
// copy of this algorithm using read-fonts for outline extraction. This copy
// is retained for the OVERSAMPLE_X/Y constants (used by GLYPH_BUF_SIZE)
// and the RasterScratch type (used by SVG rasterizer). Types that are not
// currently called are suppressed as dead code.

#[allow(dead_code)]
const MAX_SEGMENTS: usize = 2048;
#[allow(dead_code)]
const MAX_ACTIVE_EDGES: usize = 64;
#[allow(dead_code)]
const FP_SHIFT: i32 = 12;
#[allow(dead_code)]
const FP_ONE: i32 = 1 << FP_SHIFT;
#[allow(dead_code)]
const FP_HALF: i32 = FP_ONE / 2;

/// Horizontal oversampling factor for anti-aliasing.
/// Rasterise at OVERSAMPLE_X × width, then downsample into per-channel
/// (R, G, B) subpixel coverage. 6 = 3 subpixels × 2× oversampling each.
pub const OVERSAMPLE_X: i32 = 6;
/// Vertical oversampling factor for anti-aliasing.
pub const OVERSAMPLE_Y: i32 = 8;

/// A line segment in pixel-space fixed-point coordinates.
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct Segment {
    x0: i32, // fixed-point
    y0: i32,
    x1: i32,
    y1: i32,
}
/// An active edge during scanline sweep.
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct ActiveEdge {
    x: i32,         // current x intersection (fixed-point)
    x_step: i32,    // x change per sub-scanline (fixed-point)
    y_bottom: i32,  // bottom y of edge (fixed-point)
    direction: i32, // +1 or -1 for winding rule
}

/// Scratch space for rasterization. Caller allocates (typically in BSS).
///
/// Contains the glyph outline, segment array, and edge working space.
/// About 60 KiB total — negligible compared to surface buffers.
#[allow(dead_code)]
pub struct RasterScratch {
    pub outline: GlyphOutline,
    segments: [Segment; MAX_SEGMENTS],
    num_segments: usize,
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
// Coordinate conversion: font units → pixel-space fixed-point
// ---------------------------------------------------------------------------

/// Convert font units to fixed-point pixel coordinates.
/// `origin` is the origin offset (x_min or y_max of bounding box, in pixels)
/// that maps glyph coordinates to the coverage map's [0, 0] corner.
#[allow(dead_code)]
fn fu_to_fp(val: i32, size_px: u32, upem: u16, origin: i32) -> i32 {
    let px = (val as i64 * size_px as i64 * FP_ONE as i64) / upem as i64;

    px as i32 - origin * FP_ONE
}

// ---------------------------------------------------------------------------
// Bezier flattening
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn emit_segment(x0: i32, y0: i32, x1: i32, y1: i32, scratch: &mut RasterScratch) {
    if scratch.num_segments < MAX_SEGMENTS && y0 != y1 {
        scratch.segments[scratch.num_segments] = Segment { x0, y0, x1, y1 };
        scratch.num_segments += 1;
    }
}
/// Flatten one contour of the outline (index-based to avoid borrow conflict).
#[allow(dead_code)]
fn flatten_contour_from_scratch(
    scratch: &mut RasterScratch,
    start: usize,
    end: usize,
    size_px: u32,
    upem: u16,
    x_origin: i32,
    y_origin: i32,
) {
    // Walk the contour. TrueType contours can have:
    //   on, on           → line segment
    //   on, off, on      → quadratic bezier
    //   on, off, off     → two beziers with implied on-curve midpoint
    //   off, off, ...    → implied on-curve between each pair

    let mut i = start;

    while i <= end {
        let i_next = if i == end { start } else { i + 1 };
        let cur_on = scratch.outline.points[i].on_curve;

        if cur_on {
            let next_on = scratch.outline.points[i_next].on_curve;
            let (x0, y0) = outline_point_to_fp(scratch, i, size_px, upem, x_origin, y_origin);

            if next_on {
                // Line segment: on → on.
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
                    // on, off, on → one quadratic bezier.
                    let (x2, y2) =
                        outline_point_to_fp(scratch, i_after, size_px, upem, x_origin, y_origin);

                    flatten_quadratic(x0, y0, cx, cy, x2, y2, scratch, 0);

                    i += 2;
                } else {
                    // on, off, off → implied midpoint.
                    let (cx2, cy2) =
                        outline_point_to_fp(scratch, i_after, size_px, upem, x_origin, y_origin);
                    let mid_x = (cx + cx2) >> 1;
                    let mid_y = (cy + cy2) >> 1;

                    flatten_quadratic(x0, y0, cx, cy, mid_x, mid_y, scratch, 0);

                    i += 1;
                }
            }
        } else {
            // Off-curve point: handle implied on-curve between consecutive off-curves.
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
/// Walk all contours in scratch.outline, flatten beziers into scratch.segments.
///
/// Uses index-based access to avoid borrow conflicts (outline and segments
/// live in the same RasterScratch struct).
#[allow(dead_code)]
fn flatten_outline_from_scratch(
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
/// Recursively flatten a quadratic bezier (p0, p1, p2) into line segments.
///
/// Uses De Casteljau subdivision. Stops when the control point is close
/// enough to the chord midpoint (flatness test) or at max recursion depth.
#[allow(dead_code)]
fn flatten_quadratic(
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32, // control point
    x2: i32,
    y2: i32,
    scratch: &mut RasterScratch,
    depth: u32,
) {
    if scratch.num_segments >= MAX_SEGMENTS {
        return;
    }

    // Flatness test: distance from control point to chord midpoint.
    let mx = (x0 + x2) >> 1;
    let my = (y0 + y2) >> 1;
    let dx = x1 - mx;
    let dy = y1 - my;
    // Threshold: (0.5 pixel)^2 in fixed-point = (FP_ONE/2)^2.
    // But we compare dx*dx + dy*dy which is in FP^2 units.
    let flatness = (FP_ONE / 2) as i64 * (FP_ONE / 2) as i64;
    let dist_sq = dx as i64 * dx as i64 + dy as i64 * dy as i64;

    if depth >= 8 || dist_sq <= flatness {
        // Flat enough — emit line segment.
        emit_segment(x0, y0, x2, y2, scratch);
        return;
    }

    // De Casteljau split at t=0.5.
    let q0x = (x0 + x1) >> 1;
    let q0y = (y0 + y1) >> 1;
    let q1x = (x1 + x2) >> 1;
    let q1y = (y1 + y2) >> 1;
    let rx = (q0x + q1x) >> 1;
    let ry = (q0y + q1y) >> 1;

    flatten_quadratic(x0, y0, q0x, q0y, rx, ry, scratch, depth + 1);
    flatten_quadratic(rx, ry, q1x, q1y, x2, y2, scratch, depth + 1);
}
/// Convert an outline point to fixed-point pixel coordinates.
/// Reads directly from scratch.outline.points[i].
#[allow(dead_code)]
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
    // Flip Y: TrueType Y is up, coverage map Y is down.
    let fy = y_origin * FP_ONE - fu_to_fp(p.y, size_px, upem, 0);

    (fx, fy)
}

// ---------------------------------------------------------------------------
// Scanline rasterizer
// ---------------------------------------------------------------------------

/// Add coverage for a horizontal span within one sub-scanline.
#[allow(dead_code)]
fn fill_coverage_span(
    coverage: &mut [u8],
    width: u32,
    row: u32,
    x_start_fp: i32,
    x_end_fp: i32,
    oversample: i32,
) {
    // Convert fixed-point x to pixel coordinates.
    let contribution = (256 / oversample) as u16; // coverage per sub-scanline
                                                  // Pixel range (inclusive start, exclusive end).
    let px_start = x_start_fp >> FP_SHIFT;
    let px_end = (x_end_fp + FP_ONE - 1) >> FP_SHIFT;
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

        if idx < coverage.len() {
            // Partial coverage at edges, full coverage in the middle.
            let cov = if px as i32 == (x_start_fp >> FP_SHIFT)
                && px as i32 == ((x_end_fp - 1) >> FP_SHIFT)
            {
                // Span fits within one pixel.
                let frac = x_end_fp - x_start_fp;

                (contribution as i32 * frac / FP_ONE) as u16
            } else if px as i32 == (x_start_fp >> FP_SHIFT) {
                // Left edge: partial coverage.
                let right_edge = ((px + 1) as i32) << FP_SHIFT;
                let frac = right_edge - x_start_fp;

                (contribution as i32 * frac / FP_ONE) as u16
            } else if px as i32 == ((x_end_fp - 1) >> FP_SHIFT) {
                // Right edge: partial coverage.
                let left_edge = (px as i32) << FP_SHIFT;
                let frac = x_end_fp - left_edge;

                (contribution as i32 * frac / FP_ONE) as u16
            } else {
                // Fully covered.
                contribution
            };

            let val = coverage[idx] as u16 + cov;

            coverage[idx] = if val > 255 { 255 } else { val as u8 };
        }
    }
}
/// Rasterize line segments into a coverage map using scanline sweep.
///
/// Uses non-zero winding rule with 4× vertical oversampling.
#[allow(dead_code)]
fn rasterize_segments(scratch: &RasterScratch, coverage: &mut [u8], width: u32, height: u32) {
    let nseg = scratch.num_segments;

    if nseg == 0 {
        return;
    }

    // Active edge storage.
    let mut active: [ActiveEdge; MAX_ACTIVE_EDGES] = [ActiveEdge {
        x: 0,
        x_step: 0,
        y_bottom: 0,
        direction: 0,
    }; MAX_ACTIVE_EDGES];
    let mut num_active: usize;

    // Process each pixel row.
    for row in 0..height {
        let y_top_fp = row as i32 * FP_ONE;
        let _y_bot_fp = y_top_fp + FP_ONE;
        // Process OVERSAMPLE_Y sub-scanlines within this pixel row.
        let sub_step = FP_ONE / OVERSAMPLE_Y;

        for sub in 0..OVERSAMPLE_Y {
            let scan_y = y_top_fp + sub * sub_step + sub_step / 2;

            // Add new edges that start at or above this scanline.
            for si in 0..nseg {
                let seg = &scratch.segments[si];
                let (y_top, y_bot, x_top, x_bot, dir) = if seg.y0 < seg.y1 {
                    (seg.y0, seg.y1, seg.x0, seg.x1, 1i32)
                } else {
                    (seg.y1, seg.y0, seg.x1, seg.x0, -1i32)
                };

                // Edge should be active for this sub-scanline?
                if y_top <= scan_y && y_bot > scan_y {
                    // Check if we already have this edge active.
                    // (Simple approach: rebuild active list each sub-scanline.)
                    // This is O(n*m) but n and m are small for font glyphs.
                    // We'll use a simpler approach below.
                    let _ = (x_top, x_bot, dir); // used below
                }
            }

            // Simpler approach: rebuild active edges per sub-scanline.
            num_active = 0;

            for si in 0..nseg {
                let seg = &scratch.segments[si];
                let (y_top, y_bot, x_top, x_bot, dir) = if seg.y0 < seg.y1 {
                    (seg.y0, seg.y1, seg.x0, seg.x1, 1i32)
                } else {
                    (seg.y1, seg.y0, seg.x1, seg.x0, -1i32)
                };

                if y_top > scan_y || y_bot <= scan_y {
                    continue;
                }
                if num_active >= MAX_ACTIVE_EDGES {
                    break;
                }
                // Compute x at scan_y via linear interpolation.
                let dy = y_bot - y_top;
                let t = scan_y - y_top;
                let x = if dy == 0 {
                    x_top
                } else {
                    x_top + ((x_bot - x_top) as i64 * t as i64 / dy as i64) as i32
                };

                active[num_active] = ActiveEdge {
                    x,
                    x_step: 0,
                    y_bottom: y_bot,
                    direction: dir,
                };
                num_active += 1;
            }

            // Sort active edges by x (insertion sort — small N).
            for i in 1..num_active {
                let key = active[i];
                let mut j = i;

                while j > 0 && active[j - 1].x > key.x {
                    active[j] = active[j - 1];
                    j -= 1;
                }

                active[j] = key;
            }

            // Apply non-zero winding rule: walk left-to-right, toggle winding.
            let mut winding: i32 = 0;
            let mut edge_idx = 0;

            while edge_idx < num_active {
                let old_winding = winding;

                winding += active[edge_idx].direction;

                if old_winding == 0 && winding != 0 {
                    // Entering filled region.
                    let x_start = active[edge_idx].x;
                    // Find where we exit.
                    let mut ei = edge_idx + 1;

                    while ei < num_active {
                        winding += active[ei].direction;

                        if winding == 0 {
                            let x_end = active[ei].x;

                            // Fill pixels from x_start to x_end.
                            fill_coverage_span(coverage, width, row, x_start, x_end, OVERSAMPLE_Y);

                            edge_idx = ei + 1;

                            break;
                        }
                        ei += 1;
                    }

                    if winding != 0 {
                        // Unbalanced — shouldn't happen with valid outlines.
                        break;
                    }
                } else {
                    edge_idx += 1;
                }
            }
        }
    }
}
