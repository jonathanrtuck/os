//! Stroke expansion: convert stroked path commands into filled path commands.
//!
//! Given a sequence of path commands (MoveTo, LineTo, CubicTo, Close) and a
//! stroke width, produces a new sequence of filled path commands that form the
//! outline of the stroke. Uses round joins and round caps (matching Tabler
//! Icons' `stroke-linejoin="round" stroke-linecap="round"`).
//!
//! The algorithm: for each sub-path, walk the segments, generate offset curves
//! on both sides (left at +half_w, right at -half_w), connect them with round
//! joins at corners, and add round caps at endpoints.
//!
//! Round joins and caps are approximated with cubic Bézier arcs (4-point
//! circular arc approximation, kappa = 0.5522847498).

use alloc::vec::Vec;

use crate::primitives::{
    path_close, path_cubic_to, path_line_to, path_move_to, PATH_CLOSE, PATH_CLOSE_SIZE,
    PATH_CUBIC_TO, PATH_CUBIC_TO_SIZE, PATH_LINE_TO, PATH_LINE_TO_SIZE, PATH_MOVE_TO,
    PATH_MOVE_TO_SIZE,
};

/// Cubic Bézier approximation constant for quarter-circle arcs.
const KAPPA: f32 = 0.5522847498;

// ── Helpers ────────────────────────────────────────────────────────

const PI: f32 = core::f32::consts::PI;
const TWO_PI: f32 = 2.0 * PI;
const HALF_PI: f32 = core::f32::consts::FRAC_PI_2;

fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    // IEEE 754 bit-hack for initial estimate: halving the exponent ≈ sqrt.
    let mut r = f32::from_bits((x.to_bits() + 0x3f80_0000) >> 1);
    // 3 Newton-Raphson iterations: quadratic convergence from a good seed
    // gives ~24 bits of precision (full f32 mantissa).
    r = 0.5 * (r + x / r);
    r = 0.5 * (r + x / r);
    r = 0.5 * (r + x / r);
    r
}

fn floor_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;
    if x < f {
        f - 1.0
    } else {
        f
    }
}

/// Sine with Cody-Waite range reduction to [-π/4, π/4].
fn sin(x: f32) -> f32 {
    let x = x - TWO_PI * floor_f32(x / TWO_PI + 0.5);
    let (x, sign) = if x < 0.0 { (-x, -1.0) } else { (x, 1.0) };
    let quarter = PI * 0.25;
    let three_quarter = PI * 0.75;
    let r = if x <= quarter {
        sin_poly(x)
    } else if x <= three_quarter {
        cos_poly(HALF_PI - x)
    } else {
        sin_poly(PI - x)
    };
    sign * r
}

fn cos(x: f32) -> f32 {
    let x = x - TWO_PI * floor_f32(x / TWO_PI + 0.5);
    let x = if x < 0.0 { -x } else { x };
    let quarter = PI * 0.25;
    let three_quarter = PI * 0.75;
    if x <= quarter {
        cos_poly(x)
    } else if x <= three_quarter {
        -sin_poly(x - HALF_PI)
    } else {
        -cos_poly(PI - x)
    }
}

fn sin_poly(x: f32) -> f32 {
    let x2 = x * x;
    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)))
}

fn cos_poly(x: f32) -> f32 {
    let x2 = x * x;
    1.0 - x2 / 2.0 * (1.0 - x2 / 12.0 * (1.0 - x2 / 30.0))
}

/// Arctangent approximation (7th-order polynomial, max error ~0.0005 rad).
fn atan(x: f32) -> f32 {
    // For |x| > 1, use atan(x) = π/2 - atan(1/x).
    if x.abs() > 1.0 {
        let r = atan_inner(1.0 / x);
        if x > 0.0 {
            HALF_PI - r
        } else {
            -HALF_PI - r
        }
    } else {
        atan_inner(x)
    }
}

/// Minimax polynomial atan for |x| <= 1. Max error: ~3.3e-5 radians.
/// Coefficients from Abramowitz & Stegun / Cephes minimax fit (9th order).
fn atan_inner(x: f32) -> f32 {
    let x2 = x * x;
    x * (0.999_866_0
        + x2 * (-0.330_299_5 + x2 * (0.180_141_0 + x2 * (-0.085_133_0 + x2 * 0.020_835_1))))
}

fn atan2(y: f32, x: f32) -> f32 {
    if x > 0.0 {
        atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 {
            atan(y / x) + PI
        } else {
            atan(y / x) - PI
        }
    } else if y > 0.0 {
        HALF_PI
    } else if y < 0.0 {
        -HALF_PI
    } else {
        0.0
    }
}

