//! Glyph rasterizer — converts glyph outlines to coverage maps using read-fonts.
//!
//! Uses read-fonts for glyph outline extraction and metrics, then runs the
//! scanline rasterizer algorithm (bezier flattening, active edge sweep, coverage
//! map generation with grayscale anti-aliasing via vertical oversampling).
//!
//! Output is 1 byte per pixel (grayscale coverage). No subpixel (LCD) rendering.
//!
//! All math is integer/fixed-point. No floating point in the rasterizer itself.

use read_fonts::{tables::cmap::Cmap, FontRef, TableProvider};

// ---------------------------------------------------------------------------
// Font metric helpers
// ---------------------------------------------------------------------------

/// Basic font metrics extracted from the hhea and head tables.
pub struct FontMetrics {
    pub units_per_em: u16,
    /// hhea ascent (positive above baseline, in font units).
    pub ascent: i16,
    /// hhea descent (negative below baseline, in font units).
    pub descent: i16,
    /// hhea line gap (in font units).
    pub line_gap: i16,
}

/// Extract basic font metrics from raw font data.
pub fn font_metrics(font_data: &[u8]) -> Option<FontMetrics> {
    let font = FontRef::new(font_data).ok()?;
    let head = font.head().ok()?;
    let hhea = font.hhea().ok()?;

    Some(FontMetrics {
        units_per_em: head.units_per_em(),
        ascent: hhea.ascender().to_i16(),
        descent: hhea.descender().to_i16(),
        line_gap: hhea.line_gap().to_i16(),
    })
}

/// Look up the glyph ID for a Unicode codepoint using the font's cmap table.
pub fn glyph_id_for_char(font_data: &[u8], codepoint: char) -> Option<u16> {
    let font = FontRef::new(font_data).ok()?;
    let cmap = font.cmap().ok()?;

    cmap_lookup(&cmap, codepoint as u32)
}

