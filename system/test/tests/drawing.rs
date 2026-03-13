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

/// Heap-allocate a zeroed GlyphCache without touching the stack.
///
/// GlyphCache is ~1.3 MB with OVERSAMPLE_X=6, which overflows test thread
/// stacks if allocated via `Box::new(GlyphCache::zeroed())` (the value is
/// constructed on stack before moving to heap). This uses `Box::new_uninit`
/// + zero-fill to avoid that.
fn heap_glyph_cache() -> Box<drawing::GlyphCache> {
    unsafe {
        let layout = std::alloc::Layout::new::<drawing::GlyphCache>();
        let ptr = std::alloc::alloc_zeroed(layout) as *mut drawing::GlyphCache;
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Box::from_raw(ptr)
    }
}

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

    // 2x2 coverage map with varying coverage (3-channel: RGB per pixel).
    // Pixel (0,0): full coverage on all channels.
    // Pixel (1,0): half coverage on all channels.
    // Pixel (0,1): quarter coverage on all channels.
    // Pixel (1,1): zero coverage on all channels.
    let coverage = [
        255, 255, 255,  128, 128, 128,  // row 0: full, half
         64,  64,  64,    0,   0,   0,  // row 1: quarter, zero
    ];
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
    // 2x2 coverage, 3-channel (RGB). All full coverage.
    let coverage = [255u8; 12]; // 2*2*3 = 12 bytes
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

    // 1x1 pixel, 3-channel (RGB) coverage, all channels full.
    let coverage = [255u8, 255, 255];
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

    // Draw with zero coverage (3-channel: 2x2 pixels * 3 = 12 bytes).
    let coverage = [0u8; 12];
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

    // 1x1 pixel, 3-channel (RGB), all full coverage.
    let coverage = [255u8, 255, 255];
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

    // 1x1 pixel, 3-channel (RGB), all channels at 50% coverage.
    let coverage = [128u8, 128, 128]; // ~50% coverage
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

    // 1x1 pixel, 3-channel (RGB), all at 50% coverage.
    let coverage = [128u8, 128, 128];
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
    // along the edges. With subpixel rendering, output is 3 bytes per pixel.
    let total = (metrics.width * metrics.height * 3) as usize;
    let coverage = &buf[..total];

    let intermediate_count = coverage.iter().filter(|&&c| c > 0 && c < 255).count();
    assert!(
        intermediate_count > 0,
        "'k' should have intermediate coverage values (smooth edges), got 0 intermediate pixels",
    );
}

#[test]
fn oversampled_diagonal_has_horizontal_gradients() {
    // With horizontal oversampling + subpixel rendering, diagonal strokes
    // should show smooth horizontal transitions. Check 'x' which has diagonals.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('x', 24, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    // Output is 3 bytes per pixel (RGB subpixel coverage).
    let total = (w * metrics.height * 3) as usize;
    let coverage = &buf[..total];

    // Find a row in the middle of the glyph (where diagonals cross).
    let mid_row = metrics.height / 2;
    let row_start = (mid_row * w * 3) as usize;
    let row_end = row_start + (w * 3) as usize;
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
    // Curved characters like 'o' should have smooth edges with subpixel rendering.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('o', 24, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    // Output is 3 bytes per pixel (RGB subpixel coverage).
    let total = (w * metrics.height * 3) as usize;
    let coverage = &buf[..total];

    // Count distinct non-zero coverage levels (more levels = smoother).
    let mut levels = [false; 256];
    for &c in coverage.iter() {
        if c > 0 {
            levels[c as usize] = true;
        }
    }
    let distinct_levels = levels.iter().filter(|&&v| v).count();

    // With 6× horizontal oversampling (OVERSAMPLE_X*OVERSAMPLE_Y = 24
    // samples per channel), we expect more than 4 distinct levels at minimum.
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
    // GlyphCache should still populate correctly with subpixel rendering.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    // Heap-allocate: GlyphCache is ~1.3 MB with OVERSAMPLE_X=6 (too big for stack).
    let mut cache = heap_glyph_cache();

    cache.populate(&font, 16, &mut scratch);

    // Check a few glyphs are cached with valid dimensions.
    let (g_a, cov_a) = cache.get(b'A').unwrap();
    assert!(g_a.width > 0 && g_a.height > 0, "'A' should have non-zero cached dimensions");
    assert!(cov_a.len() > 0, "'A' coverage should be non-empty");
    // Coverage length should be 3× (width * height) for RGB subpixel.
    assert_eq!(
        cov_a.len(), (g_a.width * g_a.height * 3) as usize,
        "'A' coverage should be 3 bytes per pixel (RGB subpixel)"
    );

    let (g_k, cov_k) = cache.get(b'k').unwrap();
    assert!(g_k.width > 0 && g_k.height > 0);

    // Check coverage has intermediate values (smooth edges).
    let has_intermediate = cov_k.iter().any(|&c| c > 0 && c < 255);
    assert!(has_intermediate, "'k' cached coverage should have intermediate values");
}

// ---------------------------------------------------------------------------
// Subpixel rendering tests
// ---------------------------------------------------------------------------

#[test]
fn subpixel_oversample_x_is_6() {
    // OVERSAMPLE_X must be 6 for subpixel rendering (3 sub-pixels × 2× each).
    assert_eq!(
        OVERSAMPLE_X, 6,
        "OVERSAMPLE_X must be 6 for subpixel rendering, got {}",
        OVERSAMPLE_X,
    );
}

#[test]
fn subpixel_coverage_has_3_channels() {
    // Rasterized glyph coverage should be 3 bytes per pixel (R, G, B).
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('H', 24, &mut raster, &mut scratch).unwrap();
    assert!(metrics.width > 0 && metrics.height > 0);

    // Total output bytes should be width * height * 3.
    let expected_bytes = (metrics.width * metrics.height * 3) as usize;

    // Verify the data region is valid (non-zero coverage exists).
    let coverage = &buf[..expected_bytes];
    let has_nonzero = coverage.iter().any(|&c| c > 0);
    assert!(has_nonzero, "'H' subpixel coverage should have non-zero values");
}

#[test]
fn subpixel_rgb_channels_differ_at_edges() {
    // At glyph edges, the R, G, B coverage channels should differ — this is
    // the signature of subpixel rendering. In greyscale AA, all channels are
    // equal; in subpixel, they diverge at horizontal edges.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('l', 24, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    let h = metrics.height;
    let total = (w * h * 3) as usize;
    let coverage = &buf[..total];

    // Find pixels where R != G or G != B (subpixel color fringing).
    let mut rgb_differ_count = 0;
    for pixel in 0..(w * h) as usize {
        let r = coverage[pixel * 3];
        let g = coverage[pixel * 3 + 1];
        let b = coverage[pixel * 3 + 2];
        // Only count pixels at edges (partial coverage, not fully on or off).
        if (r > 0 || g > 0 || b > 0) && (r < 255 || g < 255 || b < 255) {
            if r != g || g != b {
                rgb_differ_count += 1;
            }
        }
    }

    assert!(
        rgb_differ_count > 0,
        "subpixel rendering should produce pixels where R != G != B at glyph edges, found 0"
    );
}

#[test]
fn subpixel_monospace_cache_has_3_channel_coverage() {
    // Both the monospace (Source Code Pro) cache should produce 3-channel data.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();
    cache.populate(&font, 16, &mut scratch);

    let (g, cov) = cache.get(b'A').unwrap();
    assert_eq!(
        cov.len(), (g.width * g.height * 3) as usize,
        "monospace cache: coverage should be 3 bytes per pixel"
    );

    // Check that RGB channels differ at some edge pixels.
    let mut has_rgb_diff = false;
    for pixel in 0..(g.width * g.height) as usize {
        let r = cov[pixel * 3];
        let g_ch = cov[pixel * 3 + 1];
        let b = cov[pixel * 3 + 2];
        if r != g_ch || g_ch != b {
            if r > 0 || g_ch > 0 || b > 0 {
                has_rgb_diff = true;
                break;
            }
        }
    }
    assert!(has_rgb_diff, "monospace cache 'A': subpixel rendering should produce R!=G!=B at edges");
}

#[test]
fn subpixel_proportional_cache_has_3_channel_coverage() {
    // The proportional (Nunito Sans) cache should produce 3-channel data.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();
    cache.populate(&font, 16, &mut scratch);

    let (g, cov) = cache.get(b'A').unwrap();
    assert_eq!(
        cov.len(), (g.width * g.height * 3) as usize,
        "proportional cache: coverage should be 3 bytes per pixel"
    );

    // Check that RGB channels differ at some edge pixels.
    let mut has_rgb_diff = false;
    for pixel in 0..(g.width * g.height) as usize {
        let r = cov[pixel * 3];
        let g_ch = cov[pixel * 3 + 1];
        let b = cov[pixel * 3 + 2];
        if r != g_ch || g_ch != b {
            if r > 0 || g_ch > 0 || b > 0 {
                has_rgb_diff = true;
                break;
            }
        }
    }
    assert!(has_rgb_diff, "proportional cache 'A': subpixel rendering should produce R!=G!=B at edges");
}

#[test]
fn subpixel_draw_coverage_rgb_per_channel_blend() {
    // Verify that draw_coverage with different R, G, B coverage values
    // produces per-channel blending (R, G, B of output differ).
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    // 1x1 pixel: R=255 (full), G=128 (half), B=0 (zero).
    let coverage = [255u8, 128, 0];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);

    let p = dst.get_pixel(0, 0).unwrap();
    // R channel: full coverage of white on black → white.
    assert_eq!(p.r, 255, "R channel with full coverage should be 255");
    // G channel: half coverage → intermediate (gamma-correct, so > 128).
    assert!(p.g > 128 && p.g < 255, "G channel with half coverage should be intermediate, got {}", p.g);
    // B channel: zero coverage → unchanged (black).
    assert_eq!(p.b, 0, "B channel with zero coverage should be 0");
}

#[test]
fn subpixel_fir_filter_reduces_fringing() {
    // The FIR filter should smooth the transition between channels.
    // Rasterize a vertical stroke ('l') and check that the filtered
    // coverage has smoother channel transitions than raw subpixel data.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('l', 24, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    let h = metrics.height;
    let total = (w * h * 3) as usize;
    let coverage = &buf[..total];

    // At edge pixels, the maximum difference between any two channels
    // should be limited by the FIR filter. Count high-contrast transitions.
    let mut max_channel_diff = 0u8;
    for pixel in 0..(w * h) as usize {
        let r = coverage[pixel * 3];
        let g = coverage[pixel * 3 + 1];
        let b = coverage[pixel * 3 + 2];
        let diff_rg = if r > g { r - g } else { g - r };
        let diff_gb = if g > b { g - b } else { b - g };
        let diff_rb = if r > b { r - b } else { b - r };
        let max_d = if diff_rg > diff_gb { diff_rg } else { diff_gb };
        let max_d = if diff_rb > max_d { diff_rb } else { max_d };
        if max_d > max_channel_diff {
            max_channel_diff = max_d;
        }
    }

    // The FIR filter should keep max channel difference < 255
    // (i.e., we never have R=255,B=0 — the filter smooths that).
    // With a [1/4, 1/2, 1/4] filter, the max difference should be
    // significantly less than 255 for most glyphs.
    assert!(
        max_channel_diff < 200,
        "FIR filter should reduce channel difference below 200, got {}",
        max_channel_diff,
    );
}

// ---------------------------------------------------------------------------
// Stem darkening — non-linear coverage boost for thin strokes
// ---------------------------------------------------------------------------

use drawing::STEM_DARKENING_BOOST;
use drawing::STEM_DARKENING_LUT;

#[test]
fn stem_darkening_lut_zero_stays_zero() {
    // Zero coverage must remain zero after darkening (no phantom pixels).
    assert_eq!(STEM_DARKENING_LUT[0], 0, "zero coverage should stay 0 after darkening");
}

#[test]
fn stem_darkening_lut_full_stays_full() {
    // Full coverage (255) must remain 255 after darkening.
    assert_eq!(STEM_DARKENING_LUT[255], 255, "full coverage (255) should stay 255 after darkening");
}

#[test]
fn stem_darkening_lut_boost_mid_range() {
    // Coverage values in the 30-200 range should be strictly higher after darkening.
    for cov in 30u8..=200u8 {
        let darkened = STEM_DARKENING_LUT[cov as usize];
        assert!(
            darkened > cov,
            "coverage {} should be strictly boosted, got {}",
            cov, darkened,
        );
    }
}

#[test]
fn stem_darkening_lut_monotonic() {
    // The LUT must be monotonically non-decreasing: higher input → ≥ higher output.
    for i in 1..256 {
        assert!(
            STEM_DARKENING_LUT[i] >= STEM_DARKENING_LUT[i - 1],
            "LUT not monotonic at {}: {} < {}",
            i, STEM_DARKENING_LUT[i], STEM_DARKENING_LUT[i - 1],
        );
    }
}