/// A 2D point.
#[derive(Clone, Copy)]
struct Pt {
    x: f32,
    y: f32,
}

/// A segment from the parsed path (line or flattened cubic).
#[derive(Clone, Copy)]
struct Seg {
    p0: Pt,
    p1: Pt,
}

impl Seg {
    fn dx(&self) -> f32 {
        self.p1.x - self.p0.x
    }
    fn dy(&self) -> f32 {
        self.p1.y - self.p0.y
    }
    fn len(&self) -> f32 {
        sqrt(self.dx() * self.dx() + self.dy() * self.dy())
    }
    /// Unit normal pointing to the left side (CCW rotation of direction).
    fn normal(&self) -> Pt {
        let l = self.len();
        if l < 1e-6 {
            return Pt { x: 0.0, y: -1.0 };
        }
        Pt {
            x: -self.dy() / l,
            y: self.dx() / l,
        }
    }
}

// ── Path command parsing ───────────────────────────────────────────

fn read_f32_le(data: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// A parsed sub-path: a list of line segments (cubics pre-flattened).
struct SubPath {
    segments: Vec<Seg>,
    closed: bool,
}

/// Flatten a cubic Bézier into line segments.
fn flatten_cubic_to_segs(
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    out: &mut Vec<Seg>,
    depth: u32,
) {
    // Flatness test: max control point distance from chord.
    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x0) * dy - (c1y - y0) * dx).abs();
    let d2 = ((c2x - x0) * dy - (c2y - y0) * dx).abs();
    let chord2 = dx * dx + dy * dy;
    let threshold = 0.25; // points
    let max_d = if d1 > d2 { d1 } else { d2 };
    let flat = if chord2 < 1e-10 {
        max_d <= threshold
    } else {
        depth >= 8 || max_d * max_d <= threshold * threshold * chord2
    };

    if flat || depth >= 10 {
        out.push(Seg {
            p0: Pt { x: x0, y: y0 },
            p1: Pt { x: x3, y: y3 },
        });
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

    flatten_cubic_to_segs(x0, y0, m01x, m01y, m012x, m012y, mx, my, out, depth + 1);
    flatten_cubic_to_segs(mx, my, m123x, m123y, m23x, m23y, x3, y3, out, depth + 1);
}

/// Parse path command bytes into a list of sub-paths (each a list of line segments).
fn parse_subpaths(data: &[u8]) -> Vec<SubPath> {
    let mut subpaths = Vec::new();
    let mut current_segs: Vec<Seg> = Vec::new();
    let mut cursor = Pt { x: 0.0, y: 0.0 };
    let mut contour_start = cursor;
    let mut offset = 0usize;

    while offset < data.len() {
        if offset + 4 > data.len() {
            break;
        }
        let tag = read_u32_le(data, offset);
        match tag {
            PATH_MOVE_TO => {
                if offset + PATH_MOVE_TO_SIZE > data.len() {
                    break;
                }
                // Flush previous sub-path if non-empty.
                if !current_segs.is_empty() {
                    subpaths.push(SubPath {
                        segments: core::mem::take(&mut current_segs),
                        closed: false,
                    });
                }
                let x = read_f32_le(data, offset + 4);
                let y = read_f32_le(data, offset + 8);
                cursor = Pt { x, y };
                contour_start = cursor;
                offset += PATH_MOVE_TO_SIZE;
            }
            PATH_LINE_TO => {
                if offset + PATH_LINE_TO_SIZE > data.len() {
                    break;
                }
                let x = read_f32_le(data, offset + 4);
                let y = read_f32_le(data, offset + 8);
                let next = Pt { x, y };
                // Keep zero-length segments (dots) — the stroke expander
                // renders them as circles.
                current_segs.push(Seg {
                    p0: cursor,
                    p1: next,
                });
                cursor = next;
                offset += PATH_LINE_TO_SIZE;
            }
            PATH_CUBIC_TO => {
                if offset + PATH_CUBIC_TO_SIZE > data.len() {
                    break;
                }
                let c1x = read_f32_le(data, offset + 4);
                let c1y = read_f32_le(data, offset + 8);
                let c2x = read_f32_le(data, offset + 12);
                let c2y = read_f32_le(data, offset + 16);
                let x = read_f32_le(data, offset + 20);
                let y = read_f32_le(data, offset + 24);
                flatten_cubic_to_segs(
                    cursor.x,
                    cursor.y,
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                    &mut current_segs,
                    0,
                );
                cursor = Pt { x, y };
                offset += PATH_CUBIC_TO_SIZE;
            }
            PATH_CLOSE => {
                // Close: add segment back to contour start if needed.
                let dx = contour_start.x - cursor.x;
                let dy = contour_start.y - cursor.y;
                if dx * dx + dy * dy > 1e-10 {
                    current_segs.push(Seg {
                        p0: cursor,
                        p1: contour_start,
                    });
                }
                cursor = contour_start;
                if !current_segs.is_empty() {
                    subpaths.push(SubPath {
                        segments: core::mem::take(&mut current_segs),
                        closed: true,
                    });
                }
                offset += PATH_CLOSE_SIZE;
            }
            _ => break,
        }
    }

    // Flush any remaining open sub-path.
    if !current_segs.is_empty() {
        subpaths.push(SubPath {
            segments: current_segs,
            closed: false,
        });
    }

    subpaths
}

