//! Test content generators (scaffolding).
//!
//! These exercise Content::Image and Content::Path in the render pipeline.
//! Called unconditionally during build_editor_scene; dropped after first
//! incremental text edit (update_document_content truncates to WELL_KNOWN_COUNT).
//! Remove once dedicated image/path document types exist.

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

/// Generate path commands for a 5-pointed star.
/// Coordinates are in the node's local space (0,0 to size,size).
pub fn generate_test_star(size: f32) -> Vec<u8> {
    let mut cmds = Vec::new();
    let cx = size / 2.0;
    let cy = size / 2.0;
    let outer_r = size * 0.45;
    let inner_r = size * 0.18;

    // 5 outer points + 5 inner points = 10 vertices.
    let pi = core::f32::consts::PI;
    let start_angle = -pi / 2.0; // Top point.

    for i in 0..10 {
        let angle = start_angle + (i as f32) * pi / 5.0;
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        let x = cx + r * cos_approx(angle);
        let y = cy + r * sin_approx(angle);

        if i == 0 {
            scene::path_move_to(&mut cmds, x, y);
        } else {
            scene::path_line_to(&mut cmds, x, y);
        }
    }
    scene::path_close(&mut cmds);
    cmds
}

/// Generate path commands for a rounded rectangle with cubic bezier corners.
/// Tests both LineTo and CubicTo commands.
pub fn generate_test_rounded_rect(w: f32, h: f32, r: f32) -> Vec<u8> {
    let mut cmds = Vec::new();
    // Magic number for circular arcs via cubic beziers.
    let k = r * 0.5522847;

    // Start at top-left, after the corner radius.
    scene::path_move_to(&mut cmds, r, 0.0);
    // Top edge.
    scene::path_line_to(&mut cmds, w - r, 0.0);
    // Top-right corner (cubic bezier).
    scene::path_cubic_to(&mut cmds, w - r + k, 0.0, w, r - k, w, r);
    // Right edge.
    scene::path_line_to(&mut cmds, w, h - r);
    // Bottom-right corner.
    scene::path_cubic_to(&mut cmds, w, h - r + k, w - r + k, h, w - r, h);
    // Bottom edge.
    scene::path_line_to(&mut cmds, r, h);
    // Bottom-left corner.
    scene::path_cubic_to(&mut cmds, r - k, h, 0.0, h - r + k, 0.0, h - r);
    // Left edge.
    scene::path_line_to(&mut cmds, 0.0, r);
    // Top-left corner.
    scene::path_cubic_to(&mut cmds, 0.0, r - k, r - k, 0.0, r, 0.0);
    scene::path_close(&mut cmds);
    cmds
}

/// Approximate sine (avoids pulling in libm for no_std).
fn sin_approx(x: f32) -> f32 {
    // Normalize to [-pi, pi].
    let pi = core::f32::consts::PI;
    let mut v = x % (2.0 * pi);
    if v > pi {
        v -= 2.0 * pi;
    }
    if v < -pi {
        v += 2.0 * pi;
    }
    // Bhaskara I approximation: sin(x) ≈ 16x(π−x) / (5π²−4x(π−x))
    let num = 16.0 * v * (pi - v.abs());
    let den = 5.0 * pi * pi - 4.0 * v.abs() * (pi - v.abs());
    if den.abs() < 0.001 {
        0.0
    } else {
        num / den
    }
}

/// Approximate cosine.
fn cos_approx(x: f32) -> f32 {
    sin_approx(x + core::f32::consts::FRAC_PI_2)
}