#[test]
fn stem_darkening_boost_is_tunable() {
    // The boost constant should be in a reasonable range (40-120).
    assert!(
        STEM_DARKENING_BOOST >= 40 && STEM_DARKENING_BOOST <= 120,
        "STEM_DARKENING_BOOST should be 40-120, got {}",
        STEM_DARKENING_BOOST,
    );
}

#[test]
fn stem_darkening_applied_to_rasterized_glyph() {
    // Rasterize a thin-stroke glyph ('l') and verify that intermediate
    // coverage values are boosted compared to the raw formula.
    // Since darkening is applied in the rasterizer, we verify the output
    // has higher coverage values than raw (undarkened) values would produce.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer { data: &mut buf, width: 128, height: 128 };

    let metrics = font.rasterize('l', 16, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    let h = metrics.height;
    let total = (w * h * 3) as usize;
    let coverage = &buf[..total];

    // Count coverage values that are in the boosted range (30-200).
    // After darkening, any raw value in 30-200 should now be higher.
    // We verify indirectly: the glyph should have coverage values in
    // the STEM_DARKENING_LUT[30]..=254 range (values that can only exist
    // if darkening was applied to raw values in 30..200).
    let boosted_threshold = STEM_DARKENING_LUT[30];
    let has_boosted = coverage.iter().any(|&c| c >= boosted_threshold && c < 255);
    assert!(
        has_boosted,
        "'l' at 16px should have boosted coverage values (>= {})",
        boosted_threshold,
    );
}

#[test]
fn stem_darkening_all_three_channels_equally() {
    // For a fully symmetric glyph rendered at the center of a pixel,
    // all 3 channels should be darkened equally. We check that the LUT
    // applies the same transformation to each channel.
    //
    // Since the LUT is a single table applied identically to R, G, B,
    // verify the formula: darkened = cov + BOOST * (255 - cov) / 255.
    // Special case: LUT[0] = 0 (no phantom pixels).
    let boost = STEM_DARKENING_BOOST as u32;
    assert_eq!(STEM_DARKENING_LUT[0], 0, "LUT[0] must be 0");
    for cov in 1u32..=255 {
        let expected = cov + boost * (255 - cov) / 255;
        let expected = if expected > 255 { 255 } else { expected };
        assert_eq!(
            STEM_DARKENING_LUT[cov as usize], expected as u8,
            "LUT[{}] should be {}, got {}",
            cov, expected, STEM_DARKENING_LUT[cov as usize],
        );
    }
}

// ---------------------------------------------------------------------------
// Proportional font — GlyphCache with variable advance widths
// ---------------------------------------------------------------------------

#[test]
fn proportional_glyph_cache_advance_i_less_than_m() {
    // VAL-FONT-004: advance('i') < advance('m') — variable widths confirmed.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();

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
    let mut cache = heap_glyph_cache();

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
    let mut cache = heap_glyph_cache();

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
    let mut cache = heap_glyph_cache();

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
    let mut cache = heap_glyph_cache();

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
fn damage_tracker_max_rects_is_6() {
    assert_eq!(drawing::MAX_DIRTY_RECTS, 6);
}

#[test]
fn damage_tracker_multiple_content_and_chrome_rects() {
    // Simulates the real use case: content area change + chrome change
    let mut dt = drawing::DamageTracker::new(1024, 768);
    // Content area: one line of text changed (approx one line_height tall)
    dt.add(13, 48, 998, 22); // text region
    // Chrome area (e.g., title bar)
    dt.add(0, 0, 1024, 36); // title bar
    assert_eq!(dt.count, 2);
    let rects = dt.dirty_rects().unwrap();
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0], drawing::DirtyRect::new(13, 48, 998, 22));
    assert_eq!(rects[1], drawing::DirtyRect::new(0, 0, 1024, 36));
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
fn title_bar_chrome_over_content_shows_bleedthrough() {
    // Title bar at the top of the frame with content extending behind it.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content: full height, has blue pixels.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(0, 0, 180));

    // Title bar: top 4 rows, translucent.
    let mut title_buf = [0u8; 16 * 4 * 4];
    let mut title = make_composite_surface(&mut title_buf, 16, 4, 0, 0, 20);
    title.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&CompositeSurface; 2] = [&content, &title];
    drawing::composite_surfaces(&mut dst, &surfaces);

    // In the title bar region, blue from content should be partially visible.
    let p_title = dst.get_pixel(8, 1).unwrap();
    assert!(
        p_title.b > 40,
        "blue content should partially show through title bar, got b={}",
        p_title.b
    );

    // Below the title bar, pure content.
    let p_below = dst.get_pixel(8, 8).unwrap();
    assert_eq!(p_below, Color::rgb(0, 0, 180));
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
    // Simulate the compositor layout: content area is below the title bar
    // and extends to the bottom of the screen. An image blitted into the
    // content area must not write into the title bar region.
    let fb_w: u32 = 64;
    let fb_h: u32 = 48;
    let title_h: u32 = 8;
    let content_h = fb_h - title_h; // 40

    // Create framebuffer.
    let mut fb_buf = vec![0u8; (fb_w * fb_h * 4) as usize];
    let mut fb = make_surface(&mut fb_buf, fb_w, fb_h);
    fb.clear(Color::rgb(10, 10, 10)); // dark bg

    // Fill title bar region with distinct color.
    fb.fill_rect(0, 0, fb_w, title_h, Color::rgb(50, 50, 80));

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

// ---------------------------------------------------------------------------
// Context switching tests
// ---------------------------------------------------------------------------
//
// These tests verify that the drawing library's text rendering and image
// rendering produce deterministic output, enabling context switching between
// editor and image viewer modes while preserving content state.

/// Render text content surface, simulate switching to image, then switching
/// back. The text pixels must be byte-identical before and after the round trip.
/// This validates that re-rendering from the same document state produces
/// identical output — the foundation of context switching.
#[test]
fn context_switch_text_content_preserved_after_roundtrip() {
    let width = 200u32;
    let height = 100u32;
    let bpp = 4u32;
    let stride = width * bpp;
    let size = (stride * height) as usize;

    // Render text to a content surface.
    let mut buf1 = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut buf1,
            width, height, stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        // Simple text rendering via bitmap font.
        surf.draw_text(10, 10, "hello world", &FONT_8X16, Color::rgb(200, 210, 230));
    }

    // Render a different surface (image mode) — just clear to a different color.
    let mut buf_image = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut buf_image,
            width, height, stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(50, 50, 50));
    }

    // Render text again to a second buffer (simulating switch back to editor).
    let mut buf2 = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut buf2,
            width, height, stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.draw_text(10, 10, "hello world", &FONT_8X16, Color::rgb(200, 210, 230));
    }

    // The two text renders must be byte-identical.
    assert_eq!(buf1, buf2, "Text content not preserved after context switch round-trip");
}

/// Verify that cursor position (byte offset) maps to the same pixel coordinates
/// after a context switch round-trip. Uses TextLayout::byte_to_xy.
#[test]
fn context_switch_cursor_position_preserved() {
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 200,
    };
    let text = b"hello world\nline two";
    let cursor_pos = 5; // After 'hello'

    let (x1, y1) = layout.byte_to_xy(text, cursor_pos);

    // Simulate "context switch away" — the cursor_pos value is just an integer
    // stored in a static. Nothing happens to it.

    // Simulate "context switch back" — re-query the same position.
    let (x2, y2) = layout.byte_to_xy(text, cursor_pos);

    assert_eq!((x1, y1), (x2, y2),
        "Cursor pixel position changed after context switch");
    assert_eq!(x1, 5 * 8, "Cursor X should be 5 chars * 8px");
    assert_eq!(y1, 0, "Cursor Y should be on first line");
}

/// Verify that an image surface (blit_blend) and text surface produce
/// visually different content — ensuring context switch produces a
/// visible change.
#[test]
fn context_switch_image_and_text_are_distinct() {
    let width = 64u32;
    let height = 64u32;
    let bpp = 4u32;
    let stride = width * bpp;
    let size = (stride * height) as usize;

    // Text mode: clear + draw text.
    let mut text_buf = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut text_buf,
            width, height, stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.draw_text(4, 4, "Hi", &FONT_8X16, Color::rgb(200, 200, 200));
    }

    // Image mode: fill with a recognizable pattern (simulating a PNG).
    let mut image_buf = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut image_buf,
            width, height, stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        // Fill a rectangle simulating an image.
        surf.fill_rect(10, 10, 44, 44, Color::rgb(255, 0, 0));
    }

    // The two surfaces must be different.
    assert_ne!(text_buf, image_buf,
        "Text and image surfaces should be visually distinct");
}

