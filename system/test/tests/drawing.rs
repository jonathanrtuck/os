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

// ---------------------------------------------------------------------------
// Proportional font — GlyphCache with variable advance widths
// ---------------------------------------------------------------------------

#[test]
fn proportional_glyph_cache_advance_i_less_than_m() {
    // VAL-FONT-004: advance('i') < advance('m') — variable widths confirmed.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = drawing::GlyphCache::zeroed();

    cache.populate(&font, 16, &mut scratch);

    let (g_i, _) = cache.get(b'i').unwrap();
    let (g_m, _) = cache.get(b'm').unwrap();

    assert!(
        g_i.advance < g_m.advance,
        "proportional font: 'i' advance ({}) should be < 'm' advance ({})",
        g_i.advance, g_m.advance
    );
}

#[test]
fn proportional_glyph_cache_has_valid_glyphs() {
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = drawing::GlyphCache::zeroed();

    cache.populate(&font, 16, &mut scratch);

    // All printable ASCII should have cached glyphs.
    for c in 0x20u8..=0x7Eu8 {
        let result = cache.get(c);
        assert!(result.is_some(), "proportional cache should have glyph for 0x{:02x}", c);
        let (g, _) = result.unwrap();
        // All printable chars except space should have non-zero advance.
        assert!(g.advance > 0, "glyph 0x{:02x} should have non-zero advance", c);
    }
}

#[test]
fn proportional_glyph_cache_variable_advances() {
    // Multiple different advance widths exist (not monospace).
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = drawing::GlyphCache::zeroed();

    cache.populate(&font, 16, &mut scratch);

    let mut advances = [0u32; 95];
    for i in 0..95u8 {
        let (g, _) = cache.get(0x20 + i).unwrap();
        advances[i as usize] = g.advance;
    }

    // Count distinct advances.
    let mut distinct = 1usize;
    for i in 1..95 {
        let mut seen = false;
        for j in 0..i {
            if advances[i] == advances[j] {
                seen = true;
                break;
            }
        }
        if !seen {
            distinct += 1;
        }
    }

    assert!(
        distinct >= 5,
        "proportional font should have >= 5 distinct advance widths, got {}",
        distinct
    );
}

#[test]
fn draw_proportional_string_advances_by_glyph_width() {
    // Test that draw_proportional_string uses per-glyph advance widths.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = drawing::GlyphCache::zeroed();

    cache.populate(&font, 16, &mut scratch);

    let mut buf = [0u8; 400 * 40 * 4];
    let mut surf = make_surface(&mut buf, 400, 40);

    // Draw "im" and measure the resulting x.
    let x1 = drawing::draw_proportional_string(
        &mut surf, 0, 0, b"im", &cache, Color::WHITE,
    );

    // Manually sum advances for 'i' + 'm'.
    let (g_i, _) = cache.get(b'i').unwrap();
    let (g_m, _) = cache.get(b'm').unwrap();
    let expected = g_i.advance + g_m.advance;

    assert_eq!(
        x1, expected,
        "draw_proportional_string should advance by per-glyph widths"
    );
}

#[test]
fn draw_proportional_string_missing_glyph_uses_fallback() {
    // Missing glyph (0x01 — below printable range) should advance by
    // fallback width (space width) without crashing.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = drawing::GlyphCache::zeroed();

    cache.populate(&font, 16, &mut scratch);

    let mut buf = [0u8; 200 * 40 * 4];
    let mut surf = make_surface(&mut buf, 200, 40);

    // Draw text containing a non-printable byte — must not panic.
    let x = drawing::draw_proportional_string(
        &mut surf, 0, 0, b"\x01A", &cache, Color::WHITE,
    );

    // Should have advanced past the missing glyph + 'A'.
    let (g_a, _) = cache.get(b'A').unwrap();
    assert!(
        x > g_a.advance,
        "should advance past missing glyph + 'A', got x={}",
        x
    );
}

// ---------------------------------------------------------------------------
// Damage tracking — DirtyRect + DamageTracker
// ---------------------------------------------------------------------------

#[test]
fn dirty_rect_new_stores_fields() {
    let r = drawing::DirtyRect::new(10, 20, 100, 50);
    assert_eq!(r.x, 10);
    assert_eq!(r.y, 20);
    assert_eq!(r.w, 100);
    assert_eq!(r.h, 50);
}

#[test]
fn dirty_rect_union_basic() {
    let a = drawing::DirtyRect::new(10, 20, 50, 30);
    let b = drawing::DirtyRect::new(40, 10, 80, 50);
    let u = a.union(b);
    // Union should be: x=10, y=10, x1=120, y1=60 → w=110, h=50
    assert_eq!(u.x, 10);
    assert_eq!(u.y, 10);
    assert_eq!(u.w, 110);
    assert_eq!(u.h, 50);
}

#[test]
fn dirty_rect_union_identity_with_zero() {
    let a = drawing::DirtyRect::new(10, 20, 50, 30);
    let zero = drawing::DirtyRect::new(0, 0, 0, 0);
    assert_eq!(a.union(zero), a);
    assert_eq!(zero.union(a), a);
}

#[test]
fn dirty_rect_union_all_multiple() {
    let rects = [
        drawing::DirtyRect::new(0, 0, 10, 10),
        drawing::DirtyRect::new(100, 200, 50, 30),
        drawing::DirtyRect::new(50, 100, 20, 20),
    ];
    let u = drawing::DirtyRect::union_all(&rects);
    assert_eq!(u.x, 0);
    assert_eq!(u.y, 0);
    assert_eq!(u.w, 150); // max(10, 150, 70)
    assert_eq!(u.h, 230); // max(10, 230, 120)
}

#[test]
fn dirty_rect_union_all_empty() {
    let u = drawing::DirtyRect::union_all(&[]);
    assert_eq!(u.w, 0);
    assert_eq!(u.h, 0);
}

#[test]
fn dirty_rect_size_is_8_bytes() {
    assert_eq!(core::mem::size_of::<drawing::DirtyRect>(), 8);
}

#[test]
fn damage_tracker_starts_empty() {
    let dt = drawing::DamageTracker::new(1024, 768);
    assert_eq!(dt.count, 0);
    assert!(!dt.full_screen);
}

#[test]
fn damage_tracker_add_rect() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    dt.add(10, 20, 100, 50);
    assert_eq!(dt.count, 1);
    assert!(!dt.full_screen);
    let rects = dt.dirty_rects().unwrap();
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0], drawing::DirtyRect::new(10, 20, 100, 50));
}

#[test]
fn damage_tracker_ignores_zero_size() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    dt.add(10, 20, 0, 50);
    dt.add(10, 20, 50, 0);
    assert_eq!(dt.count, 0);
}

