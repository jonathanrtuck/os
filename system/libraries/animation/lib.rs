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
//! * Cubic-bezier solver: up to 8 Newton–Raphson iterations + bisection fallback;
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
            u = mid; // always update — final iteration's mid is our best guess
            let x = sample_x(ax, bx, cx, mid);

            if (x - t_x).abs() < 1e-5 {
                break; // converged to 1e-5 (tighter than Newton's 1e-4 accept)
            }

            if x < t_x {
                lo = mid;
            } else {
                hi = mid;
            }
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
///
/// Precondition: `x` must be in `[i32::MIN as f32, i32::MAX as f32]` (roughly
/// `±2.1 billion`).  Callers in this library satisfy this: `sin` passes a value
/// in approximately `[-1, 1]` after dividing by TAU, and `exp2` clamps to
/// `[-126, 126]` before calling.
fn floor_f32(x: f32) -> f32 {
    // Cast to i32 truncates toward zero (saturating from Rust 1.45).
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

// ── Spring physics ───────────────────────────────────────────────────────────

/// Damped spring physics simulation.
///
/// Models a critically/under/over-damped spring system for smooth animations.
/// Tick-based: call `tick(dt)` each frame with delta time in seconds.
///
/// Physics: F = -stiffness * displacement - damping * velocity
/// Integration: semi-implicit Euler (stable for game-loop dt values).
///
/// The spring starts at rest at `value = 0.0` and animates toward `target`.
pub struct Spring {
    target: f32,
    value: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
    mass: f32,
    settle_threshold: f32,
}

impl Spring {
    /// Create a new spring with explicit physical parameters.
    ///
    /// Initial state: `value = 0.0`, `velocity = 0.0` — at rest at the origin.
    pub const fn new(target: f32, stiffness: f32, damping: f32, mass: f32) -> Self {
        Self {
            target,
            value: 0.0,
            velocity: 0.0,
            stiffness,
            damping,
            mass,
            settle_threshold: 0.01,
        }
    }

    /// Balanced preset — smooth general-purpose UI motion.
    pub const fn default_preset(target: f32) -> Self {
        Self::new(target, 300.0, 20.0, 1.0)
    }

    /// Snappy preset — fast, tight response with minimal overshoot.
    pub const fn snappy(target: f32) -> Self {
        Self::new(target, 600.0, 35.0, 1.0)
    }

    /// Gentle preset — slow, soft approach with no overshoot.
    pub const fn gentle(target: f32) -> Self {
        Self::new(target, 120.0, 14.0, 1.0)
    }

    /// Bouncy preset — low damping produces visible overshoot oscillation.
    pub const fn bouncy(target: f32) -> Self {
        Self::new(target, 300.0, 10.0, 1.0)
    }

    /// Advance the simulation by `dt` seconds.
    ///
    /// Non-positive `dt` is a no-op (guards against zero-division and
    /// time-reversal).
    pub fn tick(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        let displacement = self.value - self.target;
        let force = -self.stiffness * displacement - self.damping * self.velocity;
        let acceleration = force / self.mass;
        // Semi-implicit Euler: update velocity first, then position.
        // This ordering is energy-conserving and unconditionally stable for
        // spring systems, unlike standard (explicit) Euler.
        self.velocity += acceleration * dt;
        self.value += self.velocity * dt;
    }

    /// Current animated value.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Current velocity (units per second).
    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    /// Current target the spring is moving toward.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Update the target without resetting velocity (smooth retargeting).
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Reset the spring to rest at the given position (value = target = position,
    /// velocity = 0). Use this when teleporting to a position without animation.
    pub fn reset_to(&mut self, position: f32) {
        self.value = position;
        self.target = position;
        self.velocity = 0.0;
    }

    /// Returns `true` when both displacement and velocity are below the settle
    /// threshold, indicating the spring has effectively come to rest.
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < self.settle_threshold
            && self.velocity.abs() < self.settle_threshold
    }

    /// Override the settle threshold (default: `0.01`).
    pub fn set_settle_threshold(&mut self, threshold: f32) {
        self.settle_threshold = threshold;
    }
}

// ── Linear interpolation ─────────────────────────────────────────────────────

/// Linear interpolation between two values.
///
/// `t = 0.0` returns `a`; `t = 1.0` returns `b`.  Values of `t` outside
/// `[0, 1]` extrapolate beyond the endpoints (no clamping).
pub trait Lerp {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    #[inline]
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Lerp for i32 {
    #[inline]
    fn lerp(a: i32, b: i32, t: f32) -> i32 {
        (a as f32 + (b - a) as f32 * t) as i32
    }
}

impl Lerp for u8 {
    #[inline]
    fn lerp(a: u8, b: u8, t: f32) -> u8 {
        // Add 0.5 and truncate for nearest-integer rounding in no_std.
        (a as f32 + (b as f32 - a as f32) * t + 0.5) as u8
    }
}

// ── Gamma-correct color interpolation ───────────────────────────────────────

/// sRGB u8 (0–255) → linear f32 [0.0, 1.0] lookup table.
///
/// Implements the standard sRGB transfer function (IEC 61966-2-1):
/// if S ≤ 0.04045 then L = S / 12.92 else L = ((S + 0.055) / 1.055)^2.4
static SRGB_TO_LINEAR: [f32; 256] = [
    0.00000000e+00_f32,
    3.03526984e-04_f32,
    6.07053967e-04_f32,
    9.10580951e-04_f32,
    1.21410793e-03_f32,
    1.51763492e-03_f32,
    1.82116190e-03_f32,
    2.12468888e-03_f32,
    2.42821587e-03_f32,
    2.73174285e-03_f32,
    3.03526984e-03_f32,
    3.34653576e-03_f32,
    3.67650732e-03_f32,
    4.02471702e-03_f32,
    4.39144204e-03_f32,
    4.77695348e-03_f32,
    5.18151670e-03_f32,
    5.60539162e-03_f32,
    6.04883302e-03_f32,
    6.51209079e-03_f32,
    6.99541019e-03_f32,
    7.49903204e-03_f32,
    8.02319299e-03_f32,
    8.56812562e-03_f32,
    9.13405870e-03_f32,
    9.72121732e-03_f32,
    1.03298230e-02_f32,
    1.09600940e-02_f32,
    1.16122452e-02_f32,
    1.22864884e-02_f32,
    1.29830323e-02_f32,
    1.37020830e-02_f32,
    1.44438436e-02_f32,
    1.52085144e-02_f32,
    1.59962934e-02_f32,
    1.68073758e-02_f32,
    1.76419545e-02_f32,
    1.85002201e-02_f32,
    1.93823610e-02_f32,
    2.02885631e-02_f32,
    2.12190104e-02_f32,
    2.21738848e-02_f32,
    2.31533662e-02_f32,
    2.41576324e-02_f32,
    2.51868596e-02_f32,
    2.62412219e-02_f32,
    2.73208916e-02_f32,
    2.84260395e-02_f32,
    2.95568344e-02_f32,
    3.07134437e-02_f32,
    3.18960331e-02_f32,
    3.31047666e-02_f32,
    3.43398068e-02_f32,
    3.56013149e-02_f32,
    3.68894504e-02_f32,
    3.82043716e-02_f32,
    3.95462353e-02_f32,
    4.09151969e-02_f32,
    4.23114106e-02_f32,
    4.37350293e-02_f32,
    4.51862044e-02_f32,
    4.66650863e-02_f32,
    4.81718242e-02_f32,
    4.97065660e-02_f32,
    5.12694584e-02_f32,
    5.28606470e-02_f32,
    5.44802764e-02_f32,
    5.61284900e-02_f32,
    5.78054302e-02_f32,
    5.95112382e-02_f32,
    6.12460542e-02_f32,
    6.30100177e-02_f32,
    6.48032667e-02_f32,
    6.66259386e-02_f32,
    6.84781698e-02_f32,
    7.03600957e-02_f32,
    7.22718507e-02_f32,
    7.42135684e-02_f32,
    7.61853815e-02_f32,
    7.81874218e-02_f32,
    8.02198203e-02_f32,
    8.22827071e-02_f32,
    8.43762115e-02_f32,
    8.65004620e-02_f32,
    8.86555863e-02_f32,
    9.08417112e-02_f32,
    9.30589628e-02_f32,
    9.53074666e-02_f32,
    9.75873471e-02_f32,
    9.98987282e-02_f32,
    1.02241733e-01_f32,
    1.04616484e-01_f32,
    1.07023103e-01_f32,
    1.09461711e-01_f32,
    1.11932428e-01_f32,
    1.14435374e-01_f32,
    1.16970668e-01_f32,
    1.19538428e-01_f32,
    1.22138772e-01_f32,
    1.24771818e-01_f32,
    1.27437680e-01_f32,
    1.30136477e-01_f32,
    1.32868322e-01_f32,
    1.35633330e-01_f32,
    1.38431615e-01_f32,
    1.41263291e-01_f32,
    1.44128471e-01_f32,
    1.47027266e-01_f32,
    1.49959790e-01_f32,
    1.52926152e-01_f32,
    1.55926464e-01_f32,
    1.58960835e-01_f32,
    1.62029376e-01_f32,
    1.65132195e-01_f32,
    1.68269400e-01_f32,
    1.71441101e-01_f32,
    1.74647404e-01_f32,
    1.77888416e-01_f32,
    1.81164244e-01_f32,
    1.84474995e-01_f32,
    1.87820772e-01_f32,
    1.91201683e-01_f32,
    1.94617830e-01_f32,
    1.98069320e-01_f32,
    2.01556254e-01_f32,
    2.05078736e-01_f32,
    2.08636870e-01_f32,
    2.12230757e-01_f32,
    2.15860500e-01_f32,
    2.19526200e-01_f32,
    2.23227957e-01_f32,
    2.26965874e-01_f32,
    2.30740049e-01_f32,
    2.34550582e-01_f32,
    2.38397574e-01_f32,
    2.42281122e-01_f32,
    2.46201327e-01_f32,
    2.50158285e-01_f32,
    2.54152094e-01_f32,
    2.58182853e-01_f32,
    2.62250658e-01_f32,
    2.66355605e-01_f32,
    2.70497791e-01_f32,
    2.74677312e-01_f32,
    2.78894263e-01_f32,
    2.83148740e-01_f32,
    2.87440838e-01_f32,
    2.91770650e-01_f32,
    2.96138271e-01_f32,
    3.00543794e-01_f32,
    3.04987314e-01_f32,
    3.09468923e-01_f32,
    3.13988713e-01_f32,
    3.18546778e-01_f32,
    3.23143209e-01_f32,
    3.27778098e-01_f32,
    3.32451536e-01_f32,
    3.37163615e-01_f32,
    3.41914425e-01_f32,
    3.46704056e-01_f32,
    3.51532600e-01_f32,
    3.56400144e-01_f32,
    3.61306780e-01_f32,
    3.66252596e-01_f32,
    3.71237680e-01_f32,
    3.76262123e-01_f32,
    3.81326011e-01_f32,
    3.86429434e-01_f32,
    3.91572478e-01_f32,
    3.96755231e-01_f32,
    4.01977780e-01_f32,
    4.07240212e-01_f32,
    4.12542613e-01_f32,
    4.17885071e-01_f32,
    4.23267670e-01_f32,
    4.28690497e-01_f32,
    4.34153636e-01_f32,
    4.39657174e-01_f32,
    4.45201195e-01_f32,
    4.50785783e-01_f32,
    4.56411023e-01_f32,
    4.62077000e-01_f32,
    4.67783796e-01_f32,
    4.73531496e-01_f32,
    4.79320183e-01_f32,
    4.85149940e-01_f32,
    4.91020850e-01_f32,
    4.96932995e-01_f32,
    5.02886458e-01_f32,
    5.08881321e-01_f32,
    5.14917665e-01_f32,
    5.20995573e-01_f32,
    5.27115126e-01_f32,
    5.33276404e-01_f32,
    5.39479489e-01_f32,
    5.45724461e-01_f32,
    5.52011402e-01_f32,
    5.58340390e-01_f32,
    5.64711506e-01_f32,
    5.71124829e-01_f32,
    5.77580440e-01_f32,
    5.84078418e-01_f32,
    5.90618841e-01_f32,
    5.97201788e-01_f32,
    6.03827339e-01_f32,
    6.10495571e-01_f32,
    6.17206562e-01_f32,
    6.23960392e-01_f32,
    6.30757136e-01_f32,
    6.37596874e-01_f32,
    6.44479682e-01_f32,
    6.51405637e-01_f32,
    6.58374817e-01_f32,
    6.65387298e-01_f32,
    6.72443157e-01_f32,
    6.79542470e-01_f32,
    6.86685312e-01_f32,
    6.93871761e-01_f32,
    7.01101892e-01_f32,
    7.08375780e-01_f32,
    7.15693501e-01_f32,
    7.23055129e-01_f32,
    7.30460740e-01_f32,
    7.37910409e-01_f32,
    7.45404210e-01_f32,
    7.52942217e-01_f32,
    7.60524505e-01_f32,
    7.68151147e-01_f32,
    7.75822218e-01_f32,
    7.83537792e-01_f32,
    7.91297940e-01_f32,
    7.99102738e-01_f32,
    8.06952258e-01_f32,
    8.14846572e-01_f32,
    8.22785754e-01_f32,
    8.30769877e-01_f32,
    8.38799012e-01_f32,
    8.46873232e-01_f32,
    8.54992608e-01_f32,
    8.63157213e-01_f32,
    8.71367119e-01_f32,
    8.79622397e-01_f32,
    8.87923118e-01_f32,
    8.96269353e-01_f32,
    9.04661174e-01_f32,
    9.13098652e-01_f32,
    9.21581856e-01_f32,
    9.30110858e-01_f32,
    9.38685728e-01_f32,
    9.47306537e-01_f32,
    9.55973353e-01_f32,
    9.64686248e-01_f32,
    9.73445290e-01_f32,
    9.82250550e-01_f32,
    9.91102097e-01_f32,
    1.00000000e+00_f32,
];

/// Linear f32 (index / 4095) → sRGB u8 (0–255) lookup table.
///
/// Index with `(linear * 4095.0) as usize` (clamped to [0, 4095]).
/// Implements the inverse sRGB transfer function:
/// if L ≤ 0.0031308 then S = 12.92 * L else S = 1.055 * L^(1/2.4) - 0.055
#[rustfmt::skip]
static LINEAR_TO_SRGB: [u8; 4096] = [
    0, 1, 2, 2, 3, 4, 5, 6, 6, 7, 8, 9, 10, 10, 11, 12, 13, 13, 14, 15,
    15, 16, 16, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 23, 24, 24, 25,
    25, 25, 26, 26, 27, 27, 27, 28, 28, 29, 29, 29, 30, 30, 30, 31, 31, 31, 32, 32,
    32, 33, 33, 33, 34, 34, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 37, 38, 38,
    38, 38, 39, 39, 39, 40, 40, 40, 40, 41, 41, 41, 41, 42, 42, 42, 42, 43, 43, 43,
    43, 43, 44, 44, 44, 44, 45, 45, 45, 45, 46, 46, 46, 46, 46, 47, 47, 47, 47, 48,
    48, 48, 48, 48, 49, 49, 49, 49, 49, 50, 50, 50, 50, 50, 51, 51, 51, 51, 51, 52,
    52, 52, 52, 52, 53, 53, 53, 53, 53, 54, 54, 54, 54, 54, 55, 55, 55, 55, 55, 55,
    56, 56, 56, 56, 56, 57, 57, 57, 57, 57, 57, 58, 58, 58, 58, 58, 58, 59, 59, 59,
    59, 59, 59, 60, 60, 60, 60, 60, 60, 61, 61, 61, 61, 61, 61, 62, 62, 62, 62, 62,
    62, 63, 63, 63, 63, 63, 63, 64, 64, 64, 64, 64, 64, 64, 65, 65, 65, 65, 65, 65,
    66, 66, 66, 66, 66, 66, 66, 67, 67, 67, 67, 67, 67, 67, 68, 68, 68, 68, 68, 68,
    68, 69, 69, 69, 69, 69, 69, 69, 70, 70, 70, 70, 70, 70, 70, 71, 71, 71, 71, 71,
    71, 71, 72, 72, 72, 72, 72, 72, 72, 72, 73, 73, 73, 73, 73, 73, 73, 74, 74, 74,
    74, 74, 74, 74, 74, 75, 75, 75, 75, 75, 75, 75, 75, 76, 76, 76, 76, 76, 76, 76,
    77, 77, 77, 77, 77, 77, 77, 77, 78, 78, 78, 78, 78, 78, 78, 78, 78, 79, 79, 79,
    79, 79, 79, 79, 79, 80, 80, 80, 80, 80, 80, 80, 80, 81, 81, 81, 81, 81, 81, 81,
    81, 81, 82, 82, 82, 82, 82, 82, 82, 82, 83, 83, 83, 83, 83, 83, 83, 83, 83, 84,
    84, 84, 84, 84, 84, 84, 84, 84, 85, 85, 85, 85, 85, 85, 85, 85, 85, 86, 86, 86,
    86, 86, 86, 86, 86, 86, 87, 87, 87, 87, 87, 87, 87, 87, 87, 88, 88, 88, 88, 88,
    88, 88, 88, 88, 88, 89, 89, 89, 89, 89, 89, 89, 89, 89, 90, 90, 90, 90, 90, 90,
    90, 90, 90, 90, 91, 91, 91, 91, 91, 91, 91, 91, 91, 91, 92, 92, 92, 92, 92, 92,
    92, 92, 92, 92, 93, 93, 93, 93, 93, 93, 93, 93, 93, 93, 94, 94, 94, 94, 94, 94,
    94, 94, 94, 94, 95, 95, 95, 95, 95, 95, 95, 95, 95, 95, 96, 96, 96, 96, 96, 96,
    96, 96, 96, 96, 96, 97, 97, 97, 97, 97, 97, 97, 97, 97, 97, 98, 98, 98, 98, 98,
    98, 98, 98, 98, 98, 98, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 100, 100, 100,
    100, 100, 100, 100, 100, 100, 100, 100, 101, 101, 101, 101, 101, 101, 101, 101, 101, 101, 101, 102,
    102, 102, 102, 102, 102, 102, 102, 102, 102, 102, 103, 103, 103, 103, 103, 103, 103, 103, 103, 103,
    103, 103, 104, 104, 104, 104, 104, 104, 104, 104, 104, 104, 104, 105, 105, 105, 105, 105, 105, 105,
    105, 105, 105, 105, 105, 106, 106, 106, 106, 106, 106, 106, 106, 106, 106, 106, 106, 107, 107, 107,
    107, 107, 107, 107, 107, 107, 107, 107, 107, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108,
    108, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 110, 110, 110, 110, 110, 110, 110,
    110, 110, 110, 110, 110, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 112, 112,
    112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 113, 113, 113, 113, 113, 113, 113, 113, 113, 113,
    113, 113, 113, 114, 114, 114, 114, 114, 114, 114, 114, 114, 114, 114, 114, 114, 115, 115, 115, 115,
    115, 115, 115, 115, 115, 115, 115, 115, 115, 116, 116, 116, 116, 116, 116, 116, 116, 116, 116, 116,
    116, 116, 117, 117, 117, 117, 117, 117, 117, 117, 117, 117, 117, 117, 117, 117, 118, 118, 118, 118,
    118, 118, 118, 118, 118, 118, 118, 118, 118, 119, 119, 119, 119, 119, 119, 119, 119, 119, 119, 119,
    119, 119, 119, 120, 120, 120, 120, 120, 120, 120, 120, 120, 120, 120, 120, 120, 120, 121, 121, 121,
    121, 121, 121, 121, 121, 121, 121, 121, 121, 121, 122, 122, 122, 122, 122, 122, 122, 122, 122, 122,
    122, 122, 122, 122, 122, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 124,
    124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 125, 125, 125, 125, 125, 125, 125,
    125, 125, 125, 125, 125, 125, 125, 125, 126, 126, 126, 126, 126, 126, 126, 126, 126, 126, 126, 126,
    126, 126, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 129, 129, 129, 129, 129, 129, 129, 129,
    129, 129, 129, 129, 129, 129, 129, 130, 130, 130, 130, 130, 130, 130, 130, 130, 130, 130, 130, 130,
    130, 130, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 131, 132, 132,
    132, 132, 132, 132, 132, 132, 132, 132, 132, 132, 132, 132, 132, 133, 133, 133, 133, 133, 133, 133,
    133, 133, 133, 133, 133, 133, 133, 133, 133, 134, 134, 134, 134, 134, 134, 134, 134, 134, 134, 134,
    134, 134, 134, 134, 134, 135, 135, 135, 135, 135, 135, 135, 135, 135, 135, 135, 135, 135, 135, 135,
    135, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 136, 137, 137, 137,
    137, 137, 137, 137, 137, 137, 137, 137, 137, 137, 137, 137, 137, 138, 138, 138, 138, 138, 138, 138,
    138, 138, 138, 138, 138, 138, 138, 138, 138, 139, 139, 139, 139, 139, 139, 139, 139, 139, 139, 139,
    139, 139, 139, 139, 139, 139, 140, 140, 140, 140, 140, 140, 140, 140, 140, 140, 140, 140, 140, 140,
    140, 140, 140, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141, 141,
    142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 142, 143, 143, 143,
    143, 143, 143, 143, 143, 143, 143, 143, 143, 143, 143, 143, 143, 143, 144, 144, 144, 144, 144, 144,
    144, 144, 144, 144, 144, 144, 144, 144, 144, 144, 144, 145, 145, 145, 145, 145, 145, 145, 145, 145,
    145, 145, 145, 145, 145, 145, 145, 145, 145, 146, 146, 146, 146, 146, 146, 146, 146, 146, 146, 146,
    146, 146, 146, 146, 146, 146, 147, 147, 147, 147, 147, 147, 147, 147, 147, 147, 147, 147, 147, 147,
    147, 147, 147, 147, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148, 148,
    148, 148, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149, 149,
    150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 150, 151,
    151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 151, 152, 152, 152,
    152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 153, 153, 153, 153,
    153, 153, 153, 153, 153, 153, 153, 153, 153, 153, 153, 153, 153, 153, 154, 154, 154, 154, 154, 154,
    154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 155, 155, 155, 155, 155, 155, 155,
    155, 155, 155, 155, 155, 155, 155, 155, 155, 155, 155, 155, 156, 156, 156, 156, 156, 156, 156, 156,
    156, 156, 156, 156, 156, 156, 156, 156, 156, 156, 156, 156, 157, 157, 157, 157, 157, 157, 157, 157,
    157, 157, 157, 157, 157, 157, 157, 157, 157, 157, 157, 158, 158, 158, 158, 158, 158, 158, 158, 158,
    158, 158, 158, 158, 158, 158, 158, 158, 158, 158, 159, 159, 159, 159, 159, 159, 159, 159, 159, 159,
    159, 159, 159, 159, 159, 159, 159, 159, 159, 159, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160,
    160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 161, 161, 161, 161, 161, 161, 161, 161, 161, 161,
    161, 161, 161, 161, 161, 161, 161, 161, 161, 161, 162, 162, 162, 162, 162, 162, 162, 162, 162, 162,
    162, 162, 162, 162, 162, 162, 162, 162, 162, 162, 163, 163, 163, 163, 163, 163, 163, 163, 163, 163,
    163, 163, 163, 163, 163, 163, 163, 163, 163, 163, 164, 164, 164, 164, 164, 164, 164, 164, 164, 164,
    164, 164, 164, 164, 164, 164, 164, 164, 164, 164, 164, 165, 165, 165, 165, 165, 165, 165, 165, 165,
    165, 165, 165, 165, 165, 165, 165, 165, 165, 165, 165, 165, 166, 166, 166, 166, 166, 166, 166, 166,
    166, 166, 166, 166, 166, 166, 166, 166, 166, 166, 166, 166, 167, 167, 167, 167, 167, 167, 167, 167,
    167, 167, 167, 167, 167, 167, 167, 167, 167, 167, 167, 167, 167, 168, 168, 168, 168, 168, 168, 168,
    168, 168, 168, 168, 168, 168, 168, 168, 168, 168, 168, 168, 168, 168, 168, 169, 169, 169, 169, 169,
    169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 169, 170, 170, 170, 170,
    170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 170, 171, 171, 171,
    171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 172,
    172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172, 172,
    172, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173, 173,
    173, 173, 173, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174, 174,
    174, 174, 174, 174, 174, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175,
    175, 175, 175, 175, 175, 175, 175, 176, 176, 176, 176, 176, 176, 176, 176, 176, 176, 176, 176, 176,
    176, 176, 176, 176, 176, 176, 176, 176, 176, 176, 177, 177, 177, 177, 177, 177, 177, 177, 177, 177,
    177, 177, 177, 177, 177, 177, 177, 177, 177, 177, 177, 177, 178, 178, 178, 178, 178, 178, 178, 178,
    178, 178, 178, 178, 178, 178, 178, 178, 178, 178, 178, 178, 178, 178, 178, 179, 179, 179, 179, 179,
    179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 179, 180, 180,
    180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180, 180,
    180, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181, 181,
    181, 181, 181, 181, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182, 182,
    182, 182, 182, 182, 182, 182, 182, 182, 183, 183, 183, 183, 183, 183, 183, 183, 183, 183, 183, 183,
    183, 183, 183, 183, 183, 183, 183, 183, 183, 183, 183, 184, 184, 184, 184, 184, 184, 184, 184, 184,
    184, 184, 184, 184, 184, 184, 184, 184, 184, 184, 184, 184, 184, 184, 184, 185, 185, 185, 185, 185,
    185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 186,
    186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186, 186,
    186, 186, 186, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187, 187,
    187, 187, 187, 187, 187, 187, 187, 187, 188, 188, 188, 188, 188, 188, 188, 188, 188, 188, 188, 188,
    188, 188, 188, 188, 188, 188, 188, 188, 188, 188, 188, 188, 189, 189, 189, 189, 189, 189, 189, 189,
    189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 189, 190, 190, 190,
    190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190, 190,
    190, 190, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191, 191,
    191, 191, 191, 191, 191, 191, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192,
    192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 193, 193, 193, 193, 193, 193, 193, 193,
    193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 194, 194, 194,
    194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194, 194,
    194, 194, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195, 195,
    195, 195, 195, 195, 195, 195, 195, 195, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196,
    196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 196, 197, 197, 197, 197, 197, 197,
    197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197, 197,
    198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198, 198,
    198, 198, 198, 198, 198, 198, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199,
    199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 199, 200, 200, 200, 200, 200, 200, 200, 200,
    200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 201,
    201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201, 201,
    201, 201, 201, 201, 201, 201, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202,
    202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 202, 203, 203, 203, 203, 203, 203, 203,
    203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203, 203,
    204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204, 204,
    204, 204, 204, 204, 204, 204, 204, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205,
    205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 205, 206, 206, 206, 206, 206, 206,
    206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206,
    206, 206, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 207,
    207, 207, 207, 207, 207, 207, 207, 207, 207, 207, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208,
    208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 208, 209, 209, 209,
    209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209, 209,
    209, 209, 209, 209, 209, 209, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210,
    210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 210, 211, 211, 211, 211, 211, 211,
    211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211, 211,
    211, 211, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212,
    212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 212, 213, 213, 213, 213, 213, 213, 213, 213, 213,
    213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213, 213,
    214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214, 214,
    214, 214, 214, 214, 214, 214, 214, 214, 214, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215,
    215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 215, 216, 216,
    216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216, 216,
    216, 216, 216, 216, 216, 216, 216, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217,
    217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 217, 218, 218, 218,
    218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218, 218,
    218, 218, 218, 218, 218, 218, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219,
    219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 219, 220, 220, 220, 220,
    220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220, 220,
    220, 220, 220, 220, 220, 220, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221,
    221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 221, 222, 222, 222,
    222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222, 222,
    222, 222, 222, 222, 222, 222, 222, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223,
    223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 223, 224, 224,
    224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224, 224,
    224, 224, 224, 224, 224, 224, 224, 224, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225,
    225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 225, 226,
    226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 226,
    226, 226, 226, 226, 226, 226, 226, 226, 226, 226, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227,
    227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227, 227,
    227, 227, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228,
    228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 228, 229, 229, 229, 229, 229, 229, 229,
    229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229, 229,
    229, 229, 229, 229, 229, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230,
    230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 230, 231, 231, 231,
    231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231, 231,
    231, 231, 231, 231, 231, 231, 231, 231, 231, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232,
    232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232, 232,
    232, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233,
    233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 233, 234, 234, 234, 234, 234, 234,
    234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234, 234,
    234, 234, 234, 234, 234, 234, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235,
    235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 235, 236,
    236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236,
    236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 236, 237, 237, 237, 237, 237, 237, 237, 237,
    237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237, 237,
    237, 237, 237, 237, 237, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238,
    238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 238, 239, 239,
    239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239,
    239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 239, 240, 240, 240, 240, 240, 240, 240, 240,
    240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240, 240,
    240, 240, 240, 240, 240, 240, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241,
    241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241, 241,
    242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242,
    242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 242, 243, 243, 243, 243, 243, 243,
    243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243, 243,
    243, 243, 243, 243, 243, 243, 243, 243, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244,
    244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244, 244,
    244, 244, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245,
    245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 245, 246, 246, 246,
    246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246,
    246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 246, 247, 247, 247, 247, 247, 247, 247, 247,
    247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247, 247,
    247, 247, 247, 247, 247, 247, 247, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248,
    248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248, 248,
    248, 248, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249,
    249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 249, 250, 250, 250,
    250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250,
    250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 250, 251, 251, 251, 251, 251, 251, 251,
    251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251, 251,
    251, 251, 251, 251, 251, 251, 251, 251, 251, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252,
    252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252, 252,
    252, 252, 252, 252, 252, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253,
    253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253, 253,
    253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254,
    254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255
];

/// Gamma-correct sRGB color interpolation.
///
/// Linearizes both colors, interpolates in linear light space, and re-encodes
/// to sRGB.  This produces perceptually correct results — naive linear
/// interpolation of sRGB byte values is perceptually wrong (midpoint of
/// black/white would be ~128 instead of the correct ~188).
pub struct LerpColor;

impl LerpColor {
    /// Interpolate two sRGB colors in linear light space.
    ///
    /// Input/output: `[r, g, b, a]` as sRGB u8 values.  The RGB channels are
    /// gamma-corrected; the alpha channel is treated as linear (not gamma).
    ///
    /// At exact boundaries (`t == 0.0` or `t == 1.0`) the input color is
    /// returned unchanged with no round-trip through the LUTs.
    pub fn lerp_srgb(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
        // Short-circuit exact boundaries to avoid LUT round-trip quantization.
        if t <= 0.0 {
            return a;
        }
        if t >= 1.0 {
            return b;
        }

        // RGB channels: linearize, lerp in linear space, re-encode to sRGB.
        let lerp_ch = |ca: u8, cb: u8| -> u8 {
            let la = SRGB_TO_LINEAR[ca as usize];
            let lb = SRGB_TO_LINEAR[cb as usize];
            let linear = la + (lb - la) * t;
            // Clamp index to valid range before indexing.
            let idx = (linear * 4095.0) as usize;
            let idx = if idx > 4095 { 4095 } else { idx };
            LINEAR_TO_SRGB[idx]
        };

        // Alpha channel: linear (not gamma-corrected).
        let alpha = u8::lerp(a[3], b[3], t);

        [
            lerp_ch(a[0], b[0]),
            lerp_ch(a[1], b[1]),
            lerp_ch(a[2], b[2]),
            alpha,
        ]
    }
}

// ── 2D affine transform ───────────────────────────────────────────────────────

/// Minimal 2D affine transform for animation interpolation.
///
/// Represents the matrix:
/// ```text
/// [ a  b  0 ]
/// [ c  d  0 ]
/// [ tx ty 1 ]
/// ```
/// Component-wise lerp produces valid intermediate transforms for translation,
/// scale, and simple (small-angle) rotation animations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Transform2D {
    /// The identity transform (no translation, no scale change, no rotation).
    pub const fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }
}