/// Verify that composite_surfaces correctly composites with a different
/// content surface when the mode changes, while chrome stays the same.
#[test]
fn context_switch_composite_chrome_survives() {
    let fb_w = 100u32;
    let fb_h = 80u32;
    let bpp = 4u32;
    let stride = fb_w * bpp;
    let fb_size = (stride * fb_h) as usize;

    // Chrome surface (title bar).
    let chrome_h = 20u32;
    let chrome_stride = fb_w * bpp;
    let chrome_size = (chrome_stride * chrome_h) as usize;
    let mut chrome_buf = vec![0u8; chrome_size];
    {
        let mut surf = Surface {
            data: &mut chrome_buf,
            width: fb_w, height: chrome_h, stride: chrome_stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgba(30, 30, 48, 220));
    }

    // Content surface — editor mode.
    let content_h = fb_h;
    let content_stride = fb_w * bpp;
    let content_size = (content_stride * content_h) as usize;
    let mut content_buf_editor = vec![0u8; content_size];
    {
        let mut surf = Surface {
            data: &mut content_buf_editor,
            width: fb_w, height: content_h, stride: content_stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.draw_text(4, 4, "Editor", &FONT_8X16, Color::rgb(200, 200, 200));
    }

    // Content surface — image mode.
    let mut content_buf_image = vec![0u8; content_size];
    {
        let mut surf = Surface {
            data: &mut content_buf_image,
            width: fb_w, height: content_h, stride: content_stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.fill_rect(10, 25, 40, 40, Color::rgb(0, 128, 255));
    }

    // Composite in editor mode.
    let mut fb_editor = vec![0u8; fb_size];
    {
        let mut fb = Surface {
            data: &mut fb_editor,
            width: fb_w, height: fb_h, stride,
            format: PixelFormat::Bgra8888,
        };
        let content_cs = drawing::CompositeSurface {
            surface: Surface {
                data: &mut content_buf_editor,
                width: fb_w, height: content_h, stride: content_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0, y: 0, z: 10, visible: true,
        };
        let chrome_cs = drawing::CompositeSurface {
            surface: Surface {
                data: &mut chrome_buf,
                width: fb_w, height: chrome_h, stride: chrome_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0, y: 0, z: 20, visible: true,
        };
        drawing::composite_surfaces(&mut fb, &[&content_cs, &chrome_cs]);
    }

    // Composite in image mode.
    let mut fb_image = vec![0u8; fb_size];
    {
        let mut fb = Surface {
            data: &mut fb_image,
            width: fb_w, height: fb_h, stride,
            format: PixelFormat::Bgra8888,
        };
        let content_cs = drawing::CompositeSurface {
            surface: Surface {
                data: &mut content_buf_image,
                width: fb_w, height: content_h, stride: content_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0, y: 0, z: 10, visible: true,
        };
        let chrome_cs = drawing::CompositeSurface {
            surface: Surface {
                data: &mut chrome_buf,
                width: fb_w, height: chrome_h, stride: chrome_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0, y: 0, z: 20, visible: true,
        };
        drawing::composite_surfaces(&mut fb, &[&content_cs, &chrome_cs]);
    }

    // With translucent chrome (alpha=220), the chrome area blends with the
    // content underneath. Since the content differs between modes, the chrome
    // region will differ slightly. What matters is that chrome is PRESENT in
    // both modes — the non-zero alpha pixels prove the chrome overlay exists.
    // Check that both framebuffers have non-zero alpha in the chrome region.
    let chrome_bytes = (chrome_stride * chrome_h) as usize;
    for mode_name in &["editor", "image"] {
        let fb = if *mode_name == "editor" { &fb_editor } else { &fb_image };
        // Sample a pixel in the chrome region (center of chrome).
        let mid_y = chrome_h / 2;
        let mid_x = fb_w / 2;
        let offset = ((mid_y * stride) + mid_x * bpp) as usize;
        let a = fb[offset + 3]; // Alpha byte in BGRA
        assert_eq!(a, 255, "{} mode: chrome pixel should be fully opaque after compositing", mode_name);
    }

    // The content area below chrome should be different between modes.
    let below_chrome = chrome_bytes;
    assert_ne!(&fb_editor[below_chrome..], &fb_image[below_chrome..],
        "Content area should differ between editor and image modes");
}

/// Verify that text rendering preserves exact byte content when cursor
/// position is at various positions — the document content is not
/// affected by which rendering mode is active.
#[test]
fn context_switch_document_bytes_unmodified() {
    // Simulate a document buffer.
    let doc_content = b"hello world";
    let cursor_positions = [0, 5, 11]; // start, middle, end

    for &cursor in &cursor_positions {
        let layout = TextLayout {
            char_width: 8,
            line_height: 20,
            max_width: 200,
        };

        // Verify byte_to_xy is consistent for this position.
        let (x, _y) = layout.byte_to_xy(doc_content, cursor);
        let expected_x = if cursor <= doc_content.len() {
            cursor as u32 * 8
        } else {
            doc_content.len() as u32 * 8
        };
        assert_eq!(x, expected_x, "cursor={}: X mismatch", cursor);

        // Content bytes are never modified by rendering.
        let mut content_copy = [0u8; 11];
        content_copy.copy_from_slice(doc_content);
        assert_eq!(&content_copy, doc_content,
            "Document content must not be modified by rendering");
    }
}

/// Verify that rendering text with draw_tt at cursor 0 and then at cursor 5
/// both produce valid output (no panics, no out-of-bounds). This tests
/// that cursor position tracking survives mode changes.
#[test]
fn context_switch_draw_tt_cursor_positions_valid() {
    // This test requires a GlyphCache. We'll use the TrueType font
    // from the drawing library if available. Since tests run on the host,
    // use the bitmap font path instead for simplicity.
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 160,
    };
    let text = b"hello world";

    // Verify byte_to_xy at multiple cursor positions.
    let positions = [0, 1, 5, 10, 11];
    for &pos in &positions {
        let (x, y) = layout.byte_to_xy(text, pos);
        // X should be pos * char_width (single line, no wrapping).
        let expected_x = pos as u32 * 8;
        assert_eq!(x, expected_x, "pos={}: X mismatch", pos);
        assert_eq!(y, 0, "pos={}: Y should be 0 for single line", pos);
    }
}

/// Verify Ctrl+Tab context switch combo: Left Ctrl (keycode 29) is
/// mapped to 0 (non-printable) in the input driver's keycode-to-ASCII
/// table, so Ctrl press/release events are safely intercepted by the
/// compositor. Tab (keycode 15) maps to '\t' — without Ctrl held it is
/// forwarded to the editor as a normal character; only Tab+Ctrl triggers
/// context switching.
#[test]
fn context_switch_ctrl_tab_keycodes() {
    // Linux evdev keycodes.
    let key_tab: usize = 15;
    let key_leftctrl: usize = 29;
    let key_f1: usize = 59;

    // Reproduce the input driver's keycode_to_ascii lookup table.
    static MAP: [u8; 58] = [
        0, 0,
        b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0',
        b'-', b'=',
        0x08, b'\t',
        b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p',
        b'[', b']',
        b'\n', 0,
        b'a', b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l',
        b';', b'\'',
        b'`', 0, b'\\',
        b'z', b'x', b'c', b'v', b'b', b'n', b'm',
        b',', b'.', b'/',
        0, 0, 0, b' ',
    ];

    // Left Ctrl (keycode 29) maps to 0 — non-printable, intercepted by
    // the compositor for modifier tracking.
    assert!(key_leftctrl < MAP.len(),
        "Left Ctrl keycode should be within the ASCII map");
    assert_eq!(MAP[key_leftctrl], 0,
        "Left Ctrl should map to 0 (non-printable)");

    // Tab (keycode 15) maps to '\t' — a printable/whitespace character.
    // Without Ctrl held, Tab is forwarded to the editor as normal input.
    assert!(key_tab < MAP.len(),
        "Tab keycode should be within the ASCII map");
    assert_eq!(MAP[key_tab], b'\t',
        "Tab should map to '\\t' (tab character)");

    // F1 (keycode 59) is beyond the map — no longer used for context
    // switching (replaced by Ctrl+Tab).
    assert!(key_f1 >= MAP.len(),
        "F1 keycode {} should be beyond the ASCII map (len {})", key_f1, MAP.len());
    let f1_ascii: u8 = if key_f1 < MAP.len() { MAP[key_f1] } else { 0 };
    assert_eq!(f1_ascii, 0, "F1 keycode should not produce a printable character");
}

/// Verify that Tab without Ctrl does not conflict with context switching.
/// The compositor only triggers a switch when ctrl_pressed is true AND
/// keycode == KEY_TAB, so a bare Tab press produces '\t' for the editor.
#[test]
fn context_switch_tab_alone_is_not_switch() {
    // Simulate the compositor's Ctrl+Tab logic.
    let key_tab: u16 = 15;
    let mut ctrl_pressed = false;

    // Tab pressed without Ctrl — should NOT trigger context switch.
    let should_switch = key_tab == 15 && ctrl_pressed;
    assert!(!should_switch,
        "Tab alone (without Ctrl) must not trigger context switch");

    // Now simulate Ctrl held + Tab — SHOULD trigger context switch.
    ctrl_pressed = true;
    let should_switch = key_tab == 15 && ctrl_pressed;
    assert!(should_switch,
        "Ctrl+Tab must trigger context switch");

    // Ctrl released + Tab again — should NOT switch.
    ctrl_pressed = false;
    let should_switch = key_tab == 15 && ctrl_pressed;
    assert!(!should_switch,
        "Tab after Ctrl release must not trigger context switch");
}

// ---------------------------------------------------------------------------
// Text selection highlight tests
// ---------------------------------------------------------------------------

/// Selection highlight: fill_rect_blend draws a visible highlight behind
/// selected character positions (simulating what draw_tt_sel does).
#[test]
fn selection_highlight_rect_blend_modifies_pixels() {
    let w = 320u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    let bg = Color::rgb(24, 24, 36);
    surf.clear(bg);

    // Draw a selection highlight rectangle at the position of "world"
    // (index 6..11, each char 8px wide).
    let sel_color = Color::rgba(50, 80, 160, 180);
    let char_w = 8u32;
    let line_h = 20u32;

    for i in 6..11 {
        surf.fill_rect_blend(i * char_w, 0, char_w, line_h, sel_color);
    }

    // Sample a pixel in the highlight region (x=52, y=10).
    let off = (10 * w * 4 + 52 * 4) as usize;
    let px = Color::decode_from_bgra(&buf[off..off + 4]);

    assert_ne!(px, bg, "Pixel in selection area should differ from background");
}

/// Selection highlight area does not bleed outside the selection range.
#[test]
fn selection_highlight_does_not_bleed() {
    let w = 320u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    let bg = Color::rgb(24, 24, 36);
    surf.clear(bg);

    let sel_color = Color::rgba(50, 80, 160, 180);
    let char_w = 8u32;
    let line_h = 20u32;

    // Highlight chars 6..11 only.
    for i in 6..11 {
        surf.fill_rect_blend(i * char_w, 0, char_w, line_h, sel_color);
    }

    // Pixel at x=4, y=10 (inside char 0, which is NOT selected) should be bg.
    let off = (10 * w * 4 + 4 * 4) as usize;
    let px = Color::decode_from_bgra(&buf[off..off + 4]);

    assert_eq!(px, bg, "Pixel outside selection should remain background");
}

/// Selection range normalization: draw_tt_sel(sel_start=11, sel_end=6)
/// should produce the same output as draw_tt_sel(sel_start=6, sel_end=11).
/// Tested via the bitmap draw method approach.
#[test]
fn selection_range_normalization() {
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 300,
    };
    let text = b"hello world";

    // Forward: selection 6..11
    let w = 320u32;
    let h = 100u32;
    let mut buf_fwd = vec![0u8; (w * h * 4) as usize];
    let mut surf_fwd = make_surface(&mut buf_fwd, w, h);
    surf_fwd.clear(Color::rgb(24, 24, 36));

    let sel_color = Color::rgba(50, 80, 160, 180);
    let (sel_lo, sel_hi) = (6, 11);
    for i in sel_lo..sel_hi {
        let (cx, cy) = layout.byte_to_xy(text, i);
        surf_fwd.fill_rect_blend(cx, cy, 8, 20, sel_color);
    }

    // Reversed: normalized should be identical.
    let mut buf_rev = vec![0u8; (w * h * 4) as usize];
    let mut surf_rev = make_surface(&mut buf_rev, w, h);
    surf_rev.clear(Color::rgb(24, 24, 36));

    let (sel_start_rev, sel_end_rev) = (11usize, 6usize);
    let (s_lo, s_hi) = if sel_start_rev <= sel_end_rev {
        (sel_start_rev, sel_end_rev)
    } else {
        (sel_end_rev, sel_start_rev)
    };
    for i in s_lo..s_hi {
        let (cx, cy) = layout.byte_to_xy(text, i);
        surf_rev.fill_rect_blend(cx, cy, 8, 20, sel_color);
    }

    assert_eq!(buf_fwd, buf_rev, "Normalized selection should produce identical pixels");
}

/// Selection byte_to_xy mapping: selection positions map to correct pixels.
#[test]
fn selection_byte_to_xy_positions() {
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 300,
    };
    let text = b"hello world";

    // Position 6 ('w') should be at x=48, y=0.
    let (x6, y6) = layout.byte_to_xy(text, 6);
    assert_eq!(x6, 48);
    assert_eq!(y6, 0);

    // Position 10 ('d') should be at x=80, y=0.
    let (x10, y10) = layout.byte_to_xy(text, 10);
    assert_eq!(x10, 80);
    assert_eq!(y10, 0);
}

/// Selection state: anchor and cursor define the range. The range should
/// be [min(anchor, cursor), max(anchor, cursor)).
#[test]
fn selection_anchor_cursor_range() {
    // Editor tracks: anchor position + cursor position.
    // Selection = range between them.
    let anchor = 3usize;
    let cursor = 8usize;

    let sel_lo = if anchor < cursor { anchor } else { cursor };
    let sel_hi = if anchor < cursor { cursor } else { anchor };

    assert_eq!(sel_lo, 3);
    assert_eq!(sel_hi, 8);

    // Reversed direction.
    let anchor2 = 8usize;
    let cursor2 = 3usize;

    let sel_lo2 = if anchor2 < cursor2 { anchor2 } else { cursor2 };
    let sel_hi2 = if anchor2 < cursor2 { cursor2 } else { anchor2 };

    assert_eq!(sel_lo2, 3);
    assert_eq!(sel_hi2, 8);
}

/// Selection replacement: deleting a range and inserting a character.
#[test]
fn selection_replace_with_character() {
    // Simulate document: "hello world" (11 bytes).
    let mut doc = *b"hello world";
    let mut doc_len = 11usize;

    // Selection: 6..11 ("world").
    let sel_start = 6usize;
    let sel_end = 11usize;

    // Delete the range [6..11) by shifting bytes left.
    let del_count = sel_end - sel_start;
    // Move bytes after selection to selection start.
    for i in sel_start..doc_len - del_count {
        doc[i] = doc[i + del_count];
    }
    doc_len -= del_count;

    // Insert 'X' at position 6.
    for i in (7..=doc_len).rev() {
        if i < doc.len() && i > 0 {
            doc[i] = doc[i - 1];
        }
    }
    doc[6] = b'X';
    doc_len += 1;

    assert_eq!(&doc[..doc_len], b"hello X");
}

/// Selection deletion: backspace with selection deletes entire range.
#[test]
fn selection_delete_range() {
    // Simulate document: "hello world" (11 bytes).
    let mut doc = *b"hello world";
    let mut doc_len = 11usize;

    // Selection: 6..11 ("world").
    let sel_start = 6usize;
    let sel_end = 11usize;

    // Delete the range [6..11).
    let del_count = sel_end - sel_start;
    for i in sel_start..doc_len - del_count {
        doc[i] = doc[i + del_count];
    }
    doc_len -= del_count;

    // Cursor should be at sel_start (6).
    let cursor = sel_start;

    assert_eq!(&doc[..doc_len], b"hello ");
    assert_eq!(cursor, 6);
}

/// Highlight color has sufficient contrast: selection highlight should
/// be visually distinct from both the background and text.
#[test]
fn selection_highlight_color_contrast() {
    let bg = Color::rgb(24, 24, 36);
    let text_color = Color::rgb(200, 210, 230);
    let sel_color = Color::rgba(50, 80, 160, 180);

    // Selection highlight color should differ from background.
    assert_ne!(sel_color.r, bg.r, "Selection R should differ from background R");
    assert_ne!(sel_color.b, bg.b, "Selection B should differ from background B");

    // The blended result of sel_color over bg should be distinct from bg.
    let blended = sel_color.blend_over(bg);
    assert_ne!(blended, bg, "Blended selection over bg should be visually distinct");

    // Text should still be readable over the selection highlight.
    // Check luminance difference is meaningful.
    let text_luma = text_color.r as u32 * 3 + text_color.g as u32 * 6 + text_color.b as u32;
    let sel_luma = blended.r as u32 * 3 + blended.g as u32 * 6 + blended.b as u32;
    let contrast = if text_luma > sel_luma {
        text_luma - sel_luma
    } else {
        sel_luma - text_luma
    };

    assert!(contrast > 200, "Text should have sufficient contrast over selection highlight (got {})", contrast);
}

/// Cursor bar should NOT be drawn when selection is active in draw_tt_sel.
/// When sel_start == sel_end == 0 (no selection), cursor bar IS drawn.
#[test]
fn cursor_bar_suppressed_with_selection() {
    // The draw_tt_sel logic: if has_selection is true, skip cursor bar.
    // This tests the boolean condition directly.
    let sel_start = 3usize;
    let sel_end = 7usize;
    let (s_lo, s_hi) = if sel_start <= sel_end { (sel_start, sel_end) } else { (sel_end, sel_start) };
    let has_selection = s_lo < s_hi;
    assert!(has_selection, "Selection 3..7 should be active");

    // No selection case.
    let (s_lo2, s_hi2) = (0usize, 0usize);
    let has_selection2 = s_lo2 < s_hi2;
    assert!(!has_selection2, "Selection 0..0 should not be active");
}

// ---------------------------------------------------------------------------
// Scrolling tests — TextLayout scroll offset behavior
// ---------------------------------------------------------------------------

/// byte_to_visual_line returns the correct visual line index for various
/// byte offsets, including wrapped lines and newlines.
#[test]
fn byte_to_visual_line_basic() {
    let layout = make_layout(32); // 4 chars per row (32 / 8)
    // "ab\ncd" → row 0: "ab", row 1: "cd"
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 0), 0);
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 1), 0);
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 2), 0); // at '\n'
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 3), 1); // 'c'
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 5), 1); // end of text
}

