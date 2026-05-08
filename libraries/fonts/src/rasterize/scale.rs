//! Coordinate scaling helpers (integer only).
//!
//! Converts font units to pixel-space values using integer arithmetic.
//! Used by the scanline rasterizer and gvar modules.

/// Fixed-point 20.12 format for sub-pixel precision.
pub(crate) const FP_SHIFT: i32 = 12;
pub(crate) const FP_ONE: i32 = 1 << FP_SHIFT;

/// Scale a font-unit value to pixels (truncating toward zero).
pub(crate) fn scale_fu(val: i32, size_px: u32, upem: u16) -> i32 {
    ((val as i64 * size_px as i64) / upem as i64) as i32
}

/// Scale a font-unit value to pixels (ceiling for positive values).
pub(crate) fn scale_fu_ceil(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;
    if n > 0 {
        ((n + d - 1) / d) as i32
    } else {
        (n / d) as i32
    }
}

/// Scale a font-unit value to pixels (floor for negative values).
pub(crate) fn scale_fu_floor(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;
    if n < 0 {
        ((n - d + 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
