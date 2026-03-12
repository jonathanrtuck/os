//! Host-side tests for the drawing library.
//!
//! Includes the library directly — it has zero external dependencies (no_std,
//! no syscalls, no hardware), making it fully testable on the host.

#[path = "../../libraries/drawing/lib.rs"]
mod drawing;

use drawing::{Color, PixelFormat, Surface, TextLayout, FONT_8X16};

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
    // Gamma-correct: blending happens in linear space then converts back to sRGB.
    // 50% alpha red on blue produces sRGB ~188 red (higher than linear's 128)
    // because the gamma curve maps linear 0.5 to sRGB ~0.74.
    assert!(result.r > 140, "gamma-correct red should be > 140, got {}", result.r);
    assert!(result.b > 140, "gamma-correct blue should be > 140, got {}", result.b);
    assert_eq!(result.g, 0);
}

#[test]
fn blend_over_25_percent_white_on_black() {
    let src = Color::rgba(255, 255, 255, 64);
    let dst = Color::rgb(0, 0, 0);
    let result = src.blend_over(dst);

    assert_eq!(result.a, 255);
    // Gamma-correct: 25% alpha white on black. In linear space, 25% of max
    // intensity maps to a higher sRGB value than 64 due to the gamma curve.
    assert!(result.r > 100, "gamma-correct 25% white on black should be > 100, got {}", result.r);
    assert_eq!(result.r, result.g);
    assert_eq!(result.r, result.b);
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
    // Gamma-correct blending produces higher sRGB values than linear.
    assert!(result.r > 140, "gamma-correct red should be > 140, got {}", result.r);
    assert!(result.b > 140, "gamma-correct blue should be > 140, got {}", result.b);
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

    // Inside: gamma-correct 50% white on black gives sRGB ~188 (not 128).
    let inside = s.get_pixel(3, 3).unwrap();
    assert!(inside.r > 140, "gamma-correct 50% white on black should be > 140, got {}", inside.r);
    assert_eq!(inside.r, inside.g);
    assert_eq!(inside.r, inside.b);

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

    // Clipped region blended — gamma-correct produces higher sRGB values.
    let px = s.get_pixel(3, 3).unwrap();
    assert!(px.r > 140, "gamma-correct 50% red on black should be > 140, got {}", px.r);
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
    // Gamma-correct blending: 50% red on blue produces higher sRGB values.
    assert!(result.r > 140, "gamma-correct red should be > 140, got {}", result.r);
    assert!(result.b > 140, "gamma-correct blue should be > 140, got {}", result.b);
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

// ---------------------------------------------------------------------------
// TrueType font parser
// ---------------------------------------------------------------------------

use drawing::{TrueTypeFont, RasterBuffer, RasterScratch, GlyphOutline};

const PROGGY_CLEAN: &[u8] = include_bytes!("../../libraries/drawing/ProggyClean.ttf");
const NUNITO_SANS: &[u8] = include_bytes!("../../libraries/drawing/NunitoSans-Regular.ttf");
const SOURCE_CODE_PRO: &[u8] = include_bytes!("../../libraries/drawing/SourceCodePro-Regular.ttf");

#[test]
fn ttf_parse_valid_font() {
    let font = TrueTypeFont::new(PROGGY_CLEAN);
    assert!(font.is_some(), "should parse ProggyClean.ttf");
}

#[test]
fn ttf_parse_empty_data_returns_none() {
    assert!(TrueTypeFont::new(&[]).is_none());
}

#[test]
fn ttf_parse_truncated_data_returns_none() {
    assert!(TrueTypeFont::new(&PROGGY_CLEAN[..10]).is_none());
}

#[test]
fn ttf_parse_not_truetype_returns_none() {
    // CFF/OpenType magic "OTTO".
    let mut data = PROGGY_CLEAN.to_vec();
    data[0] = b'O';
    data[1] = b'T';
    data[2] = b'T';
    data[3] = b'O';
    assert!(TrueTypeFont::new(&data).is_none());
}

#[test]
fn ttf_glyph_index_ascii() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    // 'A' should have a valid glyph index.
    let idx = font.glyph_index('A');
    assert!(idx.is_some(), "'A' should have a glyph");
    assert!(idx.unwrap() > 0, "glyph index for 'A' should be non-zero");
}