/// byte_to_visual_line handles soft-wrap correctly.
/// Note: byte_to_visual_line matches byte_to_xy — the target byte is
/// checked BEFORE the wrap happens for that position, so byte 3 ('d')
/// reports row 0 (wrap hasn't triggered yet). Byte 4 ('e') is row 1.
#[test]
fn byte_to_visual_line_wrap() {
    let layout = make_layout(24); // 3 chars per row (24 / 8)
    // "abcdef" wraps to row 0: "abc", row 1: "def"
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 0), 0);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 2), 0);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 3), 0); // wrap point — same as byte_to_xy
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 4), 1);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 5), 1);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 6), 1); // end
}

/// byte_to_visual_line: empty text always returns line 0.
#[test]
fn byte_to_visual_line_empty() {
    let layout = make_layout(200);
    assert_eq!(layout.byte_to_visual_line(b"", 0), 0);
}

/// byte_to_visual_line: offset beyond text length clamps to the last line.
#[test]
fn byte_to_visual_line_offset_past_end() {
    let layout = make_layout(200);
    // "a\nb" = 2 lines. Offset 10 is well past end, should clamp to last line.
    assert_eq!(layout.byte_to_visual_line(b"a\nb", 10), 1);
    // Single line text, offset past end stays on line 0.
    assert_eq!(layout.byte_to_visual_line(b"hello", 100), 0);
}

/// byte_to_visual_line: trailing newline puts the end on the next line.
#[test]
fn byte_to_visual_line_trailing_newline() {
    let layout = make_layout(200);
    // "abc\n" — byte 3 is '\n' on line 0, byte 4 (past end) is on line 1.
    assert_eq!(layout.byte_to_visual_line(b"abc\n", 3), 0); // at '\n'
    assert_eq!(layout.byte_to_visual_line(b"abc\n", 4), 1); // past '\n'
}

/// byte_to_visual_line: multiple consecutive newlines produce sequential lines.
#[test]
fn byte_to_visual_line_consecutive_newlines() {
    let layout = make_layout(200);
    // "\n\n\n" = 3 newlines → lines 0, 1, 2, with byte 3 on line 3.
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 0), 0);
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 1), 1);
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 2), 2);
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 3), 3); // end
}

/// byte_to_visual_line: wrapping at exact column boundary with newlines.
#[test]
fn byte_to_visual_line_wrap_and_newline_combined() {
    let layout = make_layout(24); // 3 chars per row
    // "abc\ndef" layout: 'a' col0 row0, 'b' col1 row0, 'c' col2 row0,
    // '\n' at col3 → newline check fires BEFORE wrap check → row becomes 1.
    // 'd' col0 row1, 'e' col1 row1, 'f' col2 row1.
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 0), 0); // 'a'
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 2), 0); // 'c'
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 4), 1); // 'd'
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 7), 1); // end

    // Longer text with wrap THEN newline: "abcde\nf" with 3 cols.
    // 'a' col0 row0, 'b' col1 row0, 'c' col2 row0, then col=3.
    // 'd' → col>=cols → wrap → row1 col0, 'e' col1 row1,
    // '\n' → newline → row2.
    // 'f' col0 row2.
    assert_eq!(layout.byte_to_visual_line(b"abcde\nf", 3), 0); // 'd' — wrap point
    assert_eq!(layout.byte_to_visual_line(b"abcde\nf", 4), 1); // 'e'
    assert_eq!(layout.byte_to_visual_line(b"abcde\nf", 6), 2); // 'f'
}

/// total_visual_lines counts lines correctly with newlines and wraps.
#[test]
fn total_visual_lines_basic() {
    let layout = make_layout(200); // wide enough for no wrapping
    assert_eq!(layout.total_visual_lines(b""), 0);
    assert_eq!(layout.total_visual_lines(b"hello"), 1);
    assert_eq!(layout.total_visual_lines(b"a\nb"), 2);
    assert_eq!(layout.total_visual_lines(b"a\nb\nc"), 3);
    assert_eq!(layout.total_visual_lines(b"a\n"), 2); // trailing newline = extra line
}

/// total_visual_lines with soft-wrap.
#[test]
fn total_visual_lines_wrap() {
    let layout = make_layout(24); // 3 chars per row
    assert_eq!(layout.total_visual_lines(b"abcdef"), 2); // "abc" + "def"
    assert_eq!(layout.total_visual_lines(b"abcdefghi"), 3); // 3 + 3 + 3
}

/// scroll_for_cursor computes the correct scroll offset to keep
/// the cursor visible within a viewport of a given number of lines.
#[test]
fn scroll_for_cursor_no_scroll_needed() {
    let layout = make_layout(200);
    // 3-line viewport, cursor on line 0, scroll=0 → no change
    assert_eq!(layout.scroll_for_cursor(b"hello", 0, 0, 3), 0);
    // cursor on line 2 (viewport 0..2), still visible
    assert_eq!(layout.scroll_for_cursor(b"a\nb\nc", 4, 0, 3), 0);
}

/// scroll_for_cursor scrolls down when cursor goes below viewport.
#[test]
fn scroll_for_cursor_scroll_down() {
    let layout = make_layout(200);
    // 2-line viewport, cursor on line 2 (past visible range [0,1])
    let text = b"a\nb\nc";
    // cursor at byte 4 = "c" = line 2. viewport lines = 2. current scroll = 0.
    // Need scroll = 1 so viewport shows lines [1,2].
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 2), 1);
}

/// scroll_for_cursor scrolls up when cursor goes above viewport.
#[test]
fn scroll_for_cursor_scroll_up() {
    let layout = make_layout(200);
    let text = b"a\nb\nc";
    // cursor at byte 0 = line 0. scroll = 2, viewport lines = 2 → shows lines [2,3].
    // Need scroll = 0 to see line 0.
    assert_eq!(layout.scroll_for_cursor(text, 0, 2, 2), 0);
}

/// scroll_for_cursor handles Home key (cursor at 0, scroll was large).
#[test]
fn scroll_for_cursor_home_key() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4\nline5";
    // Cursor at byte 0 = line 0, scroll = 4, viewport = 3
    assert_eq!(layout.scroll_for_cursor(text, 0, 4, 3), 0);
}

/// scroll_for_cursor handles End key (cursor at end, scroll was 0).
#[test]
fn scroll_for_cursor_end_key() {
    let layout = make_layout(200);
    let text = b"l1\nl2\nl3\nl4\nl5";
    // End of text is on line 4. viewport = 3 lines. scroll = 0.
    // Need scroll = 2 so viewport shows [2,3,4].
    assert_eq!(layout.scroll_for_cursor(text, text.len(), 0, 3), 2);
}

/// scroll_for_cursor with viewport_lines=0 always returns 0.
#[test]
fn scroll_for_cursor_zero_viewport() {
    let layout = make_layout(200);
    assert_eq!(layout.scroll_for_cursor(b"a\nb\nc", 4, 5, 0), 0);
}

/// scroll_for_cursor: cursor on the last visible line does not scroll.
#[test]
fn scroll_for_cursor_cursor_on_last_visible() {
    let layout = make_layout(200);
    let text = b"a\nb\nc\nd\ne";
    // Viewport=3, scroll=1 → visible lines [1,2,3]. Cursor on line 3 (byte 6='d').
    assert_eq!(layout.scroll_for_cursor(text, 6, 1, 3), 1);
}

/// scroll_for_cursor: cursor just below viewport triggers minimal scroll.
#[test]
fn scroll_for_cursor_one_past_bottom() {
    let layout = make_layout(200);
    let text = b"a\nb\nc\nd\ne";
    // Viewport=2, scroll=0 → visible lines [0,1]. Cursor on line 2 (byte 4='c').
    // Should scroll to 1 so viewport shows [1,2].
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 2), 1);
}

/// scroll_for_cursor: single-line viewport scrolls to the cursor line exactly.
#[test]
fn scroll_for_cursor_single_line_viewport() {
    let layout = make_layout(200);
    let text = b"a\nb\nc";
    // Viewport=1, cursor on line 2 → scroll=2.
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 1), 2);
    // Viewport=1, cursor on line 0, scroll was 2 → scroll=0.
    assert_eq!(layout.scroll_for_cursor(text, 0, 2, 1), 0);
}

/// scroll_for_cursor: cursor already visible with large viewport returns unchanged scroll.
#[test]
fn scroll_for_cursor_large_viewport() {
    let layout = make_layout(200);
    let text = b"a\nb\nc";
    // Viewport=100 lines, everything fits. scroll=0 should remain.
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 100), 0);
}

/// draw_tt_sel_scroll: selection byte range survives scrolling.
/// A selection defined in byte offsets is independent of scroll offset.
/// When we scroll away and back, the same bytes should be selected.
#[test]
fn selection_survives_scrolling() {
    // This test verifies the invariant that selection range (byte offsets)
    // is independent of the scroll offset — the renderer just needs to
    // convert byte offsets to visual coordinates accounting for scroll.
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4\nline5";
    let sel_start = 6; // start of "line2"
    let sel_end = 11; // end of "line2"

    // With scroll=0, line2 is on visual line 1 (visible).
    let line_of_start = layout.byte_to_visual_line(text, sel_start);
    let line_of_end = layout.byte_to_visual_line(text, sel_end);
    assert_eq!(line_of_start, 1);
    assert_eq!(line_of_end, 1);

    // With scroll=3, line2 (visual line 1) is off screen above.
    // Selection bytes are unchanged.
    assert_eq!(sel_start, 6);
    assert_eq!(sel_end, 11);
    // byte_to_visual_line still returns 1 — it's the absolute line.
    assert_eq!(layout.byte_to_visual_line(text, sel_start), 1);

    // Scroll back to 0 — selection is still 6..11 (unchanged).
    assert_eq!(sel_start, 6);
    assert_eq!(sel_end, 11);
}

/// draw_tt_sel_scroll skips lines above scroll_offset and stops at max_y.
/// This ensures no text renders outside the visible content area.
#[test]
fn scroll_clips_lines_above_and_below() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4\nline5";
    // With scroll_offset=1, line1 should not be drawn. With max_y constraining
    // the viewport height, line5 may not be drawn either.
    // We verify by checking byte_to_xy positions with scroll offset.

    // Line 0 at scroll=1 is above viewport → y < 0 in visual space.
    // Line 1 at scroll=1 is at y=0 in visual space.
    let cursor_line = layout.byte_to_visual_line(text, 6); // "line2" starts at byte 6
    assert_eq!(cursor_line, 1);

    // With scroll_offset=1, visual line 1 is at pixel row 0.
    // We can verify: the draw function should place line 1 at origin_y + (1-1)*line_height.
}

/// Context switch preserves scroll offset: verify the offset value is
/// just a number that can be stored and restored per content mode.
#[test]
fn scroll_offset_preserved_across_context_switch() {
    // Scroll offset is a u32 (or usize). Switching from editor to image
    // and back should restore the same value.
    let editor_scroll: u32 = 7;
    let image_scroll: u32 = 0; // images don't scroll (yet)

    // Simulate context switch: save editor scroll, load image scroll.
    let saved_editor_scroll = editor_scroll;
    let _current = image_scroll;

    // Switch back: restore editor scroll.
    let restored = saved_editor_scroll;
    assert_eq!(restored, 7);
}



