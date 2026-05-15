//! Coordinate utilities for scene graph rendering.
//!
//! Pure math functions for scaling point coordinates to physical pixels.
//! No dependencies beyond `core`.

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Re-exported from `drawing` where the canonical implementation lives.
#[inline]
pub fn round_f32(x: f32) -> i32 {
    drawing::round_f32(x)
}

/// Scale a millipoint coordinate to physical pixels using fractional scale.
///
/// Input is in millipoints (1/1024 pt). Divides by 1024 to get points,
/// then multiplies by scale to get physical pixels. Uses rounding to
/// nearest pixel. For integer scale factors (1.0, 2.0) and whole-point
/// millipoint values, the result is identical to the old integer multiply.
#[inline]
pub fn scale_coord(mpt: i32, scale: f32) -> i32 {
    round_f32(mpt as f32 / 1024.0 * scale)
}

/// Compute the physical pixel size for a millipoint-based extent starting at
/// a given millipoint position, using the gap-free rounding scheme.
///
/// Physical size = round((pos + size) / 1024 * scale) - round(pos / 1024 * scale)
///
/// This guarantees that two adjacent nodes at (x, w) and (x+w, w2) share
/// the same physical boundary — no gaps and no overlaps.
#[inline]
pub fn scale_size(mpt_pos: i32, mpt_size: i32, scale: f32) -> i32 {
    let phys_start = round_f32(mpt_pos as f32 / 1024.0 * scale);
    let phys_end = round_f32((mpt_pos + mpt_size) as f32 / 1024.0 * scale);

    phys_end - phys_start
}

/// Snap a point-based border width to a whole number of physical pixels.
/// Borders must always be at least 1 physical pixel when the point-based
/// width is > 0. Uses round-to-nearest, with a floor of 1.
#[inline]
pub fn snap_border(pt_width: u32, scale: f32) -> u32 {
    if pt_width == 0 {
        return 0;
    }

    let phys = round_f32(pt_width as f32 * scale);

    if phys <= 0 {
        1
    } else {
        phys as u32
    }
}