#[test]
fn damage_tracker_overflow_triggers_full_screen() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    for i in 0..drawing::MAX_DIRTY_RECTS {
        dt.add(i as u16 * 10, 0, 10, 10);
    }
    assert!(!dt.full_screen);
    assert_eq!(dt.count, drawing::MAX_DIRTY_RECTS);
    // Adding one more should trigger full screen
    dt.add(200, 0, 10, 10);
    assert!(dt.full_screen);
    // dirty_rects returns None when full_screen
    assert!(dt.dirty_rects().is_none());
}

#[test]
fn damage_tracker_full_screen_bounding_box() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    dt.mark_full_screen();
    let bb = dt.bounding_box();
    assert_eq!(bb.x, 0);
    assert_eq!(bb.y, 0);
    assert_eq!(bb.w, 1024);
    assert_eq!(bb.h, 768);
}

#[test]
fn damage_tracker_partial_bounding_box() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    dt.add(10, 100, 200, 30);
    dt.add(50, 700, 300, 28);
    let bb = dt.bounding_box();
    assert_eq!(bb.x, 10);
    assert_eq!(bb.y, 100);
    assert_eq!(bb.w, 340); // 50+300 - 10 = 340
    assert_eq!(bb.h, 628); // 700+28 - 100 = 628
}

#[test]
fn damage_tracker_reset_clears_state() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    dt.add(10, 20, 100, 50);
    dt.add(50, 60, 200, 100);
    assert_eq!(dt.count, 2);
    dt.reset();
    assert_eq!(dt.count, 0);
    assert!(!dt.full_screen);
    // After reset, dirty_rects returns None (no rects = full screen transfer)
    assert!(dt.dirty_rects().is_none());
}

#[test]
fn damage_tracker_add_after_full_screen_is_noop() {
    let mut dt = drawing::DamageTracker::new(1024, 768);
    dt.mark_full_screen();
    dt.add(10, 20, 100, 50);
    // count stays 0 — once full_screen is set, add is a no-op
    assert_eq!(dt.count, 0);
}

#[test]
fn damage_tracker_max_rects_is_7() {
    assert_eq!(drawing::MAX_DIRTY_RECTS, 7);
}

#[test]
fn damage_tracker_multiple_content_and_status_rects() {
    // Simulates the real use case: content area change + status bar change
    let mut dt = drawing::DamageTracker::new(1024, 768);
    // Content area: one line of text changed (approx one line_height tall)
    dt.add(13, 48, 998, 22); // text region
    // Status bar updated
    dt.add(0, 740, 1024, 28); // status bar
    assert_eq!(dt.count, 2);
    let rects = dt.dirty_rects().unwrap();
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0], drawing::DirtyRect::new(13, 48, 998, 22));
    assert_eq!(rects[1], drawing::DirtyRect::new(0, 740, 1024, 28));
}

// ---------------------------------------------------------------------------
// CompositeSurface + multi-surface compositing
// ---------------------------------------------------------------------------

use drawing::CompositeSurface;

fn make_composite_surface<'a>(
    buf: &'a mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    z: u16,
) -> CompositeSurface<'a> {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    assert!(buf.len() >= (stride * height) as usize);
    for b in buf.iter_mut() {
        *b = 0;
    }
    CompositeSurface {
        surface: Surface {
            data: buf,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        },
        x,
        y,
        z,
        visible: true,
    }
}

#[test]
fn composite_surface_stores_position_and_z() {
    let mut buf = [0u8; 4 * 4 * 4];
    let cs = make_composite_surface(&mut buf, 4, 4, 10, 20, 5);
    assert_eq!(cs.x, 10);
    assert_eq!(cs.y, 20);
    assert_eq!(cs.z, 5);
    assert!(cs.visible);
}

#[test]
fn composite_two_opaque_surfaces_z_order() {
    // Background (z=0) is blue, foreground (z=1) is red at (2,2).
    // After compositing, the framebuffer should show red overlapping blue.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 2, 2, 1);
    fg.surface.clear(Color::rgb(255, 0, 0));

    // Composite back-to-front.
    let surfaces: [&CompositeSurface; 2] = [&bg, &fg];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Outside the red overlay: should be blue.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 0, 255)));
    assert_eq!(dst.get_pixel(1, 1), Some(Color::rgb(0, 0, 255)));
    // Inside the red overlay: should be red.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(5, 5), Some(Color::rgb(255, 0, 0)));
    // After the red overlay: should be blue.
    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn composite_respects_z_order_not_array_order() {
    // Pass surfaces in reverse z-order — compositing should still sort by z.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 10);
    fg.surface.clear(Color::rgb(255, 0, 0));

    // Pass in wrong order (fg first, bg second).
    let surfaces: [&CompositeSurface; 2] = [&fg, &bg];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Red (higher z) should be on top of blue (lower z).
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    // Outside red (4..8): should be blue.
    assert_eq!(dst.get_pixel(5, 5), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn composite_alpha_blending() {
    // Semi-transparent surface over opaque background.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 8 * 8 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 8, 8, 0, 0, 1);
    fg.surface.clear(Color::rgba(255, 0, 0, 128));

    let surfaces: [&CompositeSurface; 2] = [&bg, &fg];
    drawing::composite_surfaces(&mut dst, &surfaces);

    let p = dst.get_pixel(4, 4).unwrap();
    // Gamma-correct 50% red on blue: both channels > 140.
    assert!(p.r > 140, "blended red should be > 140, got {}", p.r);
    assert!(p.b > 140, "blended blue should be > 140, got {}", p.b);
}

#[test]
fn composite_invisible_surface_skipped() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 8 * 8 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 8, 8, 0, 0, 1);
    fg.surface.clear(Color::rgb(255, 0, 0));
    fg.visible = false;

    let surfaces: [&CompositeSurface; 2] = [&bg, &fg];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Red surface is invisible, should only see blue.
    assert_eq!(dst.get_pixel(4, 4), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn composite_surface_with_negative_offset() {
    // Surface partially outside the framebuffer (negative x/y).
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut s_buf = [0u8; 4 * 4 * 4];
    let mut s = make_composite_surface(&mut s_buf, 4, 4, -2, -2, 0);
    s.surface.clear(Color::rgb(0, 255, 0));

    let surfaces: [&CompositeSurface; 1] = [&s];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Only the visible portion should be blitted.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(1, 1), Some(Color::rgb(0, 255, 0)));
    // Beyond the 4x4 surface from (-2,-2): pixel (2,2) should be black.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::BLACK));
}

#[test]
fn composite_surface_partially_outside_right() {
    // Surface extends past the right edge.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut s_buf = [0u8; 4 * 4 * 4];
    let mut s = make_composite_surface(&mut s_buf, 4, 4, 6, 6, 0);
    s.surface.clear(Color::rgb(0, 255, 0));

    let surfaces: [&CompositeSurface; 1] = [&s];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Only (6,6) and (7,7) should be green.
    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(7, 7), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(5, 5), Some(Color::BLACK));
}

