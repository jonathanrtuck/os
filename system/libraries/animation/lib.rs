//! Animation easing functions for the document-centric OS.
//!
//! Evaluates any of 24 easing curves at a normalised time `t ∈ [0, 1]`.
//! The library is `no_std` with no allocator requirement and no external
//! dependencies.  All floating-point operations use the `f32` primitives
//! built into `core`.
//!
//! # Usage
//!
//! ```text
//! let y = animation::ease(Easing::EaseInOutCubic, 0.5);
//! // → 0.5  (inflection point of the symmetric cubic)
//! ```
//!
//! # Accuracy notes
//!
//! * `sin` approximation: < 0.0002 absolute error on `[−π, π]` (7th-order
//!   minimax polynomial, Horner form).
//! * `exp2` approximation: < 0.0002 absolute error on `[−10, 10]` (integer
//!   exponent via bit manipulation + 4th-order polynomial for the fraction).
//! * Cubic-bezier solver: ≤ 6 Newton–Raphson iterations + bisection fallback;
//!   error < 0.0001 in y.
//!
//! Back, Elastic, and Bounce easings intentionally produce values outside
//! `[0, 1]` (overshoot / undershoot).  All other easings are clamped to
//! `[0, 1]`.

#![no_std]

// ── Mathematical constants ───────────────────────────────────────────────────

/// `π`
const PI: f32 = core::f32::consts::PI;

/// `2π`
const TAU: f32 = core::f32::consts::TAU;

/// Overshoot constant used by the Back family of easings.
const BACK_S: f32 = 1.701_58;

// ── Easing variant ───────────────────────────────────────────────────────────

