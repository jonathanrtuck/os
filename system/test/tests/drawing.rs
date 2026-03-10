//! Host-side tests for the drawing library.
//!
//! Includes the library directly — it has zero external dependencies (no_std,
//! no syscalls, no hardware), making it fully testable on the host.

#[path = "../../library/drawing/lib.rs"]
mod drawing;

use drawing::{Color, PixelFormat, Surface, FONT_8X16};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a small test surface (zeroed buffer).
fn make_surface(buf: &mut [u8], width: u32, height: u32) -> Surface<'_> {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    assert!(buf.len() >= (stride * height) as usize);
    for b in buf.iter_mut() {
        *b = 0;
    }
    Surface {
        data: buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    }
}

// ---------------------------------------------------------------------------
// Color tests
// ---------------------------------------------------------------------------

#[test]
fn color_rgb_is_opaque() {
    let c = Color::rgb(100, 150, 200);
    assert_eq!(c.r, 100);
    assert_eq!(c.g, 150);
    assert_eq!(c.b, 200);
    assert_eq!(c.a, 255);
}

#[test]
fn color_rgba_preserves_alpha() {
    let c = Color::rgba(10, 20, 30, 128);
    assert_eq!(c.a, 128);
}

#[test]
fn color_encode_decode_roundtrip_via_pixel() {
    let mut buf = [0u8; 4]; // 1x1 surface
    let mut s = make_surface(&mut buf, 1, 1);

    let original = Color::rgba(11, 22, 33, 44);
    s.set_pixel(0, 0, original);
    assert_eq!(s.get_pixel(0, 0), Some(original));
}

#[test]
fn color_bgra_byte_order() {
    // Verify the actual byte layout in BGRA8888 format by inspecting the buffer.
    let mut buf = [0u8; 4]; // 1x1 surface
    let mut s = make_surface(&mut buf, 1, 1);

    s.set_pixel(0, 0, Color::rgba(0x11, 0x22, 0x33, 0x44));

    // BGRA order: B=0x33, G=0x22, R=0x11, A=0x44
    assert_eq!(buf, [0x33, 0x22, 0x11, 0x44]);
}

// ---------------------------------------------------------------------------
// Pixel format tests
// ---------------------------------------------------------------------------

#[test]
fn bgra8888_is_4_bytes() {
    assert_eq!(PixelFormat::Bgra8888.bytes_per_pixel(), 4);
}

// ---------------------------------------------------------------------------
// Surface: set_pixel / get_pixel
// ---------------------------------------------------------------------------

#[test]
fn set_get_pixel_roundtrip() {
    let mut buf = [0u8; 4 * 4 * 4]; // 4x4
    let mut s = make_surface(&mut buf, 4, 4);

    let red = Color::rgb(255, 0, 0);
    s.set_pixel(2, 1, red);

    assert_eq!(s.get_pixel(2, 1), Some(red));
    // Adjacent pixels untouched.
    assert_eq!(s.get_pixel(1, 1), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn set_pixel_out_of_bounds_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.set_pixel(4, 0, Color::WHITE); // x out of bounds
    s.set_pixel(0, 4, Color::WHITE); // y out of bounds
    s.set_pixel(100, 100, Color::WHITE); // way out

    // Buffer unchanged.
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn get_pixel_out_of_bounds_returns_none() {
    let mut buf = [0u8; 4 * 4 * 4];
    let s = make_surface(&mut buf, 4, 4);

    assert_eq!(s.get_pixel(4, 0), None);
    assert_eq!(s.get_pixel(0, 4), None);
}

// ---------------------------------------------------------------------------
// Surface: clear
// ---------------------------------------------------------------------------

#[test]
fn clear_fills_entire_surface() {
    let mut buf = [0u8; 8 * 8 * 4]; // 8x8
    let mut s = make_surface(&mut buf, 8, 8);

    let blue = Color::rgb(0, 0, 255);
    s.clear(blue);

    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(s.get_pixel(x, y), Some(blue), "mismatch at ({x}, {y})");
        }
    }
}

// ---------------------------------------------------------------------------
// Surface: fill_rect
// ---------------------------------------------------------------------------