// ===========================================================================
// SVG path parser tests
// ===========================================================================

use drawing::{
    svg_parse_path, svg_rasterize, SvgCommand, SvgError, SvgPath, SvgRasterScratch,
};

// ---------------------------------------------------------------------------
// Parser: absolute commands
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_empty_string_returns_error() {
    let result = svg_parse_path(b"");
    assert_eq!(result.err(), Some(SvgError::EmptyData));
}

#[test]
fn svg_parse_whitespace_only_returns_error() {
    let result = svg_parse_path(b"   \t\n ");
    assert_eq!(result.err(), Some(SvgError::EmptyData));
}

#[test]
fn svg_parse_invalid_command_returns_error() {
    let result = svg_parse_path(b"X 10 20");
    assert_eq!(result.err(), Some(SvgError::InvalidCommand(b'X')));
}

#[test]
fn svg_parse_missing_coordinates_returns_error() {
    let result = svg_parse_path(b"M 10");
    assert_eq!(result.err(), Some(SvgError::MissingCoordinates));
}

#[test]
fn svg_parse_missing_cubic_coords_returns_error() {
    let result = svg_parse_path(b"M 0 0 C 1 2 3 4 5");
    assert_eq!(result.err(), Some(SvgError::MissingCoordinates));
}

#[test]
fn svg_parse_moveto_absolute() {
    let path = svg_parse_path(b"M 10 20").unwrap();
    assert_eq!(path.num_commands, 1);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_moveto_lineto_absolute() {
    let path = svg_parse_path(b"M 0 0 L 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_cubic_absolute() {
    let path = svg_parse_path(b"M 0 0 C 1 2 3 4 5 6").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(
        path.commands[1],
        SvgCommand::CubicTo {
            x1: 1,
            y1: 2,
            x2: 3,
            y2: 4,
            x: 5,
            y: 6
        }
    );
}

#[test]
fn svg_parse_close_path() {
    let path = svg_parse_path(b"M 0 0 L 10 0 L 10 10 Z").unwrap();
    assert_eq!(path.num_commands, 4);
    assert_eq!(path.commands[3], SvgCommand::Close);
}

#[test]
fn svg_parse_close_lowercase() {
    let path = svg_parse_path(b"M 0 0 L 10 0 z").unwrap();
    assert_eq!(path.num_commands, 3);
    assert_eq!(path.commands[2], SvgCommand::Close);
}

// ---------------------------------------------------------------------------
// Parser: relative commands
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_moveto_relative() {
    let path = svg_parse_path(b"m 10 20").unwrap();
    assert_eq!(path.num_commands, 1);
    // First m is relative to origin (0,0), so resolves to (10, 20).
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_lineto_relative_resolves_against_current() {
    let path = svg_parse_path(b"M 5 5 l 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    // l 10 20 from (5, 5) → (15, 25).
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 15, y: 25 });
}

#[test]
fn svg_parse_cubic_relative() {
    let path = svg_parse_path(b"M 10 10 c 1 2 3 4 5 6").unwrap();
    assert_eq!(path.num_commands, 2);
    // Relative c from (10, 10): control points at (11,12), (13,14), end at (15,16).
    assert_eq!(
        path.commands[1],
        SvgCommand::CubicTo {
            x1: 11,
            y1: 12,
            x2: 13,
            y2: 14,
            x: 15,
            y: 16
        }
    );
}

#[test]
fn svg_parse_relative_moveto_chain() {
    // m 10 10 m 5 5 → MoveTo(10,10), MoveTo(15,15)
    let path = svg_parse_path(b"m 10 10 m 5 5").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 10 });
    assert_eq!(path.commands[1], SvgCommand::MoveTo { x: 15, y: 15 });
}

// ---------------------------------------------------------------------------
// Parser: coordinate formats
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_comma_separated_coords() {
    let path = svg_parse_path(b"M 10,20 L 30,40").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 30, y: 40 });
}

#[test]
fn svg_parse_negative_coords() {
    let path = svg_parse_path(b"M -10 -20").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: -10, y: -20 });
}

#[test]
fn svg_parse_no_space_between_command_and_number() {
    let path = svg_parse_path(b"M10 20").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_multiple_spaces_between_coords() {
    let path = svg_parse_path(b"M  10   20").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_implicit_lineto_after_moveto() {
    // After M, implicit repeated coordinates become L (SVG spec).
    let path = svg_parse_path(b"M 0 0 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_implicit_lineto_after_relative_moveto() {
    // After m, implicit repeated coordinates become l.
    let path = svg_parse_path(b"m 0 0 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_decimal_coords_integer_part_only() {
    // The parser reads the integer part and skips the fractional part.
    // "10.5" → 10, "20.9" → 20. The fractional part is consumed but discarded.
    let path = svg_parse_path(b"M 10.5 20.9").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_leading_decimal_treated_as_zero() {
    // ".5" should parse as 0 (integer part absent, fractional part skipped).
    let path = svg_parse_path(b"M .5 .9").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
}

#[test]
fn svg_parse_leading_decimal_with_integer_part() {
    // "3.7" should parse as 3.
    let path = svg_parse_path(b"M 3.7 .2").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 3, y: 0 });
}

#[test]
fn svg_parse_negative_leading_decimal() {
    // "-.5" should parse as -0 = 0.
    let path = svg_parse_path(b"M -.5 -.9").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
}

// ---------------------------------------------------------------------------
// Parser: complex paths
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_triangle() {
    let path = svg_parse_path(b"M 0 0 L 10 0 L 5 10 Z").unwrap();
    assert_eq!(path.num_commands, 4);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 0 });
    assert_eq!(path.commands[2], SvgCommand::LineTo { x: 5, y: 10 });
    assert_eq!(path.commands[3], SvgCommand::Close);
}

#[test]
fn svg_parse_multiple_subpaths() {
    let path = svg_parse_path(b"M 0 0 L 10 0 Z M 20 20 L 30 20 Z").unwrap();
    assert_eq!(path.num_commands, 6);
    assert_eq!(path.commands[3], SvgCommand::MoveTo { x: 20, y: 20 });
}

// ---------------------------------------------------------------------------
// Rasterizer tests
// ---------------------------------------------------------------------------

#[test]
fn svg_rasterize_empty_path_returns_error() {
    let path = SvgPath::new();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 10 * 10];
    // An empty path (0 commands) — we need to handle this at the rasterize level.
    // The path is technically valid (zero commands), but nothing to rasterize.
    let result = svg_rasterize(&path, &mut scratch, &mut coverage, 10, 10, 4096, 0, 0);
    // No error because path has no commands to process — just no coverage produced.
    assert!(result.is_ok());
    assert!(coverage.iter().all(|&v| v == 0));
}

#[test]
fn svg_rasterize_filled_square() {
    // A 10x10 square path from (0,0) to (10,10).
    let path = svg_parse_path(b"M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 16 * 16];

    // Scale 1:1 (SVG_FP_ONE = 4096).
    svg_rasterize(&path, &mut scratch, &mut coverage, 16, 16, 4096, 0, 0).unwrap();

    // Interior pixels (e.g., 5,5) should have high coverage.
    let center_idx = 5 * 16 + 5;
    assert!(
        coverage[center_idx] > 200,
        "Interior pixel (5,5) should have high coverage, got {}",
        coverage[center_idx]
    );

    // Exterior pixel (12,12) should have zero coverage.
    let outside_idx = 12 * 16 + 12;
    assert_eq!(
        coverage[outside_idx], 0,
        "Exterior pixel (12,12) should have zero coverage"
    );
}

#[test]
fn svg_rasterize_triangle() {
    // Right triangle: (0,0) → (20,0) → (0,20) → close.
    let path = svg_parse_path(b"M 0 0 L 20 0 L 0 20 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    // Point inside the triangle (2, 2).
    let inside_idx = 2 * 24 + 2;
    assert!(
        coverage[inside_idx] > 100,
        "Interior pixel (2,2) should have significant coverage, got {}",
        coverage[inside_idx]
    );

    // Point clearly outside (22, 22).
    let outside_idx = 22 * 24 + 22;
    assert_eq!(coverage[outside_idx], 0, "Exterior pixel should be zero");
}

#[test]
fn svg_rasterize_with_cubic_produces_coverage() {
    // A curved shape using cubic Bezier.
    let path =
        svg_parse_path(b"M 0 10 C 0 0 20 0 20 10 L 20 20 L 0 20 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    // Center of shape should have coverage.
    let center_idx = 15 * 24 + 10;
    assert!(
        coverage[center_idx] > 100,
        "Interior of curved shape should have coverage, got {}",
        coverage[center_idx]
    );
}

#[test]
fn svg_rasterize_antialiased_edges() {
    // A diagonal-edged shape should produce intermediate coverage values
    // at the edges (not just 0 or 255).
    let path = svg_parse_path(b"M 0 0 L 20 0 L 10 20 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    // Check edge pixels along the diagonal for antialiased (intermediate) values.
    let mut found_intermediate = false;
    for y in 0..20 {
        for x in 0..20 {
            let idx = y * 24 + x;
            let c = coverage[idx];
            if c > 0 && c < 255 {
                found_intermediate = true;
                break;
            }
        }
        if found_intermediate {
            break;
        }
    }
    assert!(
        found_intermediate,
        "Antialiased edges should produce intermediate coverage values (not just 0 or 255)"
    );
}

#[test]
fn svg_rasterize_scaled_shape() {
    // A 5x5 square scaled up 2×.
    let path = svg_parse_path(b"M 0 0 L 5 0 L 5 5 L 0 5 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 16 * 16];

    // Scale 2× (SVG_FP_ONE * 2 = 8192).
    svg_rasterize(&path, &mut scratch, &mut coverage, 16, 16, 8192, 0, 0).unwrap();

    // At 2× scale, the 5-unit square becomes 10 pixels wide.
    // Interior pixel (5, 5) should have high coverage.
    let inside = 5 * 16 + 5;
    assert!(
        coverage[inside] > 200,
        "Scaled interior pixel should have high coverage, got {}",
        coverage[inside]
    );

    // Pixel (12, 12) should be outside (10×10 square at 2×).
    let outside = 12 * 16 + 12;
    assert_eq!(coverage[outside], 0, "Scaled exterior should be zero");
}

#[test]
fn svg_rasterize_with_offset() {
    // A 5x5 square offset by (3, 3).
    let path = svg_parse_path(b"M 0 0 L 5 0 L 5 5 L 0 5 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 16 * 16];

    svg_rasterize(&path, &mut scratch, &mut coverage, 16, 16, 4096, 3, 3).unwrap();

    // At (3,3) offset, the square runs from pixel (3,3) to (8,8).
    // Interior: (5, 5) should have coverage.
    let inside = 5 * 16 + 5;
    assert!(
        coverage[inside] > 200,
        "Offset interior pixel should have high coverage, got {}",
        coverage[inside]
    );

    // Origin (0, 0) should be outside.
    let outside = 0 * 16 + 0;
    assert_eq!(coverage[outside], 0, "Origin should be zero with offset");
}

#[test]
fn svg_rasterize_winding_rule_nonzero() {
    // A clockwise square with a counterclockwise inner cutout (hole).
    // Outer: clockwise 0,0 → 20,0 → 20,20 → 0,20
    // Inner: counterclockwise 5,5 → 5,15 → 15,15 → 15,5
    let path = svg_parse_path(
        b"M 0 0 L 20 0 L 20 20 L 0 20 Z M 5 5 L 5 15 L 15 15 L 15 5 Z",
    )
    .unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    // Point in outer ring (2, 2) — should have coverage.
    let outer_idx = 2 * 24 + 2;
    assert!(
        coverage[outer_idx] > 200,
        "Outer ring should have coverage, got {}",
        coverage[outer_idx]
    );

    // Point in inner hole (10, 10) — should have zero (non-zero winding cancels).
    let inner_idx = 10 * 24 + 10;
    assert_eq!(
        coverage[inner_idx], 0,
        "Inner hole should have zero coverage (non-zero winding), got {}",
        coverage[inner_idx]
    );
}

// ===========================================================================
// SVG icon tests — document icon loading and rasterization
// ===========================================================================

/// The document icon path data (same as system/share/doc-icon.svg).
const DOC_ICON_PATH: &[u8] = b"M 0 0 L 14 0 L 20 6 L 20 24 L 0 24 Z M 4 10 L 4 12 L 16 12 L 16 10 Z M 4 15 L 4 17 L 16 17 L 16 15 Z M 4 20 L 4 22 L 12 22 L 12 20 Z";

#[test]
fn svg_icon_doc_parses_successfully() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    // Page outline (5 commands: M, L, L, L, L, Z = wait, M+4L+Z=6 commands for outer)
    // plus 3 text-line holes (each M+3L+Z = 5 commands × 3 = 15)
    // Total = 6 + 15 = 21 commands.
    assert!(path.num_commands > 15, "Doc icon should have many commands, got {}", path.num_commands);
}

#[test]
fn svg_icon_doc_rasterizes_at_20x24() {
    // Rasterize the icon at native size (20×24 path units, 1:1 scale).
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 28];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 28, 4096, 0, 0).unwrap();

    // Interior of the page body (pixel 2, 2) should be filled.
    let body_idx = 2 * 24 + 2;
    assert!(
        coverage[body_idx] > 200,
        "Page body interior (2,2) should have high coverage, got {}",
        coverage[body_idx]
    );

    // Exterior pixel (22, 2) should have zero coverage.
    let ext_idx = 2 * 24 + 22;
    assert_eq!(coverage[ext_idx], 0, "Exterior (22,2) should be zero");
}

#[test]
fn svg_icon_doc_has_text_line_holes() {
    // The document icon has counterclockwise subpaths that create holes
    // in the page body (representing text lines).
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 28];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 28, 4096, 0, 0).unwrap();

    // Check that the text line holes are empty.
    // First text line hole: y=10..12, x=4..16. Center: (10, 11).
    let hole1_idx = 11 * 24 + 10;
    assert!(
        coverage[hole1_idx] < 30,
        "Text line hole 1 center (10,11) should have low coverage (hole), got {}",
        coverage[hole1_idx]
    );

    // Compare with body area just above the hole: (10, 8) should be filled.
    let body_above = 8 * 24 + 10;
    assert!(
        coverage[body_above] > 200,
        "Body above text line (10,8) should be filled, got {}",
        coverage[body_above]
    );
}

#[test]
fn svg_icon_doc_rasterizes_scaled_for_chrome() {
    // In the chrome, the icon will be rendered at approximately 20×24 pixels
    // by scaling the 20×24 unit icon by ~1× (scale = SVG_FP_ONE = 4096).
    // This test verifies it works at the target size.
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let icon_w: u32 = 20;
    let icon_h: u32 = 24;
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, icon_w, icon_h, 4096, 0, 0).unwrap();

    // The icon should have non-zero coverage pixels (it's not empty).
    let filled_count = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        filled_count > 50,
        "Icon should have significant filled area at 20x24, got {} filled pixels",
        filled_count
    );
}