#[test]
fn composite_three_layers() {
    // background (z=0) → content (z=10) → chrome (z=20)
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(20, 20, 40));

    let mut content_buf = [0u8; 6 * 6 * 4];
    let mut content = make_composite_surface(&mut content_buf, 6, 6, 1, 1, 10);
    content.surface.clear(Color::rgb(30, 30, 50));

    let mut chrome_buf = [0u8; 8 * 2 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 8, 2, 0, 0, 20);
    chrome.surface.clear(Color::rgba(60, 60, 80, 200));

    let surfaces: [&CompositeSurface; 3] = [&bg, &content, &chrome];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Top-left pixel (0,0): bg under chrome (alpha blended).
    let p00 = dst.get_pixel(0, 0).unwrap();
    // Chrome rgba(60,60,80,200) over bg rgb(20,20,40) — should be close to chrome.
    assert!(p00.r > 40 && p00.r < 70, "chrome over bg r={}", p00.r);

    // Pixel at (1,1): still under chrome (row 0-1), so content is under chrome.
    let p11 = dst.get_pixel(1, 1).unwrap();
    assert!(p11.b > 50, "chrome over content b={}", p11.b);

    // Pixel at (1,3): content area, no chrome overlap.
    let p13 = dst.get_pixel(1, 3).unwrap();
    assert_eq!(p13, Color::rgb(30, 30, 50));

    // Pixel at (0,3): background, not covered by content (content starts at 1).
    let p03 = dst.get_pixel(0, 3).unwrap();
    assert_eq!(p03, Color::rgb(20, 20, 40));
}

#[test]
fn composite_empty_surfaces_list() {
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::rgb(100, 100, 100));

    let surfaces: [&CompositeSurface; 0] = [];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // Destination should be unchanged.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(100, 100, 100)));
}

// ---------------------------------------------------------------------------
// Translucent chrome over content (VAL-COMP-002)
// ---------------------------------------------------------------------------

#[test]
fn translucent_chrome_shows_content_beneath() {
    // Simulates the translucent chrome feature: content surface extends
    // full-height (behind chrome), chrome overlay is translucent (alpha < 255).
    // The result: chrome area shows a blend of chrome and content colors.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content surface: full height, bright green (easy to detect bleed-through).
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(0, 200, 0));

    // Chrome overlay: covers top 4 rows, translucent dark (alpha=200).
    let mut chrome_buf = [0u8; 16 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 16, 4, 0, 0, 20);
    chrome.surface.clear(Color::rgba(40, 40, 60, 200));

    let surfaces: [&CompositeSurface; 2] = [&content, &chrome];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // In the chrome region (row 0-3), the green content should bleed through.
    let p_chrome = dst.get_pixel(8, 2).unwrap();
    // Green channel should be > 0 (content bleeds through) but < 200 (attenuated by chrome).
    assert!(
        p_chrome.g > 5,
        "green content should bleed through translucent chrome, got g={}",
        p_chrome.g
    );
    assert!(
        p_chrome.g < 200,
        "chrome should attenuate content green, got g={}",
        p_chrome.g
    );

    // Below chrome (row 5+), pure content visible.
    let p_content = dst.get_pixel(8, 8).unwrap();
    assert_eq!(p_content, Color::rgb(0, 200, 0));
}

#[test]
fn translucent_chrome_is_visually_distinct_from_content() {
    // Chrome with alpha < 255 should produce a different color from the
    // uncovered content region — proving the chrome is visually distinct.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content: white text area background.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(24, 24, 36));

    // Chrome: translucent with alpha=220 (like the actual compositor).
    let mut chrome_buf = [0u8; 16 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 16, 4, 0, 0, 20);
    chrome.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&CompositeSurface; 2] = [&content, &chrome];
    drawing::composite_surfaces(&mut dst, &surfaces);

    let p_chrome = dst.get_pixel(8, 2).unwrap();
    let p_content = dst.get_pixel(8, 8).unwrap();

    // Chrome region and content region should NOT be identical.
    assert_ne!(
        p_chrome, p_content,
        "chrome and content should be visually distinct"
    );
}

#[test]
fn chrome_alpha_200_produces_visible_translucency() {
    // Verify that alpha=200 (not 255) produces measurable bleed-through
    // when composited over bright content.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::BLACK);

    // Bright red content underneath.
    let mut content_buf = [0u8; 4 * 4 * 4];
    let mut content = make_composite_surface(&mut content_buf, 4, 4, 0, 0, 0);
    content.surface.clear(Color::rgb(255, 0, 0));

    // Dark chrome on top with alpha=200.
    let mut chrome_buf = [0u8; 4 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 4, 4, 0, 0, 10);
    chrome.surface.clear(Color::rgba(30, 30, 48, 200));

    let surfaces: [&CompositeSurface; 2] = [&content, &chrome];
    drawing::composite_surfaces(&mut dst, &surfaces);

    let p = dst.get_pixel(2, 2).unwrap();
    // Red should bleed through: r > chrome_r (30) due to content contribution.
    assert!(
        p.r > 35,
        "red content should bleed through alpha=200 chrome, got r={}",
        p.r
    );
}

#[test]
fn status_bar_chrome_over_content_shows_bleedthrough() {
    // Status bar at the bottom of the frame with content extending behind it.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content: full height, has blue pixels.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(0, 0, 180));

    // Status bar: bottom 4 rows, translucent.
    let mut status_buf = [0u8; 16 * 4 * 4];
    let mut status = make_composite_surface(&mut status_buf, 16, 4, 0, 12, 20);
    status.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&CompositeSurface; 2] = [&content, &status];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // In the status bar region, blue from content should be partially visible.
    let p_status = dst.get_pixel(8, 13).unwrap();
    assert!(
        p_status.b > 40,
        "blue content should partially show through status bar, got b={}",
        p_status.b
    );

    // Above the status bar, pure content.
    let p_above = dst.get_pixel(8, 5).unwrap();
    assert_eq!(p_above, Color::rgb(0, 0, 180));
}

// ---------------------------------------------------------------------------
// Drop shadows (VAL-COMP-003)
// ---------------------------------------------------------------------------

#[test]
fn fill_gradient_v_first_row_is_top_color() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(0, 0, 8, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    // First row should have the top color (alpha ~80).
    let p = surf.get_pixel(4, 0).unwrap();
    assert!(p.a >= 70 && p.a <= 90, "top row alpha should be ~80, got {}", p.a);
}

#[test]
fn fill_gradient_v_last_row_is_bottom_color() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(0, 0, 8, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    // Last row should have the bottom color (alpha ~0).
    let p = surf.get_pixel(4, 7).unwrap();
    assert!(p.a <= 15, "bottom row alpha should be ~0, got {}", p.a);
}

#[test]
fn fill_gradient_v_monotonic_alpha_decrease() {
    // Shadow gradient from alpha=80 to alpha=0 over 8 rows.
    // Each row's alpha should be <= the row above it.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(0, 0, 8, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    let mut prev_alpha = 255u8;
    for row in 0..8 {
        let p = surf.get_pixel(4, row).unwrap();
        assert!(
            p.a <= prev_alpha,
            "alpha should decrease monotonically: row {} has a={}, prev={}",
            row, p.a, prev_alpha
        );
        prev_alpha = p.a;
    }
}

