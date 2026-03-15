// Coordinate scaling helpers (integer only).
//
// Convert font units to pixel coordinates. Used by GlyphCache::populate
// in lib.rs for computing ascent, descent, and line-gap pixel values.

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