#[test]
fn fill_rect_basic() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    let green = Color::rgb(0, 200, 0);
    s.fill_rect(2, 3, 4, 2, green);

    // Inside the rect.
    for y in 3..5 {
        for x in 2..6 {
            assert_eq!(s.get_pixel(x, y), Some(green), "inside at ({x}, {y})");
        }
    }
    // Outside the rect (spot checks).
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(1, 3), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(6, 3), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 5), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_clips_to_bounds() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    // Rect extends past right and bottom edges.
    s.fill_rect(2, 2, 10, 10, Color::WHITE);

    // Only the clipped region (2..4, 2..4) should be filled.
    assert_eq!(s.get_pixel(2, 2), Some(Color::WHITE));
    assert_eq!(s.get_pixel(3, 3), Some(Color::WHITE));
    // Outside the clipped region.
    assert_eq!(s.get_pixel(1, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 1), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_entirely_outside_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.fill_rect(5, 5, 10, 10, Color::WHITE); // starts past both edges

    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn fill_rect_zero_size_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.fill_rect(0, 0, 0, 5, Color::WHITE);
    s.fill_rect(0, 0, 5, 0, Color::WHITE);

    assert!(buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Surface: draw_hline / draw_vline
// ---------------------------------------------------------------------------

#[test]
fn draw_hline_basic() {
    let mut buf = [0u8; 8 * 4 * 4];
    let mut s = make_surface(&mut buf, 8, 4);

    s.draw_hline(1, 2, 5, Color::WHITE);

    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 2), Some(Color::WHITE));
    }
    // Not drawn outside.
    assert_eq!(s.get_pixel(0, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(6, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(1, 1), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_vline_basic() {
    let mut buf = [0u8; 4 * 8 * 4];
    let mut s = make_surface(&mut buf, 4, 8);

    s.draw_vline(2, 1, 5, Color::WHITE);

    for y in 1..6 {
        assert_eq!(s.get_pixel(2, y), Some(Color::WHITE));
    }
    assert_eq!(s.get_pixel(2, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 6), Some(Color::rgba(0, 0, 0, 0)));
}

// ---------------------------------------------------------------------------
// Surface: draw_rect (outline)
// ---------------------------------------------------------------------------

#[test]
fn draw_rect_outline() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    s.draw_rect(1, 1, 5, 4, Color::WHITE);

    // Top edge: (1,1)..(6,1)
    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 1), Some(Color::WHITE), "top at x={x}");
    }
    // Bottom edge: (1,4)..(6,4)
    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 4), Some(Color::WHITE), "bottom at x={x}");
    }
    // Left edge: (1,2)..(1,3)
    for y in 2..4 {
        assert_eq!(s.get_pixel(1, y), Some(Color::WHITE), "left at y={y}");
    }
    // Right edge: (5,2)..(5,3)
    for y in 2..4 {
        assert_eq!(s.get_pixel(5, y), Some(Color::WHITE), "right at y={y}");
    }
    // Interior is empty.
    for y in 2..4 {
        for x in 2..5 {
            assert_eq!(
                s.get_pixel(x, y),
                Some(Color::rgba(0, 0, 0, 0)),
                "interior at ({x}, {y})"
            );
        }
    }
}

#[test]
fn draw_rect_1x1() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_rect(1, 1, 1, 1, Color::WHITE);

    assert_eq!(s.get_pixel(1, 1), Some(Color::WHITE));
    // Only that one pixel.
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 2), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_rect_zero_size_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_rect(0, 0, 0, 5, Color::WHITE);
    s.draw_rect(0, 0, 5, 0, Color::WHITE);

    assert!(buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Surface: draw_line
// ---------------------------------------------------------------------------

#[test]
fn draw_line_horizontal() {
    let mut buf = [0u8; 8 * 4 * 4];
    let mut s = make_surface(&mut buf, 8, 4);

    s.draw_line(1, 2, 5, 2, Color::WHITE);

    for x in 1..=5 {
        assert_eq!(s.get_pixel(x, 2), Some(Color::WHITE), "at x={x}");
    }
}

#[test]
fn draw_line_vertical() {
    let mut buf = [0u8; 4 * 8 * 4];
    let mut s = make_surface(&mut buf, 4, 8);

    s.draw_line(2, 1, 2, 6, Color::WHITE);

    for y in 1..=6 {
        assert_eq!(s.get_pixel(2, y), Some(Color::WHITE), "at y={y}");
    }
}

#[test]
fn draw_line_diagonal() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    // 45-degree line from (0,0) to (4,4): Bresenham should hit each pixel.
    s.draw_line(0, 0, 4, 4, Color::WHITE);

    for i in 0..=4u32 {
        assert_eq!(s.get_pixel(i, i), Some(Color::WHITE), "at ({i}, {i})");
    }
}