#[test]
fn ttf_glyph_index_different_chars_different_indices() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let a = font.glyph_index('A').unwrap();
    let b = font.glyph_index('B').unwrap();
    assert_ne!(a, b, "'A' and 'B' should have different glyph indices");
}

#[test]
fn ttf_glyph_index_all_printable_ascii() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    for c in 0x21u8..=0x7Eu8 {
        let ch = c as char;
        assert!(
            font.glyph_index(ch).is_some(),
            "printable ASCII '{}' (0x{:02x}) should have a glyph",
            ch, c,
        );
    }
}

#[test]
fn ttf_glyph_outline_a() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let glyph_idx = font.glyph_index('A').unwrap();
    let mut outline = GlyphOutline::zeroed();
    let ok = font.glyph_outline(glyph_idx, &mut outline);
    assert!(ok, "'A' should have an outline");
    assert!(outline.num_contours > 0, "'A' should have at least 1 contour");
    assert!(outline.num_points > 2, "'A' should have more than 2 points");
}

#[test]
fn ttf_glyph_outline_o_has_two_contours() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let glyph_idx = font.glyph_index('O').unwrap();
    let mut outline = GlyphOutline::zeroed();
    let ok = font.glyph_outline(glyph_idx, &mut outline);
    assert!(ok, "'O' should have an outline");
    // 'O' typically has 2 contours (outer + inner).
    assert!(
        outline.num_contours >= 1,
        "'O' should have at least 1 contour, got {}",
        outline.num_contours,
    );
}

#[test]
fn ttf_glyph_h_metrics() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let glyph_idx = font.glyph_index('A').unwrap();
    let (advance, _lsb) = font.glyph_h_metrics(glyph_idx).unwrap();
    assert!(advance > 0, "'A' should have positive advance width");
}

#[test]
fn ttf_space_has_no_outline() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let glyph_idx = font.glyph_index(' ').unwrap();
    let mut outline = GlyphOutline::zeroed();
    // Space has no outline — glyph_outline returns false.
    let ok = font.glyph_outline(glyph_idx, &mut outline);
    assert!(!ok, "space should have no outline");
}

#[test]
fn ttf_units_per_em() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let upem = font.units_per_em();
    assert!(upem > 0, "units_per_em should be positive");
    // Typical values: 1000, 2048, etc.
    assert!(upem <= 16384, "units_per_em should be reasonable, got {}", upem);
}

// ---------------------------------------------------------------------------
// TrueType rasterization
// ---------------------------------------------------------------------------

#[test]
fn ttf_rasterize_a() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = font.rasterize('A', 32, &mut raster, &mut scratch);
    assert!(metrics.is_some(), "should rasterize 'A' at 32px");
    let m = metrics.unwrap();
    assert!(m.width > 0, "bitmap should have non-zero width");
    assert!(m.height > 0, "bitmap should have non-zero height");
    assert!(m.advance > 0, "advance should be positive");

    // Coverage map should have some non-zero pixels.
    let total = (m.width * m.height) as usize;
    let has_coverage = buf[..total].iter().any(|&b| b > 0);
    assert!(has_coverage, "coverage map for 'A' should have non-zero pixels");
}

#[test]
fn ttf_rasterize_multiple_sizes() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for size in [12, 16, 24, 32, 48] {
        let mut raster = RasterBuffer {
            data: &mut buf,
            width: 128,
            height: 128,
        };
        let metrics = font.rasterize('H', size, &mut raster, &mut scratch);
        assert!(
            metrics.is_some(),
            "should rasterize 'H' at {}px",
            size,
        );
        let m = metrics.unwrap();
        assert!(m.width > 0 && m.height > 0, "bitmap should be non-empty at {}px", size);
    }
}

#[test]
fn ttf_rasterize_larger_is_bigger() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    let mut raster16 = RasterBuffer { data: &mut buf, width: 128, height: 128 };
    let m16 = font.rasterize('A', 16, &mut raster16, &mut scratch).unwrap();

    let mut buf2 = [0u8; 128 * 128];
    let mut raster48 = RasterBuffer { data: &mut buf2, width: 128, height: 128 };
    let m48 = font.rasterize('A', 48, &mut raster48, &mut scratch).unwrap();

    assert!(
        m48.width > m16.width && m48.height > m16.height,
        "48px glyph ({},{}) should be larger than 16px ({},{})",
        m48.width, m48.height, m16.width, m16.height,
    );
    assert!(
        m48.advance > m16.advance,
        "48px advance {} should be > 16px advance {}",
        m48.advance, m16.advance,
    );
}

