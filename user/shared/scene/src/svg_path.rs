//! SVG path `d` attribute parser.
//!
//! Converts SVG path commands (M, L, H, V, C, S, Q, A, Z and lowercase
//! relatives) into the OS's native path command format (MoveTo, LineTo,
//! CubicTo, Close). Arcs are approximated as cubic Bézier curves.
//!
//! This is intentionally a subset parser — it handles the commands found
//! in Tabler Icons (and most icon sets). Full SVG path spec compliance
//! is not a goal.

use alloc::vec::Vec;

use crate::primitives::{path_close, path_cubic_to, path_line_to, path_move_to};

// ── Trig helpers (no_std) ──────────────────────────────────────────

const PI: f32 = core::f32::consts::PI;
const HALF_PI: f32 = core::f32::consts::FRAC_PI_2;
const TWO_PI: f32 = 2.0 * PI;

fn floor_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;

    if x < f {
        f - 1.0
    } else {
        f
    }
}

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

/// Sine approximation with Cody-Waite range reduction to [-π/4, π/4].
///
/// The 7th-order polynomial is only accurate for |x| ≤ π/4 ≈ 0.785.
/// At |x| = π/2, error is 0.016%; at |x| = π, error is 7.5%.
/// We reduce to [-π/4, π/4] using the identities:
///   sin(x) =  cos_poly(π/2 - x)  for x in [π/4, 3π/4]
///   cos(x) = -sin_poly(x - π/2)  for x in [π/4, 3π/4]
/// This gives full f32 accuracy at ALL angles including π/2 and π.
fn sin(x: f32) -> f32 {
    // Reduce to [-π, π].
    let x = x - TWO_PI * floor_f32(x / TWO_PI + 0.5);
    // Reduce to [0, π] using sin(-x) = -sin(x).
    let (x, sign) = if x < 0.0 { (-x, -1.0) } else { (x, 1.0) };
    // Now x in [0, π]. Split into quadrants:
    //   [0, π/4]:       sin(x) = sin_poly(x)
    //   [π/4, 3π/4]:    sin(x) = cos_poly(π/2 - x)
    //   [3π/4, π]:       sin(x) = sin_poly(π - x)
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
    // Reduce to [-π, π].
    let x = x - TWO_PI * floor_f32(x / TWO_PI + 0.5);
    // cos(-x) = cos(x).
    let x = if x < 0.0 { -x } else { x };
    // Now x in [0, π]. Split into quadrants:
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

/// sin(x) for |x| ≤ π/4. 7th-order minimax polynomial.
fn sin_poly(x: f32) -> f32 {
    let x2 = x * x;

    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)))
}

/// cos(x) for |x| ≤ π/4. 6th-order minimax polynomial.
fn cos_poly(x: f32) -> f32 {
    let x2 = x * x;

    1.0 - x2 / 2.0 * (1.0 - x2 / 12.0 * (1.0 - x2 / 30.0))
}

fn atan_inner(x: f32) -> f32 {
    let x2 = x * x;

    x * (0.999_866_0
        + x2 * (-0.330_299_5 + x2 * (0.180_141_0 + x2 * (-0.085_133_0 + x2 * 0.020_835_1))))
}

fn atan2(y: f32, x: f32) -> f32 {
    if x > 0.0 {
        let a = y / x;

        if a.abs() > 1.0 {
            let r = atan_inner(x / y);
            if y > 0.0 {
                HALF_PI - r
            } else {
                -HALF_PI - r
            }
        } else {
            atan_inner(a)
        }
    } else if x < 0.0 {
        let a = y / x;
        let base = if a.abs() > 1.0 {
            let r = atan_inner(x / y);
            if y >= 0.0 {
                HALF_PI - r
            } else {
                -HALF_PI - r
            }
        } else {
            atan_inner(a)
        };

        if y >= 0.0 {
            base + PI
        } else {
            base - PI
        }
    } else if y > 0.0 {
        HALF_PI
    } else if y < 0.0 {
        -HALF_PI
    } else {
        0.0
    }
}

// ── SVG number parser ──────────────────────────────────────────────

/// Skip whitespace and commas.
fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && (s[i] == b' ' || s[i] == b',' || s[i] == b'\t' || s[i] == b'\n') {
        i += 1;
    }

    i
}

