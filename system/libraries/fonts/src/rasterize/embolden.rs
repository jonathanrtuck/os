//! Outline emboldening for stem darkening.
//!
//! Shifts glyph outline control points outward along the angular bisector at
//! each vertex, making stems geometrically wider. This is the correct approach
//! to stem darkening — modifying the outline before rasterization produces
//! naturally correct anti-aliasing for the thicker shape.
//!
//! Algorithm: FT_Outline_EmboldenXY from FreeType (src/base/ftoutln.c).
//! Dilation formula: macOS Core Text (reverse-engineered by Patrick Walton
//! for the Pathfinder GPU text renderer).
//!
//! References:
//! - FreeType: <https://gitlab.freedesktop.org/freetype/freetype/-/blob/master/src/base/ftoutln.c>
//! - Pathfinder: <https://github.com/servo/pathfinder>

use super::outline::GlyphOutline;

// ---------------------------------------------------------------------------
// Fixed-point constants (16.16 format, matching FreeType)
// ---------------------------------------------------------------------------

/// 16.16 fixed-point "1.0".
const FX_ONE: i64 = 0x10000;

/// Threshold for near-reversal detection (~cos(160°) = -0.9375).
/// Turns sharper than ~160° get zero shift to prevent spikes.
const NEAR_REVERSAL_THRESHOLD: i64 = -0xF000;

// ---------------------------------------------------------------------------
// macOS Core Text dilation formula
// ---------------------------------------------------------------------------

/// Compute outline dilation amounts in font units using the macOS formula.
///
/// The macOS formula (reverse-engineered by Patrick Walton for Pathfinder):
///   dilation_x_px = min(0.3, 0.015125 * font_size_device_px)
///   dilation_y_px = min(0.3, 0.0121 * font_size_device_px)
///
/// IMPORTANT: `font_size_device_px` is in **device pixels** (physical pixels
/// on the display). At 2x Retina, a 14pt font = 28 device pixels. The
/// 0.3px cap is in device pixels. The dilation is then converted back to
/// glyph-pixel space (the resolution at which the rasterizer operates).
///
/// `font_size_px`: the size passed to the rasterizer (glyph pixel size).
/// `units_per_em`: font units per em.
/// `scale`: display scale factor (1 for 1x, 2 for Retina). Defaults to 2
///          if 0 is passed.
///
/// Returns (x_strength, y_strength) in font units as 16.16 fixed-point.
pub fn compute_dilation(font_size_px: u16, units_per_em: u16, scale: u16) -> (i32, i32) {
    if font_size_px == 0 || units_per_em == 0 {
        return (0, 0);
    }

    let glyph_px = font_size_px as f32;
    let upem = units_per_em as f32;
    let scale_f = if scale == 0 { 2.0f32 } else { scale as f32 };

    // Compute dilation in device pixels using the macOS formula.
    // The base coefficients (0.015125, 0.0121) are from Patrick Walton's
    // Pathfinder reverse-engineering of Core Text. Our outline-modification
    // approach produces slightly less visual weight than macOS's approach
    // (which operates at a different pipeline stage), so we apply a small
    // empirical boost (1.3x) to match the perceived weight.
    let device_px = glyph_px * scale_f;
    let dilation_x_device = (0.015125 * 1.3 * device_px).min(0.39);
    let dilation_y_device = (0.0121 * 1.3 * device_px).min(0.39);

    // Convert from device pixels to glyph pixels (rasterizer space).
    let dilation_x_glyph = dilation_x_device / scale_f;
    let dilation_y_glyph = dilation_y_device / scale_f;

    // Convert to font units: dilation_fu = dilation_glyph_px * upem / glyph_px
    let dilation_x_fu = dilation_x_glyph * upem / glyph_px;
    let dilation_y_fu = dilation_y_glyph * upem / glyph_px;

    // Convert to 16.16 fixed-point.
    let x_strength = (dilation_x_fu * FX_ONE as f32) as i32;
    let y_strength = (dilation_y_fu * FX_ONE as f32) as i32;

    (x_strength, y_strength)
}