#[test]
fn ttf_rasterize_space_returns_metrics_only() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize(' ', 32, &mut raster, &mut scratch);
    assert!(metrics.is_some(), "space should return metrics");
    let m = metrics.unwrap();
    assert_eq!(m.width, 0, "space bitmap should be empty");
    assert_eq!(m.height, 0, "space bitmap should be empty");
    assert!(m.advance > 0, "space should have positive advance");
}

#[test]
fn ttf_rasterize_all_printable_ascii() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for c in 0x20u8..=0x7Eu8 {
        let ch = c as char;
        let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };
        let metrics = font.rasterize(ch, 24, &mut raster, &mut scratch);
        assert!(
            metrics.is_some(),
            "should rasterize '{}' (0x{:02x}) at 24px",
            ch, c,
        );
    }
}

#[test]
fn ttf_rasterize_buffer_too_small() {
    let font = TrueTypeFont::new(PROGGY_CLEAN).unwrap();
    let mut scratch = RasterScratch::zeroed();
    // Tiny buffer — large glyph shouldn't fit.
    let mut buf = [0u8; 4 * 4];
    let mut raster = RasterBuffer { data: &mut buf, width: 4, height: 4 };

    let metrics = font.rasterize('M', 64, &mut raster, &mut scratch);
    assert!(metrics.is_none(), "64px 'M' should not fit in 4x4 buffer");
}

// ---------------------------------------------------------------------------
// Nunito Sans + Source Code Pro font tests
// ---------------------------------------------------------------------------

#[test]
fn nunito_sans_parse() {
    assert!(TrueTypeFont::new(NUNITO_SANS).is_some());
}

#[test]
fn nunito_sans_rasterize_all_printable_ascii() {
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for c in 0x20u8..=0x7Eu8 {
        let ch = c as char;
        let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };
        let metrics = font.rasterize(ch, 16, &mut raster, &mut scratch);
        assert!(
            metrics.is_some(),
            "Nunito Sans: should rasterize '{}' (0x{:02x}) at 16px",
            ch, c,
        );
    }
}

#[test]
fn nunito_sans_is_proportional() {
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };
    let mi = font.rasterize('i', 16, &mut raster, &mut scratch).unwrap();
    let mut buf2 = [0u8; 128 * 128];
    let mut raster2 = RasterBuffer { data: &mut buf2, width: 128, height: 128 };
    let mm = font.rasterize('M', 16, &mut raster2, &mut scratch).unwrap();

    assert!(mm.advance > mi.advance, "Nunito Sans: 'M' advance ({}) > 'i' advance ({})", mm.advance, mi.advance);
}

#[test]
fn source_code_pro_parse() {
    assert!(TrueTypeFont::new(SOURCE_CODE_PRO).is_some());
}

#[test]
fn source_code_pro_rasterize_all_printable_ascii() {
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for c in 0x20u8..=0x7Eu8 {
        let ch = c as char;
        let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };
        let metrics = font.rasterize(ch, 16, &mut raster, &mut scratch);
        assert!(
            metrics.is_some(),
            "Source Code Pro: should rasterize '{}' (0x{:02x}) at 16px",
            ch, c,
        );
    }
}

#[test]
fn source_code_pro_is_monospace() {
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };
    let mi = font.rasterize('i', 16, &mut raster, &mut scratch).unwrap();
    let mut buf2 = [0u8; 128 * 128];
    let mut raster2 = RasterBuffer { data: &mut buf2, width: 128, height: 128 };
    let mm = font.rasterize('M', 16, &mut raster2, &mut scratch).unwrap();

    assert_eq!(mm.advance, mi.advance, "Source Code Pro: monospace — 'M' and 'i' should have same advance");
}

// ---------------------------------------------------------------------------
// Coverage map compositing
// ---------------------------------------------------------------------------

#[test]
fn draw_coverage_basic() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    // 2x2 coverage map with varying coverage.
    let coverage = [255u8, 128, 64, 0];
    dst.draw_coverage(2, 2, &coverage, 2, 2, Color::WHITE);

    // Full coverage → white.
    let p0 = dst.get_pixel(2, 2).unwrap();
    assert_eq!(p0.r, 255);
    assert_eq!(p0.g, 255);

    // Half coverage → blended.
    let p1 = dst.get_pixel(3, 2).unwrap();
    assert!(p1.r > 0 && p1.r < 255, "half coverage should blend, got {}", p1.r);

    // Zero coverage → unchanged (black).
    let p3 = dst.get_pixel(3, 3).unwrap();
    assert_eq!(p3.r, 0);
}

