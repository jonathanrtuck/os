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

/// Generate path commands for the pointer cursor.
/// 12 × 12 pt. Tip (hotspot) at (0, 0), body extends down-right.
/// Proportions inspired by Tabler's pointer icon — wider, more balanced
/// angle than a classic arrow, with a diagonal shaft and notch.
pub fn generate_arrow_cursor() -> Vec<u8> {
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 3.0, 10.5);
    scene::path_line_to(&mut cmds, 4.7, 10.6);
    scene::path_line_to(&mut cmds, 6.3, 8.2);
    scene::path_line_to(&mut cmds, 10.0, 12.0);
    scene::path_line_to(&mut cmds, 12.0, 10.0);
    scene::path_line_to(&mut cmds, 8.2, 6.3);
    scene::path_line_to(&mut cmds, 10.6, 4.7);
    scene::path_line_to(&mut cmds, 10.5, 3.0);
    scene::path_close(&mut cmds);
    cmds
}