#[test]
fn draw_line_clips_negative_coords() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    // Line starts outside surface (negative coords).
    s.draw_line(-2, -2, 1, 1, Color::WHITE);

    // Should draw the visible portion without panicking.
    assert_eq!(s.get_pixel(0, 0), Some(Color::WHITE));
    assert_eq!(s.get_pixel(1, 1), Some(Color::WHITE));
}

#[test]
fn draw_line_single_point() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_line(2, 2, 2, 2, Color::WHITE);

    assert_eq!(s.get_pixel(2, 2), Some(Color::WHITE));
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn surface_with_stride_padding() {
    // Stride > width * bpp (rows have padding bytes).
    let stride = 5 * 4 + 8; // 28 bytes per row (5 pixels + 8 padding)
    let mut buf = vec![0u8; stride as usize * 4]; // 4 rows
    let mut s = Surface {
        data: &mut buf,
        width: 5,
        height: 4,
        stride,
        format: PixelFormat::Bgra8888,
    };

    let red = Color::rgb(255, 0, 0);
    s.set_pixel(4, 3, red); // last pixel, last row

    assert_eq!(s.get_pixel(4, 3), Some(red));
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_saturating_add_no_overflow() {
    // Ensure that x + w doesn't overflow u32.
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.fill_rect(2, 0, u32::MAX, 1, Color::WHITE);

    // Should fill from x=2 to x=3 (clipped to width).
    assert_eq!(s.get_pixel(2, 0), Some(Color::WHITE));
    assert_eq!(s.get_pixel(3, 0), Some(Color::WHITE));
}

// ---------------------------------------------------------------------------
// BitmapFont tests
// ---------------------------------------------------------------------------

#[test]
fn font_8x16_dimensions() {
    assert_eq!(FONT_8X16.glyph_width, 8);
    assert_eq!(FONT_8X16.glyph_height, 16);
}

#[test]
fn font_glyph_returns_correct_length() {
    let glyph = FONT_8X16.glyph('A').unwrap();
    assert_eq!(glyph.len(), 16);
}

#[test]
fn font_glyph_space_is_blank() {
    let glyph = FONT_8X16.glyph(' ').unwrap();
    assert!(glyph.iter().all(|&b| b == 0));
}

#[test]
fn font_glyph_printable_ascii_all_present() {
    for c in 0x20u8..=0x7E {
        assert!(
            FONT_8X16.glyph(c as char).is_some(),
            "missing glyph for 0x{c:02X} '{}'",
            c as char
        );
    }
}

#[test]
fn font_glyph_outside_range_returns_none() {
    assert!(FONT_8X16.glyph('\0').is_none());
    assert!(FONT_8X16.glyph('\x1F').is_none());
    assert!(FONT_8X16.glyph('\x7F').is_none());
    assert!(FONT_8X16.glyph('é').is_none());
}

#[test]
fn font_glyph_a_has_nonzero_rows() {
    let glyph = FONT_8X16.glyph('A').unwrap();
    assert!(glyph.iter().any(|&b| b != 0));
}

// ---------------------------------------------------------------------------
// Surface: draw_glyph
// ---------------------------------------------------------------------------

#[test]
fn draw_glyph_exclamation_mark() {
    // '!' row 2 = 0x18 (bits 3,4 set), row 9 = 0x00 (gap), row 10 = 0x18 (dot).
    let mut buf = [0u8; 16 * 16 * 4];
    let mut s = make_surface(&mut buf, 16, 16);

    s.draw_glyph(0, 0, '!', &FONT_8X16, Color::WHITE);

    assert_eq!(s.get_pixel(3, 2), Some(Color::WHITE));
    assert_eq!(s.get_pixel(4, 2), Some(Color::WHITE));
    assert_eq!(s.get_pixel(0, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(3, 9), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(3, 10), Some(Color::WHITE));
}

#[test]
fn draw_glyph_unknown_char_is_noop() {
    let mut buf = [0u8; 16 * 16 * 4];
    let mut s = make_surface(&mut buf, 16, 16);

    s.draw_glyph(0, 0, '\x7F', &FONT_8X16, Color::WHITE);

    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn draw_glyph_clips_at_surface_edge() {
    // Place an 8x16 glyph on a tiny 4x4 surface — no panic.
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_glyph(0, 0, 'A', &FONT_8X16, Color::WHITE);

    // 'A' row 2 = 0x10 → bit 4 → pixel column 3. Surface is 4 wide, so col 3 is visible.
    assert_eq!(s.get_pixel(3, 2), Some(Color::WHITE));
}

// ---------------------------------------------------------------------------
// Surface: draw_text
// ---------------------------------------------------------------------------

#[test]
fn draw_text_returns_advanced_x() {
    let mut buf = [0u8; 64 * 16 * 4];
    let mut s = make_surface(&mut buf, 64, 16);

    let end_x = s.draw_text(0, 0, "Hi", &FONT_8X16, Color::WHITE);

    assert_eq!(end_x, 16);
}

#[test]
fn draw_text_empty_string() {
    let mut buf = [0u8; 16 * 16 * 4];
    let mut s = make_surface(&mut buf, 16, 16);

    let end_x = s.draw_text(5, 0, "", &FONT_8X16, Color::WHITE);

    assert_eq!(end_x, 5);
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn draw_text_two_glyphs_are_adjacent() {
    let mut buf = [0u8; 32 * 16 * 4];
    let mut s = make_surface(&mut buf, 32, 16);

    s.draw_text(0, 0, "!!", &FONT_8X16, Color::WHITE);

    // First '!' at x=0: pixel (3,2) set.
    assert_eq!(s.get_pixel(3, 2), Some(Color::WHITE));
    // Second '!' at x=8: pixel (11,2) set.
    assert_eq!(s.get_pixel(11, 2), Some(Color::WHITE));
    // Gap between glyphs: pixel (7,2) should be blank.
    assert_eq!(s.get_pixel(7, 2), Some(Color::rgba(0, 0, 0, 0)));
}

// ---------------------------------------------------------------------------
// Blit tests
// ---------------------------------------------------------------------------

#[test]
fn blit_basic() {
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);

    // Create a 4x4 red source.
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let src_stride = 4 * bpp;
    let mut src_buf = [0u8; 4 * 4 * 4];
    let red = Color::rgb(255, 0, 0);
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: src_stride,
            format: PixelFormat::Bgra8888,
        };
        src.clear(red);
    }

    dst.blit(&src_buf, 4, 4, src_stride, 2, 3);

    // Pixel inside blit region should be red.
    assert_eq!(dst.get_pixel(2, 3), Some(red));
    assert_eq!(dst.get_pixel(5, 6), Some(red));
    // Pixel outside blit region should be black/zeroed.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn blit_clips_at_edges() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 4 * bpp,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgb(0, 255, 0));
    }

    // Place at (6, 6) — only 2x2 pixels should fit.
    dst.blit(&src_buf, 4, 4, 4 * bpp, 6, 6);

    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(7, 7), Some(Color::rgb(0, 255, 0)));
    // (5, 6) is outside the blit region.
    assert_eq!(dst.get_pixel(5, 6), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn blit_entirely_outside() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    let src_buf = [0xFFu8; 4 * 4 * 4];

    // Place entirely outside destination.
    dst.blit(&src_buf, 4, 4, 16, 8, 8);

    // Nothing should have changed.
    assert!(dst_buf.iter().all(|&b| b == 0));
}

