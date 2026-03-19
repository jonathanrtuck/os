//! 2D affine transforms and supporting math helpers for `no_std`.

// ── Affine transform ────────────────────────────────────────────────

/// 2D affine transform stored as a 3×3 matrix with the bottom row
/// implicitly `[0, 0, 1]`.
///
/// ```text
/// ┌         ┐
/// │ a  c  tx│
/// │ b  d  ty│
/// │ 0  0   1│
/// └         ┘
/// ```
///
/// Standard 2D affine convention: `a`, `b` are the first column,
/// `c`, `d` are the second column, `tx`, `ty` are the translation.
///
/// Transforming a point `(x, y)`:
///   `x' = a*x + c*y + tx`
///   `y' = b*x + d*y + ty`
///
/// Identity by default (a=1, d=1, all others 0).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct AffineTransform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

// Compile-time size assertion: 6 × f32 = 24 bytes.
const _: () = assert!(core::mem::size_of::<AffineTransform>() == 24);

impl AffineTransform {
    /// Identity transform — no translation, rotation, scale, or skew.
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

    /// Pure translation by `(x, y)`.
    pub const fn translate(x: f32, y: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: x,
            ty: y,
        }
    }

    /// Rotation by `radians` counter-clockwise.
    pub fn rotate(radians: f32) -> Self {
        let (sin, cos) = sin_cos_f32(radians);
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Non-uniform scale by `(sx, sy)`.
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Horizontal skew (shear) by `angle` radians.
    /// The x-coordinate is shifted proportional to the y-coordinate:
    /// `x' = x + tan(angle) * y`.
    pub fn skew_x(angle: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: tan_f32(angle),
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Matrix multiplication: `self × other`.
    ///
    /// The resulting transform applies `other` first, then `self`.
    /// Use to compose parent and child transforms:
    /// `world = parent.compose(child)`.
    pub fn compose(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            tx: self.a * other.tx + self.c * other.ty + self.tx,
            ty: self.b * other.tx + self.d * other.ty + self.ty,
        }
    }

    /// Compute the inverse of this affine transform.
    /// Returns `None` if the matrix is singular (determinant ≈ 0).
    pub fn inverse(&self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if det > -1e-10 && det < 1e-10 {
            return None; // Singular matrix.
        }
        let inv_det = 1.0 / det;
        Some(Self {
            a: self.d * inv_det,
            b: -self.b * inv_det,
            c: -self.c * inv_det,
            d: self.a * inv_det,
            tx: (self.c * self.ty - self.d * self.tx) * inv_det,
            ty: (self.b * self.tx - self.a * self.ty) * inv_det,
        })
    }

    /// Returns `true` if this is the identity transform.
    pub fn is_identity(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.tx == 0.0
            && self.ty == 0.0
    }

    /// Returns true if this is a pure integer translation (no rotation/scale/skew).
    /// Note: for |tx| or |ty| > 2^31, the f32→i32 cast saturates, which may
    /// produce incorrect results. Not reachable in practice (scene coords are i16).
    pub fn is_integer_translation(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.tx == (self.tx as i32) as f32
            && self.ty == (self.ty as i32) as f32
    }

    /// Transform a point `(x, y)` by this matrix.
    pub fn transform_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }

    /// Compute the axis-aligned bounding box (AABB) of a rectangle
    /// `(x, y, w, h)` after applying this transform.
    ///
    /// Returns `(min_x, min_y, width, height)` of the bounding box.
    pub fn transform_aabb(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
        // Transform all four corners of the input rectangle.
        let (x0, y0) = self.transform_point(x, y);
        let (x1, y1) = self.transform_point(x + w, y);
        let (x2, y2) = self.transform_point(x + w, y + h);
        let (x3, y3) = self.transform_point(x, y + h);

        let min_x = min4(x0, x1, x2, x3);
        let min_y = min4(y0, y1, y2, y3);
        let max_x = max4(x0, x1, x2, x3);
        let max_y = max4(y0, y1, y2, y3);

        (min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

// ── Math helpers ────────────────────────────────────────────────────

/// Minimum of four f32 values.
fn min4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let ab = if a < b { a } else { b };
    let cd = if c < d { c } else { d };
    if ab < cd {
        ab
    } else {
        cd
    }
}

/// Maximum of four f32 values.
fn max4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let ab = if a > b { a } else { b };
    let cd = if c > d { c } else { d };
    if ab > cd {
        ab
    } else {
        cd
    }
}

/// Sine and cosine for `no_std`. Uses Taylor series with Payne-Hanek-style
/// range reduction to `[-π/2, π/2]` for accuracy across the full circle.
fn sin_cos_f32(x: f32) -> (f32, f32) {
    // Range-reduce to [-π, π].
    let pi = core::f32::consts::PI;
    let two_pi = 2.0 * pi;
    let half_pi = core::f32::consts::FRAC_PI_2;
    let mut a = x;
    if a > pi || a < -pi {
        a = a - ((a / two_pi).floor_f32() * two_pi);
        if a > pi {
            a -= two_pi;
        }
    }

    // Further reduce to [-π/2, π/2] using symmetry.
    let (sin_sign, cos_sign, reduced) = if a > half_pi {
        // sin(π - a) = sin(a), cos(π - a) = -cos(a)
        (1.0_f32, -1.0_f32, pi - a)
    } else if a < -half_pi {
        // sin(-π - a) = -sin(-a) = sin(a), cos(-π - a) = -cos(a)
        (1.0_f32, -1.0_f32, -pi - a)
    } else {
        (1.0_f32, 1.0_f32, a)
    };

    // Taylor series around 0, valid for [-π/2, π/2] with excellent accuracy.
    let a2 = reduced * reduced;

    // sin: x - x³/6 + x⁵/120 - x⁷/5040 + x⁹/362880 - x¹¹/39916800
    let sin_val = reduced
        * (1.0
            - a2 / 6.0
                * (1.0 - a2 / 20.0 * (1.0 - a2 / 42.0 * (1.0 - a2 / 72.0 * (1.0 - a2 / 110.0)))));

    // cos: 1 - x²/2 + x⁴/24 - x⁶/720 + x⁸/40320 - x¹⁰/3628800
    let cos_val = 1.0
        - a2 / 2.0 * (1.0 - a2 / 12.0 * (1.0 - a2 / 30.0 * (1.0 - a2 / 56.0 * (1.0 - a2 / 90.0))));

    (sin_sign * sin_val, cos_sign * cos_val)
}

/// Tangent for `no_std`. Computed as `sin / cos`.
fn tan_f32(x: f32) -> f32 {
    let (sin, cos) = sin_cos_f32(x);
    if cos.abs_f32() < 1e-10 {
        if sin >= 0.0 {
            f32::MAX
        } else {
            f32::MIN
        }
    } else {
        sin / cos
    }
}

/// Helper trait for f32 methods missing in `no_std`.
trait F32Ext {
    fn floor_f32(self) -> f32;
    fn abs_f32(self) -> f32;
}

impl F32Ext for f32 {
    #[inline]
    fn floor_f32(self) -> f32 {
        let i = self as i32;
        let f = i as f32;
        if self < f {
            f - 1.0
        } else {
            f
        }
    }

    #[inline]
    fn abs_f32(self) -> f32 {
        if self < 0.0 {
            -self
        } else {
            self
        }
    }
}