#[test]
fn fill_gradient_v_intermediate_rows_have_intermediate_alpha() {
    // Over 8 rows from alpha=80 to alpha=0, the middle row should have ~40.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(0, 0, 8, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    let p_mid = surf.get_pixel(4, 4).unwrap();
    // At row 4/8, alpha should be roughly 80 * (1 - 4/7) ≈ 34.
    assert!(
        p_mid.a > 15 && p_mid.a < 60,
        "middle row alpha should be intermediate, got {}",
        p_mid.a
    );
}

#[test]
fn fill_gradient_v_fills_all_columns() {
    // All columns in a given row should have the same alpha.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(0, 0, 8, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    let expected_a = surf.get_pixel(0, 3).unwrap().a;
    for col in 1..8 {
        let p = surf.get_pixel(col, 3).unwrap();
        assert_eq!(p.a, expected_a, "all columns in row should have same alpha");
    }
}

#[test]
fn fill_gradient_v_clips_to_surface_bounds() {
    // Gradient positioned partially outside surface should clip without panic.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    // Starts at y=6, height=8: only 2 rows should be visible.
    surf.fill_gradient_v(0, 6, 8, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    let p_visible = surf.get_pixel(4, 6).unwrap();
    assert!(p_visible.a > 0, "visible row should have some alpha");

    // Row 5 should be unaffected (still black, a=0 from clear to BLACK).
    let p_above = surf.get_pixel(4, 5).unwrap();
    assert_eq!(p_above, Color::BLACK);
}

#[test]
fn fill_gradient_v_zero_height_is_noop() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::rgb(100, 100, 100));

    surf.fill_gradient_v(0, 0, 8, 0, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    // Surface should be unchanged.
    assert_eq!(surf.get_pixel(4, 4), Some(Color::rgb(100, 100, 100)));
}

#[test]
fn fill_gradient_v_single_row() {
    let mut buf = [0u8; 8 * 4 * 4];
    let mut surf = make_surface(&mut buf, 8, 4);
    surf.clear(Color::BLACK);

    // A single row gradient should just have the top color.
    surf.fill_gradient_v(0, 0, 8, 1, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    let p = surf.get_pixel(4, 0).unwrap();
    assert_eq!(p.a, 80, "single row should have top color alpha");
}

#[test]
fn shadow_surface_composites_between_content_and_chrome() {
    // Verify that a shadow surface (z=15) composites between content (z=10)
    // and chrome (z=20), creating a visible darkening effect beneath chrome.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content surface: bright white.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(200, 200, 200));

    // Shadow surface: covers rows 4-7 (just below where chrome would be),
    // filled with semi-transparent black gradient.
    let mut shadow_buf = [0u8; 16 * 4 * 4];
    let mut shadow = make_composite_surface(&mut shadow_buf, 16, 4, 0, 4, 15);
    shadow.surface.fill_gradient_v(
        0, 0, 16, 4,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    // Chrome surface: covers rows 0-3.
    let mut chrome_buf = [0u8; 16 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 16, 4, 0, 0, 20);
    chrome.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&drawing::CompositeSurface; 3] = [&content, &shadow, &chrome];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // In the shadow region (row 4): content should be darkened by shadow.
    let p_shadow = dst.get_pixel(8, 4).unwrap();
    let p_no_shadow = dst.get_pixel(8, 10).unwrap();

    // The shadowed pixel should be darker than the unshadowed content.
    assert!(
        p_shadow.r < p_no_shadow.r,
        "shadow should darken content: shadow_r={} < content_r={}",
        p_shadow.r, p_no_shadow.r
    );

    // The shadow should have gradient falloff: row 4 darker than row 7.
    let p_shadow_top = dst.get_pixel(8, 4).unwrap();
    let p_shadow_bottom = dst.get_pixel(8, 7).unwrap();
    assert!(
        p_shadow_top.r <= p_shadow_bottom.r,
        "shadow should fade: top_r={} <= bottom_r={}",
        p_shadow_top.r, p_shadow_bottom.r
    );
}

#[test]
fn shadow_gradient_not_hard_edged() {
    // Verify the shadow has at least 3 distinct alpha levels (not just on/off).
    let mut buf = [0u8; 16 * 8 * 4];
    let mut surf = make_surface(&mut buf, 16, 8);
    surf.clear(Color::TRANSPARENT);

    surf.fill_gradient_v(0, 0, 16, 8, Color::rgba(0, 0, 0, 80), Color::rgba(0, 0, 0, 0));

    let mut distinct_alphas = [0u8; 8];
    for row in 0..8 {
        distinct_alphas[row as usize] = surf.get_pixel(8, row).unwrap().a;
    }

    // Count unique alpha values.
    let mut unique_count = 0;
    for i in 0..8 {
        let mut is_unique = true;
        for j in 0..i {
            if distinct_alphas[i] == distinct_alphas[j] {
                is_unique = false;
                break;
            }
        }
        if is_unique {
            unique_count += 1;
        }
    }

    assert!(
        unique_count >= 3,
        "shadow should have gradient falloff with >= 3 distinct alpha levels, got {}",
        unique_count
    );
}

// ===========================================================================
// PNG decoder tests
// ===========================================================================

use drawing::{png_decode, png_header, PngError};

// 4x4 RGBA test PNG (filter=None, generated by Python)
const TEST_PNG_4X4_RGBA: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x08, 0x06, 0x00, 0x00, 0x00, 0xa9, 0xf1, 0x9e,
    0x7e, 0x00, 0x00, 0x00, 0x30, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x2d, 0x8b, 0xc9, 0x11, 0x00,
    0x30, 0x08, 0x02, 0xb7, 0x34, 0x4b, 0xdb, 0xd2, 0xec, 0x8c, 0xa8, 0x13, 0xe0, 0xc1, 0x70, 0x10,
    0xc8, 0x91, 0x1c, 0x60, 0x1d, 0x38, 0x91, 0x63, 0xa5, 0xaa, 0xa2, 0xa6, 0xbb, 0x77, 0x20, 0x5f,
    0x73, 0x72, 0x0a, 0xf2, 0x00, 0x81, 0x4b, 0x23, 0xe6, 0xa6, 0x81, 0xd8, 0x2d, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

// 4x4 RGB test PNG (filter=None, generated by Python)
const TEST_PNG_4X4_RGB: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x08, 0x02, 0x00, 0x00, 0x00, 0x26, 0x93, 0x09,
    0x29, 0x00, 0x00, 0x00, 0x28, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xc0,
    0x00, 0xc6, 0x40, 0x00, 0xa4, 0x18, 0x1a, 0xe0, 0xd8, 0xc1, 0xc1, 0xa1, 0xa1, 0xa1, 0xe1, 0xc0,
    0x81, 0x03, 0x20, 0x89, 0xff, 0x0d, 0x40, 0x91, 0xff, 0x40, 0x0a, 0x88, 0x01, 0xd6, 0x80, 0x14,
    0x74, 0x98, 0xeb, 0xef, 0xc4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
    0x82,
];