#[test]
fn blit_zero_size_source() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    // Zero-width source — should be a no-op.
    dst.blit(&[], 0, 0, 0, 0, 0);
    assert!(dst_buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Alpha blending: Color::blend_over
// ---------------------------------------------------------------------------

#[test]
fn blend_over_opaque_src_returns_src() {
    let src = Color::rgb(255, 0, 0);
    let dst = Color::rgb(0, 0, 255);
    assert_eq!(src.blend_over(dst), src);
}

#[test]
fn blend_over_transparent_src_returns_dst() {
    let src = Color::TRANSPARENT;
    let dst = Color::rgb(0, 255, 0);
    assert_eq!(src.blend_over(dst), dst);
}

#[test]
fn blend_over_50_percent_red_on_opaque_blue() {
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgb(0, 0, 255);
    let result = src.blend_over(dst);

    // out_a = 128 + 255*(255-128)/255 = 128 + 127 = 255
    assert_eq!(result.a, 255);
    // out_r = (255*128 + 0) / 255 = 128
    assert_eq!(result.r, 128);
    // out_b = (0 + 255*255*127/255) / 255 = (255*127)/255 = 127
    assert_eq!(result.b, 127);
    assert_eq!(result.g, 0);
}

#[test]
fn blend_over_25_percent_white_on_black() {
    let src = Color::rgba(255, 255, 255, 64);
    let dst = Color::rgb(0, 0, 0);
    let result = src.blend_over(dst);

    assert_eq!(result.a, 255);
    // out_r = (255*64 + 0) / 255 = 64
    assert_eq!(result.r, 64);
    assert_eq!(result.g, 64);
    assert_eq!(result.b, 64);
}

#[test]
fn blend_over_both_transparent() {
    let src = Color::TRANSPARENT;
    let dst = Color::TRANSPARENT;
    assert_eq!(src.blend_over(dst), Color::TRANSPARENT);
}

#[test]
fn blend_over_semi_on_semi() {
    // 50% red on 50% blue — both semi-transparent.
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgba(0, 0, 255, 128);
    let result = src.blend_over(dst);

    // out_a = 128 + 128*127/255 ≈ 191
    assert!(result.a >= 190 && result.a <= 192, "a={}", result.a);
    // Source (red) dominates since it's on top.
    assert!(result.r > result.b, "r={} should > b={}", result.r, result.b);
}

#[test]
fn blend_over_commutative_only_when_symmetric() {
    // Blending is NOT commutative in general — order matters.
    let a = Color::rgba(255, 0, 0, 128);
    let b = Color::rgba(0, 255, 0, 128);

    let ab = a.blend_over(b);
    let ba = b.blend_over(a);

    // Red-on-green: more red. Green-on-red: more green.
    assert!(ab.r > ab.g);
    assert!(ba.g > ba.r);
}

// ---------------------------------------------------------------------------
// Alpha blending: Surface::blend_pixel
// ---------------------------------------------------------------------------

#[test]
fn blend_pixel_on_opaque_background() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.set_pixel(1, 1, Color::rgb(0, 0, 255));
    s.blend_pixel(1, 1, Color::rgba(255, 0, 0, 128));

    let result = s.get_pixel(1, 1).unwrap();
    assert_eq!(result.r, 128);
    assert_eq!(result.b, 127);
    assert_eq!(result.a, 255);
}