/// Parse a floating-point number from the SVG path string.
/// Returns (value, next_index).
fn parse_number(s: &[u8], start: usize) -> Option<(f32, usize)> {
    let mut i = skip_ws(s, start);
    if i >= s.len() {
        return None;
    }

    let mut neg = false;

    if s[i] == b'-' {
        neg = true;
        i += 1;
    } else if s[i] == b'+' {
        i += 1;
    }

    if i >= s.len() {
        return None;
    }

    // Must start with digit or '.'
    if !s[i].is_ascii_digit() && s[i] != b'.' {
        return None;
    }

    let num_start = i;

    // Integer part.
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    // Fractional part.
    if i < s.len() && s[i] == b'.' {
        i += 1;

        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
    }

    // Parse the number manually (no f32::from_str in no_std).
    let mut val: f64 = 0.0;
    let mut seen_dot = false;
    let mut frac_div: f64 = 1.0;

    for &ch in &s[num_start..i] {
        if ch == b'.' {
            seen_dot = true;
        } else {
            let d = (ch - b'0') as f64;

            if seen_dot {
                frac_div *= 10.0;
                val += d / frac_div;
            } else {
                val = val * 10.0 + d;
            }
        }
    }

    if neg {
        val = -val;
    }

    Some((val as f32, i))
}

/// Parse a flag (0 or 1) for SVG arc commands.
fn parse_flag(s: &[u8], start: usize) -> Option<(bool, usize)> {
    let i = skip_ws(s, start);

    if i >= s.len() {
        return None;
    }

    match s[i] {
        b'0' => Some((false, i + 1)),
        b'1' => Some((true, i + 1)),
        _ => None,
    }
}

// ── Arc-to-cubic conversion ────────────────────────────────────────

/// Convert an SVG endpoint-parameterized arc to center parameterization,
/// then approximate with cubic Bézier curves.
///
/// Reference: SVG 1.1 spec, Appendix F.6 "Implementation Notes"
fn arc_to_cubics(
    out: &mut Vec<u8>,
    x1: f32,
    y1: f32,
    mut rx: f32,
    mut ry: f32,
    x_rot_deg: f32,
    large_arc: bool,
    sweep: bool,
    x2: f32,
    y2: f32,
) {
    // Degenerate: zero radius → line.
    if rx.abs() < 1e-6 || ry.abs() < 1e-6 {
        path_line_to(out, x2, y2);

        return;
    }

    rx = rx.abs();
    ry = ry.abs();

    // Degenerate: same endpoint → skip.
    let dx = x2 - x1;
    let dy = y2 - y1;

    if dx * dx + dy * dy < 1e-10 {
        return;
    }

    let phi = x_rot_deg * PI / 180.0;
    let cos_phi = cos(phi);
    let sin_phi = sin(phi);
    // Step 1: Transform to unit-circle space.
    let dx2 = (x1 - x2) * 0.5;
    let dy2 = (y1 - y2) * 0.5;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;
    // Step 2: Correct out-of-range radii (F.6.6).
    let x1p2 = x1p * x1p;
    let y1p2 = y1p * y1p;
    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let lambda = x1p2 / rx2 + y1p2 / ry2;

    if lambda > 1.0 {
        let sq = sqrt(lambda);

        rx *= sq;
        ry *= sq;
    }

    let rx2 = rx * rx;
    let ry2 = ry * ry;
    // Step 3: Compute center (F.6.5).
    let num = (rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2).max(0.0);
    let den = rx2 * y1p2 + ry2 * x1p2;
    let sq = if den > 1e-10 { sqrt(num / den) } else { 0.0 };
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let cxp = sign * sq * (rx * y1p / ry);
    let cyp = sign * sq * (-(ry * x1p / rx));
    // Center in original coordinates.
    let cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) * 0.5;
    // Step 4: Compute start angle and sweep.
    let theta1 = atan2((y1p - cyp) / ry, (x1p - cxp) / rx);
    let mut dtheta = atan2((-y1p - cyp) / ry, (-x1p - cxp) / rx) - theta1;

    if sweep && dtheta < 0.0 {
        dtheta += TWO_PI;
    } else if !sweep && dtheta > 0.0 {
        dtheta -= TWO_PI;
    }

    // Step 5: Emit cubic Bézier arcs (one per ≤90° segment).
    // Subtract a small tolerance before ceiling to prevent float noise
    // from splitting exact quarter-circle arcs into extra segments
    // (e.g., dtheta = π/2 + 1 ULP → ratio = 1.0000001 → ceil = 2).
    let n_segs = (((dtheta.abs() / HALF_PI) - 1e-4).ceil_no_std().max(1.0) as usize).max(1);
    let seg_angle = dtheta / n_segs as f32;
    let mut angle = theta1;

    for _ in 0..n_segs {
        let a1 = angle;
        let a2 = angle + seg_angle;

        emit_arc_segment(out, cx, cy, rx, ry, cos_phi, sin_phi, a1, a2);

        angle = a2;
    }
}