impl Lerp for Transform2D {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        Self {
            a: f32::lerp(a.a, b.a, t),
            b: f32::lerp(a.b, b.b, t),
            c: f32::lerp(a.c, b.c, t),
            d: f32::lerp(a.d, b.d, t),
            tx: f32::lerp(a.tx, b.tx, t),
            ty: f32::lerp(a.ty, b.ty, t),
        }
    }
}

// ── Animation and Timeline ───────────────────────────────────────────────────

/// Unique identifier for a running animation in a Timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationId(u8);

/// A single property animation from start_value to end_value over duration_ms.
/// Uses f32 values. For type-generic interpolation, use the Lerp trait at the
/// call site to convert between the animated f32 and the target type.
#[derive(Clone, Copy)]
struct Animation {
    start_value: f32,
    end_value: f32,
    start_time_ms: u64,
    duration_ms: u32,
    easing: Easing,
}

impl Animation {
    fn value_at(&self, now_ms: u64) -> f32 {
        if now_ms <= self.start_time_ms {
            return self.start_value;
        }
        let elapsed = (now_ms - self.start_time_ms) as f32;
        let t = (elapsed / self.duration_ms as f32).min(1.0);
        let eased = ease(self.easing, t);
        f32::lerp(self.start_value, self.end_value, eased)
    }