#[test]
fn blend_pixel_transparent_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    let blue = Color::rgb(0, 0, 255);
    s.set_pixel(1, 1, blue);
    s.blend_pixel(1, 1, Color::TRANSPARENT);

    assert_eq!(s.get_pixel(1, 1), Some(blue));
}

#[test]
fn blend_pixel_opaque_overwrites() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.set_pixel(1, 1, Color::rgb(0, 0, 255));
    s.blend_pixel(1, 1, Color::rgb(255, 0, 0));

    assert_eq!(s.get_pixel(1, 1), Some(Color::rgb(255, 0, 0)));
}

#[test]
fn blend_pixel_out_of_bounds_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.blend_pixel(10, 10, Color::rgba(255, 0, 0, 128));

    assert!(buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Alpha blending: Surface::fill_rect_blend
// ---------------------------------------------------------------------------

#[test]
fn fill_rect_blend_on_opaque_background() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    s.clear(Color::BLACK);
    s.fill_rect_blend(2, 2, 4, 4, Color::rgba(255, 255, 255, 128));

    // Inside: ~128 gray (50% white on black).
    let inside = s.get_pixel(3, 3).unwrap();
    assert_eq!(inside.r, 128);
    assert_eq!(inside.g, 128);
    assert_eq!(inside.b, 128);

    // Outside: still black.
    assert_eq!(s.get_pixel(0, 0), Some(Color::BLACK));
}