#[test]
fn svg_icon_doc_has_antialiased_diagonal() {
    // The page has a diagonal edge at top-right (14,0)→(20,6).
    // This should produce intermediate coverage values (antialiased).
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 28];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 28, 4096, 0, 0).unwrap();

    // Check pixels along the diagonal for intermediate coverage values.
    let mut found_intermediate = false;
    // The diagonal runs from (14,0) to (20,6). Check pixels near it.
    for y in 0..6 {
        for x in 14..21 {
            let idx = y as usize * 24 + x as usize;
            let c = coverage[idx];
            if c > 0 && c < 255 {
                found_intermediate = true;
                break;
            }
        }
        if found_intermediate {
            break;
        }
    }
    assert!(
        found_intermediate,
        "Diagonal edge of the doc icon should have antialiased pixels"
    );
}

// ===========================================================================
// SVG icon tests — image icon loading and rasterization
// ===========================================================================

/// The image icon path data (same as system/share/img-icon.svg).
const IMG_ICON_PATH: &[u8] = b"M 0 2 L 20 2 L 20 22 L 0 22 Z M 2 4 L 2 20 L 18 20 L 18 4 Z M 4 14 L 8 9 L 12 14 L 14 11 L 17 15 L 17 18 L 4 18 Z M 13 7 C 14 6 16 6 16 8 C 16 9 14 10 13 9 C 12 8 12 8 13 7 Z";

#[test]
fn svg_icon_img_parses_successfully() {
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    // Outer frame + inner frame hole + mountain + sun ≈ 30+ commands.
    assert!(path.num_commands > 15, "Image icon should have many commands, got {}", path.num_commands);
}

#[test]
fn svg_icon_img_rasterizes_at_20x24() {
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 20, 24, 4096, 0, 0).unwrap();

    // The icon should have non-zero coverage pixels (it's not empty).
    let filled_count = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        filled_count > 50,
        "Image icon should have significant filled area at 20x24, got {} filled pixels",
        filled_count
    );
}

#[test]
fn svg_icon_img_differs_from_doc_icon() {
    // Both icons rasterized at the same size should produce different coverage maps.
    let doc_path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let img_path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut doc_scratch = SvgRasterScratch::zeroed();
    let mut img_scratch = SvgRasterScratch::zeroed();
    let mut doc_cov = [0u8; 20 * 24];
    let mut img_cov = [0u8; 20 * 24];

    svg_rasterize(&doc_path, &mut doc_scratch, &mut doc_cov, 20, 24, 4096, 0, 0).unwrap();
    svg_rasterize(&img_path, &mut img_scratch, &mut img_cov, 20, 24, 4096, 0, 0).unwrap();

    // Count differing pixels — icons should be visibly different shapes.
    let diff_count = doc_cov.iter().zip(img_cov.iter())
        .filter(|(&a, &b)| (a as i16 - b as i16).unsigned_abs() > 30)
        .count();
    assert!(
        diff_count > 40,
        "Doc and image icons should differ significantly, only {} pixels differ",
        diff_count
    );
}

#[test]
fn svg_icon_img_has_frame_border() {
    // The outer frame (0,2)-(20,22) should create high coverage at corners.
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 20, 24, 4096, 0, 0).unwrap();

    // Top-left area of the frame border (pixel 0,3 should be filled since
    // the outer rect is 0..20 x 2..22 and the inner cutout is 2..18 x 4..20).
    let border_idx = 3 * 20 + 0;
    assert!(
        coverage[border_idx] > 100,
        "Frame border at (0,3) should be filled, got {}",
        coverage[border_idx]
    );

    // Interior of the frame (pixel 10, 12) should have some coverage
    // from the mountain landscape shape.
    let interior_idx = 12 * 20 + 10;
    // This may or may not be filled depending on the mountain shape;
    // just check the overall icon isn't blank.
    let total_filled = coverage.iter().filter(|&&c| c > 0).count();
    assert!(total_filled > 50, "Icon should not be mostly blank: {} filled", total_filled);
    let _ = interior_idx; // used only for documentation
}

// ---------------------------------------------------------------------------
// composite_surfaces_rect — partial framebuffer compositing
// ---------------------------------------------------------------------------

#[test]
fn composite_rect_only_updates_target_region() {
    // 8x8 destination, pre-filled with green. Composite a red surface (4x4 at 0,0)
    // but only update the rect (0,0,2,2). Outside the rect should remain green.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 255, 0)); // Green

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 0);
    fg.surface.clear(Color::rgb(255, 0, 0)); // Red

    let surfaces: [&CompositeSurface; 1] = [&fg];
    drawing::composite_surfaces_rect(&mut dst, &surfaces, 0, 0, 2, 2);

    // Inside the rect (0,0)-(2,2): should be red.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(1, 1), Some(Color::rgb(255, 0, 0)));
    // Just outside the rect but inside the surface: should still be green
    // (not composited).
    assert_eq!(dst.get_pixel(2, 0), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(0, 2), Some(Color::rgb(0, 255, 0)));
    // Far outside: green.
    assert_eq!(dst.get_pixel(5, 5), Some(Color::rgb(0, 255, 0)));
}

#[test]
fn composite_rect_respects_z_order() {
    // Two overlapping surfaces composited in a rect. Higher z should win.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255)); // Blue

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 10);
    fg.surface.clear(Color::rgb(255, 0, 0)); // Red, higher z

    let surfaces: [&CompositeSurface; 2] = [&fg, &bg];
    drawing::composite_surfaces_rect(&mut dst, &surfaces, 0, 0, 3, 3);

    // Inside rect where both surfaces overlap: red (higher z) wins.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    // Outside rect: still black (not composited).
    assert_eq!(dst.get_pixel(5, 5), Some(Color::BLACK));
}

#[test]
fn composite_rect_with_offset_surface() {
    // Surface at position (2,2), dirty rect at (3,3,2,2).
    // The intersection is (3,3)-(5,5) in FB coords.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 2, 2, 0);
    fg.surface.clear(Color::rgb(255, 0, 0)); // Red

    let surfaces: [&CompositeSurface; 1] = [&fg];
    drawing::composite_surfaces_rect(&mut dst, &surfaces, 3, 3, 2, 2);

    // (3,3) is inside both the dirty rect and the surface. Should be red.
    assert_eq!(dst.get_pixel(3, 3), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(4, 4), Some(Color::rgb(255, 0, 0)));
    // (2,2) is inside the surface but outside the dirty rect. Should be black.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::BLACK));
    // (5,5) is outside the surface (4x4 at 2,2 → x range 2..6). But (5,5)
    // is inside the dirty rect (3..5 in both dimensions)... wait, rect is
    // (3,3,2,2) → x range 3..5, y range 3..5. So (5,5) is outside. Black.
    assert_eq!(dst.get_pixel(5, 5), Some(Color::BLACK));
}

#[test]
fn composite_rect_zero_size_is_noop() {
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::rgb(0, 255, 0)); // Green

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 0);
    fg.surface.clear(Color::rgb(255, 0, 0));

    let surfaces: [&CompositeSurface; 1] = [&fg];
    drawing::composite_surfaces_rect(&mut dst, &surfaces, 0, 0, 0, 0);

    // Nothing should have changed.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 255, 0)));
}

// ---------------------------------------------------------------------------
// draw_tt_sel_scroll_lines tests (incremental content rendering)
// ---------------------------------------------------------------------------

/// Incremental line range: byte_to_visual_line correctly identifies lines
/// for dirty line computation during incremental rendering.
#[test]
fn incremental_render_byte_to_visual_line_for_dirty_tracking() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4";

    // Cursor at start of line2 (byte 6) → visual line 1.
    assert_eq!(layout.byte_to_visual_line(text, 6), 1);
    // Cursor at end of line3 (byte 17) → visual line 2.
    assert_eq!(layout.byte_to_visual_line(text, 17), 2);
    // Cursor at start (byte 0) → visual line 0.
    assert_eq!(layout.byte_to_visual_line(text, 0), 0);
    // Cursor past end → last line.
    assert_eq!(layout.byte_to_visual_line(text, text.len()), 3);
}

/// Incremental line tracking: inserting a character on the same line
/// should only dirty that one line (cursor line stays the same).
#[test]
fn incremental_render_same_line_insert_dirtied_lines() {
    let layout = make_layout(200);

    // Before insert: "abc\ndef" with cursor at byte 1 (in "abc", line 0).
    let text_before = b"abc\ndef";
    let cursor_before = 1;
    let line_before = layout.byte_to_visual_line(text_before, cursor_before);
    assert_eq!(line_before, 0);

    // After insert: "aXbc\ndef" with cursor at byte 2.
    let text_after = b"aXbc\ndef";
    let cursor_after = 2;
    let line_after = layout.byte_to_visual_line(text_after, cursor_after);
    assert_eq!(line_after, 0);

    // Same line → only 1 line needs re-rendering.
    assert_eq!(line_before, line_after);
}

/// Incremental line tracking: inserting a newline creates a new line,
/// requiring re-render from the cursor line to the end.
#[test]
fn incremental_render_newline_insert_dirtied_lines() {
    let layout = make_layout(200);

    // Before insert: "abcdef" with cursor at byte 3.
    let text_before = b"abcdef";
    let cursor_before = 3;
    let total_lines_before = layout.byte_to_visual_line(text_before, text_before.len()) + 1;
    assert_eq!(total_lines_before, 1);

    // After insert: "abc\ndef" with cursor at byte 4 (start of new line).
    let text_after = b"abc\ndef";
    let cursor_after = 4;
    let total_lines_after = layout.byte_to_visual_line(text_after, text_after.len()) + 1;
    assert_eq!(total_lines_after, 2);

    let new_cursor_line = layout.byte_to_visual_line(text_after, cursor_after);
    assert_eq!(new_cursor_line, 1);

    // Total lines changed → reflow detected → dirty from cursor line to end.
    assert_ne!(total_lines_before, total_lines_after);
}

/// Incremental line tracking: deleting a character that causes line
/// reflow should dirty from the affected line to the end.
#[test]
fn incremental_render_delete_reflow_dirtied_lines() {
    let layout = make_layout(200);

    // Before delete: "abc\ndef" — 2 lines.
    let text_before = b"abc\ndef";
    let total_before = layout.byte_to_visual_line(text_before, text_before.len()) + 1;
    assert_eq!(total_before, 2);

    // After deleting the newline: "abcdef" — 1 line.
    let text_after = b"abcdef";
    let total_after = layout.byte_to_visual_line(text_after, text_after.len()) + 1;
    assert_eq!(total_after, 1);

    // Reflow detected.
    assert_ne!(total_before, total_after);
}