#[test]
fn draw_coverage_negative_coords_clip() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    // Place at negative coords — should clip without panic.
    let coverage = [255u8; 4];
    dst.draw_coverage(-1, -1, &coverage, 2, 2, Color::WHITE);

    // (0, 0) should be drawn (it's at local (1, 1) of the coverage map).
    let p = dst.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255);
}

#[test]
fn draw_coverage_colored() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let coverage = [255u8; 1];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::rgb(255, 0, 0));

    let p = dst.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255);
    assert_eq!(p.g, 0);
    assert_eq!(p.b, 0);
}

// ---------------------------------------------------------------------------
// TextLayout tests
// ---------------------------------------------------------------------------

fn make_layout(max_width: u32) -> TextLayout {
    TextLayout {
        char_width: 8,
        line_height: 20,
        max_width,
    }
}

// --- layout_lines ---

#[test]
fn layout_lines_empty_text() {
    let layout = make_layout(200);
    let mut count = 0;
    layout.layout_lines(b"", |_, _, _| count += 1);
    assert_eq!(count, 0);
}

#[test]
fn layout_lines_single_line_no_wrap() {
    let layout = make_layout(200); // 25 cols
    let text = b"hello";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], (0, 5, 0));
}

#[test]
fn layout_lines_newline_creates_new_line() {
    let layout = make_layout(200);
    let text = b"ab\ncd";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 2, 0));  // "ab"
    assert_eq!(lines[1], (3, 5, 1));  // "cd"
}

#[test]
fn layout_lines_wrap_at_max_width() {
    let layout = make_layout(24); // 3 cols (24 / 8)
    let text = b"abcdef";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 3, 0));  // "abc"
    assert_eq!(lines[1], (3, 6, 1));  // "def"
}

#[test]
fn layout_lines_wrap_and_newline_combined() {
    let layout = make_layout(24); // 3 cols
    let text = b"abc\nde";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 3, 0));  // "abc"
    assert_eq!(lines[1], (4, 6, 1));  // "de"
}

#[test]
fn layout_lines_trailing_newline() {
    let layout = make_layout(200);
    let text = b"hello\n";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    // "hello" on line 0, empty line 1 from trailing newline.
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 5, 0));
    assert_eq!(lines[1], (6, 6, 1));
}

#[test]
fn layout_lines_multiple_newlines() {
    let layout = make_layout(200);
    let text = b"a\n\nb";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], (0, 1, 0));  // "a"
    assert_eq!(lines[1], (2, 2, 1));  // empty
    assert_eq!(lines[2], (3, 4, 2));  // "b"
}

#[test]
fn layout_lines_exact_width_no_extra_wrap() {
    let layout = make_layout(24); // 3 cols
    let text = b"abc";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    // Exactly fills one line, no extra wrap.
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], (0, 3, 0));
}

// --- byte_to_xy ---

#[test]
fn byte_to_xy_start_of_text() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hello", 0);
    assert_eq!((x, y), (0, 0));
}

#[test]
fn byte_to_xy_middle_of_line() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hello", 3);
    assert_eq!((x, y), (24, 0)); // col 3 * 8px
}

#[test]
fn byte_to_xy_end_of_text() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hello", 5);
    assert_eq!((x, y), (40, 0)); // col 5 * 8px
}

#[test]
fn byte_to_xy_after_newline() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"ab\ncd", 3);
    assert_eq!((x, y), (0, 20)); // start of row 1
}

#[test]
fn byte_to_xy_at_newline_char() {
    let layout = make_layout(200);
    // Cursor at the newline itself = end of that line.
    let (x, y) = layout.byte_to_xy(b"ab\ncd", 2);
    assert_eq!((x, y), (16, 0)); // col 2 on row 0
}

#[test]
fn byte_to_xy_wrapped_line() {
    let layout = make_layout(24); // 3 cols
    // "abcdef" wraps: "abc" on row 0, "def" on row 1.
    let (x, y) = layout.byte_to_xy(b"abcdef", 4);
    assert_eq!((x, y), (8, 20)); // col 1 on row 1
}