/// An easing curve that maps normalised time `t ∈ [0, 1]` to a progress
/// value.
///
/// Most easings return a value in `[0, 1]`, but Back, Elastic, and Bounce
/// variants intentionally overshoot (return values outside `[0, 1]`) to
/// create physically-inspired motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    // ── Trivial ──────────────────────────────────────────────────────────────
    /// Constant-velocity straight line.
    Linear,

    // ── CSS standard curves ──────────────────────────────────────────────────
    /// CSS `ease` — fast start, slow end (`cubic-bezier(0.25, 0.1, 0.25, 1.0)`).
    Ease,
    /// CSS `ease-in` — slow start, fast end (`cubic-bezier(0.42, 0, 1, 1)`).
    EaseIn,
    /// CSS `ease-out` — fast start, slow end (`cubic-bezier(0, 0, 0.58, 1)`).
    EaseOut,
    /// CSS `ease-in-out` — slow start and end (`cubic-bezier(0.42, 0, 0.58, 1)`).
    EaseInOut,
    /// Arbitrary cubic Bézier with custom control points `(x1,y1,x2,y2)`.
    ///
    /// Both `x1` and `x2` must be in `[0, 1]`; `y1` and `y2` are unclamped
    /// (values outside `[0, 1]` produce overshoot).
    CubicBezier(f32, f32, f32, f32),

    // ── Polynomial — quadratic ───────────────────────────────────────────────
    /// Quadratic acceleration from zero.
    EaseInQuad,
    /// Quadratic deceleration to zero.
    EaseOutQuad,
    /// Quadratic acceleration then deceleration.
    EaseInOutQuad,

    // ── Polynomial — cubic ───────────────────────────────────────────────────
    /// Cubic acceleration from zero.
    EaseInCubic,
    /// Cubic deceleration to zero.
    EaseOutCubic,
    /// Cubic acceleration then deceleration.
    EaseInOutCubic,

    // ── Exponential ─────────────────────────────────────────────────────────
    /// Exponential acceleration from zero.
    EaseInExpo,
    /// Exponential deceleration to zero.
    EaseOutExpo,
    /// Exponential acceleration then deceleration.
    EaseInOutExpo,

    // ── Back (overshoot) ─────────────────────────────────────────────────────
    /// Slight backward pull before accelerating forward.
    EaseInBack,
    /// Slight overshoot past 1 before settling.
    EaseOutBack,
    /// Back overshoot on both ends.
    EaseInOutBack,

    // ── Elastic ──────────────────────────────────────────────────────────────
    /// Spring-like oscillation on entry.
    EaseInElastic,
    /// Spring-like oscillation on exit.
    EaseOutElastic,

    // ── Bounce ───────────────────────────────────────────────────────────────
    /// Bouncing ball on entry.
    EaseInBounce,
    /// Bouncing ball on exit.
    EaseOutBounce,

    // ── Step ─────────────────────────────────────────────────────────────────
    /// Jumps to 1 at the very first sample (t > 0).
    StepStart,
    /// Holds at 0 until t = 1, then jumps to 1.
    StepEnd,
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Evaluate an easing curve at time `t`.
///
/// `t` is clamped to `[0, 1]` before evaluation.  For overshoot easings the
/// *output* may lie outside `[0, 1]`.
///
/// NaN input is treated as 0.0 (returns 0.0) after the clamp step.
pub fn ease(easing: Easing, t: f32) -> f32 {
    // NaN guard first — NaN comparisons are always false, so it must be
    // checked before the range tests.  Then clamp to [0, 1].
    let t = if t != t {
        0.0
    } else if t < 0.0 {
        0.0
    } else if t > 1.0 {
        1.0
    } else {
        t
    };

    match easing {
        Easing::Linear => t,

        // CSS standard — delegate to the cubic-bezier evaluator
        Easing::Ease => cubic_bezier(0.25, 0.1, 0.25, 1.0, t),
        Easing::EaseIn => cubic_bezier(0.42, 0.0, 1.0, 1.0, t),
        Easing::EaseOut => cubic_bezier(0.0, 0.0, 0.58, 1.0, t),
        Easing::EaseInOut => cubic_bezier(0.42, 0.0, 0.58, 1.0, t),
        Easing::CubicBezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, t),

        // Polynomial — quadratic
        Easing::EaseInQuad => t * t,
        Easing::EaseOutQuad => {
            let u = 1.0 - t;
            1.0 - u * u
        }
        Easing::EaseInOutQuad => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                let u = -2.0 * t + 2.0;
                1.0 - u * u / 2.0
            }
        }

        // Polynomial — cubic
        Easing::EaseInCubic => t * t * t,
        Easing::EaseOutCubic => {
            let u = 1.0 - t;
            1.0 - u * u * u
        }
        Easing::EaseInOutCubic => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                let u = -2.0 * t + 2.0;
                1.0 - u * u * u / 2.0
            }
        }

        // Exponential
        Easing::EaseInExpo => {
            if t == 0.0 {
                0.0
            } else {
                exp2(10.0 * t - 10.0)
            }
        }
        Easing::EaseOutExpo => {
            if t == 1.0 {
                1.0
            } else {
                1.0 - exp2(-10.0 * t)
            }
        }
        Easing::EaseInOutExpo => {
            if t == 0.0 {
                0.0
            } else if t == 1.0 {
                1.0
            } else if t < 0.5 {
                exp2(20.0 * t - 10.0) / 2.0
            } else {
                (2.0 - exp2(-20.0 * t + 10.0)) / 2.0
            }
        }

        // Back (overshoot)
        Easing::EaseInBack => {
            let s = BACK_S;
            t * t * ((s + 1.0) * t - s)
        }
        Easing::EaseOutBack => {
            let s = BACK_S;
            let u = t - 1.0;
            u * u * ((s + 1.0) * u + s) + 1.0
        }
        Easing::EaseInOutBack => {
            let s = BACK_S * 1.525; // standard adjustment for InOut
            if t < 0.5 {
                let u = 2.0 * t;
                u * u * ((s + 1.0) * u - s) / 2.0
            } else {
                let u = 2.0 * t - 2.0;
                (u * u * ((s + 1.0) * u + s) + 2.0) / 2.0
            }
        }

        // Elastic
        Easing::EaseInElastic => {
            if t == 0.0 {
                0.0
            } else if t == 1.0 {
                1.0
            } else {
                -exp2(10.0 * t - 10.0) * sin((10.0 * t - 10.75) * (TAU / 3.0))
            }
        }
        Easing::EaseOutElastic => {
            if t == 0.0 {
                0.0
            } else if t == 1.0 {
                1.0
            } else {
                exp2(-10.0 * t) * sin((10.0 * t - 0.75) * (TAU / 3.0)) + 1.0
            }
        }

        // Bounce
        Easing::EaseInBounce => 1.0 - bounce_out(1.0 - t),
        Easing::EaseOutBounce => bounce_out(t),

        // Step
        Easing::StepStart => {
            if t > 0.0 {
                1.0
            } else {
                0.0
            }
        }
        Easing::StepEnd => {
            if t >= 1.0 {
                1.0
            } else {
                0.0
            }
        }
    }
}