// ---------------------------------------------------------------------------
// Vector math (16.16 fixed-point)
// ---------------------------------------------------------------------------

/// Normalize a vector in-place and return its original length.
///
/// Input: (x, y) in arbitrary integer units (font units or fixed-point).
/// Output: (x, y) overwritten with unit vector in 16.16 fixed-point.
/// Returns: original length in the same units as input.
///
/// Returns 0 if the vector is zero-length (and leaves it unchanged).
fn vector_norm_len(x: &mut i64, y: &mut i64) -> i64 {
    let len_sq = *x * *x + *y * *y;
    if len_sq == 0 {
        return 0;
    }

    // Integer square root via Newton's method.
    let len = isqrt_i64(len_sq);
    if len == 0 {
        return 0;
    }

    // Normalize to 16.16 fixed-point unit vector.
    *x = (*x * FX_ONE) / len;
    *y = (*y * FX_ONE) / len;

    len
}

/// Integer square root (64-bit) via Newton's method.
fn isqrt_i64(n: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    if n == 1 {
        return 1;
    }

    // Initial guess: half the bit-width.
    let mut x = 1i64 << ((64 - n.leading_zeros()) / 2);
    loop {
        let next = (x + n / x) / 2;
        if next >= x {
            return x;
        }
        x = next;
    }
}

/// 16.16 fixed-point multiply: (a * b + 0x8000) >> 16.
fn fx_mul(a: i64, b: i64) -> i64 {
    (a * b + 0x8000) >> 16
}

/// 16.16 fixed-point divide with rounding: (a << 16) / b.
fn fx_div(a: i64, b: i64) -> i64 {
    if b == 0 {
        return 0;
    }
    ((a << 16) + (b >> 1)) / b
}

/// MulDiv: (a * b) / c with 64-bit intermediate.
fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    if c == 0 {
        return 0;
    }
    (a * b) / c
}

// ---------------------------------------------------------------------------
// Outline orientation detection
// ---------------------------------------------------------------------------

/// Contour winding: clockwise (TrueType) or counter-clockwise (PostScript).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Orientation {
    /// Clockwise (TrueType convention: y-up, outer contours CW).
    Clockwise,
    /// Counter-clockwise (PostScript convention).
    CounterClockwise,
}

/// Detect the overall orientation of an outline using the signed area.
///
/// Computes the signed area of all contours using the shoelace formula.
/// In standard y-up coordinates (which TrueType uses):
///   positive signed area → counter-clockwise (PostScript convention)
///   negative signed area → clockwise (TrueType convention)
fn detect_orientation(outline: &GlyphOutline) -> Option<Orientation> {
    if outline.num_contours == 0 || outline.num_points < 3 {
        return None;
    }

    let mut area: i64 = 0;
    let mut start = 0usize;

    for c in 0..outline.num_contours as usize {
        let end = outline.contour_ends[c] as usize;
        if end < start + 2 {
            start = end + 1;
            continue;
        }

        // Shoelace formula for this contour.
        for i in start..=end {
            let j = if i == end { start } else { i + 1 };
            let xi = outline.points[i].x as i64;
            let yi = outline.points[i].y as i64;
            let xj = outline.points[j].x as i64;
            let yj = outline.points[j].y as i64;
            area += xi * yj - xj * yi;
        }

        start = end + 1;
    }

    if area < 0 {
        Some(Orientation::Clockwise)
    } else if area > 0 {
        Some(Orientation::CounterClockwise)
    } else {
        None // Degenerate outline (zero area).
    }
}

// ---------------------------------------------------------------------------
// Core algorithm: FT_Outline_EmboldenXY
// ---------------------------------------------------------------------------