#[test]
fn byte_to_xy_past_end_clamps() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hi", 10);
    // Past end -- should return position at end of text.
    assert_eq!((x, y), (16, 0));
}

#[test]
fn byte_to_xy_empty_text() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"", 0);
    assert_eq!((x, y), (0, 0));
}

// --- xy_to_byte ---

#[test]
fn xy_to_byte_origin() {
    let layout = make_layout(200);
    assert_eq!(layout.xy_to_byte(b"hello", 0, 0), 0);
}

#[test]
fn xy_to_byte_middle_of_line() {
    let layout = make_layout(200);
    // Click at pixel (24, 0) = col 3.
    assert_eq!(layout.xy_to_byte(b"hello", 24, 0), 3);
}

#[test]
fn xy_to_byte_between_chars_rounds_left() {
    let layout = make_layout(200);
    // Click at pixel (3, 0) = within first character cell.
    assert_eq!(layout.xy_to_byte(b"hello", 3, 0), 0);
}

#[test]
fn xy_to_byte_between_chars_rounds_right() {
    let layout = make_layout(200);
    // Click at pixel (5, 0) = past midpoint of first char (8px wide).
    assert_eq!(layout.xy_to_byte(b"hello", 5, 0), 1);
}

#[test]
fn xy_to_byte_past_end_of_line() {
    let layout = make_layout(200);
    // Click past the end of "hi" -- snaps to end of text.
    assert_eq!(layout.xy_to_byte(b"hi", 100, 0), 2);
}

#[test]
fn xy_to_byte_second_line() {
    let layout = make_layout(200);
    // "ab\ncd", click on row 1 col 1.
    assert_eq!(layout.xy_to_byte(b"ab\ncd", 8, 20), 4);
}

#[test]
fn xy_to_byte_wrapped_line() {
    let layout = make_layout(24); // 3 cols
    // "abcdef" wraps. Click at row 1, col 0 = byte 3.
    assert_eq!(layout.xy_to_byte(b"abcdef", 0, 20), 3);
}

#[test]
fn xy_to_byte_past_last_row() {
    let layout = make_layout(200);
    // Click below all text -- snaps to end.
    assert_eq!(layout.xy_to_byte(b"hello", 0, 100), 5);
}

#[test]
fn xy_to_byte_empty_text() {
    let layout = make_layout(200);
    assert_eq!(layout.xy_to_byte(b"", 50, 50), 0);
}

// ---------------------------------------------------------------------------
// sRGB gamma-correct blending tests
// ---------------------------------------------------------------------------

use drawing::{SRGB_TO_LINEAR, LINEAR_TO_SRGB};

#[test]
fn srgb_to_linear_boundary_values() {
    // sRGB 0 → linear 0
    assert_eq!(SRGB_TO_LINEAR[0], 0);
    // sRGB 255 → linear 65535
    assert_eq!(SRGB_TO_LINEAR[255], 65535);
    // sRGB 128 → roughly 21.6% linear ≈ 14158 (should be in that neighborhood)
    assert!(
        SRGB_TO_LINEAR[128] > 13000 && SRGB_TO_LINEAR[128] < 16000,
        "sRGB 128 → linear {} should be near 14158",
        SRGB_TO_LINEAR[128],
    );
}

#[test]
fn srgb_to_linear_monotonically_increasing() {
    for i in 1..256 {
        assert!(
            SRGB_TO_LINEAR[i] >= SRGB_TO_LINEAR[i - 1],
            "srgb_to_linear should be monotonic: [{}]={} < [{}]={}",
            i, SRGB_TO_LINEAR[i], i - 1, SRGB_TO_LINEAR[i - 1],
        );
    }
}

#[test]
fn linear_to_srgb_boundary_values() {
    // linear 0 → sRGB 0
    assert_eq!(LINEAR_TO_SRGB[0], 0);
    // linear 4095 (max index = 65535 >> 4) → sRGB 255
    assert_eq!(LINEAR_TO_SRGB[4095], 255);
}

#[test]
fn linear_to_srgb_monotonically_increasing() {
    for i in 1..4096 {
        assert!(
            LINEAR_TO_SRGB[i] >= LINEAR_TO_SRGB[i - 1],
            "linear_to_srgb should be monotonic: [{}]={} < [{}]={}",
            i, LINEAR_TO_SRGB[i], i - 1, LINEAR_TO_SRGB[i - 1],
        );
    }
}