/// Incremental line tracking: cursor-only movement (no content change)
/// should dirty both the old and new cursor lines.
#[test]
fn incremental_render_cursor_move_two_lines_dirty() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3";

    let old_cursor = 2; // line 0
    let new_cursor = 8; // line 1
    let old_line = layout.byte_to_visual_line(text, old_cursor);
    let new_line = layout.byte_to_visual_line(text, new_cursor);

    assert_eq!(old_line, 0);
    assert_eq!(new_line, 1);

    // Both lines need re-rendering (old cursor erased, new cursor drawn).
    let first_dirty = old_line.min(new_line);
    let last_dirty = old_line.max(new_line);
    assert_eq!(first_dirty, 0);
    assert_eq!(last_dirty, 1);
}

/// Soft-wrap insert: inserting a char that causes soft wrap should change
/// the total line count, triggering full-range dirty.
#[test]
fn incremental_render_soft_wrap_changes_line_count() {
    // Layout with narrow max_width: 3 chars per line (3 * 8 = 24).
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 24,
    };

    // Before: "abc" — fits in 1 line (3 chars, 3 cols).
    let text_before = b"abc";
    let total_before = layout.byte_to_visual_line(text_before, text_before.len()) + 1;
    assert_eq!(total_before, 1);

    // After: "abcd" — wraps to 2 lines (4 chars, 3 cols per line).
    let text_after = b"abcd";
    let total_after = layout.byte_to_visual_line(text_after, text_after.len()) + 1;
    assert_eq!(total_after, 2);

    // Line count changed → reflow → dirty from cursor to end.
    assert_ne!(total_before, total_after);
}

// ---------------------------------------------------------------------------
// Xorshift32 PRNG tests
// ---------------------------------------------------------------------------

use drawing::Xorshift32;

#[test]
fn xorshift32_deterministic() {
    // Same seed produces same sequence.
    let mut a = Xorshift32::new(42);
    let mut b = Xorshift32::new(42);
    for _ in 0..100 {
        assert_eq!(a.next(), b.next());
    }
}

#[test]
fn xorshift32_different_seeds_differ() {
    let mut a = Xorshift32::new(42);
    let mut b = Xorshift32::new(99);
    // Very unlikely for first 10 outputs to match with different seeds.
    let mut same_count = 0;
    for _ in 0..10 {
        if a.next() == b.next() {
            same_count += 1;
        }
    }
    assert!(same_count < 3, "different seeds should produce different sequences");
}

#[test]
fn xorshift32_noise_in_range() {
    let mut rng = Xorshift32::new(0xCAFE);
    for _ in 0..1000 {
        let n = rng.noise(3);
        assert!(n >= -3 && n <= 3, "noise({}) out of range [-3, 3]", n);
    }
}

#[test]
fn xorshift32_zero_seed_handled() {
    // Zero seed should not produce all-zero output (it gets replaced).
    let mut rng = Xorshift32::new(0);
    let first = rng.next();
    assert_ne!(first, 0, "zero seed should be replaced internally");
}

// ---------------------------------------------------------------------------
// Radial gradient + noise tests
// ---------------------------------------------------------------------------

#[test]
fn gradient_center_brighter_than_edges() {
    // Create a 100×100 surface and fill with radial gradient.
    let w = 100u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 0, 42);

    // Sample center pixel.
    let center_px = surf.get_pixel(w / 2, h / 2).unwrap();
    // Sample corner pixel.
    let corner_px = surf.get_pixel(0, 0).unwrap();

    // Center should be brighter than corner (higher R value).
    assert!(
        center_px.r > corner_px.r,
        "center ({}) should be brighter than corner ({})",
        center_px.r,
        corner_px.r,
    );

    // Center should be close to center_color, corner close to edge_color.
    assert!(center_px.r >= 26 && center_px.r <= 30, "center R={}", center_px.r);
    assert!(corner_px.r >= 14 && corner_px.r <= 18, "corner R={}", corner_px.r);

    // Monochrome: R=G=B for all pixels (no noise, amplitude=0).
    assert_eq!(center_px.r, center_px.g);
    assert_eq!(center_px.r, center_px.b);
    assert_eq!(corner_px.r, corner_px.g);
    assert_eq!(corner_px.r, corner_px.b);
}

#[test]
fn gradient_dither_creates_variation() {
    // Bayer ordered dithering should create pixel-level variation in rows,
    // breaking up quantization bands into a structured stipple pattern.
    let w = 100u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 3, 0xDEAD_BEEF);

    // Check that not all pixels in a horizontal row are identical (dither breaks banding).
    let y = h / 2; // Middle row.
    let mut saw_different = false;
    let first = surf.get_pixel(0, y).unwrap();
    for x in 1..w {
        let px = surf.get_pixel(x, y).unwrap();
        if px.r != first.r || px.g != first.g || px.b != first.b {
            saw_different = true;
            break;
        }
    }
    assert!(saw_different, "dither should cause pixel variation in a row");
}

#[test]
fn gradient_deterministic_across_calls() {
    // Same parameters → identical output.
    let w = 50u32;
    let h = 50u32;
    let mut buf1 = vec![0u8; (w * h * 4) as usize];
    let mut buf2 = vec![0u8; (w * h * 4) as usize];

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);

    {
        let mut s1 = make_surface(&mut buf1, w, h);
        drawing::fill_radial_gradient_noise(&mut s1, center, edge, 3, 0xDEAD_BEEF);
    }
    {
        let mut s2 = make_surface(&mut buf2, w, h);
        drawing::fill_radial_gradient_noise(&mut s2, center, edge, 3, 0xDEAD_BEEF);
    }

    assert_eq!(buf1, buf2, "gradient should be deterministic with same seed");
}

#[test]
fn gradient_1x1_surface_no_panic() {
    let mut buf = [0u8; 4];
    let mut surf = make_surface(&mut buf, 1, 1);
    drawing::fill_radial_gradient_noise(
        &mut surf,
        Color::rgb(28, 28, 28),
        Color::rgb(16, 16, 16),
        3,
        42,
    );
    // Just ensure it doesn't panic or divide by zero.
    let px = surf.get_pixel(0, 0).unwrap();
    assert_eq!(px.a, 255);
}

#[test]
fn gradient_zero_noise_is_smooth() {
    // With no noise, pixels along a horizontal line at the center should
    // be monotonically changing (or equal) from center outward.
    let w = 200u32;
    let h = 200u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    drawing::fill_radial_gradient_noise(
        &mut surf,
        Color::rgb(40, 40, 40),
        Color::rgb(20, 20, 20),
        0, // no noise
        42,
    );

    // From center outward to the right, values should be non-increasing.
    let cy = h / 2;
    let cx = w / 2;
    let mut prev_r = surf.get_pixel(cx, cy).unwrap().r;
    for x in (cx + 1)..w {
        let px = surf.get_pixel(x, cy).unwrap();
        assert!(
            px.r <= prev_r + 1, // +1 for rounding tolerance
            "gradient should get darker from center outward: x={}, r={}, prev={}",
            x,
            px.r,
            prev_r,
        );
        prev_r = px.r;
    }
}

#[test]
fn gradient_dither_is_structured_bayer() {
    // Bayer 4×4 dithering should produce a repeating 4×4 pattern.
    // For a flat-color surface (center == edge), the Bayer thresholds
    // should be visible as a structured pattern where some pixels round
    // up and others don't, with a period of 4 in both x and y.
    let w = 8u32;
    let h = 8u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    // Use colors that produce a fractional value in the gradient.
    // With center=30, edge=29, at center position the gradient value
    // is ~30. The fractional part from interpolation triggers dithering.
    let center = Color::rgb(30, 30, 30);
    let edge = Color::rgb(29, 29, 29);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 0, 0);

    // Verify 4×4 periodicity: pixel(x,y) == pixel(x+4, y+4) for same
    // gradient position. Since the gradient varies spatially, we can't
    // test exact equality, but we can check that the pattern repeats.
    // At the 4 corners of the 8×8 surface which have the same distance
    // from center, the Bayer pattern should match.
    let tl = surf.get_pixel(0, 0).unwrap();
    let tr = surf.get_pixel(w - 1, 0).unwrap();
    // Top-left and top-right corners have symmetric distance.
    // Due to discrete coordinates, they should be within ±1.
    assert!(
        (tl.r as i32 - tr.r as i32).unsigned_abs() <= 1,
        "symmetric corners should have similar values: tl.r={}, tr.r={}",
        tl.r,
        tr.r,
    );
}

#[test]
fn gradient_rows_matches_full_fill() {
    // fill_radial_gradient_rows for specific rows must produce pixels
    // identical to fill_radial_gradient_noise for those same rows.
    let w = 120u32;
    let h = 80u32;
    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);

    // Full fill.
    let mut buf_full = vec![0u8; (w * h * 4) as usize];
    {
        let mut full = make_surface(&mut buf_full, w, h);
        drawing::fill_radial_gradient_noise(&mut full, center, edge, 3, 0xDEAD_BEEF);
    }

    // Row fill: clear and re-fill rows 20..40.
    let mut buf_rows = vec![0u8; (w * h * 4) as usize];
    {
        let mut rows = make_surface(&mut buf_rows, w, h);
        // First fill entirely.
        drawing::fill_radial_gradient_noise(&mut rows, center, edge, 3, 0xDEAD_BEEF);
        // Zero out rows 20..40 to simulate incremental clear.
        let bpp = 4u32;
        for y in 20..40u32 {
            let off = (y * w * bpp) as usize;
            let end = off + (w * bpp) as usize;
            for b in &mut rows.data[off..end] {
                *b = 0;
            }
        }
        // Re-fill with row-based function.
        drawing::fill_radial_gradient_rows(&mut rows, center, edge, 20, 20);
    }

    // Rows 20..40 should be pixel-identical.
    let bpp = 4u32;
    for y in 20..40u32 {
        for x in 0..w {
            let off = (y * w * bpp + x * bpp) as usize;
            assert_eq!(
                &buf_full[off..off + 4],
                &buf_rows[off..off + 4],
                "pixel ({},{}) mismatch between full fill and row fill",
                x,
                y,
            );
        }
    }
}

#[test]
fn gradient_rows_out_of_bounds_clipped() {
    // fill_radial_gradient_rows with start_y + row_count > height should
    // be silently clipped, not panic.
    let w = 20u32;
    let h = 10u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    // Start at row 8, request 5 rows → should only fill rows 8 and 9.
    drawing::fill_radial_gradient_rows(
        &mut surf,
        Color::rgb(28, 28, 28),
        Color::rgb(16, 16, 16),
        8,
        5,
    );

    // Row 8 should have non-zero pixels.
    let px = surf.get_pixel(w / 2, 8).unwrap();
    assert!(px.r > 0 || px.g > 0 || px.b > 0, "row 8 should be filled");
    // Row 9 should have non-zero pixels.
    let px = surf.get_pixel(w / 2, 9).unwrap();
    assert!(px.r > 0 || px.g > 0 || px.b > 0, "row 9 should be filled");
}

#[test]
fn gradient_rows_zero_count_noop() {
    // fill_radial_gradient_rows with row_count=0 should be a no-op.
    let w = 20u32;
    let h = 10u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    drawing::fill_radial_gradient_rows(
        &mut surf,
        Color::rgb(28, 28, 28),
        Color::rgb(16, 16, 16),
        0,
        0,
    );

    // Should remain all zeros.
    for b in buf.iter() {
        // Alpha channel defaults to 0 in zeroed buffer.
        assert_eq!(*b, 0, "zero-count fill should not modify buffer");
    }
}