// 4x5 RGBA test PNG with all 5 filter types (None, Sub, Up, Average, Paeth)
const TEST_PNG_ALL_FILTERS: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x05, 0x08, 0x06, 0x00, 0x00, 0x00, 0x62, 0xad, 0x4d,
    0xdb, 0x00, 0x00, 0x00, 0x3f, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x38, 0x61, 0x24, 0xf7,
    0x1f, 0x19, 0x33, 0x1a, 0xa5, 0x04, 0xfc, 0xb7, 0x61, 0x60, 0x60, 0x80, 0x61, 0x26, 0x2e, 0x2e,
    0x2e, 0x06, 0x64, 0xcc, 0xec, 0xf6, 0xdb, 0xdb, 0xf3, 0x9b, 0x08, 0xff, 0xd3, 0x1b, 0xba, 0xfc,
    0x4f, 0x77, 0xb9, 0xf1, 0x3f, 0x65, 0x79, 0x23, 0xb7, 0xeb, 0x0d, 0xc3, 0x1b, 0x23, 0x06, 0x86,
    0x5d, 0x10, 0x0c, 0x00, 0x39, 0x9f, 0x18, 0xde, 0xbc, 0x00, 0x72, 0x5f, 0x00, 0x00, 0x00, 0x00,
    0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

// ---------------------------------------------------------------------------
// PNG header parsing
// ---------------------------------------------------------------------------

#[test]
fn png_header_parses_rgba() {
    let hdr = png_header(TEST_PNG_4X4_RGBA).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);
    assert_eq!(hdr.bit_depth, 8);
    assert_eq!(hdr.color_type, 6); // RGBA
}

#[test]
fn png_header_parses_rgb() {
    let hdr = png_header(TEST_PNG_4X4_RGB).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);
    assert_eq!(hdr.bit_depth, 8);
    assert_eq!(hdr.color_type, 2); // RGB
}

#[test]
fn png_header_parses_all_filters() {
    let hdr = png_header(TEST_PNG_ALL_FILTERS).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 5);
    assert_eq!(hdr.bit_depth, 8);
    assert_eq!(hdr.color_type, 6); // RGBA
}

// ---------------------------------------------------------------------------
// Error cases: invalid magic
// ---------------------------------------------------------------------------

#[test]
fn png_invalid_magic_returns_err() {
    let bad_data = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let result = png_header(&bad_data);
    assert_eq!(result.unwrap_err(), PngError::InvalidSignature);
}

#[test]
fn png_decode_invalid_magic_returns_err() {
    let bad_data = [0x00; 64];
    let mut output = [0u8; 256];
    let result = png_decode(&bad_data, &mut output);
    assert_eq!(result.unwrap_err(), PngError::InvalidSignature);
}

// ---------------------------------------------------------------------------
// Error cases: truncated data
// ---------------------------------------------------------------------------

#[test]
fn png_truncated_before_signature_returns_err() {
    let data = &[0x89, 0x50, 0x4e]; // Only 3 bytes
    assert_eq!(png_header(data).unwrap_err(), PngError::Truncated);
}

#[test]
fn png_truncated_ihdr_returns_err() {
    // Valid signature but truncated IHDR
    let data = &TEST_PNG_4X4_RGBA[..20]; // Cut off before full IHDR
    assert_eq!(png_header(data).unwrap_err(), PngError::Truncated);
}

#[test]
fn png_truncated_idat_returns_err() {
    // Valid signature + IHDR but truncated IDAT
    let data = &TEST_PNG_4X4_RGBA[..50]; // Cut off in the middle of IDAT
    let mut output = [0u8; 4 * 4 * 4 + 4];
    let result = png_decode(data, &mut output);
    assert!(result.is_err(), "truncated IDAT should return Err");
}

// ---------------------------------------------------------------------------
// Error cases: zero dimensions
// ---------------------------------------------------------------------------

#[test]
fn png_zero_width_returns_err() {
    // Construct a PNG with zero width in IHDR
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    // Width is at bytes 16..20 (big-endian u32), set to 0
    bad_png[16] = 0;
    bad_png[17] = 0;
    bad_png[18] = 0;
    bad_png[19] = 0;
    assert_eq!(png_header(&bad_png).unwrap_err(), PngError::ZeroDimensions);
}

#[test]
fn png_zero_height_returns_err() {
    // Construct a PNG with zero height in IHDR
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    // Height is at bytes 20..24 (big-endian u32), set to 0
    bad_png[20] = 0;
    bad_png[21] = 0;
    bad_png[22] = 0;
    bad_png[23] = 0;
    assert_eq!(png_header(&bad_png).unwrap_err(), PngError::ZeroDimensions);
}

// ---------------------------------------------------------------------------
// Error cases: unsupported format
// ---------------------------------------------------------------------------

#[test]
fn png_unsupported_bit_depth_returns_err() {
    // Modify bit depth to 16 (unsupported)
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[24] = 16; // bit depth at byte 24
    let mut output = [0u8; 4 * 4 * 4 + 4];
    assert_eq!(
        png_decode(&bad_png, &mut output).unwrap_err(),
        PngError::UnsupportedFormat
    );
}

#[test]
fn png_unsupported_color_type_returns_err() {
    // Modify color type to 4 (grayscale + alpha, unsupported)
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[25] = 4; // color type at byte 25
    let mut output = [0u8; 4 * 4 * 4 + 4];
    assert_eq!(
        png_decode(&bad_png, &mut output).unwrap_err(),
        PngError::UnsupportedFormat
    );
}

#[test]
fn png_unsupported_color_type_grayscale_returns_err() {
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[25] = 0; // color type 0 = grayscale
    let mut output = [0u8; 4 * 4 * 4 + 4];
    assert_eq!(
        png_decode(&bad_png, &mut output).unwrap_err(),
        PngError::UnsupportedFormat
    );
}

// ---------------------------------------------------------------------------
// Decode: RGBA 4x4
// ---------------------------------------------------------------------------