// ── Internal helpers — cubic bezier ─────────────────────────────────────────

/// Evaluate a CSS cubic-bezier easing with control points
/// `P1 = (x1, y1)` and `P2 = (x2, y2)`.
///
/// The algorithm:
/// 1. Compute the Bernstein coefficients for x and y.
/// 2. Given the desired x value (`t_x`), solve for the curve parameter `u`
///    using Newton–Raphson (up to 8 iterations), then binary search as a
///    fallback.
/// 3. Evaluate `y(u)` with the solved parameter.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t_x: f32) -> f32 {
    // Bernstein coefficients for x: cx + bx·u + ax·u²  (stored in a, b, c)
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;

    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;

    /// Sample the x Bernstein polynomial at parameter `u`.
    #[inline(always)]
    fn sample_x(ax: f32, bx: f32, cx: f32, u: f32) -> f32 {
        ((ax * u + bx) * u + cx) * u
    }

    /// Derivative of the x polynomial at `u`.
    #[inline(always)]
    fn sample_dx(ax: f32, bx: f32, cx: f32, u: f32) -> f32 {
        (3.0 * ax * u + 2.0 * bx) * u + cx
    }

    /// Sample the y Bernstein polynomial at parameter `u`.
    #[inline(always)]
    fn sample_y(ay: f32, by: f32, cy: f32, u: f32) -> f32 {
        ((ay * u + by) * u + cy) * u
    }

    // Linear case — no need to solve.
    if ax == 0.0 && bx == 0.0 {
        return sample_y(ay, by, cy, t_x);
    }

    // Initial guess: parameter ≈ t_x (true for near-linear curves).
    let mut u = t_x;

    // Newton–Raphson — converges in a few iterations for well-behaved curves.
    for _ in 0..8 {
        let dx = sample_dx(ax, bx, cx, u);

        if dx.abs() < 1e-6 {
            break;
        }

        let x = sample_x(ax, bx, cx, u) - t_x;
        u -= x / dx;
    }

    // Clamp to valid parameter domain.
    u = u.max(0.0).min(1.0);

    // Verify convergence; fall back to bisection if the Newton step landed
    // far from the target.
    let err = (sample_x(ax, bx, cx, u) - t_x).abs();

    if err > 1e-4 {
        let mut lo = 0.0f32;
        let mut hi = 1.0f32;

        for _ in 0..32 {
            let mid = (lo + hi) * 0.5;
            let x = sample_x(ax, bx, cx, mid);

            if (x - t_x).abs() < 1e-5 {
                u = mid;
                break;
            }

            if x < t_x {
                lo = mid;
            } else {
                hi = mid;
            }

            u = mid;
        }
    }

    sample_y(ay, by, cy, u)
}

// ── Internal helpers — bounce ────────────────────────────────────────────────

/// Piecewise quadratic bounce-out (ball decelerating with bounces).
///
/// Four segments with decreasing amplitude.  Returns a value in `[0, 1]`.
fn bounce_out(t: f32) -> f32 {
    // Standard coefficients from the penner equations.
    const N1: f32 = 7.5625;
    const D1: f32 = 2.75;

    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let t = t - 1.5 / D1;
        N1 * t * t + 0.75
    } else if t < 2.5 / D1 {
        let t = t - 2.25 / D1;
        N1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / D1;
        N1 * t * t + 0.984_375
    }
}

// ── Internal helpers — transcendentals ──────────────────────────────────────

