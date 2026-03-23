//! OS icon data — Tabler Icons (MIT) converted to native path commands.
//!
//! Each icon is stored as SVG `d` attribute strings. At boot, icons are
//! pre-rasterized into BGRA pixel buffers using the CPU scanline path
//! rasterizer, then displayed as `Content::Image` nodes. This bypasses
//! the metal-render stencil pipeline (which struggles with the complex
//! concave geometry from stroke expansion) and produces pixel-perfect
//! results on all three render backends.

/// Tabler `file-text` icon — document with text lines.
/// Used for text/plain and similar document types.
pub const FILE_TEXT: &[&str] = &[
    "M14 3v4a1 1 0 0 0 1 1h4",
    "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2z",
    "M9 9l1 0",
    "M9 13l6 0",
    "M9 17l6 0",
];

/// Tabler `photo` icon — image with landscape.
/// Used for image/* types.
pub const PHOTO: &[&str] = &[
    "M15 8h.01",
    "M3 6a3 3 0 0 1 3 -3h12a3 3 0 0 1 3 3v12a3 3 0 0 1 -3 3h-12a3 3 0 0 1 -3 -3v-12z",
    "M3 16l5 -5c.928 -.893 2.072 -.893 3 0l5 5",
    "M14 14l1 -1c.928 -.893 2.072 -.893 3 0l3 3",
];

use alloc::vec::Vec;
use alloc::vec;

/// Pre-rasterize a stroked Tabler icon into BGRA pixels.
///
/// The icon is rendered at `size_px` × `size_px` physical pixels with the
/// given color. `stroke_w` is in viewbox units (Tabler's 24×24 space);
/// the native default is 2.0 — use smaller values for thinner strokes.
/// Returns BGRA pixel data (4 bytes/pixel, `size_px * size_px * 4` bytes).
pub fn rasterize_icon(
    paths: &[&str],
    size_px: u32,
    color: scene::Color,
    stroke_w: f32,
) -> Vec<u8> {
    // Work in Tabler's 24×24 viewbox space for SVG parsing and stroke
    // expansion. The rasterizer scales from viewbox → pixel coordinates.
    let scale = size_px as f32 / 24.0;

    let w = size_px;
    let h = size_px;
    let stride = w * 4;
    let mut pixels = vec![0u8; (stride * h) as usize];

    // Render each SVG sub-path independently (painter's algorithm).
    // Avoids winding-rule interference at overlapping strokes.
    for d in paths {
        let cmds = scene::svg_path::parse_svg_path(d);
        let expanded = scene::stroke::expand_stroke(&cmds, stroke_w);
        if expanded.is_empty() {
            continue;
        }

        let mut surface = drawing::Surface {
            data: &mut pixels,
            width: w,
            height: h,
            stride,
            format: drawing::PixelFormat::Bgra8888,
        };

        // scale converts viewbox coords (0-24) → pixel coords (0-size_px).
        render::scene_render::path_raster::render_path_data(
            &mut surface,
            &expanded,
            scale,
            color,
            scene::FillRule::Winding,
            0,
            0,
            w as i32,
            h as i32,
        );
    }

    pixels
}

/// Viewbox size for the pointer cursor. The arrow shape occupies (0,0)-(12,12);
/// 1 unit of margin on each side accommodates the stroke outline.
pub const CURSOR_VIEWBOX: f32 = 14.0;

/// Hotspot offset in viewbox units (arrow tip position within the viewbox).
const CURSOR_OFFSET: f32 = 1.0;

/// Pre-rasterize a macOS-style pointer cursor: black fill with white outline.
///
/// Returns BGRA pixel data at `size_px × size_px`. The arrow tip (hotspot) is
/// at viewbox position (1, 1), so callers should offset the display node by
/// `-(size_pt / CURSOR_VIEWBOX)` points to align the hotspot with the mouse.
pub fn rasterize_cursor(size_px: u32) -> Vec<u8> {
    let scale = size_px as f32 / CURSOR_VIEWBOX;
    let outline_w = 1.2_f32; // viewbox units

    let w = size_px;
    let h = size_px;
    let stride = w * 4;
    let mut pixels = vec![0u8; (stride * h) as usize];

    // Arrow path with offset to leave margin for the stroke outline.
    let o = CURSOR_OFFSET;
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0 + o, 0.0 + o);
    scene::path_line_to(&mut cmds, 3.0 + o, 10.5 + o);
    scene::path_line_to(&mut cmds, 4.7 + o, 10.6 + o);
    scene::path_line_to(&mut cmds, 6.3 + o, 8.2 + o);
    scene::path_line_to(&mut cmds, 10.0 + o, 12.0 + o);
    scene::path_line_to(&mut cmds, 12.0 + o, 10.0 + o);
    scene::path_line_to(&mut cmds, 8.2 + o, 6.3 + o);
    scene::path_line_to(&mut cmds, 10.6 + o, 4.7 + o);
    scene::path_line_to(&mut cmds, 10.5 + o, 3.0 + o);
    scene::path_close(&mut cmds);

    // Pass 1: White stroke outline.
    let expanded = scene::stroke::expand_stroke(&cmds, outline_w);
    if !expanded.is_empty() {
        let mut surface = drawing::Surface {
            data: &mut pixels,
            width: w,
            height: h,
            stride,
            format: drawing::PixelFormat::Bgra8888,
        };
        render::scene_render::path_raster::render_path_data(
            &mut surface,
            &expanded,
            scale,
            scene::Color::rgb(255, 255, 255),
            scene::FillRule::Winding,
            0,
            0,
            w as i32,
            h as i32,
        );
    }

    // Pass 2: Black fill on top.
    {
        let mut surface = drawing::Surface {
            data: &mut pixels,
            width: w,
            height: h,
            stride,
            format: drawing::PixelFormat::Bgra8888,
        };
        render::scene_render::path_raster::render_path_data(
            &mut surface,
            &cmds,
            scale,
            scene::Color::rgb(0, 0, 0),
            scene::FillRule::Winding,
            0,
            0,
            w as i32,
            h as i32,
        );
    }

    pixels
}
