//! Glyph rasterizer — converts glyph outlines to coverage maps using read-fonts.
//!
//! Uses read-fonts for glyph outline extraction and metrics, then runs the
//! scanline rasterizer algorithm (bezier flattening, active edge sweep, coverage
//! map generation with subpixel rendering).
//!
//! All math is integer/fixed-point. No floating point in the rasterizer itself.

use read_fonts::{FontRef, TableProvider};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Metrics for a single rasterized glyph.
#[derive(Clone, Copy, Debug)]
pub struct GlyphMetrics {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal offset from pen position to left edge of bitmap.
    pub bearing_x: i32,
    /// Vertical offset from baseline to top edge of bitmap (positive = up).
    pub bearing_y: i32,
    /// Horizontal advance to next glyph in pixels.
    pub advance: u32,
}

/// Caller-provided buffer for rasterization output (1 byte per pixel coverage).
pub struct RasterBuffer<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}

// ---------------------------------------------------------------------------
// Glyph outline (intermediate, used during rasterization)
// ---------------------------------------------------------------------------

/// Maximum points per glyph outline.
const MAX_GLYPH_POINTS: usize = 512;
/// Maximum contours per glyph.
const MAX_CONTOURS: usize = 64;

/// Decoded glyph outline — contours of on-curve and off-curve points.
pub struct GlyphOutline {
    pub points: [GlyphPoint; MAX_GLYPH_POINTS],
    pub num_points: u16,
    pub contour_ends: [u16; MAX_CONTOURS],
    pub num_contours: u16,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

/// A point in a glyph outline, in font units.
#[derive(Clone, Copy, Default)]
pub struct GlyphPoint {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

impl GlyphOutline {
    pub const fn zeroed() -> Self {
        GlyphOutline {
            points: [GlyphPoint {
                x: 0,
                y: 0,
                on_curve: false,
            }; MAX_GLYPH_POINTS],
            num_points: 0,
            contour_ends: [0u16; MAX_CONTOURS],
            num_contours: 0,
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Scanline rasterizer constants and types
// ---------------------------------------------------------------------------

/// Maximum line segments after bezier flattening.
const MAX_SEGMENTS: usize = 2048;
/// Maximum edges active on a single scanline.
const MAX_ACTIVE_EDGES: usize = 64;
/// Fixed-point 20.12 format for sub-pixel precision.
const FP_SHIFT: i32 = 12;
const FP_ONE: i32 = 1 << FP_SHIFT;

/// Horizontal oversampling factor for anti-aliasing.
/// 6 = 3 subpixels × 2× oversampling each.
pub const OVERSAMPLE_X: i32 = 6;
/// Vertical oversampling factor for anti-aliasing.
pub const OVERSAMPLE_Y: i32 = 4;

/// Maximum glyph dimensions for buffer sizing.
const GLYPH_MAX_W: usize = 48;

/// Tunable boost constant for stem darkening.
pub const STEM_DARKENING_BOOST: u32 = 70;

/// Pre-computed lookup table for stem darkening.
pub const STEM_DARKENING_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let boost = STEM_DARKENING_BOOST;
    let mut i = 1u32;
    while i < 256 {
        let darkened = i + boost * (255 - i) / 255;
        lut[i as usize] = if darkened > 255 { 255 } else { darkened as u8 };
        i += 1;
    }
    lut
};

/// A line segment in pixel-space fixed-point coordinates.
#[derive(Clone, Copy, Default)]
struct Segment {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

/// An active edge during scanline sweep.
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct ActiveEdge {
    x: i32,
    x_step: i32,
    y_bottom: i32,
    direction: i32,
}

/// Scratch space for rasterization. Caller allocates (typically in BSS).
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
// read-fonts outline extraction
// ---------------------------------------------------------------------------

/// Extract glyph outline from font data using read-fonts.
///
/// Populates `outline` with contour points for the given glyph ID.
/// Returns `(advance_width_fu, lsb_fu, upem)` on success, or None if
/// the glyph has no outline (e.g., space) or the glyph ID is invalid.
fn extract_outline(
    font_data: &[u8],
    glyph_id: u16,
    outline: &mut GlyphOutline,
) -> Option<(u16, i16, u16)> {
    let font = FontRef::new(font_data).ok()?;

    // Get units_per_em
    let head = font.head().ok()?;
    let upem = head.units_per_em();

    // Get horizontal metrics
    let hmtx = font.hmtx().ok()?;
    let hhea = font.hhea().ok()?;
    let num_h_metrics = hhea.number_of_h_metrics();
    let gid = read_fonts::types::GlyphId::new(glyph_id as u32);

    let (advance_fu, lsb_fu) = if (glyph_id as u16) < num_h_metrics {
        let metrics = hmtx.h_metrics();
        let m = metrics.get(glyph_id as usize)?;
        (m.advance.get(), m.side_bearing.get())
    } else {
        // Glyphs beyond num_h_metrics share the last advance width
        let metrics = hmtx.h_metrics();
        let last = metrics.get(num_h_metrics as usize - 1)?;
        let advance = last.advance.get();
        let lsb_data = hmtx.left_side_bearings();
        let lsb_idx = (glyph_id as usize).checked_sub(num_h_metrics as usize)?;
        let lsb = lsb_data.get(lsb_idx).map(|v| v.get()).unwrap_or(0);
        (advance, lsb)
    };

    // Get glyph outline from glyf table
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;

    outline.num_points = 0;
    outline.num_contours = 0;

    // Get the glyph data
    let glyph_data = loca.get_glyf(gid, &glyf).ok()??;

    match glyph_data {
        read_fonts::tables::glyf::Glyph::Simple(simple) => {
            // Extract bounding box
            outline.x_min = simple.x_min();
            outline.y_min = simple.y_min();
            outline.x_max = simple.x_max();
            outline.y_max = simple.y_max();

            // Extract contours and points
            let num_contours = simple.number_of_contours() as usize;
            if num_contours > MAX_CONTOURS {
                return None;
            }

            let end_pts = simple.end_pts_of_contours();
            for (i, ep) in end_pts.iter().enumerate() {
                if i >= MAX_CONTOURS {
                    return None;
                }
                outline.contour_ends[i] = ep.get();
            }
            outline.num_contours = num_contours as u16;

            // Iterate points
            let mut pt_idx = 0usize;
            let num_points = simple.num_points();
            if num_points > MAX_GLYPH_POINTS {
                return None;
            }

            for point in simple.points() {
                if pt_idx >= MAX_GLYPH_POINTS {
                    return None;
                }
                outline.points[pt_idx] = GlyphPoint {
                    x: point.x as i32,
                    y: point.y as i32,
                    on_curve: point.on_curve,
                };
                pt_idx += 1;
            }
            outline.num_points = pt_idx as u16;
        }
        read_fonts::tables::glyf::Glyph::Composite(composite) => {
            // Extract bounding box
            outline.x_min = composite.x_min();
            outline.y_min = composite.y_min();
            outline.x_max = composite.x_max();
            outline.y_max = composite.y_max();

            // For composite glyphs, recursively extract component outlines
            for component in composite.components() {
                let comp_gid = component.glyph.to_u32() as u16;
                let flags = component.flags;

                // Get component offsets
                let (dx, dy) = match component.anchor {
                    read_fonts::tables::glyf::Anchor::Offset { x, y } => (x as i32, y as i32),
                    _ => (0, 0),
                };

                // Recursively extract the component outline
                let pts_before = outline.num_points as usize;
                let contours_before = outline.num_contours as usize;

                // Get component glyph data
                let comp_gid_rf = read_fonts::types::GlyphId::new(comp_gid as u32);
                if let Ok(Some(comp_data)) = loca.get_glyf(comp_gid_rf, &glyf) {
                    match comp_data {
                        read_fonts::tables::glyf::Glyph::Simple(comp_simple) => {
                            let comp_nc = comp_simple.number_of_contours() as usize;
                            if contours_before + comp_nc > MAX_CONTOURS {
                                continue;
                            }

                            let comp_end_pts = comp_simple.end_pts_of_contours();
                            for (i, ep) in comp_end_pts.iter().enumerate() {
                                outline.contour_ends[contours_before + i] =
                                    ep.get() + pts_before as u16;
                            }
                            outline.num_contours = (contours_before + comp_nc) as u16;

                            let mut pt_idx = pts_before;
                            for point in comp_simple.points() {
                                if pt_idx >= MAX_GLYPH_POINTS {
                                    break;
                                }
                                outline.points[pt_idx] = GlyphPoint {
                                    x: point.x as i32 + dx,
                                    y: point.y as i32 + dy,
                                    on_curve: point.on_curve,
                                };
                                pt_idx += 1;
                            }
                            outline.num_points = pt_idx as u16;
                        }
                        _ => {
                            // Nested composites: skip for simplicity
                            continue;
                        }
                    }
                }

                let _ = flags; // flags used above for anchor type
            }

            if outline.num_points == 0 {
                return None;
            }
        }
    }

    Some((advance_fu, lsb_fu, upem))
}

// ---------------------------------------------------------------------------
// Coordinate scaling helpers (integer only)
// ---------------------------------------------------------------------------

fn scale_fu(val: i32, size_px: u32, upem: u16) -> i32 {
    ((val as i64 * size_px as i64) / upem as i64) as i32
}

fn scale_fu_ceil(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;
    if n > 0 {
        ((n + d - 1) / d) as i32
    } else {
        (n / d) as i32
    }
}

fn scale_fu_floor(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;
    if n < 0 {
        ((n - d + 1) / d) as i32
    } else {
        (n / d) as i32
    }
}

// ---------------------------------------------------------------------------
// Coordinate conversion: font units → pixel-space fixed-point
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
// Scanline rasterizer
// ---------------------------------------------------------------------------

fn fill_coverage_span(
    coverage: &mut [u8],
    width: u32,
    row: u32,
    x_start_fp: i32,
    x_end_fp: i32,
    oversample: i32,
) {
    let contribution = (256 / oversample) as u16;
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
            let cov = if px as i32 == (x_start_fp >> FP_SHIFT)
                && px as i32 == ((x_end_fp - 1) >> FP_SHIFT)
            {
                let frac = x_end_fp - x_start_fp;
                (contribution as i32 * frac / FP_ONE) as u16
            } else if px as i32 == (x_start_fp >> FP_SHIFT) {
                let right_edge = ((px + 1) as i32) << FP_SHIFT;
                let frac = right_edge - x_start_fp;
                (contribution as i32 * frac / FP_ONE) as u16
            } else if px as i32 == ((x_end_fp - 1) >> FP_SHIFT) {
                let left_edge = (px as i32) << FP_SHIFT;
                let frac = x_end_fp - left_edge;
                (contribution as i32 * frac / FP_ONE) as u16
            } else {
                contribution
            };
            let val = coverage[idx] as u16 + cov;
            coverage[idx] = if val > 255 { 255 } else { val as u8 };
        }
    }
}

fn rasterize_segments(scratch: &RasterScratch, coverage: &mut [u8], width: u32, height: u32) {
    let nseg = scratch.num_segments;
    if nseg == 0 {
        return;
    }
    let mut active: [ActiveEdge; MAX_ACTIVE_EDGES] = [ActiveEdge {
        x: 0,
        x_step: 0,
        y_bottom: 0,
        direction: 0,
    }; MAX_ACTIVE_EDGES];
    let mut num_active: usize;
    for row in 0..height {
        let y_top_fp = row as i32 * FP_ONE;
        let sub_step = FP_ONE / OVERSAMPLE_Y;
        for sub in 0..OVERSAMPLE_Y {
            let scan_y = y_top_fp + sub * sub_step + sub_step / 2;
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
            // Apply non-zero winding rule.
            let mut winding: i32 = 0;
            let mut edge_idx = 0;
            while edge_idx < num_active {
                let old_winding = winding;
                winding += active[edge_idx].direction;
                if old_winding == 0 && winding != 0 {
                    let x_start = active[edge_idx].x;
                    let mut ei = edge_idx + 1;
                    while ei < num_active {
                        winding += active[ei].direction;
                        if winding == 0 {
                            let x_end = active[ei].x;
                            fill_coverage_span(coverage, width, row, x_start, x_end, OVERSAMPLE_Y);
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
    }
}

// ---------------------------------------------------------------------------
// Public API: rasterize a glyph by ID
// ---------------------------------------------------------------------------

/// Rasterize a glyph by its ID from font data into a coverage buffer.
///
/// Uses read-fonts for glyph outline extraction and the scanline rasterizer
/// for coverage map generation with subpixel rendering.
///
/// Returns `None` if the glyph ID is invalid, has no outline (e.g. space
/// returns `Some` with zero-size bitmap), or exceeds the buffer dimensions.
pub fn rasterize(
    font_data: &[u8],
    glyph_id: u16,
    size_px: u16,
    buffer: &mut RasterBuffer,
    scratch: &mut RasterScratch,
) -> Option<GlyphMetrics> {
    let size_px = size_px as u32;

    let (advance_fu, lsb_fu, upem) =
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

    // Read bounding box values
    let x_min_fu = scratch.outline.x_min;
    let y_min_fu = scratch.outline.y_min;
    let x_max_fu = scratch.outline.x_max;
    let y_max_fu = scratch.outline.y_max;

    // Scale bounding box to pixels
    let x_min_px = scale_fu_floor(x_min_fu as i32, size_px, upem);
    let y_min_px = scale_fu_floor(y_min_fu as i32, size_px, upem);
    let x_max_px = scale_fu_ceil(x_max_fu as i32, size_px, upem);
    let y_max_px = scale_fu_ceil(y_max_fu as i32, size_px, upem);
    let _ = y_min_px;
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

    // Subpixel rendering: rasterize at OVERSAMPLE_X × width
    let over_w = bmp_w * OVERSAMPLE_X as u32;
    let over_total = (over_w * bmp_h) as usize;
    let out_total = (bmp_w * bmp_h * 3) as usize;

    if over_total > buffer.data.len() {
        return None;
    }

    // Clear the oversampled coverage region
    for b in buffer.data[..over_total].iter_mut() {
        *b = 0;
    }

    // Flatten outline into line segments
    scratch.num_segments = 0;
    flatten_outline_from_scratch(scratch, size_px, upem, x_min_px, y_max_px);

    // Scale segment x-coordinates by OVERSAMPLE_X
    for i in 0..scratch.num_segments {
        scratch.segments[i].x0 *= OVERSAMPLE_X;
        scratch.segments[i].x1 *= OVERSAMPLE_X;
    }

    // Rasterize at oversampled width
    rasterize_segments(scratch, &mut buffer.data[..over_total], over_w, bmp_h);

    // Downsample into 3-channel (RGB) subpixel coverage
    let samples_per_channel = (OVERSAMPLE_X / 3) as u32;
    for row in 0..bmp_h {
        for col in 0..bmp_w {
            let src_base = (row * over_w + col * OVERSAMPLE_X as u32) as usize;
            let dst_base = (row * bmp_w * 3 + col * 3) as usize;
            let mut sum_r = 0u32;
            for s in 0..samples_per_channel {
                sum_r += buffer.data[src_base + s as usize] as u32;
            }
            let mut sum_g = 0u32;
            for s in 0..samples_per_channel {
                sum_g += buffer.data[src_base + samples_per_channel as usize + s as usize] as u32;
            }
            let mut sum_b = 0u32;
            for s in 0..samples_per_channel {
                sum_b +=
                    buffer.data[src_base + 2 * samples_per_channel as usize + s as usize] as u32;
            }
            buffer.data[dst_base] = (sum_r / samples_per_channel) as u8;
            buffer.data[dst_base + 1] = (sum_g / samples_per_channel) as u8;
            buffer.data[dst_base + 2] = (sum_b / samples_per_channel) as u8;
        }
    }

    // FIR color-fringe filter [1/4, 1/2, 1/4]
    {
        let stride3 = (bmp_w * 3) as usize;
        let mut tmp = [0u8; GLYPH_MAX_W * 3];
        for row in 0..bmp_h {
            let row_start = (row * bmp_w * 3) as usize;
            for i in 0..stride3 {
                tmp[i] = buffer.data[row_start + i];
            }
            for i in 0..stride3 {
                let prev = if i > 0 {
                    tmp[i - 1] as u32
                } else {
                    tmp[0] as u32
                };
                let curr = tmp[i] as u32;
                let next = if i + 1 < stride3 {
                    tmp[i + 1] as u32
                } else {
                    tmp[stride3 - 1] as u32
                };
                let filtered = (prev + 2 * curr + next + 2) / 4;
                buffer.data[row_start + i] = if filtered > 255 { 255 } else { filtered as u8 };
            }
        }
    }

    // Stem darkening
    {
        for i in 0..out_total {
            buffer.data[i] = STEM_DARKENING_LUT[buffer.data[i] as usize];
        }
    }

    let advance = scale_fu(advance_fu as i32, size_px, upem) as u32;
    let bearing_x = scale_fu(lsb_fu as i32, size_px, upem);
    let bearing_y = y_max_px;

    Some(GlyphMetrics {
        width: bmp_w,
        height: bmp_h,
        bearing_x,
        bearing_y,
        advance,
    })
}