/// Emit a single arc segment (≤90°) as a cubic Bézier.
fn emit_arc_segment(
    out: &mut Vec<u8>,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    cos_phi: f32,
    sin_phi: f32,
    a1: f32,
    a2: f32,
) {
    let da = a2 - a1;
    let alpha = sin(da) * (sqrt(4.0 + 3.0 * tan_half_sq(da)) - 1.0) / 3.0;
    let cos1 = cos(a1);
    let sin1 = sin(a1);
    let cos2 = cos(a2);
    let sin2 = sin(a2);
    // Endpoint on the unit ellipse.
    let e1x = rx * cos1;
    let e1y = ry * sin1;
    let e2x = rx * cos2;
    let e2y = ry * sin2;
    // Tangent vectors (derivatives).
    let d1x = -rx * sin1;
    let d1y = ry * cos1;
    let d2x = -rx * sin2;
    let d2y = ry * cos2;
    // Control points in ellipse space.
    let c1x = e1x + alpha * d1x;
    let c1y = e1y + alpha * d1y;
    let c2x = e2x - alpha * d2x;
    let c2y = e2y - alpha * d2y;
    // Rotate and translate to world coordinates.
    let q1x = cos_phi * c1x - sin_phi * c1y + cx;
    let q1y = sin_phi * c1x + cos_phi * c1y + cy;
    let q2x = cos_phi * c2x - sin_phi * c2y + cx;
    let q2y = sin_phi * c2x + cos_phi * c2y + cy;
    let px = cos_phi * e2x - sin_phi * e2y + cx;
    let py = sin_phi * e2x + cos_phi * e2y + cy;

    path_cubic_to(out, q1x, q1y, q2x, q2y, px, py);
}

/// tan(x/2)² — used in the alpha formula for arc-to-cubic.
fn tan_half_sq(x: f32) -> f32 {
    let half = x * 0.5;
    let c = cos(half);

    if c.abs() < 1e-6 {
        return 1e6; // near π, cap the value
    }

    let s = sin(half);

    (s * s) / (c * c)
}

/// Ceiling for no_std (f32 doesn't have .ceil() without std).
trait CeilNoStd {
    fn ceil_no_std(self) -> f32;
}
impl CeilNoStd for f32 {
    fn ceil_no_std(self) -> f32 {
        let f = floor_f32(self);

        if self > f {
            f + 1.0
        } else {
            f
        }
    }
}

// ── Main parser ────────────────────────────────────────────────────