// ── Stroke expansion ───────────────────────────────────────────────
//
// Algorithm reference: Cairo (cairo-path-stroke-polygon.c),
// Skia (SkStroke.cpp), Kurbo (stroke.rs).
//
// For each sub-path:
//   1. Flatten cubics to line segments.
//   2. Compute unit normal for each segment: n = (-dy/len, dx/len).
//      In y-down screen coords, n points right-of-travel. "Left offset"
//      (vertex + n*hw) is the inner side for CW paths, outer for CCW.
//   3. At each join, determine turn direction via tangent cross product.
//      cross > 0 → CW turn → right side (−n) is outer → arc on right.
//      cross < 0 → CCW turn → left side (+n) is outer → arc on left.
//      Arcs always use the shortest path (emit_arc_join).
//   4. Open paths: left forward → end cap → right backward → start cap → close.
//      Closed paths: left forward (with closing join) → close;
//                    right backward (separate contour) → close.

/// Compute the tangent cross product at a join between segments i and i+1.
/// In y-down screen coordinates:
///   Positive = CW turn → right side (−normal) is outer.
///   Negative = CCW turn → left side (+normal) is outer.
fn turn_cross(segs: &[Seg], i: usize, next: usize) -> f32 {
    let t0x = segs[i].dx();
    let t0y = segs[i].dy();
    let t1x = segs[next].dx();
    let t1y = segs[next].dy();
    t0x * t1y - t0y * t1x
}

/// Emit a circular arc from `start_pt` to `end_pt`, both at distance `r`
/// from `center`. The arc sweeps through the angle from `start_pt` to
/// `end_pt` around `center`, in the direction given by `sweep_sign`
/// (+1.0 = CCW, -1.0 = CW).
///
/// Emits pre-flattened line segments (not cubic Béziers) to avoid
/// double-flattening: stroke expansion already flattens input cubics
/// to line segments, and downstream consumers (stencil pipeline, CPU
/// rasterizer) would flatten cubics again. Line segments eliminate the
/// point-count multiplication that exceeds metal-render's 512-point
/// budget for fan tessellation.
///
/// Segment count is based on angular step ≤ π/4 (45°), which keeps
/// chord deviation under 0.3 units at any radius — well within visual
/// tolerance for stroke joins and caps.
fn emit_arc(out: &mut Vec<u8>, center: Pt, r: f32, start_pt: Pt, end_pt: Pt, sweep_sign: f32) {
    let a0 = atan2(start_pt.y - center.y, start_pt.x - center.x);
    let a1 = atan2(end_pt.y - center.y, end_pt.x - center.x);
    let mut sweep = a1 - a0;

    // Normalize sweep to [-π, π] first (shortest arc).
    if sweep > PI {
        sweep -= TWO_PI;
    }
    if sweep < -PI {
        sweep += TWO_PI;
    }

    // If the sweep direction doesn't match the requested sign, flip.
    if sweep_sign > 0.0 && sweep < 0.0 {
        sweep += TWO_PI;
    } else if sweep_sign < 0.0 && sweep > 0.0 {
        sweep -= TWO_PI;
    }

    // Skip tiny arcs.
    if sweep.abs() < 0.01 {
        path_line_to(out, end_pt.x, end_pt.y);
        return;
    }

    // Angular step ≤ π/4: at most 8 segments per full circle.
    // Chord deviation = r * (1 - cos(π/8)) ≈ 0.076r — sub-pixel for
    // typical stroke widths (r ≤ 4).
    let max_step = PI * 0.25;
    let n_segs = {
        let ratio = sweep.abs() / max_step;
        let f = floor_f32(ratio);
        (if ratio > f {
            f as usize + 1
        } else {
            f as usize
        })
        .max(1)
    };
    let step = sweep / n_segs as f32;

    let mut a = a0;
    for i in 1..=n_segs {
        // Use exact end angle on the last segment to avoid drift.
        let a_next = if i == n_segs { a0 + sweep } else { a + step };
        let x = center.x + r * cos(a_next);
        let y = center.y + r * sin(a_next);
        path_line_to(out, x, y);
        a = a_next;
    }
}