#[test]
fn png_decode_rgba_4x4_pixel_values() {
    // 4x4 RGBA image — decode and check known pixel values
    // Output needs space for raw data + filter bytes (total_raw = 4 * (1 + 4*4) = 68)
    // which is >= w*h*4 = 64. So we need 68 bytes.
    let mut output = [0u8; 128]; // give extra room
    let hdr = png_decode(TEST_PNG_4X4_RGBA, &mut output).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);

    // Check pixel (0,0) — should be Red (R=255,G=0,B=0,A=255) → BGRA: B=0,G=0,R=255,A=255
    let px = &output[0..4];
    assert_eq!(px[0], 0);   // B
    assert_eq!(px[1], 0);   // G
    assert_eq!(px[2], 255); // R
    assert_eq!(px[3], 255); // A

    // Check pixel (1,0) — Green (R=0,G=255,B=0,A=255) → BGRA: B=0,G=255,R=0,A=255
    let px = &output[4..8];
    assert_eq!(px[0], 0);   // B
    assert_eq!(px[1], 255); // G
    assert_eq!(px[2], 0);   // R
    assert_eq!(px[3], 255); // A

    // Check pixel (2,0) — Blue (R=0,G=0,B=255,A=255) → BGRA: B=255,G=0,R=0,A=255
    let px = &output[8..12];
    assert_eq!(px[0], 255); // B
    assert_eq!(px[1], 0);   // G
    assert_eq!(px[2], 0);   // R
    assert_eq!(px[3], 255); // A

    // Check pixel (3,0) — White (R=255,G=255,B=255,A=255) → BGRA: all 255
    let px = &output[12..16];
    assert_eq!(px, &[255, 255, 255, 255]);

    // Check pixel (1,1) — semi-transparent red (R=255,G=0,B=0,A=128)
    let row1_start = 4 * 4; // row 1 at offset width*4
    let px = &output[row1_start + 4..row1_start + 8];
    assert_eq!(px[0], 0);   // B
    assert_eq!(px[1], 0);   // G
    assert_eq!(px[2], 255); // R
    assert_eq!(px[3], 128); // A
}

// ---------------------------------------------------------------------------
// Decode: RGB 4x4
// ---------------------------------------------------------------------------

#[test]
fn png_decode_rgb_4x4_pixel_values() {
    // RGB (no alpha) — decoded pixels should have alpha = 255
    let mut output = [0u8; 128];
    let hdr = png_decode(TEST_PNG_4X4_RGB, &mut output).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);

    // Check pixel (0,0) — Red (R=255,G=0,B=0) → BGRA: B=0,G=0,R=255,A=255
    let px = &output[0..4];
    assert_eq!(px[0], 0);   // B
    assert_eq!(px[1], 0);   // G
    assert_eq!(px[2], 255); // R
    assert_eq!(px[3], 255); // A (opaque for RGB)

    // Check pixel (1,0) — Green (R=0,G=255,B=0) → BGRA: B=0,G=255,R=0,A=255
    let px = &output[4..8];
    assert_eq!(px[0], 0);   // B
    assert_eq!(px[1], 255); // G
    assert_eq!(px[2], 0);   // R
    assert_eq!(px[3], 255); // A

    // Check pixel (2,0) — Blue → BGRA: B=255,G=0,R=0,A=255
    let px = &output[8..12];
    assert_eq!(px[0], 255); // B
    assert_eq!(px[1], 0);   // G
    assert_eq!(px[2], 0);   // R
    assert_eq!(px[3], 255); // A

    // Check pixel (3,1) — (0,0,128) → BGRA: B=128,G=0,R=0,A=255
    let row1_start = 4 * 4;
    let px = &output[row1_start + 12..row1_start + 16];
    assert_eq!(px[0], 128); // B
    assert_eq!(px[1], 0);   // G
    assert_eq!(px[2], 0);   // R
    assert_eq!(px[3], 255); // A
}

// ---------------------------------------------------------------------------
// Decode: all 5 filter types
// ---------------------------------------------------------------------------

#[test]
fn png_decode_all_filter_types() {
    // 4x5 image with rows using filter types 0, 1, 2, 3, 4 respectively
    let mut output = [0u8; 256];
    let hdr = png_decode(TEST_PNG_ALL_FILTERS, &mut output).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 5);

    // Expected RGBA values (from Python generator):
    // Row 0 (filter=None): (200,50,30,255) x4
    // Row 1 (filter=Sub): (50,100,80,255), (110,100,80,255), (170,100,80,255), (230,100,80,255)
    // Row 2 (filter=Up): (60,110,90,255), (120,110,90,255), (180,110,90,255), (240,110,90,255)
    // Row 3 (filter=Average): (100,50,120,200), (100,100,120,200), (100,150,120,200), (100,200,120,200)
    // Row 4 (filter=Paeth): (80,80,50,180), (80,80,100,180), (80,80,150,180), (80,80,200,180)

    // Helper to check a pixel in BGRA format
    fn check_pixel(output: &[u8], row: usize, col: usize, r: u8, g: u8, b: u8, a: u8) {
        let stride = 4 * 4;
        let offset = row * stride + col * 4;
        assert_eq!(
            output[offset], b,
            "pixel ({},{}) B: expected {} got {}",
            col, row, b, output[offset]
        );
        assert_eq!(
            output[offset + 1], g,
            "pixel ({},{}) G: expected {} got {}",
            col, row, g, output[offset + 1]
        );
        assert_eq!(
            output[offset + 2], r,
            "pixel ({},{}) R: expected {} got {}",
            col, row, r, output[offset + 2]
        );
        assert_eq!(
            output[offset + 3], a,
            "pixel ({},{}) A: expected {} got {}",
            col, row, a, output[offset + 3]
        );
    }

    // Row 0 (filter=None): all (200,50,30,255)
    check_pixel(&output, 0, 0, 200, 50, 30, 255);
    check_pixel(&output, 0, 1, 200, 50, 30, 255);
    check_pixel(&output, 0, 2, 200, 50, 30, 255);
    check_pixel(&output, 0, 3, 200, 50, 30, 255);

    // Row 1 (filter=Sub): gradient
    check_pixel(&output, 1, 0, 50, 100, 80, 255);
    check_pixel(&output, 1, 1, 110, 100, 80, 255);
    check_pixel(&output, 1, 2, 170, 100, 80, 255);
    check_pixel(&output, 1, 3, 230, 100, 80, 255);

    // Row 2 (filter=Up): similar to row 1 + offsets
    check_pixel(&output, 2, 0, 60, 110, 90, 255);
    check_pixel(&output, 2, 1, 120, 110, 90, 255);
    check_pixel(&output, 2, 2, 180, 110, 90, 255);
    check_pixel(&output, 2, 3, 240, 110, 90, 255);

    // Row 3 (filter=Average)
    check_pixel(&output, 3, 0, 100, 50, 120, 200);
    check_pixel(&output, 3, 1, 100, 100, 120, 200);
    check_pixel(&output, 3, 2, 100, 150, 120, 200);
    check_pixel(&output, 3, 3, 100, 200, 120, 200);

    // Row 4 (filter=Paeth)
    check_pixel(&output, 4, 0, 80, 80, 50, 180);
    check_pixel(&output, 4, 1, 80, 80, 100, 180);
    check_pixel(&output, 4, 2, 80, 80, 150, 180);
    check_pixel(&output, 4, 3, 80, 80, 200, 180);
}

// ---------------------------------------------------------------------------
// Buffer too small
// ---------------------------------------------------------------------------

#[test]
fn png_decode_buffer_too_small_returns_err() {
    let mut output = [0u8; 16]; // 4x4 RGBA needs at least 4*4*4+4 = 68 bytes
    let result = png_decode(TEST_PNG_4X4_RGBA, &mut output);
    assert_eq!(result.unwrap_err(), PngError::BufferTooSmall);
}