/// Look up a codepoint in the cmap table, trying all supported subtables.
fn cmap_lookup(cmap: &Cmap, codepoint: u32) -> Option<u16> {
    for record in cmap.encoding_records() {
        if let Ok(subtable) = record.subtable(cmap.offset_data()) {
            if let Some(gid) = subtable.map_codepoint(codepoint) {
                let id = gid.to_u32() as u16;
                if id > 0 {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Get horizontal metrics for a glyph: (advance_width, left_side_bearing).
pub fn glyph_h_metrics(font_data: &[u8], glyph_id: u16) -> Option<(u16, i16)> {
    let font = FontRef::new(font_data).ok()?;
    let hmtx = font.hmtx().ok()?;
    let hhea = font.hhea().ok()?;
    let num_h_metrics = hhea.number_of_h_metrics();

    if (glyph_id as u16) < num_h_metrics {
        let metrics = hmtx.h_metrics();
        let m = metrics.get(glyph_id as usize)?;
        Some((m.advance.get(), m.side_bearing.get()))
    } else {
        let metrics = hmtx.h_metrics();
        let last = metrics.get(num_h_metrics as usize - 1)?;
        let advance = last.advance.get();
        let lsb_data = hmtx.left_side_bearings();
        let lsb_idx = (glyph_id as usize).checked_sub(num_h_metrics as usize)?;
        let lsb = lsb_data.get(lsb_idx).map(|v| v.get()).unwrap_or(0);
        Some((advance, lsb))
    }
}

// ---------------------------------------------------------------------------
// Variable font axis helpers
// ---------------------------------------------------------------------------

/// Information about a single variation axis in a variable font.
#[derive(Debug, Clone)]
pub struct FontAxis {
    /// 4-byte axis tag (e.g. b"wght", b"opsz", b"wdth", b"YTLC").
    pub tag: [u8; 4],
    /// Minimum axis value.
    pub min_value: f32,
    /// Default axis value.
    pub default_value: f32,
    /// Maximum axis value.
    pub max_value: f32,
}

/// A user-specified axis value for variable font rendering (e.g., wght=700).
#[derive(Debug, Clone, Copy)]
pub struct AxisValue {
    /// 4-byte axis tag (e.g. b"wght", b"opsz").
    pub tag: [u8; 4],
    /// Desired axis value in design-space units.
    pub value: f32,
}

/// Parse all variation axes from a variable font.
///
/// Returns an empty `Vec` for non-variable fonts or parse failure.
pub fn font_axes(font_data: &[u8]) -> alloc::vec::Vec<FontAxis> {
    let font = match FontRef::new(font_data) {
        Ok(f) => f,
        Err(_) => return alloc::vec::Vec::new(),
    };
    let fvar = match font.fvar() {
        Ok(f) => f,
        Err(_) => return alloc::vec::Vec::new(),
    };
    let axes = match fvar.axes() {
        Ok(a) => a,
        Err(_) => return alloc::vec::Vec::new(),
    };
    axes.iter()
        .map(|axis| FontAxis {
            tag: axis.axis_tag().into_bytes(),
            min_value: axis.min_value().to_f32(),
            default_value: axis.default_value().to_f32(),
            max_value: axis.max_value().to_f32(),
        })
        .collect()
}

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

/// Vertical oversampling factor for anti-aliasing.
pub const OVERSAMPLE_Y: i32 = 8;

/// Maximum glyph dimensions for buffer sizing.
const GLYPH_MAX_W: usize = 50;

/// Tunable boost constant for stem darkening.
pub const STEM_DARKENING_BOOST: u32 = 90;

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

pub(crate) fn scale_fu(val: i32, size_px: u32, upem: u16) -> i32 {
    ((val as i64 * size_px as i64) / upem as i64) as i32
}

pub(crate) fn scale_fu_ceil(val: i32, size_px: u32, upem: u16) -> i32 {
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

    // Scale bounding box to pixels, then expand by 1px on each side to
    // prevent AA overshoot clipping at glyph edges.
    let x_min_px = scale_fu_floor(x_min_fu as i32, size_px, upem) - 1;
    let y_min_px = scale_fu_floor(y_min_fu as i32, size_px, upem);
    let x_max_px = scale_fu_ceil(x_max_fu as i32, size_px, upem) + 1;
    let y_max_px = scale_fu_ceil(y_max_fu as i32, size_px, upem) + 1;
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

    // Grayscale anti-aliasing: rasterize at 1× width with vertical oversampling.
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

    // Stem darkening (applied per grayscale byte)
    {
        for i in 0..out_total {
            buffer.data[i] = STEM_DARKENING_LUT[buffer.data[i] as usize];
        }
    }

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

// ---------------------------------------------------------------------------
// Variable font axis support: rasterize with gvar deltas
// ---------------------------------------------------------------------------

/// Normalize a user-space axis value to the F2Dot14 range (-1.0 to ~+1.0)
/// using the font's axis min/default/max.
///
/// - value == default → 0.0
/// - value < default → (value - default) / (default - min)  (range [-1, 0])
/// - value > default → (value - default) / (max - default)  (range [0, 1])
/// - Out-of-range values are clamped to the font's axis range first.
fn normalize_axis_value(value: f32, min: f32, default: f32, max: f32) -> f32 {
    // Clamp to font's valid range.
    let clamped = if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    };

    if (clamped - default).abs() < f32::EPSILON {
        0.0
    } else if clamped < default {
        let range = default - min;
        if range.abs() < f32::EPSILON {
            0.0
        } else {
            (clamped - default) / range
        }
    } else {
        let range = max - default;
        if range.abs() < f32::EPSILON {
            0.0
        } else {
            (clamped - default) / range
        }
    }
}

/// Build normalized F2Dot14 coordinate array from user-space axis values.
///
/// Returns a Vec of F2Dot14 values, one per axis in the font's fvar table.
/// Axes not specified in `axis_values` use the default (0.0).
fn build_normalized_coords(
    font_data: &[u8],
    axis_values: &[AxisValue],
) -> alloc::vec::Vec<read_fonts::types::F2Dot14> {
    let font_axes = font_axes(font_data);
    font_axes
        .iter()
        .map(|axis| {
            let user_val = axis_values
                .iter()
                .find(|av| av.tag == axis.tag)
                .map(|av| av.value)
                .unwrap_or(axis.default_value);
            let norm =
                normalize_axis_value(user_val, axis.min_value, axis.default_value, axis.max_value);
            read_fonts::types::F2Dot14::from_f32(norm)
        })
        .collect()
}

/// Interpolation of Unreferenced Points (IUP) — OpenType gvar spec.
///
/// When gvar stores sparse deltas (only some points have explicit deltas),
/// unreferenced points must be interpolated from neighboring referenced
/// points within the same contour.
fn iup_contour(
    orig: &[GlyphPoint],
    delta_x: &mut [i32],
    delta_y: &mut [i32],
    touched: &[bool],
    start: usize,
    end: usize, // inclusive
) {
    let n = end - start + 1;
    if n == 0 {
        return;
    }

    // Find first touched point in this contour.
    let first_touched = (start..=end).find(|&i| touched[i]);
    let first_touched = match first_touched {
        Some(ft) => ft,
        None => return, // No touched points — deltas stay 0.
    };

    // Check if all points are touched.
    if (start..=end).all(|i| touched[i]) {
        return;
    }

    // Walk the contour, interpolating runs of untouched points.
    let mut i = first_touched;
    loop {
        // Skip touched points.
        while touched[i] {
            let next = if i == end { start } else { i + 1 };
            if next == first_touched && touched[next] {
                return; // Wrapped around — done.
            }
            i = next;
        }

        // i is the first untouched point. Find the run.
        let run_start = i;
        // prev_touched is the touched point before this run.
        let prev_touched = if run_start == start { end } else { run_start - 1 };
        // Find end of untouched run.
        while !touched[i] {
            let next = if i == end { start } else { i + 1 };
            if next == run_start {
                break; // Shouldn't happen (we know at least one is touched).
            }
            i = next;
        }
        // i is the next touched point after the run.
        let next_touched = i;

        // Interpolate each axis independently.
        for axis in 0..2u8 {
            let get_coord = |idx: usize| -> i32 {
                if axis == 0 {
                    orig[idx].x
                } else {
                    orig[idx].y
                }
            };
            let get_delta = |idx: usize| -> i32 {
                if axis == 0 {
                    delta_x[idx]
                } else {
                    delta_y[idx]
                }
            };
            let set_delta = |idx: usize, val: i32, dx: &mut [i32], dy: &mut [i32]| {
                if axis == 0 {
                    dx[idx] = val;
                } else {
                    dy[idx] = val;
                }
            };

            let a_coord = get_coord(prev_touched);
            let b_coord = get_coord(next_touched);
            let a_delta = get_delta(prev_touched);
            let b_delta = get_delta(next_touched);

            // Walk the untouched run.
            let mut j = run_start;
            loop {
                let p_coord = get_coord(j);

                let interp = if a_coord == b_coord {
                    // Both reference points same coord — average deltas.
                    (a_delta + b_delta + 1) / 2
                } else {
                    let (lo_coord, lo_delta, hi_coord, hi_delta) = if a_coord < b_coord {
                        (a_coord, a_delta, b_coord, b_delta)
                    } else {
                        (b_coord, b_delta, a_coord, a_delta)
                    };

                    if p_coord <= lo_coord {
                        lo_delta
                    } else if p_coord >= hi_coord {
                        hi_delta
                    } else {
                        // Linear interpolation.
                        let t_num = (p_coord - lo_coord) as i64;
                        let t_den = (hi_coord - lo_coord) as i64;
                        (lo_delta as i64 + (hi_delta as i64 - lo_delta as i64) * t_num / t_den)
                            as i32
                    }
                };

                set_delta(j, interp, delta_x, delta_y);

                let next = if j == end { start } else { j + 1 };
                if next == next_touched {
                    break;
                }
                j = next;
            }
        }

        // Continue from next_touched.
        if i == first_touched {
            break;
        }
    }
}

/// Apply gvar deltas with IUP to a simple glyph outline.
///
/// Accumulates explicit deltas from all active tuples, then runs IUP
/// for each contour to fill in unreferenced points.
fn apply_gvar_simple<'a>(
    outline: &mut GlyphOutline,
    orig_points: &[GlyphPoint],
    var_data: &read_fonts::tables::gvar::GlyphVariationData<'a>,
    coords: &'a [read_fonts::types::F2Dot14],
    advance_fu: u16,
    lsb_fu: i16,
) -> (u16, i16) {
    let num_points = outline.num_points as usize;
    let total_points = num_points + 4;
    let mut delta_x = alloc::vec![0i32; total_points];
    let mut delta_y = alloc::vec![0i32; total_points];
    let mut touched = alloc::vec![false; total_points];

    for (tuple, scalar) in var_data.active_tuples_at(coords) {
        let scalar_bits = scalar.to_bits() as i64;
        // Reset touched flags per-tuple, then IUP, then accumulate.
        let mut tuple_dx = alloc::vec![0i32; total_points];
        let mut tuple_dy = alloc::vec![0i32; total_points];
        let mut tuple_touched = alloc::vec![false; total_points];

        for td in tuple.deltas() {
            let ix = td.position as usize;
            if ix < total_points {
                let sx = ((td.x_delta as i64 * scalar_bits + 0x8000) >> 16) as i32;
                let sy = ((td.y_delta as i64 * scalar_bits + 0x8000) >> 16) as i32;
                tuple_dx[ix] = sx;
                tuple_dy[ix] = sy;
                tuple_touched[ix] = true;
            }
        }

        // IUP: interpolate untouched points per contour.
        let nc = outline.num_contours as usize;
        let mut contour_start = 0usize;
        for c in 0..nc {
            let contour_end = outline.contour_ends[c] as usize;
            if contour_end >= contour_start {
                iup_contour(
                    orig_points,
                    &mut tuple_dx,
                    &mut tuple_dy,
                    &tuple_touched,
                    contour_start,
                    contour_end,
                );
            }
            contour_start = contour_end + 1;
        }

        // Accumulate into final deltas.
        for i in 0..total_points {
            delta_x[i] += tuple_dx[i];
            delta_y[i] += tuple_dy[i];
            if tuple_touched[i] {
                touched[i] = true;
            }
        }
    }

    // Apply deltas to outline points.
    for i in 0..num_points {
        outline.points[i].x += delta_x[i];
        outline.points[i].y += delta_y[i];
    }

    // Recompute bounding box.
    if num_points > 0 {
        let mut x_min = outline.points[0].x;
        let mut y_min = outline.points[0].y;
        let mut x_max = outline.points[0].x;
        let mut y_max = outline.points[0].y;
        for i in 1..num_points {
            let p = &outline.points[i];
            if p.x < x_min {
                x_min = p.x;
            }
            if p.x > x_max {
                x_max = p.x;
            }
            if p.y < y_min {
                y_min = p.y;
            }
            if p.y > y_max {
                y_max = p.y;
            }
        }
        outline.x_min = x_min as i16;
        outline.y_min = y_min as i16;
        outline.x_max = x_max as i16;
        outline.y_max = y_max as i16;
    }

    let new_advance = advance_fu as i32 + delta_x[num_points + 1] - delta_x[num_points];
    let new_lsb = lsb_fu as i32 + delta_x[num_points];

    (new_advance.max(0) as u16, new_lsb as i16)
}

/// Extract glyph outline from a variable font at specific axis values.
///
/// Handles both simple and composite glyphs correctly:
/// - Simple glyphs: applies gvar deltas with IUP interpolation.
/// - Composite glyphs: applies gvar component offset deltas, then
///   recursively extracts each component with its own variation.
///
/// Returns `(advance_width_fu, lsb_fu, upem)` on success.
fn extract_outline_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    axis_values: &[AxisValue],
    outline: &mut GlyphOutline,
) -> Option<(u16, i16, u16)> {
    if axis_values.is_empty() {
        return extract_outline(font_data, glyph_id, outline);
    }

    let coords = build_normalized_coords(font_data, axis_values);
    if coords.is_empty() || coords.iter().all(|c| c.to_f32().abs() < f32::EPSILON) {
        return extract_outline(font_data, glyph_id, outline);
    }

    let font = FontRef::new(font_data).ok()?;
    let head = font.head().ok()?;
    let upem = head.units_per_em();
    let hmtx = font.hmtx().ok()?;
    let hhea = font.hhea().ok()?;
    let num_h_metrics = hhea.number_of_h_metrics();
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;
    let gid = read_fonts::types::GlyphId::new(glyph_id as u32);

    let (advance_fu, lsb_fu) = if (glyph_id as u16) < num_h_metrics {
        let m = hmtx.h_metrics().get(glyph_id as usize)?;
        (m.advance.get(), m.side_bearing.get())
    } else {
        let last = hmtx.h_metrics().get(num_h_metrics as usize - 1)?;
        let lsb_data = hmtx.left_side_bearings();
        let lsb_idx = (glyph_id as usize).checked_sub(num_h_metrics as usize)?;
        let lsb = lsb_data.get(lsb_idx).map(|v| v.get()).unwrap_or(0);
        (last.advance.get(), lsb)
    };

    let glyph_data = loca.get_glyf(gid, &glyf).ok()??;

    match glyph_data {
        read_fonts::tables::glyf::Glyph::Simple(ref _simple) => {
            // Extract the default outline.
            let (_, _, _) = extract_outline(font_data, glyph_id, outline)?;

            // Save original points for IUP reference.
            let num_points = outline.num_points as usize;
            let mut orig_points = alloc::vec![GlyphPoint { x: 0, y: 0, on_curve: false }; num_points];
            for i in 0..num_points {
                orig_points[i] = outline.points[i];
            }

            let gvar = match font.gvar() {
                Ok(g) => g,
                Err(_) => return Some((advance_fu, lsb_fu, upem)),
            };
            let var_data = match gvar.glyph_variation_data(gid) {
                Ok(Some(vd)) => vd,
                _ => return Some((advance_fu, lsb_fu, upem)),
            };

            let (new_advance, new_lsb) =
                apply_gvar_simple(outline, &orig_points, &var_data, &coords, advance_fu, lsb_fu);

            Some((new_advance, new_lsb, upem))
        }
        read_fonts::tables::glyf::Glyph::Composite(ref composite) => {
            // For composite glyphs, gvar stores deltas for:
            //   [component_0_offset, component_1_offset, ..., phantom0..3]
            // NOT for individual outline points.
            let components: alloc::vec::Vec<_> = composite.components().collect();
            let num_components = components.len();

            // Get gvar deltas for component offsets + phantom points.
            let gvar = match font.gvar() {
                Ok(g) => g,
                Err(_) => {
                    return extract_outline(font_data, glyph_id, outline)
                        .map(|(_, _, u)| (advance_fu, lsb_fu, u));
                }
            };
            let gvar_total = num_components + 4;
            let mut comp_dx = alloc::vec![0i32; gvar_total];
            let mut comp_dy = alloc::vec![0i32; gvar_total];

            if let Ok(Some(var_data)) = gvar.glyph_variation_data(gid) {
                for (tuple, scalar) in var_data.active_tuples_at(&coords) {
                    let scalar_bits = scalar.to_bits() as i64;
                    for td in tuple.deltas() {
                        let ix = td.position as usize;
                        if ix < gvar_total {
                            comp_dx[ix] +=
                                ((td.x_delta as i64 * scalar_bits + 0x8000) >> 16) as i32;
                            comp_dy[ix] +=
                                ((td.y_delta as i64 * scalar_bits + 0x8000) >> 16) as i32;
                        }
                    }
                }
            }

            // Extract each component with its own gvar variation, applying
            // the adjusted component offsets.
            outline.num_points = 0;
            outline.num_contours = 0;
            outline.x_min = i16::MAX;
            outline.y_min = i16::MAX;
            outline.x_max = i16::MIN;
            outline.y_max = i16::MIN;

            for (ci, component) in components.iter().enumerate() {
                let comp_gid = component.glyph.to_u32() as u16;
                let (base_dx, base_dy) = match component.anchor {
                    read_fonts::tables::glyf::Anchor::Offset { x, y } => (x as i32, y as i32),
                    _ => (0, 0),
                };
                let adj_dx = base_dx + comp_dx[ci];
                let adj_dy = base_dy + comp_dy[ci];

                let pts_before = outline.num_points as usize;
                let contours_before = outline.num_contours as usize;

                // Recursively extract component (typically simple) with variation.
                // Use a heap-allocated temporary outline to avoid stack overflow.
                let mut comp_outline: alloc::boxed::Box<GlyphOutline> = unsafe {
                    let layout = alloc::alloc::Layout::new::<GlyphOutline>();
                    let ptr = alloc::alloc::alloc_zeroed(layout) as *mut GlyphOutline;
                    if ptr.is_null() {
                        continue;
                    }
                    alloc::boxed::Box::from_raw(ptr)
                };

                let comp_result = extract_outline_with_axes(
                    font_data, comp_gid, axis_values, &mut comp_outline,
                );
                if comp_result.is_none() {
                    // Fall back to default outline for this component.
                    if extract_outline(font_data, comp_gid, &mut comp_outline).is_none() {
                        continue;
                    }
                }

                // Append component points with adjusted offset.
                let comp_npts = comp_outline.num_points as usize;
                let comp_nc = comp_outline.num_contours as usize;
                if pts_before + comp_npts > MAX_GLYPH_POINTS {
                    continue;
                }
                if contours_before + comp_nc > MAX_CONTOURS {
                    continue;
                }

                for i in 0..comp_npts {
                    outline.points[pts_before + i] = GlyphPoint {
                        x: comp_outline.points[i].x + adj_dx,
                        y: comp_outline.points[i].y + adj_dy,
                        on_curve: comp_outline.points[i].on_curve,
                    };
                }
                outline.num_points = (pts_before + comp_npts) as u16;

                for i in 0..comp_nc {
                    outline.contour_ends[contours_before + i] =
                        comp_outline.contour_ends[i] + pts_before as u16;
                }
                outline.num_contours = (contours_before + comp_nc) as u16;
            }

            // Recompute bounding box.
            let num_points = outline.num_points as usize;
            if num_points > 0 {
                let mut x_min = outline.points[0].x;
                let mut y_min = outline.points[0].y;
                let mut x_max = outline.points[0].x;
                let mut y_max = outline.points[0].y;
                for i in 1..num_points {
                    let p = &outline.points[i];
                    if p.x < x_min { x_min = p.x; }
                    if p.x > x_max { x_max = p.x; }
                    if p.y < y_min { y_min = p.y; }
                    if p.y > y_max { y_max = p.y; }
                }
                outline.x_min = x_min as i16;
                outline.y_min = y_min as i16;
                outline.x_max = x_max as i16;
                outline.y_max = y_max as i16;
            } else {
                return None;
            }

            // Advance from phantom point deltas.
            let new_advance =
                advance_fu as i32 + comp_dx[num_components + 1] - comp_dx[num_components];
            let new_lsb = lsb_fu as i32 + comp_dx[num_components];

            Some((new_advance.max(0) as u16, new_lsb as i16, upem))
        }
    }
}

/// Rasterize a glyph from a variable font at specific axis positions.
///
/// Like `rasterize()`, but applies variation (gvar) deltas for the given
/// axis values before rasterization. Axis values are clamped to the font's
/// declared range. Non-variable fonts or fonts without gvar data fall back
/// to the default outline.
///
/// `axis_values` is a slice of `AxisValue` structs specifying design-space
/// axis values (e.g., `AxisValue { tag: *b"wght", value: 700.0 }`).
pub fn rasterize_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    size_px: u16,
    buffer: &mut RasterBuffer,
    scratch: &mut RasterScratch,
    axis_values: &[AxisValue],
) -> Option<GlyphMetrics> {
    if axis_values.is_empty() {
        return rasterize(font_data, glyph_id, size_px, buffer, scratch);
    }

    let size_px_u32 = size_px as u32;

    let (advance_fu, lsb_fu, upem) =
        match extract_outline_with_axes(font_data, glyph_id, axis_values, &mut scratch.outline) {
            Some(v) => v,
            None => {
                // Try to get metrics even if outline is empty (space-like glyphs).
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

                let loca = font.loca(None).ok()?;
                let glyf = font.glyf().ok()?;
                let gid = read_fonts::types::GlyphId::new(glyph_id as u32);
                match loca.get_glyf(gid, &glyf) {
                    Ok(None) => {
                        let advance = scale_fu(advance_fu as i32, size_px_u32, upem) as u32;
                        return Some(GlyphMetrics {
                            width: 0,
                            height: 0,
                            bearing_x: 0,
                            bearing_y: 0,
                            advance,
                        });
                    }
                    Ok(Some(_)) => return None,
                    Err(_) => return None,
                }
            }
        };

    // The rest is identical to rasterize() — use the outline from scratch.
    let x_min_fu = scratch.outline.x_min;
    let y_min_fu = scratch.outline.y_min;
    let x_max_fu = scratch.outline.x_max;
    let y_max_fu = scratch.outline.y_max;

    // Scale bounding box to pixels, expand by 1px on each side for AA.
    let x_min_px = scale_fu_floor(x_min_fu as i32, size_px_u32, upem) - 1;
    let y_min_px = scale_fu_floor(y_min_fu as i32, size_px_u32, upem);
    let x_max_px = scale_fu_ceil(x_max_fu as i32, size_px_u32, upem) + 1;
    let y_max_px = scale_fu_ceil(y_max_fu as i32, size_px_u32, upem) + 1;
    let _ = y_min_px;
    let bmp_w = (x_max_px - x_min_px) as u32;
    let bmp_h = (y_max_px - y_min_px) as u32;

    if bmp_w == 0 || bmp_h == 0 {
        let advance = scale_fu(advance_fu as i32, size_px_u32, upem) as u32;
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

    // Grayscale anti-aliasing: rasterize at 1× width with vertical oversampling.
    // Output is 1 byte per pixel (grayscale coverage).
    let out_total = (bmp_w * bmp_h) as usize;

    if out_total > buffer.data.len() {
        return None;
    }

    for b in buffer.data[..out_total].iter_mut() {
        *b = 0;
    }

    scratch.num_segments = 0;
    flatten_outline_from_scratch(scratch, size_px_u32, upem, x_min_px, y_max_px);

    // Rasterize at native width (no horizontal oversampling)
    rasterize_segments(scratch, &mut buffer.data[..out_total], bmp_w, bmp_h);

    // Stem darkening (applied per grayscale byte).
    for i in 0..out_total {
        buffer.data[i] = STEM_DARKENING_LUT[buffer.data[i] as usize];
    }

    let advance = scale_fu(advance_fu as i32, size_px_u32, upem) as u32;
    // bearing_x = x_min_px: the bitmap starts at the leftmost pixel of the
    // gvar-adjusted outline. This is correct for both default and varied
    // instances (lsb_fu only reflects the pre-variation hmtx value).
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

// ---------------------------------------------------------------------------
// Automatic optical sizing
// ---------------------------------------------------------------------------

/// Compute the optical size value from a rendered pixel size and display DPI.
///
/// Uses the traditional typographic formula: `opsz = font_size_px × 72 / dpi`,
/// converting the rendered pixel size to an equivalent point size. This maps
/// display pixels to the font's optical size axis, so small text gets the
/// small-optical-size cut (wider, sturdier letterforms) and large text gets
/// the display cut.
///
/// The result is NOT clamped to any font's opsz axis range — the caller
/// should clamp to the font's declared min/max.
pub fn compute_optical_size(font_size_px: u16, dpi: u16) -> f32 {
    if dpi == 0 {
        return font_size_px as f32;
    }
    font_size_px as f32 * 72.0 / dpi as f32
}

/// Compute automatic optical size axis values for a font.
///
/// If the font has an `opsz` variation axis, returns an `AxisValue` array
/// with the optical size set to the computed value (clamped to the font's
/// declared opsz range). If the font has no `opsz` axis (e.g., Source Code
/// Pro variable), returns an empty Vec — a no-op for the rendering pipeline.
///
/// This function is the main entry point for automatic optical sizing.
/// Callers pass the result directly to `rasterize_with_axes` or
/// `shape_with_variations` — no explicit opsz parameter needed.
pub fn auto_axis_values_for_opsz(
    font_data: &[u8],
    font_size_px: u16,
    dpi: u16,
) -> alloc::vec::Vec<AxisValue> {
    let axes = font_axes(font_data);
    let opsz_axis = match axes.iter().find(|a| &a.tag == b"opsz") {
        Some(a) => a,
        None => return alloc::vec::Vec::new(), // No opsz axis — no-op.
    };

    let raw_opsz = compute_optical_size(font_size_px, dpi);

    // Clamp to the font's declared opsz range.
    let clamped = if raw_opsz < opsz_axis.min_value {
        opsz_axis.min_value
    } else if raw_opsz > opsz_axis.max_value {
        opsz_axis.max_value
    } else {
        raw_opsz
    };

    alloc::vec![AxisValue {
        tag: *b"opsz",
        value: clamped,
    }]
}

/// Compute a deterministic hash of axis values for use as a glyph cache key component.
///
/// The hash is computed from the axis tags and values. An empty axis values
/// slice produces hash 0.
pub fn axis_values_hash(axis_values: &[AxisValue]) -> u32 {
    if axis_values.is_empty() {
        return 0;
    }
    // Simple FNV-1a-like hash.
    let mut h: u32 = 0x811c_9dc5;
    for av in axis_values {
        for &b in &av.tag {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        let bits = av.value.to_bits();
        for shift in [0, 8, 16, 24] {
            h ^= (bits >> shift) as u32 & 0xFF;
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

// ---------------------------------------------------------------------------
// Dark mode weight correction
// ---------------------------------------------------------------------------

/// sRGB-to-linear lookup table (256 entries, 0.0–1.0 range as u16 fixed-point).
///
/// Pre-computed at build time for accuracy and speed. Each entry maps an sRGB
/// byte value (0–255) to its linear-light equivalent scaled to 0–65535 (u16).
/// This avoids needing a pow() function at runtime in no_std.
///
/// The sRGB transfer function is:
/// - For values ≤ 0.04045: linear = sRGB / 12.92
/// - For values > 0.04045: linear = ((sRGB + 0.055) / 1.055)^2.4
///
/// We approximate the 2.4 exponent using integer arithmetic at build time.
const SRGB_TO_LINEAR_LUT: [u16; 256] = {
    let mut lut = [0u16; 256];
    let mut i = 0u32;
    while i < 256 {
        let s = i as f64 / 255.0;
        let linear = if s <= 0.04045 {
            s / 12.92
        } else {
            // (s + 0.055) / 1.055 raised to 2.4.
            // In const context we can use f64 operations.
            let base = (s + 0.055) / 1.055;
            // x^2.4 = exp(2.4 * ln(x)) — use the manual approach.
            // Since this is const-eval at compile time, we use a Taylor series.
            // More practically: x^2.4 = x^2 * x^0.4
            // x^0.4 = x^(2/5) = (x^2)^(1/5) = fifth_root(x^2)
            // Fifth root via iterative Newton refinement at compile time.
            let base_sq = base * base;
            // Newton: solve t^5 = base_sq → t = ((4*t + base_sq / t^4) / 5)
            let mut t = base; // initial guess for fifth_root(base_sq)
            let mut iter = 0;
            while iter < 50 {
                let t2 = t * t;
                let t4 = t2 * t2;
                if t4 < 1e-15 {
                    break;
                }
                let t_new = (4.0 * t + base_sq / t4) / 5.0;
                let diff = t_new - t;
                if diff < 1e-12 && diff > -1e-12 {
                    break;
                }
                t = t_new;
                iter += 1;
            }
            base_sq * t // base^2 * base^0.4 = base^2.4
        };
        // Scale to u16 range (0–65535).
        let scaled = (linear * 65535.0 + 0.5) as u32;
        lut[i as usize] = if scaled > 65535 { 65535 } else { scaled as u16 };
        i += 1;
    }
    lut
};

/// Convert an sRGB component (0–255) to linear light (0.0–1.0).
///
/// Uses a pre-computed lookup table for accuracy and no_std compatibility.
fn srgb_to_linear(value: u8) -> f32 {
    SRGB_TO_LINEAR_LUT[value as usize] as f32 / 65535.0
}

/// Compute relative luminance of an sRGB color per WCAG 2.0.
///
/// Returns a value between 0.0 (black) and 1.0 (white).
/// Formula: L = 0.2126 * R_lin + 0.7152 * G_lin + 0.0722 * B_lin
fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);
    0.2126 * rl + 0.7152 * gl + 0.0722 * bl
}

/// Compute the weight correction factor for dark mode text rendering.
///
/// When light text is rendered on a dark background, human vision causes
/// bright areas to perceptually spread into dark areas (irradiation). This
/// makes light-on-dark text appear heavier than the same weight rendered
/// dark-on-light. To compensate, we reduce the font weight proportionally
/// to the foreground/background luminance contrast.
///
/// # Behavior
///
/// - **Foreground lighter than background**: returns a factor < 1.0
///   (weight reduction). Higher contrast → smaller factor (more reduction).
/// - **Foreground darker than background**: returns exactly 1.0
///   (no reduction needed).
/// - **Same foreground and background**: returns exactly 1.0.
///
/// The correction is **continuous and proportional** — NOT a binary
/// light/dark switch. The factor is computed from the WCAG contrast ratio:
///
/// ```text
/// contrast = (L_lighter + 0.05) / (L_darker + 0.05)
/// reduction = (contrast - 1.0) / 20.0  // maps contrast 1–21 to 0.0–1.0
/// factor = 1.0 - clamp(reduction, 0.0, 0.15)  // max 15% weight reduction
/// ```
///
/// # Arguments
///
/// * `fg_r`, `fg_g`, `fg_b` — foreground color in sRGB (0–255)
/// * `bg_r`, `bg_g`, `bg_b` — background color in sRGB (0–255)
///
/// # Returns
///
/// A correction factor in the range [0.85, 1.0]. Multiply the font's
/// base weight by this factor to get the adjusted weight.
pub fn weight_correction_factor(fg_r: u8, fg_g: u8, fg_b: u8, bg_r: u8, bg_g: u8, bg_b: u8) -> f32 {
    let fg_lum = relative_luminance(fg_r, fg_g, fg_b);
    let bg_lum = relative_luminance(bg_r, bg_g, bg_b);

    // Only reduce weight when foreground is lighter than background.
    if fg_lum <= bg_lum {
        return 1.0;
    }

    // WCAG contrast ratio: (lighter + 0.05) / (darker + 0.05).
    // Range is [1.0, 21.0] where 21:1 is maximum (white on black).
    let contrast = (fg_lum + 0.05) / (bg_lum + 0.05);

    // Map contrast range [1, 21] to a weight reduction factor.
    // We use a continuous curve: factor = 1.0 - max_reduction * (contrast - 1) / 20.
    // Max reduction is 15% (factor = 0.85) at maximum contrast (21:1).
    // This ensures monotonic decrease across the full contrast range.
    let normalized = (contrast - 1.0) / 20.0; // 0.0 at contrast=1, 1.0 at contrast=21
    let clamped = if normalized < 0.0 {
        0.0
    } else if normalized > 1.0 {
        1.0
    } else {
        normalized
    };

    1.0 - 0.15 * clamped
}

/// Compute automatic weight correction axis values for dark mode rendering.
///
/// For a variable font with a `wght` axis, computes the corrected weight
/// value based on foreground/background luminance contrast. Returns an
/// `AxisValue` array with the adjusted weight, clamped to the font's
/// declared wght axis range.
///
/// For fonts **without** a `wght` axis (non-variable fonts, or variable
/// fonts that lack a weight axis), returns an empty Vec — a no-op for
/// the rendering pipeline. No error is produced.
///
/// # Arguments
///
/// * `font_data` — raw font file bytes
/// * `fg_r`, `fg_g`, `fg_b` — foreground color in sRGB (0–255)
/// * `bg_r`, `bg_g`, `bg_b` — background color in sRGB (0–255)
///
/// # Returns
///
/// A `Vec<AxisValue>` containing the corrected `wght` axis value, or
/// empty if the font has no `wght` axis or no correction is needed.
pub fn auto_weight_correction_axes(
    font_data: &[u8],
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
) -> alloc::vec::Vec<AxisValue> {
    let axes = font_axes(font_data);
    let wght_axis = match axes.iter().find(|a| &a.tag == b"wght") {
        Some(a) => a,
        None => return alloc::vec::Vec::new(), // No wght axis — no-op.
    };

    let factor = weight_correction_factor(fg_r, fg_g, fg_b, bg_r, bg_g, bg_b);

    // If factor is 1.0, no correction needed — return empty to avoid
    // unnecessary axis variation overhead.
    if (factor - 1.0).abs() < f32::EPSILON {
        return alloc::vec::Vec::new();
    }

    // Apply correction to the font's default weight.
    let adjusted = wght_axis.default_value * factor;

    // Clamp to the font's declared wght axis range.
    let clamped = if adjusted < wght_axis.min_value {
        wght_axis.min_value
    } else if adjusted > wght_axis.max_value {
        wght_axis.max_value
    } else {
        adjusted
    };

    alloc::vec![AxisValue {
        tag: *b"wght",
        value: clamped,
    }]
}

/// Get the axis-adjusted horizontal advance for a glyph.
///
/// Applies gvar deltas for the given axis values and returns the advance
/// width in pixels. Useful for computing char_width without rasterizing.
/// Returns None if the glyph ID is invalid or the font cannot be parsed.
pub fn glyph_advance_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    size_px: u16,
    axis_values: &[AxisValue],
) -> Option<u32> {
    let font = FontRef::new(font_data).ok()?;
    let head = font.head().ok()?;
    let upem = head.units_per_em();
    let hmtx = font.hmtx().ok()?;
    let hhea = font.hhea().ok()?;
    let num_h_metrics = hhea.number_of_h_metrics();

    let base_advance = if (glyph_id as u16) < num_h_metrics {
        hmtx.h_metrics().get(glyph_id as usize)?.advance.get()
    } else {
        hmtx.h_metrics()
            .get(num_h_metrics as usize - 1)?
            .advance
            .get()
    };

    if axis_values.is_empty() {
        return Some(scale_fu(base_advance as i32, size_px as u32, upem) as u32);
    }

    // Apply gvar phantom point deltas to get axis-adjusted advance.
    // Heap-allocate outline (~6 KiB) to avoid stack overflow on 16 KiB stacks.
    let mut outline: alloc::boxed::Box<GlyphOutline> = unsafe {
        let layout = alloc::alloc::Layout::new::<GlyphOutline>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut GlyphOutline;
        if ptr.is_null() {
            return Some(scale_fu(base_advance as i32, size_px as u32, upem) as u32);
        }
        // SAFETY: alloc_zeroed returns valid, zero-initialized memory.
        // GlyphOutline::zeroed() is all-zeros, matching the allocation.
        alloc::boxed::Box::from_raw(ptr)
    };
    match extract_outline_with_axes(font_data, glyph_id, axis_values, &mut outline) {
        Some((adj_advance, _, adj_upem)) => {
            Some(scale_fu(adj_advance as i32, size_px as u32, adj_upem) as u32)
        }
        None => {
            // No outline (space-like glyph) — apply advance delta from phantom points.
            let coords = build_normalized_coords(font_data, axis_values);
            if coords.is_empty() || coords.iter().all(|c| c.to_f32().abs() < f32::EPSILON) {
                return Some(scale_fu(base_advance as i32, size_px as u32, upem) as u32);
            }
            // Try to get gvar deltas for phantom points even without an outline.
            let gvar = font.gvar().ok()?;
            let gid = read_fonts::types::GlyphId::new(glyph_id as u32);
            let var_data = gvar.glyph_variation_data(gid).ok()??;
            let loca = font.loca(None).ok()?;
            let glyf = font.glyf().ok()?;
            // Count points in the glyph (0 for space).
            let num_pts = match loca.get_glyf(gid, &glyf) {
                Ok(Some(read_fonts::tables::glyf::Glyph::Simple(s))) => s.num_points(),
                _ => 0,
            };
            let mut dx_origin = 0i32;
            let mut dx_advance = 0i32;
            for (tuple, scalar) in var_data.active_tuples_at(&coords) {
                let scalar_bits = scalar.to_bits() as i64;
                for td in tuple.deltas() {
                    let ix = td.position as usize;
                    if ix == num_pts {
                        dx_origin += ((td.x_delta as i64 * scalar_bits + 0x8000) >> 16) as i32;
                    } else if ix == num_pts + 1 {
                        dx_advance += ((td.x_delta as i64 * scalar_bits + 0x8000) >> 16) as i32;
                    }
                }
            }
            let adj = base_advance as i32 + dx_advance - dx_origin;
            Some(scale_fu(adj.max(0), size_px as u32, upem) as u32)
        }
    }
}