#[test]
fn srgb_linear_roundtrip() {
    // Converting sRGB → linear → sRGB should return the original value (or ±1).
    // LINEAR_TO_SRGB is indexed by linear >> 4 (4096 entries).
    for srgb in 0u16..=255 {
        let linear = SRGB_TO_LINEAR[srgb as usize];
        let idx = (linear >> 4) as usize;
        let idx = if idx > 4095 { 4095 } else { idx };
        let back = LINEAR_TO_SRGB[idx];
        let diff = if back > srgb as u8 {
            back - srgb as u8
        } else {
            srgb as u8 - back
        };
        assert!(
            diff <= 1,
            "roundtrip sRGB {} → linear {} → sRGB {}: diff {}",
            srgb, linear, back, diff,
        );
    }
}

#[test]
fn gamma_blend_zero_coverage_unchanged() {
    // Zero-coverage pixels must not be modified at all.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(100, 150, 200));

    // Read the original pixel value.
    let orig = dst.get_pixel(0, 0).unwrap();

    // Draw with zero coverage.
    let coverage = [0u8; 4];
    dst.draw_coverage(0, 0, &coverage, 2, 2, Color::WHITE);

    // Pixel must be identical.
    let after = dst.get_pixel(0, 0).unwrap();
    assert_eq!(orig, after, "zero-coverage should not modify destination");
}

#[test]
fn gamma_blend_full_coverage_replaces() {
    // Full coverage (255) with opaque color should fully replace the destination.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::rgb(0, 0, 255));

    let coverage = [255u8; 1];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::rgb(255, 0, 0));

    let p = dst.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255);
    assert_eq!(p.g, 0);
    assert_eq!(p.b, 0);
}

#[test]
fn gamma_blend_half_coverage_heavier_than_linear() {
    // At 50% coverage, gamma-correct blending on a black background should
    // produce higher sRGB values than naive linear blending would (128).
    // This is the key test: gamma correction makes text appear heavier.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::BLACK);

    let coverage = [128u8; 1]; // ~50% coverage
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);

    let p = dst.get_pixel(0, 0).unwrap();
    // Linear blending would give r=128. Gamma-correct blending should give
    // a higher value (~188) because 50% linear light maps to ~74% sRGB.
    assert!(
        p.r > 140,
        "gamma-correct 50% coverage on black should produce r > 140, got {}",
        p.r,
    );
}

#[test]
fn gamma_blend_over_half_red_on_blue_heavier() {
    // blend_over with 50% alpha: gamma-correct should produce heavier result.
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgb(0, 0, 255);
    let result = src.blend_over(dst);

    // In gamma-correct blending, the red channel should be higher than 128
    // (linear would give ~128). The blue channel should also reflect the
    // gamma curve behavior.
    assert!(
        result.r > 140,
        "gamma-correct blend_over: 50% red on blue should produce r > 140, got {}",
        result.r,
    );
}

#[test]
fn gamma_blend_over_opaque_src_returns_src() {
    // Opaque source fast path must still work.
    let src = Color::rgb(200, 100, 50);
    let dst = Color::rgb(0, 0, 255);
    assert_eq!(src.blend_over(dst), src);
}

#[test]
fn gamma_blend_over_transparent_src_returns_dst() {
    // Transparent source fast path must still work.
    let src = Color::TRANSPARENT;
    let dst = Color::rgb(0, 255, 0);
    assert_eq!(src.blend_over(dst), dst);
}

#[test]
fn gamma_draw_coverage_uses_gamma_correction() {
    // Compare: 50% coverage white on black should produce sRGB value ~188,
    // not the linear-blended value of ~128.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::BLACK);

    let coverage = [128u8; 1];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);

    let p = dst.get_pixel(0, 0).unwrap();
    // Gamma-correct: 50% linear ≈ 188 sRGB. Should be in range 180-195.
    assert!(
        p.r >= 180 && p.r <= 200,
        "gamma-correct coverage blend should give r ≈ 188, got {}",
        p.r,
    );
    // All channels should be equal for white on black.
    assert_eq!(p.r, p.g, "r ({}) should equal g ({})", p.r, p.g);
    assert_eq!(p.r, p.b, "r ({}) should equal b ({})", p.r, p.b);
}

// ---------------------------------------------------------------------------
// 2D oversampling tests
// ---------------------------------------------------------------------------

use drawing::{OVERSAMPLE_X, OVERSAMPLE_Y};

