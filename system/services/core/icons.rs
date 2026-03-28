//! Pointer cursor rendering.
//!
//! The mouse cursor is a hand-built geometric primitive, not an icon from
//! the icon library. Operational cursors need pixel-precise hotspots at
//! small sizes — different requirements from symbolic icons.

use alloc::{vec, vec::Vec};

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
    let outline_w = 1.8_f32; // viewbox units

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
