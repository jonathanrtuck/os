//! Test content generators (scaffolding).
//!
//! Provides a test image for image-mode and the pointer cursor shape.
//! Remove once dedicated image document types exist.

use alloc::vec::Vec;

/// Generate a 32x32 BGRA gradient image for testing.
/// Returns pixel data in BGRA8 format (4 bytes/pixel, 4096 bytes total).
pub fn generate_test_image() -> Vec<u8> {
    let w: u32 = 32;
    let h: u32 = 32;
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / (w - 1)) as u8;
            let g = (y * 255 / (h - 1)) as u8;
            let b = 128u8;
            let a = 255u8;
            // BGRA order (matches VIRGL_FORMAT_B8G8R8A8_UNORM).
            pixels.push(b);
            pixels.push(g);
            pixels.push(r);
            pixels.push(a);
        }
    }
    pixels
}

/// Generate path commands for a standard arrow pointer cursor.
/// 10 × 16 pt. Tip at (0, 0), body extends down-right.
pub fn generate_arrow_cursor() -> Vec<u8> {
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 0.0, 14.0);
    scene::path_line_to(&mut cmds, 4.0, 11.0);
    scene::path_line_to(&mut cmds, 7.0, 17.0);
    scene::path_line_to(&mut cmds, 9.0, 16.0);
    scene::path_line_to(&mut cmds, 6.0, 10.0);
    scene::path_line_to(&mut cmds, 10.0, 10.0);
    scene::path_close(&mut cmds);
    cmds
}