/// Parse an SVG path `d` attribute string and emit native path commands.
///
/// Supports: M/m, L/l, H/h, V/v, C/c, S/s, Q/q, A/a, Z/z.
/// All coordinates are converted to absolute and emitted as MoveTo,
/// LineTo, CubicTo, Close.
pub fn parse_svg_path(d: &str) -> Vec<u8> {
    let s = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut cx: f32 = 0.0; // current x
    let mut cy: f32 = 0.0; // current y
    let mut last_cmd = b'M';
    let mut last_c2x: f32 = 0.0; // last cubic control point 2 (for S)
    let mut last_c2y: f32 = 0.0;
    let mut subpath_x: f32 = 0.0;
    let mut subpath_y: f32 = 0.0;

    while i < s.len() {
        i = skip_ws(s, i);

        if i >= s.len() {
            break;
        }

        // Check if this is a command letter.
        let cmd = if s[i].is_ascii_alphabetic() {
            let c = s[i];

            i += 1;

            c
        } else {
            // Implicit repeat of last command (except M becomes L).
            match last_cmd {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            }
        };

        match cmd {
            b'M' => {
                if let Some((x, ni)) = parse_number(s, i) {
                    i = ni;

                    if let Some((y, ni)) = parse_number(s, i) {
                        i = ni;
                        cx = x;
                        cy = y;
                        subpath_x = cx;
                        subpath_y = cy;

                        path_move_to(&mut out, cx, cy);
                    }
                }
            }
            b'm' => {
                if let Some((dx, ni)) = parse_number(s, i) {
                    i = ni;

                    if let Some((dy, ni)) = parse_number(s, i) {
                        i = ni;
                        cx += dx;
                        cy += dy;
                        subpath_x = cx;
                        subpath_y = cy;

                        path_move_to(&mut out, cx, cy);
                    }
                }
            }
            b'L' => {
                if let Some((x, ni)) = parse_number(s, i) {
                    i = ni;

                    if let Some((y, ni)) = parse_number(s, i) {
                        i = ni;
                        cx = x;
                        cy = y;

                        path_line_to(&mut out, cx, cy);
                    }
                }
            }
            b'l' => {
                if let Some((dx, ni)) = parse_number(s, i) {
                    i = ni;

                    if let Some((dy, ni)) = parse_number(s, i) {
                        i = ni;
                        cx += dx;
                        cy += dy;

                        path_line_to(&mut out, cx, cy);
                    }
                }
            }
            b'H' => {
                if let Some((x, ni)) = parse_number(s, i) {
                    i = ni;
                    cx = x;

                    path_line_to(&mut out, cx, cy);
                }
            }
            b'h' => {
                if let Some((dx, ni)) = parse_number(s, i) {
                    i = ni;
                    cx += dx;

                    path_line_to(&mut out, cx, cy);
                }
            }
            b'V' => {
                if let Some((y, ni)) = parse_number(s, i) {
                    i = ni;
                    cy = y;

                    path_line_to(&mut out, cx, cy);
                }
            }
            b'v' => {
                if let Some((dy, ni)) = parse_number(s, i) {
                    i = ni;
                    cy += dy;

                    path_line_to(&mut out, cx, cy);
                }
            }
            b'C' => {
                let c1x;
                let c1y;
                let c2x;
                let c2y;
                let x;
                let y;

                if let Some((v, ni)) = parse_number(s, i) {
                    c1x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    c1y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    c2x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    c2y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;

                path_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'c' => {
                let dc1x;
                let dc1y;
                let dc2x;
                let dc2y;
                let dx;
                let dy;

                if let Some((v, ni)) = parse_number(s, i) {
                    dc1x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dc1y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dc2x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dc2y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dy = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                let c1x = cx + dc1x;
                let c1y = cy + dc1y;
                let c2x = cx + dc2x;
                let c2y = cy + dc2y;

                last_c2x = c2x;
                last_c2y = c2y;
                cx += dx;
                cy += dy;

                path_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'S' => {
                // Smooth cubic: reflect previous c2 as c1.
                let c1x = 2.0 * cx - last_c2x;
                let c1y = 2.0 * cy - last_c2y;
                let c2x;
                let c2y;
                let x;
                let y;

                if let Some((v, ni)) = parse_number(s, i) {
                    c2x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    c2y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;

                path_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b's' => {
                let c1x = 2.0 * cx - last_c2x;
                let c1y = 2.0 * cy - last_c2y;
                let dc2x;
                let dc2y;
                let dx;
                let dy;

                if let Some((v, ni)) = parse_number(s, i) {
                    dc2x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dc2y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dy = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                let c2x = cx + dc2x;
                let c2y = cy + dc2y;

                last_c2x = c2x;
                last_c2y = c2y;
                cx += dx;
                cy += dy;

                path_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'Q' => {
                // Quadratic → cubic via degree elevation.
                let qx;
                let qy;
                let x;
                let y;

                if let Some((v, ni)) = parse_number(s, i) {
                    qx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    qy = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                let c1x = cx + 2.0 / 3.0 * (qx - cx);
                let c1y = cy + 2.0 / 3.0 * (qy - cy);
                let c2x = x + 2.0 / 3.0 * (qx - x);
                let c2y = y + 2.0 / 3.0 * (qy - y);

                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;

                path_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'q' => {
                let dqx;
                let dqy;
                let dx;
                let dy;

                if let Some((v, ni)) = parse_number(s, i) {
                    dqx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dqy = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dy = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                let qx = cx + dqx;
                let qy = cy + dqy;
                let x = cx + dx;
                let y = cy + dy;
                let c1x = cx + 2.0 / 3.0 * (qx - cx);
                let c1y = cy + 2.0 / 3.0 * (qy - cy);
                let c2x = x + 2.0 / 3.0 * (qx - x);
                let c2y = y + 2.0 / 3.0 * (qy - y);

                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;

                path_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'A' => {
                let arx;
                let ary;
                let rot;
                let la;
                let sw;
                let x;
                let y;

                if let Some((v, ni)) = parse_number(s, i) {
                    arx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    ary = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    rot = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_flag(s, i) {
                    la = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_flag(s, i) {
                    sw = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    x = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    y = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                arc_to_cubics(&mut out, cx, cy, arx, ary, rot, la, sw, x, y);

                cx = x;
                cy = y;
            }
            b'a' => {
                let arx;
                let ary;
                let rot;
                let la;
                let sw;
                let dx;
                let dy;

                if let Some((v, ni)) = parse_number(s, i) {
                    arx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    ary = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    rot = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_flag(s, i) {
                    la = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_flag(s, i) {
                    sw = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dx = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }
                if let Some((v, ni)) = parse_number(s, i) {
                    dy = v;
                    i = ni;
                } else {
                    last_cmd = cmd;

                    continue;
                }

                let x = cx + dx;
                let y = cy + dy;

                arc_to_cubics(&mut out, cx, cy, arx, ary, rot, la, sw, x, y);

                cx = x;
                cy = y;
            }
            b'Z' | b'z' => {
                path_close(&mut out);

                cx = subpath_x;
                cy = subpath_y;
            }
            _ => {
                // Unknown command — skip.
                i += 1;

                continue;
            }
        }

        // Reset last control point for non-cubic commands.
        match cmd {
            b'C' | b'c' | b'S' | b's' => {}
            _ => {
                last_c2x = cx;
                last_c2y = cy;
            }
        }

        last_cmd = cmd;
    }

    out
}

/// Debug: expose atan2 for testing.
pub fn debug_atan2(y: f32, x: f32) -> f32 {
    atan2(y, x)
}

/// Debug: expose sin for testing.
pub fn debug_sin(x: f32) -> f32 {
    sin(x)
}

/// Debug helper: expose internal arc computation for testing.
/// Returns (theta1, dtheta, n_segs, cx, cy, t1_y, t1_x, cxp, cyp, x1p, y1p).
pub fn debug_arc_params(
    x1: f32,
    y1: f32,
    rx_in: f32,
    ry_in: f32,
    x_rot_deg: f32,
    large_arc: bool,
    sweep: bool,
    x2: f32,
    y2: f32,
) -> (f32, f32, usize, f32, f32, f32, f32, f32, f32, f32, f32) {
    let mut rx = rx_in.abs();
    let mut ry = ry_in.abs();
    let phi = x_rot_deg * PI / 180.0;
    let cos_phi = cos(phi);
    let sin_phi = sin(phi);
    let dx2 = (x1 - x2) * 0.5;
    let dy2 = (y1 - y2) * 0.5;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;
    let x1p2 = x1p * x1p;
    let y1p2 = y1p * y1p;
    let lambda = x1p2 / (rx * rx) + y1p2 / (ry * ry);

    if lambda > 1.0 {
        let sq = sqrt(lambda);

        rx *= sq;
        ry *= sq;
    }

    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let num = (rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2).max(0.0);
    let den = rx2 * y1p2 + ry2 * x1p2;
    let sq = if den > 1e-10 { sqrt(num / den) } else { 0.0 };
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let cxp = sign * sq * (rx * y1p / ry);
    let cyp = sign * sq * (-(ry * x1p / rx));
    let cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) * 0.5;
    let theta1 = atan2((y1p - cyp) / ry, (x1p - cxp) / rx);
    let mut dtheta = atan2((-y1p - cyp) / ry, (-x1p - cxp) / rx) - theta1;

    if sweep && dtheta < 0.0 {
        dtheta += TWO_PI;
    } else if !sweep && dtheta > 0.0 {
        dtheta -= TWO_PI;
    }

    let n_segs = (((dtheta.abs() / HALF_PI) - 1e-4).ceil_no_std().max(1.0) as usize).max(1);
    let t1_y = (y1p - cyp) / ry;
    let t1_x = (x1p - cxp) / rx;

    (
        theta1, dtheta, n_segs, cx, cy, t1_y, t1_x, cxp, cyp, x1p, y1p,
    )
}