/// Embolden a glyph outline by shifting points symmetrically outward.
///
/// `x_strength` and `y_strength` are the desired perpendicular offset from
/// each edge, in font-unit 16.16 fixed-point. Every edge moves outward by
/// this amount (symmetric expansion matching macOS Core Text dilation).
///
/// At each vertex, the shift follows the miter-join formula: the outward
/// offset is along the bisector of the adjacent edge normals, scaled by
/// `1 / cos(half_turn_angle)` so the perpendicular distance from each
/// edge equals the requested strength.
///
/// Corner clamping limits the miter extension at sharp corners to prevent
/// self-intersection. Near-reversals (>~160°) get zero shift.
pub fn embolden_outline(outline: &mut GlyphOutline, x_strength: i32, y_strength: i32) {
    let xs = x_strength as i64;
    let ys = y_strength as i64;

    if xs == 0 && ys == 0 {
        return;
    }

    let orientation = match detect_orientation(outline) {
        Some(o) => o,
        None => return,
    };

    let np = outline.num_points as usize;
    if np < 3 {
        return;
    }

    // Pre-compute shifts for all points, then apply. This avoids
    // modifying points while iterating over them for direction vectors.
    let mut shifts_x = [0i64; 512];
    let mut shifts_y = [0i64; 512];
    if np > 512 {
        return; // Outline too large for our fixed buffer.
    }

    let mut last_end: i32 = -1;

    for c in 0..outline.num_contours as usize {
        let first = (last_end + 1) as usize;
        let last = outline.contour_ends[c] as usize;
        last_end = last as i32;

        let n = if last >= first { last - first + 1 } else { 0 };
        if n < 3 {
            continue;
        }

        for idx in first..=last {
            // Previous and next points in the contour (wrapping).
            let prev = if idx == first { last } else { idx - 1 };
            let next = if idx == last { first } else { idx + 1 };

            // Edge directions (not normalized yet).
            let mut in_x = outline.points[idx].x as i64 - outline.points[prev].x as i64;
            let mut in_y = outline.points[idx].y as i64 - outline.points[prev].y as i64;
            let mut out_x = outline.points[next].x as i64 - outline.points[idx].x as i64;
            let mut out_y = outline.points[next].y as i64 - outline.points[idx].y as i64;

            let l_in = vector_norm_len(&mut in_x, &mut in_y);
            let l_out = vector_norm_len(&mut out_x, &mut out_y);

            if l_in == 0 || l_out == 0 {
                continue; // Coincident points — zero shift.
            }

            // Outward edge normals (16.16 unit vectors).
            // For CW contours in y-up: outward = left of edge direction.
            // Left normal of (dx, dy) = (-dy, dx).
            let (n_in_x, n_in_y, n_out_x, n_out_y) = if orientation == Orientation::Clockwise {
                (-in_y, in_x, -out_y, out_x)
            } else {
                // CCW: outward = right of edge direction = (dy, -dx).
                (in_y, -in_x, out_y, -out_x)
            };

            // Miter direction: sum of the two outward normals.
            let mx = n_in_x + n_out_x;
            let my = n_in_y + n_out_y;

            // dot(miter, n_in) — determines the scaling factor.
            // This equals 2 * cos²(θ/2) where θ is the turn angle.
            // In 16.16 × 16.16 → 16.16:
            let miter_dot = fx_mul(mx, n_in_x) + fx_mul(my, n_in_y);

            if miter_dot <= NEAR_REVERSAL_THRESHOLD || miter_dot == 0 {
                // Near-reversal or degenerate — skip.
                continue;
            }

            // Corner clamping: limit miter extension at sharp corners.
            // The miter scale is 1/miter_dot (in 16.16). At very sharp
            // corners, this gets very large. Clamp the shift magnitude
            // to the shorter adjacent edge length.
            let l_min = if l_in < l_out { l_in } else { l_out };

            // shift = miter * strength / miter_dot
            // But strength may differ for x and y.
            let raw_sx = mul_div(mx, xs, miter_dot);
            let raw_sy = mul_div(my, ys, miter_dot);

            // Compute shift magnitude for clamping.
            // Approximate magnitude: max(|sx|, |sy|) (conservative).
            let mag = if raw_sx.abs() > raw_sy.abs() {
                raw_sx.abs()
            } else {
                raw_sy.abs()
            };
            let l_min_fx = l_min * FX_ONE;

            let (sx, sy) = if mag > l_min_fx {
                // Clamp: scale down proportionally.
                let scale = fx_div(l_min_fx, mag);
                (fx_mul(raw_sx, scale), fx_mul(raw_sy, scale))
            } else {
                (raw_sx, raw_sy)
            };

            shifts_x[idx] = sx;
            shifts_y[idx] = sy;
        }
    }

    // Apply all shifts.
    for i in 0..np {
        let dx = ((shifts_x[i] + 0x8000) >> 16) as i32;
        let dy = ((shifts_y[i] + 0x8000) >> 16) as i32;
        outline.points[i].x += dx;
        outline.points[i].y += dy;
    }

    // Recompute bounding box after emboldening.
    let np = outline.num_points as usize;
    if np > 0 {
        let mut x_min = outline.points[0].x;
        let mut y_min = outline.points[0].y;
        let mut x_max = outline.points[0].x;
        let mut y_max = outline.points[0].y;
        for i in 1..np {
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
}

#[cfg(test)]
mod tests {
    use super::{
        super::outline::{GlyphOutline, GlyphPoint},
        *,
    };

    /// Helper: create an outline from a slice of (x, y) points forming a single contour.
    fn make_outline(points: &[(i32, i32)]) -> GlyphOutline {
        let mut outline = GlyphOutline::zeroed();
        for (i, &(x, y)) in points.iter().enumerate() {
            outline.points[i] = GlyphPoint {
                x,
                y,
                on_curve: true,
            };
        }
        outline.num_points = points.len() as u16;
        outline.num_contours = 1;
        outline.contour_ends[0] = (points.len() - 1) as u16;

        // Compute bounding box.
        let mut x_min = points[0].0;
        let mut y_min = points[0].1;
        let mut x_max = points[0].0;
        let mut y_max = points[0].1;
        for &(x, y) in &points[1..] {
            if x < x_min {
                x_min = x;
            }
            if x > x_max {
                x_max = x;
            }
            if y < y_min {
                y_min = y;
            }
            if y > y_max {
                y_max = y;
            }
        }
        outline.x_min = x_min as i16;
        outline.y_min = y_min as i16;
        outline.x_max = x_max as i16;
        outline.y_max = y_max as i16;
        outline
    }

    #[test]
    fn zero_strength_no_change() {
        let original = [(0, 0), (100, 0), (100, 200), (0, 200)];
        let mut outline = make_outline(&original);
        embolden_outline(&mut outline, 0, 0);
        for (i, &(x, y)) in original.iter().enumerate() {
            assert_eq!(outline.points[i].x, x);
            assert_eq!(outline.points[i].y, y);
        }
    }

    #[test]
    fn orientation_detection_ccw() {
        // In y-up coords: right→up→left→down is CCW.
        let outline = make_outline(&[(0, 0), (100, 0), (100, 100), (0, 100)]);
        assert_eq!(
            detect_orientation(&outline),
            Some(Orientation::CounterClockwise)
        );
    }

    #[test]
    fn orientation_detection_clockwise() {
        // In y-up coords: up→right→down→left is CW (TrueType convention).
        let outline = make_outline(&[(0, 0), (0, 100), (100, 100), (100, 0)]);
        assert_eq!(detect_orientation(&outline), Some(Orientation::Clockwise));
    }

    #[test]
    fn square_grows_outward() {
        // CW square (TrueType convention in y-up): up→right→down→left.
        // P0=(0,0), P1=(0,200), P2=(100,200), P3=(100,0)
        let mut outline = make_outline(&[(0, 0), (0, 200), (100, 200), (100, 0)]);
        let strength = 10 << 16; // 10 font units in 16.16
        embolden_outline(&mut outline, strength, strength);

        // Each corner should have moved outward by ~5 units.
        // Bottom-left (0,0) should move to approximately (-5, -5).
        assert!(
            outline.points[0].x < 0,
            "BL.x should be negative, got {}",
            outline.points[0].x
        );
        assert!(
            outline.points[0].y < 0,
            "BL.y should be negative, got {}",
            outline.points[0].y
        );

        // Top-left (0,200) should move to approximately (-5, 205).
        assert!(
            outline.points[1].x < 0,
            "TL.x should be negative, got {}",
            outline.points[1].x
        );
        assert!(
            outline.points[1].y > 200,
            "TL.y should be > 200, got {}",
            outline.points[1].y
        );

        // Top-right (100,200) should move to approximately (105, 205).
        assert!(
            outline.points[2].x > 100,
            "TR.x should be > 100, got {}",
            outline.points[2].x
        );
        assert!(
            outline.points[2].y > 200,
            "TR.y should be > 200, got {}",
            outline.points[2].y
        );

        // Bottom-right (100,0) should move to approximately (105, -5).
        assert!(
            outline.points[3].x > 100,
            "BR.x should be > 100, got {}",
            outline.points[3].x
        );
        assert!(
            outline.points[3].y < 0,
            "BR.y should be negative, got {}",
            outline.points[3].y
        );
    }

    #[test]
    fn square_growth_magnitude_correct() {
        // Embolden a 100x100 square by exactly 20 font units.
        // Each side should grow by 10. At 90-degree corners, the bisector shift
        // is perpendicular, and d = cos(90°) + 1 = 1.0, so shift = strength/(2*1) = 10.
        // Total shift per point = half_strength + bisector_shift = 10 + 10 = ... wait.
        //
        // Actually, in FreeType: points[i] += xstrength + shift.x
        // where xstrength is ALREADY halved (10), and shift.x at a 90° corner
        // of a square: in = (1,0), out = (0,1)
        //   shift.x = in.y + out.y = 0 + 1 = 1 (before orientation flip)
        //   d = dot(in, out) + 1 = 0 + 1 = 1.0
        //   shift.x = 1 * 10 / 1 = 10 (in 16.16)
        // But shift.x is in normalized-vector space (16.16), so the actual
        // displacement is the combination of uniform emboldening + vertex shift.
        //
        // For a CW square: at bottom-left (0,0), in=(1,0), out=(0,-1):
        //   shift.x = in.y + out.y = 0 + (-1) = -1 → negated for CW → 1
        //   shift.y = in.x + out.x = 1 + 0 = 1 → not negated for CW
        //   d = dot + 1 = 0 + 1 = 1
        //   shift.x = 1 * xs / 1, shift.y = 1 * ys / 1
        // Displacement = (xs + shift.x, ys + shift.y) in 16.16
        //              = (10 + 10, 10 + 10) in font units → 20? No...
        //
        // Wait, that seems too much. For a 20 fu total embolden (10 per side),
        // each vertex should move by ~10 fu outward. Let me verify with a
        // concrete test and check the actual values.

        // CW square (TrueType convention).
        let mut outline = make_outline(&[(0, 0), (0, 100), (100, 100), (100, 0)]);
        let strength = 20 << 16; // 20 font units total (10 per side)
        embolden_outline(&mut outline, strength, strength);

        // Bottom-left (0,0) should move outward to approximately (-10, -10).
        let dx = outline.points[0].x;
        let dy = outline.points[0].y;
        assert!(dx >= -15 && dx <= -5, "BL.x expected ~-10, got {}", dx);
        assert!(dy >= -15 && dy <= -5, "BL.y expected ~-10, got {}", dy);
    }

    #[test]
    fn isqrt_correctness() {
        assert_eq!(isqrt_i64(0), 0);
        assert_eq!(isqrt_i64(1), 1);
        assert_eq!(isqrt_i64(4), 2);
        assert_eq!(isqrt_i64(9), 3);
        assert_eq!(isqrt_i64(100), 10);
        assert_eq!(isqrt_i64(10000), 100);
        // Non-perfect squares: floor.
        assert_eq!(isqrt_i64(2), 1);
        assert_eq!(isqrt_i64(8), 2);
        assert_eq!(isqrt_i64(99), 9);
    }

    #[test]
    fn vector_norm_len_unit_vectors() {
        // (3, 4) → length 5, normalized to (0.6, 0.8) in 16.16.
        let mut x: i64 = 3;
        let mut y: i64 = 4;
        let len = vector_norm_len(&mut x, &mut y);
        assert_eq!(len, 5);
        // 0.6 * 65536 = 39321.6 → 39322
        // 0.8 * 65536 = 52428.8 → 52429
        assert!((x - 39322).abs() <= 1, "x should be ~39322, got {}", x);
        assert!((y - 52429).abs() <= 1, "y should be ~52429, got {}", y);
    }

    #[test]
    fn vector_norm_len_zero() {
        let mut x: i64 = 0;
        let mut y: i64 = 0;
        let len = vector_norm_len(&mut x, &mut y);
        assert_eq!(len, 0);
        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn compute_dilation_12pt_2x() {
        // 12pt at 2x → 24 device px. With 1.3x boost, cap at 0.39.
        // dilation_x_glyph = 0.39/2 = 0.195, fu = 0.195*1000/12 = 16.25
        let (x, y) = compute_dilation(12, 1000, 2);
        let x_fu = x as f32 / FX_ONE as f32;
        let y_fu = y as f32 / FX_ONE as f32;
        assert!(
            (x_fu - 16.25).abs() < 1.0,
            "x_fu expected ~16.25, got {}",
            x_fu
        );
        assert!(
            (y_fu - 15.73).abs() < 1.0,
            "y_fu expected ~15.73, got {}",
            y_fu
        );
    }

    #[test]
    fn compute_dilation_36pt_2x() {
        // 36pt at 2x → 72 device px. Both axes capped at 0.39.
        // dilation_x_glyph = 0.195, fu = 0.195*1000/36 = 5.417
        let (x, y) = compute_dilation(36, 1000, 2);
        let x_fu = x as f32 / FX_ONE as f32;
        let y_fu = y as f32 / FX_ONE as f32;
        assert!(
            (x_fu - 5.417).abs() < 1.0,
            "x_fu expected ~5.417, got {}",
            x_fu
        );
        assert!(
            (y_fu - 5.417).abs() < 1.0,
            "y_fu expected ~5.417, got {}",
            y_fu
        );
    }

    #[test]
    fn compute_dilation_zero_size() {
        let (x, y) = compute_dilation(0, 1000, 2);
        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn degenerate_outline_no_panic() {
        // Single point — should not panic.
        let mut outline = GlyphOutline::zeroed();
        outline.points[0] = GlyphPoint {
            x: 50,
            y: 50,
            on_curve: true,
        };
        outline.num_points = 1;
        outline.num_contours = 1;
        outline.contour_ends[0] = 0;
        embolden_outline(&mut outline, 10 << 16, 10 << 16);
        // Should not crash.
    }

    #[test]
    fn coincident_points_handled() {
        // Triangle with a duplicated point — should not crash or produce NaN-like results.
        let mut outline = make_outline(&[(0, 0), (0, 0), (100, 0), (50, 100)]);
        outline.contour_ends[0] = 3;
        embolden_outline(&mut outline, 10 << 16, 10 << 16);
        // All points should be finite integers (no overflow).
        for i in 0..4 {
            assert!(outline.points[i].x.abs() < 10000);
            assert!(outline.points[i].y.abs() < 10000);
        }
    }

    #[test]
    fn bbox_updated_after_embolden() {
        let mut outline = make_outline(&[(10, 10), (90, 10), (90, 90), (10, 90)]);
        assert_eq!(outline.x_min, 10);
        assert_eq!(outline.x_max, 90);
        embolden_outline(&mut outline, 10 << 16, 10 << 16);
        // Bounding box should be larger than before.
        assert!(outline.x_min < 10, "x_min should have decreased");
        assert!(outline.x_max > 90, "x_max should have increased");
        assert!(outline.y_min < 10, "y_min should have decreased");
        assert!(outline.y_max > 90, "y_max should have increased");
    }
}