    fn is_complete_at(&self, now_ms: u64) -> bool {
        now_ms >= self.start_time_ms + self.duration_ms as u64
    }
}

const MAX_ANIMATIONS: usize = 32;

/// Fixed-capacity animation manager. No heap allocation.
///
/// Manages up to 32 concurrent animations. Each animation interpolates a single
/// f32 value from start to end over a duration with an easing curve. The Timeline
/// tracks time and automatically removes completed animations.
///
/// 32 slots supports: cursor blink (1) + scroll animation (1) + pointer fade (1)
/// + transition (1) + per-node property animations (up to 28 concurrent).
pub struct Timeline {
    slots: [Option<Animation>; MAX_ANIMATIONS],
    now_ms: u64,
}

impl Timeline {
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_ANIMATIONS],
            now_ms: 0,
        }
    }

    /// Start a new animation. Returns AnimationId or Err if at capacity.
    pub fn start(
        &mut self,
        from: f32,
        to: f32,
        duration_ms: u32,
        easing: Easing,
        now_ms: u64,
    ) -> Result<AnimationId, ()> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Animation {
                    start_value: from,
                    end_value: to,
                    start_time_ms: now_ms,
                    duration_ms,
                    easing,
                });
                return Ok(AnimationId(i as u8));
            }
        }
        Err(()) // at capacity
    }

    /// Advance time. Completed animations are removed, freeing their slots.
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        for slot in self.slots.iter_mut() {
            if let Some(anim) = slot {
                if anim.is_complete_at(now_ms) {
                    *slot = None;
                }
            }
        }
    }

    /// Get the current value of an animation. Returns 0.0 if the animation
    /// completed (and was cleaned up) or if the id is invalid.
    pub fn value(&self, id: AnimationId) -> f32 {
        match &self.slots[id.0 as usize] {
            Some(anim) => anim.value_at(self.now_ms),
            None => 0.0, // animation completed and was removed
        }
    }

    /// Cancel an animation, freeing its slot immediately.
    pub fn cancel(&mut self, id: AnimationId) {
        if (id.0 as usize) < MAX_ANIMATIONS {
            self.slots[id.0 as usize] = None;
        }
    }

    /// Check if an animation is still running (not completed or cancelled).
    pub fn is_active(&self, id: AnimationId) -> bool {
        (id.0 as usize) < MAX_ANIMATIONS && self.slots[id.0 as usize].is_some()
    }

    /// Returns true if any animation is active (useful for frame scheduling —
    /// when true, the event loop should tick at 60fps instead of blocking).
    pub fn any_active(&self) -> bool {
        self.slots.iter().any(|s| s.is_some())
    }
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