#[test]
fn fill_rect_blend_opaque_fast_path() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    // Opaque fill_rect_blend should behave identically to fill_rect.
    s.fill_rect_blend(1, 1, 3, 3, Color::rgb(200, 100, 50));

    assert_eq!(s.get_pixel(2, 2), Some(Color::rgb(200, 100, 50)));
}

#[test]
fn fill_rect_blend_transparent_is_noop() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    s.clear(Color::WHITE);
    s.fill_rect_blend(0, 0, 8, 8, Color::TRANSPARENT);

    assert_eq!(s.get_pixel(0, 0), Some(Color::WHITE));
}

#[test]
fn fill_rect_blend_clips_to_bounds() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.clear(Color::BLACK);
    s.fill_rect_blend(2, 2, 10, 10, Color::rgba(255, 0, 0, 128));

    // Clipped region blended.
    let px = s.get_pixel(3, 3).unwrap();
    assert_eq!(px.r, 128);
    // Outside clipped region unchanged.
    assert_eq!(s.get_pixel(1, 1), Some(Color::BLACK));
}

// ---------------------------------------------------------------------------
// Alpha blending: Surface::blit_blend
// ---------------------------------------------------------------------------

#[test]
fn blit_blend_transparent_pixels_pass_through() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    let blue = Color::rgb(0, 0, 255);
    dst.clear(blue);

    // Source is all transparent (zeroed).
    let src_buf = [0u8; 4 * 4 * 4];
    dst.blit_blend(&src_buf, 4, 4, 16, 2, 2);

    // Destination unchanged.
    assert_eq!(dst.get_pixel(3, 3), Some(blue));
}

#[test]
fn blit_blend_opaque_pixels_overwrite() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));

    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgb(255, 0, 0));
    }

    dst.blit_blend(&src_buf, 4, 4, 16, 2, 2);

    assert_eq!(dst.get_pixel(3, 3), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn blit_blend_semi_transparent() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));

    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgba(255, 0, 0, 128));
    }

    dst.blit_blend(&src_buf, 4, 4, 16, 2, 2);

    let result = dst.get_pixel(3, 3).unwrap();
    assert_eq!(result.r, 128);
    assert_eq!(result.b, 127);
    assert_eq!(result.a, 255);
}

#[test]
fn blit_blend_clips_at_edges() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgb(255, 0, 0));
    }

    // Place at (6, 6) — only 2x2 should fit.
    dst.blit_blend(&src_buf, 4, 4, 16, 6, 6);

    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(7, 7), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(5, 6), Some(Color::BLACK));
}

#[test]
fn blit_blend_entirely_outside_is_noop() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::WHITE);

    let src_buf = [0xFFu8; 4 * 4 * 4];
    dst.blit_blend(&src_buf, 4, 4, 16, 8, 8);

    assert_eq!(dst.get_pixel(0, 0), Some(Color::WHITE));
}

#[test]
fn blit_blend_mixed_alpha_pixels() {
    // Source has both transparent and semi-transparent pixels.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));

    let mut src_buf = [0u8; 4 * 2 * 4]; // 4x2, starts transparent
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 2,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        // Left half: opaque red. Right half: stays transparent.
        src.fill_rect(0, 0, 2, 2, Color::rgb(255, 0, 0));
    }

    dst.blit_blend(&src_buf, 4, 2, 16, 2, 2);

    // Left half: overwritten with red.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(3, 3), Some(Color::rgb(255, 0, 0)));
    // Right half: blue shows through (transparent source).
    assert_eq!(dst.get_pixel(4, 2), Some(Color::rgb(0, 0, 255)));
    assert_eq!(dst.get_pixel(5, 3), Some(Color::rgb(0, 0, 255)));
}
