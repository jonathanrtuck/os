//! Variable font support — gvar deltas, IUP interpolation, axis normalization.
//!
//! Implements the OpenType `gvar` table processing pipeline:
//! axis normalization, tuple scalar computation, delta accumulation,
//! Interpolation of Untouched Points (IUP), and the `rasterize_with_axes`
//! public API for rendering variable font instances.

use read_fonts::{FontRef, TableProvider};

use super::metrics::{font_axes, AxisValue, GlyphMetrics, RasterBuffer};
use super::outline::{extract_outline, GlyphOutline, GlyphPoint, MAX_CONTOURS, MAX_GLYPH_POINTS};
use super::scale::{scale_fu, scale_fu_ceil, scale_fu_floor};
use super::scanline::{
    flatten_outline_from_scratch, rasterize_segments, RasterScratch, STEM_DARKENING_LUT,
};

// ---------------------------------------------------------------------------
// Axis normalization
// ---------------------------------------------------------------------------

/// Normalize a user-space axis value to the F2Dot14 range (-1.0 to ~+1.0)
/// using the font's axis min/default/max.
///
/// - value == default -> 0.0
/// - value < default -> (value - default) / (default - min)  (range [-1, 0])
/// - value > default -> (value - default) / (max - default)  (range [0, 1])
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
pub(crate) fn build_normalized_coords(
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

// ---------------------------------------------------------------------------
// IUP (Interpolation of Untouched Points)
// ---------------------------------------------------------------------------

/// Interpolation of Unreferenced Points (IUP) -- OpenType gvar spec.
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
        None => return, // No touched points -- deltas stay 0.
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
                return; // Wrapped around -- done.
            }
            i = next;
        }

        // i is the first untouched point. Find the run.
        let run_start = i;
        // prev_touched is the touched point before this run.
        let prev_touched_idx = if run_start == start {
            end
        } else {
            run_start - 1
        };
        // Find end of untouched run.
        while !touched[i] {
            let next = if i == end { start } else { i + 1 };
            if next == run_start {
                break; // Shouldn't happen (we know at least one is touched).
            }
            i = next;
        }
        // i is the next touched point after the run.
        let next_touched_idx = i;

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

            let a_coord = get_coord(prev_touched_idx);
            let b_coord = get_coord(next_touched_idx);
            let a_delta = get_delta(prev_touched_idx);
            let b_delta = get_delta(next_touched_idx);

            // Walk the untouched run.
            let mut j = run_start;
            loop {
                let p_coord = get_coord(j);

                let interp = if a_coord == b_coord {
                    // Both reference points same coord -- average deltas.
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
                        (lo_delta as i64
                            + (hi_delta as i64 - lo_delta as i64) * t_num / t_den)
                            as i32
                    }
                };

                set_delta(j, interp, delta_x, delta_y);

                let next = if j == end { start } else { j + 1 };
                if next == next_touched_idx {
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

// ---------------------------------------------------------------------------
// gvar delta application
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Variable outline extraction
// ---------------------------------------------------------------------------

/// Extract glyph outline from a variable font at specific axis values.
///
/// Handles both simple and composite glyphs correctly:
/// - Simple glyphs: applies gvar deltas with IUP interpolation.
/// - Composite glyphs: applies gvar component offset deltas, then
///   recursively extracts each component with its own variation.
///
/// Returns `(advance_width_fu, lsb_fu, upem)` on success.
pub(crate) fn extract_outline_with_axes(
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
            let mut orig_points =
                alloc::vec![GlyphPoint { x: 0, y: 0, on_curve: false }; num_points];
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
                    font_data,
                    comp_gid,
                    axis_values,
                    &mut comp_outline,
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

// ---------------------------------------------------------------------------
// Public API: rasterize with variable font axes
// ---------------------------------------------------------------------------

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
        return super::scanline::rasterize(font_data, glyph_id, size_px, buffer, scratch);
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

    // The rest is identical to rasterize() -- use the outline from scratch.
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

    // Grayscale anti-aliasing: rasterize at 1x width with vertical oversampling.
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
            // No outline (space-like glyph) -- apply advance delta from phantom points.
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