/// Emit a circular arc using the shortest path from `start_pt` to `end_pt`.
///
/// For round joins, the shortest arc is always geometrically correct: the
/// gap between offset edges at a corner subtends an angle equal to the turn
/// angle, and the shortest arc fills exactly that gap. This avoids the need
/// to compute a sweep sign, which varies per-corner and is easy to get wrong.
fn emit_arc_join(out: &mut Vec<u8>, center: Pt, r: f32, start_pt: Pt, end_pt: Pt) {
    let a0 = atan2(start_pt.y - center.y, start_pt.x - center.x);
    let a1 = atan2(end_pt.y - center.y, end_pt.x - center.x);
    let mut sweep = a1 - a0;
    // Normalize to [-π, π] — this is the shortest arc.
    if sweep > PI {
        sweep -= TWO_PI;
    }
    if sweep < -PI {
        sweep += TWO_PI;
    }
    // Determine the sign that matches the shortest direction.
    let sign = if sweep >= 0.0 { 1.0 } else { -1.0 };
    emit_arc(out, center, r, start_pt, end_pt, sign);
}

/// Emit a round cap (semicircle) at a path endpoint.
/// A cap is a round join with sweep = π (Kurbo insight).
/// `left` is the left offset point, `right` is the right offset point.
/// `forward` is the unit tangent pointing away from the path.
fn emit_round_cap(out: &mut Vec<u8>, center: Pt, left: Pt, right: Pt, half_w: f32, is_end: bool) {
    if is_end {
        // End cap: sweep from left side → right side (going forward).
        // The cap bulges in the tangent direction (away from the path).
        // CCW sweep from right to left (since left is CCW from right).
        emit_arc(out, center, half_w, left, right, -1.0);
    } else {
        // Start cap: sweep from right side → left side (going backward).
        emit_arc(out, center, half_w, right, left, -1.0);
    }
}

/// Left-side offset point for segment `i` at its start.
fn left_start(segs: &[Seg], normals: &[Pt], i: usize, hw: f32) -> Pt {
    Pt {
        x: segs[i].p0.x + normals[i].x * hw,
        y: segs[i].p0.y + normals[i].y * hw,
    }
}

/// Left-side offset point for segment `i` at its end.
fn left_end(segs: &[Seg], normals: &[Pt], i: usize, hw: f32) -> Pt {
    Pt {
        x: segs[i].p1.x + normals[i].x * hw,
        y: segs[i].p1.y + normals[i].y * hw,
    }
}

/// Right-side offset point for segment `i` at its start.
fn right_start(segs: &[Seg], normals: &[Pt], i: usize, hw: f32) -> Pt {
    Pt {
        x: segs[i].p0.x - normals[i].x * hw,
        y: segs[i].p0.y - normals[i].y * hw,
    }
}

/// Right-side offset point for segment `i` at its end.
fn right_end(segs: &[Seg], normals: &[Pt], i: usize, hw: f32) -> Pt {
    Pt {
        x: segs[i].p1.x - normals[i].x * hw,
        y: segs[i].p1.y - normals[i].y * hw,
    }
}

/// Expand a stroked path into filled path commands.
///
/// `data` is the original path command bytes (MoveTo/LineTo/CubicTo/Close).
/// `stroke_width` is in points.
///
/// Returns new path command bytes representing the filled outline of the stroke.
/// Uses the winding fill rule (outer CCW, inner CW for CCW-wound input).
pub fn expand_stroke(data: &[u8], stroke_width: f32) -> Vec<u8> {
    if data.is_empty() || stroke_width <= 0.0 {
        return Vec::new();
    }

    let half_w = stroke_width * 0.5;
    let subpaths = parse_subpaths(data);
    let mut out = Vec::new();

    for sp in &subpaths {
        if sp.segments.is_empty() {
            continue;
        }
        expand_subpath(&mut out, &sp.segments, sp.closed, half_w);
    }

    out
}