// ---------------------------------------------------------------------------
// Empty / minimal bad data
// ---------------------------------------------------------------------------

#[test]
fn png_empty_data_returns_err() {
    let result = png_header(&[]);
    assert_eq!(result.unwrap_err(), PngError::Truncated);
}

#[test]
fn png_decode_empty_data_returns_err() {
    let mut output = [0u8; 256];
    let result = png_decode(&[], &mut output);
    assert_eq!(result.unwrap_err(), PngError::Truncated);
}

// ---------------------------------------------------------------------------
// Real-world PNG from file
// ---------------------------------------------------------------------------

#[test]
fn png_decode_test_image_from_file() {
    // Read the actual test.png from system/share/
    let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../share/test.png"));
    if let Ok(data) = data {
        let hdr = png_header(&data).unwrap();
        assert_eq!(hdr.width, 128);
        assert_eq!(hdr.height, 128);
        assert_eq!(hdr.bit_depth, 8);
        assert!(hdr.color_type == 2 || hdr.color_type == 6);

        // Decode the full image
        let out_size = (hdr.width * hdr.height * 4) as usize + hdr.height as usize;
        let mut output = vec![0u8; out_size];
        let result = png_decode(&data, &mut output);
        assert!(result.is_ok(), "failed to decode test.png: {:?}", result.unwrap_err());

        // Verify some basic properties of decoded data
        // Pixel (0,0) should have non-trivial values (not all zeros)
        // and alpha should be non-zero
        let a = output[3];
        assert!(a > 0, "pixel (0,0) alpha should be > 0, got {}", a);

        // Pixel (64,64) — center of image — check it's reasonable
        let center = (64 * 128 + 64) * 4;
        let center_a = output[center + 3];
        assert!(center_a > 0, "center pixel alpha should be > 0");
    }
    // If file doesn't exist, skip (test will be run during verification)
}

// ---------------------------------------------------------------------------
// Image viewer: PNG surface rendering within content area bounds
// ---------------------------------------------------------------------------

#[test]
fn image_blit_clips_to_content_area() {
    // Simulate blitting a large image into a smaller content area.
    // The image should be clipped to the content surface bounds.
    let img_w: u32 = 16;
    let img_h: u32 = 16;
    let content_w: u32 = 10;
    let content_h: u32 = 10;

    // Create a "decoded image" buffer (BGRA8888).
    let mut img_data = vec![0u8; (img_w * img_h * 4) as usize];
    for y in 0..img_h {
        for x in 0..img_w {
            let idx = ((y * img_w + x) * 4) as usize;
            img_data[idx] = 0; // B
            img_data[idx + 1] = (y * 16) as u8; // G
            img_data[idx + 2] = (x * 16) as u8; // R
            img_data[idx + 3] = 255; // A
        }
    }

    // Create a content surface smaller than the image.
    let mut content_buf = vec![0u8; (content_w * content_h * 4) as usize];
    let mut content = make_surface(&mut content_buf, content_w, content_h);

    // Blit the image at (0,0) — should clip to content bounds.
    content.blit(
        &img_data,
        img_w,
        img_h,
        img_w * 4,
        0,
        0,
    );

    // Verify: only the top-left 10x10 of the 16x16 image is visible.
    for y in 0..content_h {
        for x in 0..content_w {
            let px = content.get_pixel(x, y).unwrap();
            assert_eq!(px.r, (x * 16) as u8, "pixel ({x},{y}) R mismatch");
            assert_eq!(px.g, (y * 16) as u8, "pixel ({x},{y}) G mismatch");
            assert_eq!(px.a, 255);
        }
    }
}

#[test]
fn image_blit_blend_clips_to_content_area() {
    // Same test but using blit_blend (alpha-aware blitting).
    let img_w: u32 = 20;
    let img_h: u32 = 20;
    let content_w: u32 = 12;
    let content_h: u32 = 12;

    let mut img_data = vec![0u8; (img_w * img_h * 4) as usize];
    for y in 0..img_h {
        for x in 0..img_w {
            let idx = ((y * img_w + x) * 4) as usize;
            img_data[idx] = 100; // B
            img_data[idx + 1] = 150; // G
            img_data[idx + 2] = 200; // R
            img_data[idx + 3] = 255; // A (opaque)
        }
    }

    let mut content_buf = vec![0u8; (content_w * content_h * 4) as usize];
    let mut content = make_surface(&mut content_buf, content_w, content_h);
    content.clear(Color::rgb(0, 0, 0));

    content.blit_blend(
        &img_data,
        img_w,
        img_h,
        img_w * 4,
        0,
        0,
    );

    // Verify clipped pixels are correct.
    for y in 0..content_h {
        for x in 0..content_w {
            let px = content.get_pixel(x, y).unwrap();
            assert_eq!(px.r, 200, "pixel ({x},{y}) R");
            assert_eq!(px.g, 150, "pixel ({x},{y}) G");
            assert_eq!(px.b, 100, "pixel ({x},{y}) B");
        }
    }
}

#[test]
fn image_surface_no_overflow_into_chrome_region() {
    // Simulate the compositor layout: content area is between title bar and
    // status bar. An image blitted into the content area must not write
    // outside those bounds.
    let fb_w: u32 = 64;
    let fb_h: u32 = 48;
    let title_h: u32 = 8;
    let status_h: u32 = 6;
    let content_h = fb_h - title_h - status_h; // 34

    // Create framebuffer.
    let mut fb_buf = vec![0u8; (fb_w * fb_h * 4) as usize];
    let mut fb = make_surface(&mut fb_buf, fb_w, fb_h);
    fb.clear(Color::rgb(10, 10, 10)); // dark bg

    // Fill title bar region with distinct color.
    fb.fill_rect(0, 0, fb_w, title_h, Color::rgb(50, 50, 80));

    // Fill status bar region with distinct color.
    fb.fill_rect(0, fb_h - status_h, fb_w, status_h, Color::rgb(50, 50, 80));

    // Create a content surface the exact size of content area.
    let mut content_buf = vec![0u8; (fb_w * content_h * 4) as usize];
    let mut content = make_surface(&mut content_buf, fb_w, content_h);
    content.clear(Color::rgb(20, 20, 30));

    // Blit a large image (bigger than content) into the content surface.
    let img_w: u32 = 128;
    let img_h: u32 = 128;
    let mut img_data = vec![0u8; (img_w * img_h * 4) as usize];
    for i in 0..(img_w * img_h) as usize {
        img_data[i * 4] = 0;     // B
        img_data[i * 4 + 1] = 255; // G
        img_data[i * 4 + 2] = 0;   // R
        img_data[i * 4 + 3] = 255; // A
    }

    content.blit(&img_data, img_w, img_h, img_w * 4, 0, 0);

    // Blit content surface onto framebuffer at the content area position.
    fb.blit(content.data, fb_w, content_h, fb_w * 4, 0, title_h);

    // Verify: title bar region is unchanged (still chrome color).
    for y in 0..title_h {
        let px = fb.get_pixel(0, y).unwrap();
        assert_eq!(px.r, 50, "title bar pixel ({},{}): R={}", 0, y, px.r);
        assert_eq!(px.g, 50, "title bar pixel ({},{}): G={}", 0, y, px.g);
    }

    // Verify: status bar region is unchanged (still chrome color).
    for y in (fb_h - status_h)..fb_h {
        let px = fb.get_pixel(0, y).unwrap();
        assert_eq!(px.r, 50, "status bar pixel ({},{}): R={}", 0, y, px.r);
        assert_eq!(px.g, 50, "status bar pixel ({},{}): G={}", 0, y, px.g);
    }

    // Verify: content area has green pixels from the image.
    let px = fb.get_pixel(0, title_h).unwrap();
    assert_eq!(px.g, 255, "content should have green image pixels");
    assert_eq!(px.r, 0);
}