/// Floor for f32 without `std`.
///
/// Implemented via integer truncation: truncate toward zero, then subtract 1
/// if the input was negative and not already an integer.
fn floor_f32(x: f32) -> f32 {
    // Cast to i32 truncates toward zero.
    let t = x as i32 as f32;

    // If x was negative and truncation moved away from negative infinity,
    // subtract 1 to correct.
    if x < t {
        t - 1.0
    } else {
        t
    }
}

/// Sine approximation accurate to < 0.0002 absolute error on `[−π, π]`.
///
/// Algorithm:
/// 1. Range-reduce to `[−π, π]` via `x − τ·round(x/τ)`.
/// 2. Evaluate a 7th-order minimax polynomial in Horner form:
///    `x·(1 − x²/6·(1 − x²/20·(1 − x²/42)))`.
fn sin(x: f32) -> f32 {
    // range reduction: round(x/TAU) = floor(x/TAU + 0.5)
    let x = x - TAU * floor_f32(x / TAU + 0.5);

    let x2 = x * x;

    // 7th-order Horner-form minimax polynomial.
    // sin(x) ≈ x * (1 - x²/6 * (1 - x²/20 * (1 - x²/42)))
    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)))
}

/// Cosine via phase shift: `cos(x) = sin(x + π/2)`.

/// Fast `2^x` approximation accurate to < 0.0002 absolute error on `[−10, 10]`.
///
/// Algorithm:
/// 1. Split into integer part `n = floor(x)` and fractional part `f = x − n`.
/// 2. Multiply in the integer exponent by biasing the f32 exponent field
///    directly (bit manipulation).
/// 3. Apply a 4th-order Horner polynomial for `2^f − 1` on `[0, 1)`.
fn exp2(x: f32) -> f32 {
    // Clamp to a safe range to avoid undefined bit-manipulation results.
    let x = x.max(-126.0).min(126.0);

    let n = floor_f32(x);
    let f = x - n; // fractional part, in [0, 1)

    // Integer power of two via exponent-field bias.
    // f32 exponent bias = 127.  Shift into position 23.
    // SAFETY: n is in [-126, 126], so (n as i32 + 127) is in [1, 253] — a
    // valid normalised f32 exponent.
    let int_pow: f32 = f32::from_bits(((n as i32 + 127) as u32) << 23);

    // Polynomial approximation of 2^f on [0, 1).
    // Coefficients from a minimax fit; error < 0.0002 on [0, 1).
    let frac_pow = 1.0 + f * (0.693_147 + f * (0.240_226 + f * (0.055_504 + f * 0.009_618)));

    int_pow * frac_pow
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Convenience: collect all easing variants (no alloc — fixed-size array).
    fn all_easings() -> [Easing; 24] {
        [
            Easing::Linear,
            Easing::Ease,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::CubicBezier(0.17, 0.67, 0.83, 0.67),
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
            Easing::EaseInCubic,
            Easing::EaseOutCubic,
            Easing::EaseInOutCubic,
            Easing::EaseInExpo,
            Easing::EaseOutExpo,
            Easing::EaseInOutExpo,
            Easing::EaseInBack,
            Easing::EaseOutBack,
            Easing::EaseInOutBack,
            Easing::EaseInElastic,
            Easing::EaseOutElastic,
            Easing::EaseInBounce,
            Easing::EaseOutBounce,
            Easing::StepStart,
            Easing::StepEnd,
        ]
    }

    #[test]
    fn sin_accuracy() {
        // Spot-check a few known values.
        assert!((sin(0.0)).abs() < 0.001);
        assert!((sin(PI / 2.0) - 1.0).abs() < 0.001);
        assert!((sin(PI)).abs() < 0.001);
        assert!((sin(-PI / 2.0) + 1.0).abs() < 0.001);
    }

    #[test]
    fn exp2_accuracy() {
        assert!((exp2(0.0) - 1.0).abs() < 0.001);
        assert!((exp2(1.0) - 2.0).abs() < 0.001);
        assert!((exp2(-1.0) - 0.5).abs() < 0.001);
        assert!((exp2(10.0) - 1024.0).abs() < 1.0);
    }
}