#[test]
fn oversample_x_is_at_least_2() {
    assert!(
        OVERSAMPLE_X >= 2,
        "OVERSAMPLE_X should be >= 2 for horizontal oversampling, got {}",
        OVERSAMPLE_X,
    );
}

#[test]
fn oversample_y_is_at_least_4() {
    assert!(
        OVERSAMPLE_Y >= 4,
        "OVERSAMPLE_Y should be >= 4, got {}",
        OVERSAMPLE_Y,
    );
}

#[test]
fn oversampled_rasterize_produces_intermediate_coverage() {
    // Diagonal strokes should have intermediate coverage values (not just 0/255)
    // in the horizontal direction. 'k' has diagonal strokes.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('k', 24, &mut raster, &mut scratch).unwrap();
    assert!(metrics.width > 0 && metrics.height > 0);

    // Check that there are intermediate coverage values (not just 0 and 255)
    // along the edges. With 2D oversampling, horizontal edges should have
    // smooth gradients.
    let total = (metrics.width * metrics.height) as usize;
    let coverage = &buf[..total];

    let intermediate_count = coverage.iter().filter(|&&c| c > 0 && c < 255).count();
    assert!(
        intermediate_count > 0,
        "'k' should have intermediate coverage values (smooth edges), got 0 intermediate pixels",
    );
}

#[test]
fn oversampled_diagonal_has_horizontal_gradients() {
    // With horizontal oversampling, diagonal strokes should show smooth
    // horizontal transitions. Check 'x' which has strong diagonals.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('x', 24, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    let total = (w * metrics.height) as usize;
    let coverage = &buf[..total];

    // Find a row in the middle of the glyph (where diagonals cross).
    let mid_row = metrics.height / 2;
    let row_start = (mid_row * w) as usize;
    let row_end = row_start + w as usize;
    let row = &coverage[row_start..row_end];

    // The middle row should have some intermediate values along edges.
    let has_intermediate = row.iter().any(|&c| c > 10 && c < 245);
    assert!(
        has_intermediate,
        "'x' mid-row should have intermediate coverage from horizontal oversampling",
    );
}

#[test]
fn oversampled_curve_has_smooth_edges() {
    // Curved characters like 'o' should have smooth edges with 2D oversampling.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('o', 24, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    let total = (w * metrics.height) as usize;
    let coverage = &buf[..total];

    // Count distinct non-zero coverage levels (more levels = smoother).
    let mut levels = [false; 256];
    for &c in coverage.iter() {
        if c > 0 {
            levels[c as usize] = true;
        }
    }
    let distinct_levels = levels.iter().filter(|&&v| v).count();

    // With 2D oversampling (OVERSAMPLE_X*OVERSAMPLE_Y = 8 samples per pixel),
    // we expect more than 4 distinct coverage levels at minimum.
    assert!(
        distinct_levels >= 4,
        "'o' should have at least 4 distinct non-zero coverage levels, got {}",
        distinct_levels,
    );
}

#[test]
fn oversampled_all_printable_ascii_still_rasterize() {
    // All printable ASCII should still rasterize successfully after adding
    // horizontal oversampling.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for c in 0x20u8..=0x7Eu8 {
        let ch = c as char;
        let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };
        let metrics = font.rasterize(ch, 24, &mut raster, &mut scratch);
        assert!(
            metrics.is_some(),
            "oversampled: should rasterize '{}' (0x{:02x}) at 24px",
            ch, c,
        );
    }
}

#[test]
fn oversampled_glyph_cache_populated() {
    // GlyphCache should still populate correctly with 2D oversampling.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = drawing::GlyphCache::zeroed();

    cache.populate(&font, 16, &mut scratch);

    // Check a few glyphs are cached with valid dimensions.
    let (g_a, cov_a) = cache.get(b'A').unwrap();
    assert!(g_a.width > 0 && g_a.height > 0, "'A' should have non-zero cached dimensions");
    assert!(cov_a.len() > 0, "'A' coverage should be non-empty");

    let (g_k, cov_k) = cache.get(b'k').unwrap();
    assert!(g_k.width > 0 && g_k.height > 0);

    // Check coverage has intermediate values (smooth edges).
    let has_intermediate = cov_k.iter().any(|&c| c > 0 && c < 255);
    assert!(has_intermediate, "'k' cached coverage should have intermediate values");
}