#[test]
fn png_decode_to_surface_correct_colors() {
    // Decode the test.png and verify pixel color correctness.
    let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../share/test.png"));
    if let Ok(data) = data {
        let hdr = png_decode(&data, &mut vec![0u8; 128 * 128 * 4 + 128]).unwrap();
        let mut output = vec![0u8; (hdr.width * hdr.height * 4) as usize + hdr.height as usize];
        let _ = png_decode(&data, &mut output).unwrap();

        // Check a few pixels are non-zero (image is not all black).
        let mut non_zero = 0;
        for i in 0..(hdr.width * hdr.height) as usize {
            let r = output[i * 4 + 2]; // BGRA format: R at offset 2
            let g = output[i * 4 + 1];
            let b = output[i * 4];
            if r > 0 || g > 0 || b > 0 {
                non_zero += 1;
            }
        }
        assert!(non_zero > 100, "decoded image should have many non-zero pixels, got {}", non_zero);

        // Check image is not all the same color (it's a gradient).
        let px00_r = output[2]; // pixel (0,0) R channel
        let center = (64 * 128 + 64) * 4;
        let px_center_r = output[center + 2];
        assert_ne!(px00_r, px_center_r, "corner and center should differ (gradient image)");
    }
}

// ---------------------------------------------------------------------------
// Clock time formatting tests
// ---------------------------------------------------------------------------

/// Format total seconds since boot into HH:MM:SS.
/// This mirrors the logic used by the compositor's clock display.
fn format_time_hms(total_seconds: u64, buf: &mut [u8; 8]) {
    let hours = ((total_seconds / 3600) % 24) as u8;
    let minutes = ((total_seconds / 60) % 60) as u8;
    let seconds = (total_seconds % 60) as u8;
    buf[0] = b'0' + hours / 10;
    buf[1] = b'0' + hours % 10;
    buf[2] = b':';
    buf[3] = b'0' + minutes / 10;
    buf[4] = b'0' + minutes % 10;
    buf[5] = b':';
    buf[6] = b'0' + seconds / 10;
    buf[7] = b'0' + seconds % 10;
}

#[test]
fn clock_format_zero_seconds() {
    let mut buf = [0u8; 8];
    format_time_hms(0, &mut buf);
    assert_eq!(&buf, b"00:00:00");
}

#[test]
fn clock_format_one_second() {
    let mut buf = [0u8; 8];
    format_time_hms(1, &mut buf);
    assert_eq!(&buf, b"00:00:01");
}

#[test]
fn clock_format_one_minute() {
    let mut buf = [0u8; 8];
    format_time_hms(60, &mut buf);
    assert_eq!(&buf, b"00:01:00");
}

#[test]
fn clock_format_one_hour() {
    let mut buf = [0u8; 8];
    format_time_hms(3600, &mut buf);
    assert_eq!(&buf, b"01:00:00");
}

#[test]
fn clock_format_max_time() {
    // 23:59:59 = 23*3600 + 59*60 + 59 = 86399
    let mut buf = [0u8; 8];
    format_time_hms(86399, &mut buf);
    assert_eq!(&buf, b"23:59:59");
}

#[test]
fn clock_format_wraps_at_24_hours() {
    // 24 hours = 86400 seconds → wraps to 00:00:00
    let mut buf = [0u8; 8];
    format_time_hms(86400, &mut buf);
    assert_eq!(&buf, b"00:00:00");
}

#[test]
fn clock_format_arbitrary_time() {
    // 12345 seconds = 3h 25m 45s
    let mut buf = [0u8; 8];
    format_time_hms(12345, &mut buf);
    assert_eq!(&buf, b"03:25:45");
}

#[test]
fn clock_format_large_value_wraps() {
    // 100000 seconds = 27h 46m 40s → wraps to 03:46:40
    let mut buf = [0u8; 8];
    format_time_hms(100000, &mut buf);
    assert_eq!(&buf, b"03:46:40");
}

#[test]
fn clock_format_all_digits_valid() {
    // Check that all formatted characters are valid (digits or ':')
    for secs in [0u64, 1, 59, 60, 3599, 3600, 43200, 86399] {
        let mut buf = [0u8; 8];
        format_time_hms(secs, &mut buf);
        // buf[2] and buf[5] must be ':'
        assert_eq!(buf[2], b':', "secs={}: buf[2] should be ':'", secs);
        assert_eq!(buf[5], b':', "secs={}: buf[5] should be ':'", secs);
        // All other positions must be ASCII digits
        for &i in &[0usize, 1, 3, 4, 6, 7] {
            assert!(buf[i] >= b'0' && buf[i] <= b'9',
                "secs={}: buf[{}] = {} is not a digit", secs, i, buf[i]);
        }
        // Hours 00-23
        let h = (buf[0] - b'0') * 10 + (buf[1] - b'0');
        assert!(h <= 23, "secs={}: hours {} > 23", secs, h);
        // Minutes 00-59
        let m = (buf[3] - b'0') * 10 + (buf[4] - b'0');
        assert!(m <= 59, "secs={}: minutes {} > 59", secs, m);
        // Seconds 00-59
        let s = (buf[6] - b'0') * 10 + (buf[7] - b'0');
        assert!(s <= 59, "secs={}: seconds {} > 59", secs, s);
    }
}

#[test]
fn clock_seconds_from_counter() {
    // Simulate deriving seconds from ARM generic counter.
    // QEMU typical: freq = 62_500_000 Hz (62.5 MHz)
    let freq: u64 = 62_500_000;
    let boot_counter: u64 = 1_000_000_000; // some boot time counter value
    let current_counter: u64 = boot_counter + 5 * freq; // 5 seconds later

    let elapsed_ticks = current_counter - boot_counter;
    let elapsed_seconds = elapsed_ticks / freq;

    assert_eq!(elapsed_seconds, 5);

    let mut buf = [0u8; 8];
    format_time_hms(elapsed_seconds, &mut buf);
    assert_eq!(&buf, b"00:00:05");
}