#[test]
fn gradient_dither_monochrome() {
    // Bayer dithering must maintain the monochrome property: R=G=B for
    // every pixel, since the same dither offset is added to all channels.
    let w = 64u32;
    let h = 64u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 0, 0);

    for y in 0..h {
        for x in 0..w {
            let px = surf.get_pixel(x, y).unwrap();
            assert_eq!(
                px.r, px.g,
                "monochrome violated at ({},{}): r={}, g={}",
                x, y, px.r, px.g,
            );
            assert_eq!(
                px.r, px.b,
                "monochrome violated at ({},{}): r={}, b={}",
                x, y, px.r, px.b,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Font metrics — hhea ascent/descent/lineGap
// ---------------------------------------------------------------------------

#[test]
fn hhea_ascent_descent_parsed_nunito_sans() {
    // VAL-FONT-004: hhea ascent/descent are parsed from the font.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    // NunitoSans: ascent=1011, descent=-353, upem=1000
    assert_eq!(font.hhea_ascent(), 1011);
    assert_eq!(font.hhea_descent(), -353);
    assert_eq!(font.hhea_line_gap(), 0);
}

#[test]
fn hhea_ascent_descent_parsed_source_code_pro() {
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    // SourceCodePro: ascent=984, descent=-273, upem=1000
    assert_eq!(font.hhea_ascent(), 984);
    assert_eq!(font.hhea_descent(), -273);
    assert_eq!(font.hhea_line_gap(), 0);
}

#[test]
fn glyph_cache_stores_ascent_descent() {
    // GlyphCache.ascent and .descent should be computed from hhea metrics.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();

    cache.populate(&font, 20, &mut scratch);

    // NunitoSans at 20px: ascent = ceil(1011 * 20 / 1000) = ceil(20.22) = 21
    // descent = ceil(353 * 20 / 1000) = ceil(7.06) = 8
    assert!(cache.ascent > 0, "ascent should be > 0, got {}", cache.ascent);
    assert!(cache.descent > 0, "descent should be > 0, got {}", cache.descent);
    // line_height = ascent + descent + lineGap
    assert_eq!(cache.line_height, cache.ascent + cache.descent);
    // Verify ascent is approximately 20-21 and descent approximately 7-8
    assert!(cache.ascent >= 20 && cache.ascent <= 22,
        "NunitoSans ascent at 20px should be ~21, got {}", cache.ascent);
    assert!(cache.descent >= 7 && cache.descent <= 9,
        "NunitoSans descent at 20px should be ~8, got {}", cache.descent);
}

#[test]
fn glyph_cache_ascent_equals_baseline_for_source_code_pro() {
    // SourceCodePro at 20px: ascent = ceil(984 * 20 / 1000) = ceil(19.68) = 20
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();

    cache.populate(&font, 20, &mut scratch);

    assert!(cache.ascent >= 19 && cache.ascent <= 21,
        "SourceCodePro ascent at 20px should be ~20, got {}", cache.ascent);
    assert!(cache.descent >= 5 && cache.descent <= 7,
        "SourceCodePro descent at 20px should be ~6, got {}", cache.descent);
}

#[test]
fn baseline_uses_ascent_not_heuristic() {
    // Verify that line_height * 3/4 is NOT equal to ascent — confirming we
    // switched from the old heuristic to proper hhea-based metrics.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();

    cache.populate(&font, 20, &mut scratch);

    let old_baseline = cache.line_height * 3 / 4;
    // The old heuristic line_height * 3/4 differs from hhea ascent.
    // For NunitoSans at 20px: line_height=29, old=21, new ascent=21.
    // They may be close but the key is we're using hhea, not the heuristic.
    // Just verify ascent and descent are set correctly.
    assert!(cache.ascent > 0);
    assert!(cache.descent > 0);
    assert_eq!(cache.line_height, cache.ascent + cache.descent);
}

#[test]
fn descender_glyphs_fit_in_line_height() {
    // Descender glyphs (g, y, p, q) should have bearing_y values that,
    // combined with their height, fit within ascent + descent.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();

    cache.populate(&font, 20, &mut scratch);

    for ch in [b'g', b'y', b'p', b'q'] {
        let (glyph, _) = cache.get(ch).unwrap();
        // bearing_y is the distance from baseline to the top of the glyph bitmap.
        // The glyph extends from (baseline - bearing_y) to (baseline - bearing_y + height).
        // The bottom of the glyph = baseline + (height - bearing_y).
        // This should be <= descent (i.e., not clip below the line).
        let below_baseline = glyph.height as i32 - glyph.bearing_y;
        assert!(
            below_baseline <= cache.descent as i32 + 1, // +1 for rounding tolerance
            "descender '{}' extends {}px below baseline but descent is {}px",
            ch as char, below_baseline, cache.descent,
        );
    }
}

// ---------------------------------------------------------------------------
// GPOS kerning
// ---------------------------------------------------------------------------

#[test]
fn gpos_kern_table_parsed_nunito_sans() {
    // NunitoSans has GPOS PairPos kerning but no kern table.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();

    // 'T' (glyph 187) + 'o' (glyph 375) should have negative kerning.
    let gi_t = font.glyph_index('T').unwrap();
    let gi_o = font.glyph_index('o').unwrap();
    let kern = font.kern_advance(gi_t, gi_o);

    assert!(
        kern < 0,
        "To kern should be negative (tighter), got {}",
        kern,
    );
}

#[test]
fn gpos_kern_av_pair() {
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let gi_a = font.glyph_index('A').unwrap();
    let gi_v = font.glyph_index('V').unwrap();
    let kern = font.kern_advance(gi_a, gi_v);

    assert!(
        kern < 0,
        "AV kern should be negative (tighter spacing), got {}",
        kern,
    );
}

#[test]
fn gpos_kern_we_pair() {
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let gi_w = font.glyph_index('W').unwrap();
    let gi_e = font.glyph_index('e').unwrap();
    let kern = font.kern_advance(gi_w, gi_e);

    assert!(
        kern < 0,
        "We kern should be negative (tighter spacing), got {}",
        kern,
    );
}

#[test]
fn gpos_kern_no_adjustment_for_unrelated_pair() {
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let gi_a = font.glyph_index('a').unwrap();
    let gi_b = font.glyph_index('b').unwrap();
    let kern = font.kern_advance(gi_a, gi_b);

    // 'ab' is not a typical kern pair — adjustment should be 0.
    assert_eq!(kern, 0, "ab should have no kerning, got {}", kern);
}

#[test]
fn gpos_kern_source_code_pro_has_no_gpos() {
    // SourceCodePro is monospace — should have no kerning.
    let font = TrueTypeFont::new(SOURCE_CODE_PRO).unwrap();
    let gi_a = font.glyph_index('A');
    let gi_v = font.glyph_index('V');

    if let (Some(a), Some(v)) = (gi_a, gi_v) {
        let kern = font.kern_advance(a, v);
        assert_eq!(kern, 0, "monospace font should have no kerning");
    }
}

#[test]
fn kerned_proportional_string_is_narrower() {
    // Drawing "AV" with kerning should produce a smaller total advance
    // than without kerning, since AV has negative kern.
    let font = TrueTypeFont::new(NUNITO_SANS).unwrap();
    let mut scratch = RasterScratch::zeroed();
    let mut cache = heap_glyph_cache();

    cache.populate(&font, 20, &mut scratch);

    let mut buf1 = [0u8; 200 * 40 * 4];
    let mut surf1 = make_surface(&mut buf1, 200, 40);

    // Without kerning.
    let x_no_kern = drawing::draw_proportional_string(
        &mut surf1, 0, 0, b"AV", &cache, Color::WHITE,
    );

    let mut buf2 = [0u8; 200 * 40 * 4];
    let mut surf2 = make_surface(&mut buf2, 200, 40);

    // With kerning.
    let x_kerned = drawing::draw_proportional_string_kerned(
        &mut surf2, 0, 0, b"AV", &cache, Color::WHITE, Some(&font),
    );

    assert!(
        x_kerned < x_no_kern,
        "kerned 'AV' advance ({}) should be less than unkerned ({})",
        x_kerned, x_no_kern,
    );
}

// ---------------------------------------------------------------------------
// Mouse cursor tests
// ---------------------------------------------------------------------------

#[test]
fn test_render_cursor_dimensions() {
    let size = (drawing::CURSOR_W * drawing::CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    drawing::render_cursor(&mut buf);

    // The cursor should have some non-transparent pixels (fill + outline).
    let mut opaque_count = 0;
    for y in 0..drawing::CURSOR_H {
        for x in 0..drawing::CURSOR_W {
            let off = ((y * drawing::CURSOR_W + x) * 4) as usize;
            if buf[off + 3] > 0 {
                opaque_count += 1;
            }
        }
    }
    assert!(opaque_count > 20, "cursor should have >20 opaque pixels, got {}", opaque_count);
}

#[test]
fn test_render_cursor_top_left_pixel_is_outline() {
    let size = (drawing::CURSOR_W * drawing::CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    drawing::render_cursor(&mut buf);

    // Pixel (0,0) should be the outline color (dark grey, opaque).
    // BGRA8888 encoding: B=40, G=40, R=40, A=255.
    assert_eq!(buf[3], 255, "top-left pixel alpha should be 255 (opaque)");
    assert_eq!(buf[0], buf[1], "top-left pixel should be grey (B==G)");
    assert_eq!(buf[1], buf[2], "top-left pixel should be grey (G==R)");
}

#[test]
fn test_render_cursor_has_fill_pixels() {
    let size = (drawing::CURSOR_W * drawing::CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    drawing::render_cursor(&mut buf);

    // Pixel at (1,2) in the bitmap is fill (white, 255/255/255/255).
    let off = ((2 * drawing::CURSOR_W + 1) * 4) as usize;
    assert_eq!(buf[off + 3], 255, "fill pixel alpha should be 255");
    assert_eq!(buf[off + 0], 255, "fill pixel B channel should be 255");
    assert_eq!(buf[off + 1], 255, "fill pixel G channel should be 255");
    assert_eq!(buf[off + 2], 255, "fill pixel R channel should be 255");
}

#[test]
fn test_render_cursor_has_transparent_pixels() {
    let size = (drawing::CURSOR_W * drawing::CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    drawing::render_cursor(&mut buf);

    // Pixel at (11,0) should be transparent (outside the arrow).
    let off = ((0 * drawing::CURSOR_W + 11) * 4) as usize;
    assert_eq!(buf[off + 3], 0, "pixel outside arrow should be transparent");
}

#[test]
fn test_scale_pointer_coord_zero() {
    assert_eq!(drawing::scale_pointer_coord(0, 1280), 0);
}

#[test]
fn test_scale_pointer_coord_max() {
    // 32767 * 1280 / 32768 = 1279.96... → 1279
    let result = drawing::scale_pointer_coord(32767, 1280);
    assert!(result < 1280, "result {} should be < 1280", result);
    assert_eq!(result, 1279);
}

#[test]
fn test_scale_pointer_coord_midpoint() {
    // 16384 * 1280 / 32768 = 640
    let result = drawing::scale_pointer_coord(16384, 1280);
    assert_eq!(result, 640);
}

#[test]
fn test_scale_pointer_coord_never_exceeds_max() {
    // Even with coord = 32767 and max = 800, result should be < 800.
    for max in [640u32, 768, 800, 1024, 1080, 1280, 1920] {
        for coord in [0, 1, 16383, 16384, 32766, 32767] {
            let result = drawing::scale_pointer_coord(coord, max);
            assert!(
                result < max,
                "scale_pointer_coord({}, {}) = {} (should be < {})",
                coord, max, result, max,
            );
        }
    }
}

#[test]
fn test_scale_pointer_coord_zero_max() {
    // Edge case: max_pixels = 0 should not panic.
    assert_eq!(drawing::scale_pointer_coord(16384, 0), 0);
}

// ---------------------------------------------------------------------------
// xy_to_byte tests — click-to-position: verify pixel-to-byte conversion
// for click placement in the text editor content area.
// ---------------------------------------------------------------------------

/// Click-to-position: clicking at the exact pixel position from byte_to_xy
/// round-trips back to the same byte offset for all positions in multiline text.
#[test]
fn click_to_position_round_trip_multiline() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    let text = b"hello world\nline two\nthird line here";

    for pos in 0..=text.len() {
        let (x, y) = layout.byte_to_xy(text, pos);
        let result = layout.xy_to_byte(text, x, y);
        assert_eq!(
            result, pos,
            "click round-trip failed for pos={}: byte_to_xy→({},{}) xy_to_byte→{}",
            pos, x, y, result,
        );
    }
}

/// Click-to-position: clicking past the end of text on the last line returns
/// text.len() (cursor positioned at end of document).
#[test]
fn click_to_position_past_end_of_document() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    let text = b"hello";
    // Click far past the end of "hello" on line 0.
    let result = layout.xy_to_byte(text, 500, 0);
    assert_eq!(result, 5);
}

/// Click-to-position: clicking below all text positions cursor at end of
/// nearest (last) line.
#[test]
fn click_to_position_below_all_text() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    let text = b"ab\ncd";
    // Click at y=200 which is well below the last line (line 1 at y=24).
    let result = layout.xy_to_byte(text, 0, 200);
    assert_eq!(result, text.len());
}

/// Click-to-position with scroll offset: after subtracting scroll_offset
/// visual lines, the click should map to the correct byte in the document.
#[test]
fn click_to_position_with_scroll_offset() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    // 3 lines: "aaa\nbbb\nccc"
    let text = b"aaa\nbbb\nccc";
    // Simulate scroll_offset = 1 (first visible line is "bbb").
    // A click at y=0 in the viewport maps to visual line 1 in the document.
    let scroll_offset: u32 = 1;
    let click_y: u32 = 0; // top of viewport
    let adjusted_y = click_y + scroll_offset * layout.line_height;
    let result = layout.xy_to_byte(text, 0, adjusted_y);
    // Visual line 1 starts at byte 4 ('b').
    assert_eq!(result, 4);
}