/// Expand a single sub-path (open or closed) into filled stroke geometry.
fn expand_subpath(out: &mut Vec<u8>, segs: &[Seg], closed: bool, hw: f32) {
    if segs.is_empty() {
        return;
    }

    // Single zero-length segment → circle (dot).
    if segs.len() == 1 {
        let s = &segs[0];
        let d = s.dx() * s.dx() + s.dy() * s.dy();
        if d < 1e-10 {
            emit_circle(out, s.p0, hw);
            return;
        }
    }

    let normals: Vec<Pt> = segs.iter().map(|s| s.normal()).collect();
    let n = segs.len();

    // ── Left side (forward) ────────────────────────────────────────

    path_move_to(
        out,
        left_start(segs, &normals, 0, hw).x,
        left_start(segs, &normals, 0, hw).y,
    );

    for i in 0..n {
        let le = left_end(segs, &normals, i, hw);
        path_line_to(out, le.x, le.y);

        // Join to next segment.
        let next = if i + 1 < n {
            Some(i + 1)
        } else if closed {
            Some(0)
        } else {
            None
        };

        if let Some(ni) = next {
            let cross = turn_cross(segs, i, ni);
            let next_ls = left_start(segs, &normals, ni, hw);

            if cross < -1e-6 {
                // CCW turn (y-down): left side is OUTER → round arc.
                emit_arc_join(out, segs[i].p1, hw, le, next_ls);
            }
            // CW turn or straight: left side is inner → straight line.
            path_line_to(out, next_ls.x, next_ls.y);
        }
    }

    if closed {
        // Close left (outer) contour.
        path_close(out);

        // ── Right side (backward, separate contour) ────────────────
        let last = n - 1;
        let re_last = right_end(segs, &normals, last, hw);
        path_move_to(out, re_last.x, re_last.y);

        for j in 0..n {
            let i = (last + n - j) % n; // walk backward: last, last-1, ..., 0
            let rs = right_start(segs, &normals, i, hw);
            path_line_to(out, rs.x, rs.y);

            // Join to previous segment (= next in reverse).
            let prev = if i > 0 {
                Some(i - 1)
            } else if closed {
                Some(last)
            } else {
                None
            };

            if let Some(pi) = prev {
                let cross = turn_cross(segs, pi, i);
                let prev_re = right_end(segs, &normals, pi, hw);

                if cross > 1e-6 {
                    // CW turn (y-down): right side is OUTER → round arc.
                    emit_arc_join(out, segs[i].p0, hw, rs, prev_re);
                }
                // CCW turn or straight: right side is inner → straight line.
                path_line_to(out, prev_re.x, prev_re.y);
            }
        }

        path_close(out);
    } else {
        // ── Open path: end cap → right backward → start cap → close ──

        let last = n - 1;
        let le_last = left_end(segs, &normals, last, hw);
        let re_last = right_end(segs, &normals, last, hw);

        // End cap (semicircle from left to right, sweeping forward).
        emit_round_cap(out, segs[last].p1, le_last, re_last, hw, true);

        // Right side (backward).
        for j in 0..n {
            let i = last - j;
            let rs = right_start(segs, &normals, i, hw);
            path_line_to(out, rs.x, rs.y);

            if i > 0 {
                let pi = i - 1;
                let cross = turn_cross(segs, pi, i);
                let prev_re = right_end(segs, &normals, pi, hw);

                if cross > 1e-6 {
                    // CW turn (y-down): right side is OUTER → round arc.
                    emit_arc_join(out, segs[i].p0, hw, rs, prev_re);
                }
                path_line_to(out, prev_re.x, prev_re.y);
            }
        }

        // Start cap (semicircle from right to left, sweeping backward).
        let rs_first = right_start(segs, &normals, 0, hw);
        let ls_first = left_start(segs, &normals, 0, hw);
        emit_round_cap(out, segs[0].p0, ls_first, rs_first, hw, false);

        path_close(out);
    }
}

/// Emit a filled circle (for zero-length segments / dots).
fn emit_circle(out: &mut Vec<u8>, center: Pt, r: f32) {
    let k = KAPPA * r;
    path_move_to(out, center.x + r, center.y);
    path_cubic_to(
        out,
        center.x + r,
        center.y + k,
        center.x + k,
        center.y + r,
        center.x,
        center.y + r,
    );
    path_cubic_to(
        out,
        center.x - k,
        center.y + r,
        center.x - r,
        center.y + k,
        center.x - r,
        center.y,
    );
    path_cubic_to(
        out,
        center.x - r,
        center.y - k,
        center.x - k,
        center.y - r,
        center.x,
        center.y - r,
    );
    path_cubic_to(
        out,
        center.x + k,
        center.y - r,
        center.x + r,
        center.y - k,
        center.x + r,
        center.y,
    );
    path_close(out);
}
