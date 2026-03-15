// TrueType glyph outline types and coordinate scaling helpers.
//
// Shared types used by the scanline rasterizer (rasterizer.rs). All custom
// font table parsing has been replaced by read-fonts (via the shaping library).

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
// Coordinate scaling helpers (integer only)
// ---------------------------------------------------------------------------

fn min_usize(a: usize, b: usize) -> usize {
    if a < b {
        a
    } else {
        b
    }
}
/// Scale a value in font units to pixels: `val * size_px / units_per_em`.
fn scale_fu(val: i32, size_px: u32, upem: u16) -> i32 {
    ((val as i64 * size_px as i64) / upem as i64) as i32
}
/// Scale and round toward positive infinity (ceil).
fn scale_fu_ceil(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;

    if n > 0 {
        ((n + d - 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
/// Scale and round toward negative infinity (floor).
fn scale_fu_floor(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;

    if n < 0 {
        ((n - d + 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
