//! Mouse cursor — procedural arrow cursor generation.

use drawing::Color;

/// Width of the procedural arrow cursor in pixels.
pub const CURSOR_W: u32 = 12;
/// Height of the procedural arrow cursor in pixels.
pub const CURSOR_H: u32 = 16;

/// Procedural arrow cursor bitmap: 1 = fill (white), 2 = outline (dark grey),
/// 0 = transparent. 12 wide × 16 tall, stored row-major.
///
/// Shape: classic arrow pointer pointing up-left.
const CURSOR_BITMAP: [u8; (CURSOR_W * CURSOR_H) as usize] = [
    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  0
    2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  1
    2, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  2
    2, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, //  3
    2, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, //  4
    2, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, //  5
    2, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, //  6
    2, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, //  7
    2, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, //  8
    2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, //  9
    2, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 0, // 10
    2, 1, 1, 2, 1, 1, 2, 0, 0, 0, 0, 0, // 11
    2, 1, 2, 0, 2, 1, 1, 2, 0, 0, 0, 0, // 12
    2, 2, 0, 0, 2, 1, 1, 2, 0, 0, 0, 0, // 13
    2, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0, 0, // 14
    0, 0, 0, 0, 0, 2, 2, 2, 0, 0, 0, 0, // 15
];

/// Render the procedural arrow cursor onto a BGRA8888 pixel buffer.
///
/// The buffer must be at least `CURSOR_W * CURSOR_H * 4` bytes.
/// Uses palette colors: CURSOR_FILL (white) for the fill, CURSOR_OUTLINE
/// (dark grey) for the outline, and transparent (alpha 0) elsewhere.
pub fn render_cursor(buf: &mut [u8]) {
    let stride = CURSOR_W * 4;
    let total = (CURSOR_W * CURSOR_H * 4) as usize;

    if buf.len() < total {
        return;
    }

    // Clear to fully transparent.
    let mut i = 0;

    while i < total {
        buf[i] = 0; // B
        buf[i + 1] = 0; // G
        buf[i + 2] = 0; // R
        buf[i + 3] = 0; // A
        i += 4;
    }

    let fill = drawing::CURSOR_FILL;
    let outline = drawing::CURSOR_OUTLINE;
    let mut y = 0u32;

    while y < CURSOR_H {
        let mut x = 0u32;

        while x < CURSOR_W {
            let idx = (y * CURSOR_W + x) as usize;
            let color = match CURSOR_BITMAP[idx] {
                1 => fill,
                2 => outline,
                _ => {
                    x += 1;
                    continue;
                }
            };
            let off = (y * stride + x * 4) as usize;
            // Encode as BGRA8888 (same as Color::encode for Bgra8888).
            buf[off] = color.b;
            buf[off + 1] = color.g;
            buf[off + 2] = color.r;
            buf[off + 3] = color.a;

            x += 1;
        }
        y += 1;
    }
}
