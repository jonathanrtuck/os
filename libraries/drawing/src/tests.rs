//! Tests for the drawing primitives library.

use crate::*;

// ============================================================================
// Helpers
// ============================================================================

/// Create a surface backed by the given buffer with the standard BGRA format.
fn make_surface(buf: &mut [u8], width: u32, height: u32) -> Surface<'_> {
    let stride = width * 4;
    Surface {
        data: buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    }
}

// ============================================================================
// 1. Color / PixelFormat operations
// ============================================================================

#[test]
fn color_rgb_sets_opaque_alpha() {
    let c = Color::rgb(10, 20, 30);
    assert_eq!(c.r, 10);
    assert_eq!(c.g, 20);
    assert_eq!(c.b, 30);
    assert_eq!(c.a, 255);
}

#[test]
fn color_rgba_preserves_all_channels() {
    let c = Color::rgba(1, 2, 3, 128);
    assert_eq!(c.r, 1);
    assert_eq!(c.g, 2);
    assert_eq!(c.b, 3);
    assert_eq!(c.a, 128);
}

#[test]
fn color_constants_correct() {
    assert_eq!(Color::WHITE, Color::rgb(255, 255, 255));
    assert_eq!(Color::BLACK, Color::rgb(0, 0, 0));
    assert_eq!(Color::TRANSPARENT, Color::rgba(0, 0, 0, 0));
}

#[test]
fn color_encode_bgra8888() {
    let c = Color::rgb(0xAA, 0xBB, 0xCC);
    let encoded = c.encode(PixelFormat::Bgra8888);
    // BGRA order: [B, G, R, A]
    assert_eq!(encoded, [0xCC, 0xBB, 0xAA, 0xFF]);
}

#[test]
fn color_decode_bgra8888() {
    // BGRA byte order in the buffer.
    let bytes: [u8; 4] = [0xCC, 0xBB, 0xAA, 0xFF];
    let c = Color::decode(&bytes, PixelFormat::Bgra8888);
    assert_eq!(c.r, 0xAA);
    assert_eq!(c.g, 0xBB);
    assert_eq!(c.b, 0xCC);
    assert_eq!(c.a, 0xFF);
}

#[test]
fn color_decode_from_bgra_matches_decode() {
    let bytes: [u8; 4] = [0x10, 0x20, 0x30, 0x40];
    let via_decode = Color::decode(&bytes, PixelFormat::Bgra8888);
    let via_bgra = Color::decode_from_bgra(&bytes);
    assert_eq!(via_decode, via_bgra);
}

#[test]
fn color_encode_decode_roundtrip() {
    let original = Color::rgba(42, 99, 200, 180);
    let encoded = original.encode(PixelFormat::Bgra8888);
    let decoded = Color::decode(&encoded, PixelFormat::Bgra8888);
    assert_eq!(original, decoded);
}

#[test]
fn pixel_format_bytes_per_pixel() {
    assert_eq!(PixelFormat::Bgra8888.bytes_per_pixel(), 4);
}

// ============================================================================
// 2. Surface creation and access
// ============================================================================

#[test]
fn surface_is_valid_correct_dimensions() {
    let mut buf = [0u8; 40]; // 10 pixels * 4 bpp = 40 bytes for 10x1
    let s = make_surface(&mut buf, 10, 1);
    assert!(s.is_valid());
}

#[test]
fn surface_is_valid_rejects_too_small_buffer() {
    let mut buf = [0u8; 10]; // Too small for 10x1
    let s = Surface {
        data: &mut buf,
        width: 10,
        height: 1,
        stride: 40,
        format: PixelFormat::Bgra8888,
    };
    assert!(!s.is_valid());
}

#[test]
fn surface_is_valid_rejects_narrow_stride() {
    // stride < width * bpp
    let mut buf = [0u8; 100];
    let s = Surface {
        data: &mut buf,
        width: 10,
        height: 1,
        stride: 20, // needs 40
        format: PixelFormat::Bgra8888,
    };
    assert!(!s.is_valid());
}

#[test]
fn surface_get_set_pixel_roundtrip() {
    let mut buf = [0u8; 4 * 4 * 4]; // 4x4 surface
    let mut s = make_surface(&mut buf, 4, 4);

    let color = Color::rgb(100, 150, 200);
    s.set_pixel(2, 3, color);
    assert_eq!(s.get_pixel(2, 3), Some(color));
}

#[test]
fn surface_get_pixel_out_of_bounds_returns_none() {
    let mut buf = [0u8; 4 * 4 * 4];
    let s = make_surface(&mut buf, 4, 4);
    assert_eq!(s.get_pixel(4, 0), None);
    assert_eq!(s.get_pixel(0, 4), None);
    assert_eq!(s.get_pixel(100, 100), None);
}

#[test]
fn surface_set_pixel_out_of_bounds_is_noop() {
    let mut buf = [0u8; 16]; // 1x1 surface (4 bytes used)
    let mut s = make_surface(&mut buf, 1, 1);
    // Should not panic.
    s.set_pixel(5, 5, Color::WHITE);
    // Original pixel should still be black (zeroed).
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn surface_pixel_offset_within_bounds() {
    let mut buf = [0u8; 4 * 4 * 4]; // 4x4
    let s = make_surface(&mut buf, 4, 4);
    // pixel (2, 1): offset = 1 * 16 + 2 * 4 = 24
    assert_eq!(s.pixel_offset(2, 1), Some(24));
}

#[test]
fn surface_pixel_offset_out_of_bounds() {
    let mut buf = [0u8; 4 * 4 * 4];
    let s = make_surface(&mut buf, 4, 4);
    assert_eq!(s.pixel_offset(4, 0), None);
    assert_eq!(s.pixel_offset(0, 4), None);
}

#[test]
fn surface_clear_fills_all_pixels() {
    let mut buf = [0u8; 4 * 3 * 3]; // 3x3
    let mut s = make_surface(&mut buf, 3, 3);
    let color = Color::rgb(0xAA, 0xBB, 0xCC);
    s.clear(color);

    for y in 0..3 {
        for x in 0..3 {
            assert_eq!(s.get_pixel(x, y), Some(color), "mismatch at ({x}, {y})");
        }
    }
}

#[test]
fn surface_with_stride_padding() {
    // 2x2 surface with stride = 16 (4 extra bytes padding per row).
    let mut buf = [0u8; 16 * 2]; // 2 rows, each 16 bytes
    let mut s = Surface {
        data: &mut buf,
        width: 2,
        height: 2,
        stride: 16,
        format: PixelFormat::Bgra8888,
    };
    assert!(s.is_valid());

    let c = Color::rgb(10, 20, 30);
    s.set_pixel(1, 1, c);
    assert_eq!(s.get_pixel(1, 1), Some(c));
    // Padding area should be untouched (zero).
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

// ============================================================================
// 3. Blending operations
// ============================================================================

#[test]
fn blend_over_opaque_source_replaces_dst() {
    let src = Color::rgb(100, 150, 200);
    let dst = Color::rgb(50, 60, 70);
    assert_eq!(src.blend_over(dst), src);
}

#[test]
fn blend_over_transparent_source_preserves_dst() {
    let src = Color::TRANSPARENT;
    let dst = Color::rgb(50, 60, 70);
    assert_eq!(src.blend_over(dst), dst);
}

#[test]
fn blend_over_zero_alpha_dst_yields_src_weighted() {
    let src = Color::rgba(200, 100, 50, 128);
    let dst = Color::TRANSPARENT;
    let result = src.blend_over(dst);
    // With transparent dst, the result should be close to the source color
    // at the source alpha.
    assert_eq!(result.a, 128);
    // The R channel should be close to 200 (exact value depends on gamma).
    assert!(result.r > 150, "expected r > 150, got {}", result.r);
}

#[test]
fn blend_over_both_transparent_yields_transparent() {
    let result = Color::TRANSPARENT.blend_over(Color::TRANSPARENT);
    assert_eq!(result, Color::TRANSPARENT);
}

#[test]
fn blend_over_semi_transparent_produces_intermediate() {
    let src = Color::rgba(255, 0, 0, 128); // semi-transparent red
    let dst = Color::rgb(0, 0, 255); // opaque blue

    let result = src.blend_over(dst);
    // Alpha should be fully opaque (src + dst_eff = 128 + div255(255*127) = 128 + ~127 = ~255).
    assert!(
        result.a >= 254,
        "expected near-opaque alpha, got {}",
        result.a
    );
    // Result should have both red and blue components.
    assert!(result.r > 0, "expected some red, got 0");
    assert!(result.b > 0, "expected some blue, got 0");
}

#[test]
fn blend_pixel_opaque_overwrites() {
    let mut buf = [0u8; 16]; // 1x1 with extra room
    let mut s = make_surface(&mut buf, 1, 1);
    s.set_pixel(0, 0, Color::rgb(10, 20, 30));
    s.blend_pixel(0, 0, Color::rgb(200, 100, 50));
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgb(200, 100, 50)));
}

#[test]
fn blend_pixel_transparent_is_noop() {
    let mut buf = [0u8; 16];
    let mut s = make_surface(&mut buf, 1, 1);
    let original = Color::rgb(10, 20, 30);
    s.set_pixel(0, 0, original);
    s.blend_pixel(0, 0, Color::TRANSPARENT);
    assert_eq!(s.get_pixel(0, 0), Some(original));
}

#[test]
fn blend_pixel_out_of_bounds_is_noop() {
    let mut buf = [0u8; 16];
    let mut s = make_surface(&mut buf, 1, 1);
    // Should not panic.
    s.blend_pixel(10, 10, Color::rgb(255, 0, 0));
}

// ============================================================================
// 4. Fill operations
// ============================================================================

#[test]
fn fill_rect_basic() {
    let mut buf = [0u8; 4 * 8 * 8]; // 8x8
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(0xFF, 0x00, 0xFF);

    s.fill_rect(2, 2, 4, 3, color);

    // Inside the filled region.
    for y in 2..5 {
        for x in 2..6 {
            assert_eq!(
                s.get_pixel(x, y),
                Some(color),
                "should be filled at ({x}, {y})"
            );
        }
    }
    // Outside the filled region (should be zero).
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(7, 7), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_clips_to_bounds() {
    let mut buf = [0u8; 4 * 4 * 4]; // 4x4
    let mut s = make_surface(&mut buf, 4, 4);
    let color = Color::rgb(100, 100, 100);

    // Fill extends beyond the surface.
    s.fill_rect(2, 2, 100, 100, color);

    // Bottom-right corner should be filled.
    assert_eq!(s.get_pixel(3, 3), Some(color));
    // Top-left should be untouched.
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_at_origin_out_of_bounds_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    // x >= width: should be a no-op.
    s.fill_rect(4, 0, 2, 2, Color::WHITE);
    assert_eq!(s.get_pixel(3, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_zero_dimensions_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.fill_rect(0, 0, 0, 4, Color::WHITE);
    s.fill_rect(0, 0, 4, 0, Color::WHITE);
    // Nothing should be modified.
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_blend_opaque_delegates_to_fill_rect() {
    let mut buf1 = [0u8; 4 * 4 * 4];
    let mut buf2 = [0u8; 4 * 4 * 4];

    let mut s1 = make_surface(&mut buf1, 4, 4);
    let mut s2 = make_surface(&mut buf2, 4, 4);

    let color = Color::rgb(200, 100, 50); // fully opaque
    s1.fill_rect(0, 0, 4, 4, color);
    s2.fill_rect_blend(0, 0, 4, 4, color);

    // Results should be identical.
    assert_eq!(s1.data, s2.data);
}

#[test]
fn fill_rect_blend_transparent_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.clear(Color::rgb(100, 100, 100));

    // Snapshot the buffer state before the blend.
    let mut before = [0u8; 4 * 4 * 4];
    before.copy_from_slice(s.data);

    s.fill_rect_blend(0, 0, 4, 4, Color::TRANSPARENT);
    assert_eq!(s.data, &before[..]);
}

#[test]
fn fill_rect_blend_semi_transparent_modifies_surface() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.clear(Color::rgb(0, 0, 255)); // blue background

    let mut before = [0u8; 4 * 4 * 4];
    before.copy_from_slice(s.data);

    s.fill_rect_blend(0, 0, 4, 4, Color::rgba(255, 0, 0, 128)); // semi-red
    assert_ne!(s.data, &before[..], "surface should be modified");

    // Each pixel should have some red and some blue.
    let px = s.get_pixel(0, 0).unwrap();
    assert!(px.r > 0, "expected red component");
    assert!(px.b > 0, "expected blue component");
}

#[test]
fn fill_gradient_v_single_row() {
    let mut buf = [0u8; 4 * 8]; // 8x1
    let mut s = make_surface(&mut buf, 8, 1);
    let top = Color::rgb(255, 0, 0);
    let bot = Color::rgb(0, 0, 255);
    s.fill_gradient_v(0, 0, 8, 1, top, bot);
    // h=1: should be filled with color_top.
    assert_eq!(s.get_pixel(0, 0), Some(top));
    assert_eq!(s.get_pixel(7, 0), Some(top));
}

#[test]
fn fill_gradient_v_two_rows() {
    let mut buf = [0u8; 4 * 4 * 2]; // 4x2
    let mut s = make_surface(&mut buf, 4, 2);
    let top = Color::rgb(0, 0, 0);
    let bot = Color::rgb(200, 200, 200);
    s.fill_gradient_v(0, 0, 4, 2, top, bot);
    // First row = top color, last row = bottom color.
    assert_eq!(s.get_pixel(0, 0), Some(top));
    assert_eq!(s.get_pixel(0, 1), Some(bot));
}

#[test]
fn fill_rounded_rect_zero_radius_same_as_fill_rect() {
    let mut buf1 = [0u8; 4 * 8 * 8];
    let mut buf2 = [0u8; 4 * 8 * 8];

    let mut s1 = make_surface(&mut buf1, 8, 8);
    let mut s2 = make_surface(&mut buf2, 8, 8);

    let color = Color::rgb(128, 64, 32);
    s1.fill_rect(1, 1, 6, 4, color);
    s2.fill_rounded_rect(1, 1, 6, 4, 0, color);

    assert_eq!(s1.data, s2.data);
}

#[test]
fn fill_rounded_rect_blend_zero_alpha_is_noop() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    s.clear(Color::rgb(100, 100, 100));

    let mut before = [0u8; 4 * 8 * 8];
    before.copy_from_slice(s.data);

    s.fill_rounded_rect_blend(1, 1, 6, 4, 2, Color::TRANSPARENT);
    assert_eq!(s.data, &before[..]);
}

// ============================================================================
// 5. Line drawing basic cases
// ============================================================================

#[test]
fn draw_hline_fills_row() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(255, 0, 0);
    s.draw_hline(1, 3, 5, color);
    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 3), Some(color), "pixel at ({x}, 3)");
    }
    // Should not touch adjacent rows.
    assert_eq!(s.get_pixel(1, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(1, 4), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_vline_fills_column() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(0, 255, 0);
    s.draw_vline(2, 1, 4, color);
    for y in 1..5 {
        assert_eq!(s.get_pixel(2, y), Some(color), "pixel at (2, {y})");
    }
    // Adjacent columns untouched.
    assert_eq!(s.get_pixel(1, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(3, 2), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_rect_1x1() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    let color = Color::rgb(255, 255, 0);
    s.draw_rect(1, 1, 1, 1, color);
    assert_eq!(s.get_pixel(1, 1), Some(color));
    // Neighbors untouched.
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_rect_3x3_border() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(255, 0, 255);
    s.draw_rect(2, 2, 3, 3, color);

    // Top edge.
    assert_eq!(s.get_pixel(2, 2), Some(color));
    assert_eq!(s.get_pixel(3, 2), Some(color));
    assert_eq!(s.get_pixel(4, 2), Some(color));
    // Bottom edge.
    assert_eq!(s.get_pixel(2, 4), Some(color));
    assert_eq!(s.get_pixel(3, 4), Some(color));
    assert_eq!(s.get_pixel(4, 4), Some(color));
    // Left/right edges.
    assert_eq!(s.get_pixel(2, 3), Some(color));
    assert_eq!(s.get_pixel(4, 3), Some(color));
    // Interior should be empty.
    assert_eq!(s.get_pixel(3, 3), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_rect_zero_dimensions_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.draw_rect(0, 0, 0, 0, Color::WHITE);
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_line_single_point() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(0, 255, 0);
    s.draw_line(3, 3, 3, 3, color);
    assert_eq!(s.get_pixel(3, 3), Some(color));
}

#[test]
fn draw_line_horizontal() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(255, 255, 255);
    s.draw_line(1, 4, 6, 4, color);
    for x in 1..=6 {
        assert_eq!(s.get_pixel(x as u32, 4), Some(color), "pixel at ({x}, 4)");
    }
}

#[test]
fn draw_line_vertical() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(255, 255, 255);
    s.draw_line(3, 1, 3, 5, color);
    for y in 1..=5 {
        assert_eq!(s.get_pixel(3, y as u32), Some(color), "pixel at (3, {y})");
    }
}

#[test]
fn draw_line_diagonal_45_degrees() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    let color = Color::rgb(200, 200, 200);
    s.draw_line(0, 0, 4, 4, color);
    // Each diagonal pixel should be set.
    for i in 0..=4 {
        assert_eq!(
            s.get_pixel(i as u32, i as u32),
            Some(color),
            "pixel at ({i}, {i})"
        );
    }
}

#[test]
fn draw_line_negative_coords_no_panic() {
    let mut buf = [0u8; 4 * 8 * 8];
    let mut s = make_surface(&mut buf, 8, 8);
    // Lines partially or fully out of bounds should not panic.
    s.draw_line(-5, -5, 10, 10, Color::WHITE);
    s.draw_line(-100, 4, -50, 4, Color::WHITE);
}

// ============================================================================
// 6. Math helpers
// ============================================================================

#[test]
fn div255_exact_for_alpha_range() {
    // div255 must be exact for all values in 0..=65025 (255*255).
    for x in (0..=65025u32).step_by(255) {
        assert_eq!(div255(x), x / 255, "mismatch for x={x}");
    }
    // Spot-check boundaries.
    assert_eq!(div255(0), 0);
    assert_eq!(div255(255), 1);
    assert_eq!(div255(65025), 255);
    assert_eq!(div255(32640), 128); // 128 * 255 = 32640
}

#[test]
fn min_helper() {
    assert_eq!(min(5, 10), 5);
    assert_eq!(min(10, 5), 5);
    assert_eq!(min(7, 7), 7);
    assert_eq!(min(0, u32::MAX), 0);
}

#[test]
fn abs_helper() {
    assert_eq!(abs(0), 0);
    assert_eq!(abs(5), 5);
    assert_eq!(abs(-5), 5);
    assert_eq!(abs(i32::MAX), i32::MAX);
}

#[test]
fn round_f32_positive() {
    assert_eq!(round_f32(0.0), 0);
    assert_eq!(round_f32(0.4), 0);
    assert_eq!(round_f32(0.5), 1);
    assert_eq!(round_f32(0.9), 1);
    assert_eq!(round_f32(1.0), 1);
    assert_eq!(round_f32(2.5), 3);
}

#[test]
fn round_f32_negative() {
    assert_eq!(round_f32(-0.4), 0);
    assert_eq!(round_f32(-0.5), -1);
    assert_eq!(round_f32(-0.9), -1);
    assert_eq!(round_f32(-1.0), -1);
    assert_eq!(round_f32(-2.5), -3);
}

#[test]
fn isqrt_fp_zero() {
    assert_eq!(isqrt_fp(0), 0);
}

#[test]
fn isqrt_fp_perfect_squares() {
    // sqrt(1) = 1
    assert_eq!(isqrt_fp(1), 1);
    // sqrt(4) = 2
    assert_eq!(isqrt_fp(4), 2);
    // sqrt(256 * 256) = 256 (which is 1.0 in 8.8 fixed-point).
    assert_eq!(isqrt_fp(256 * 256), 256);
}

#[test]
fn isqrt_fp_non_perfect() {
    // sqrt(2) ~ 1.414, in 8.8 FP: sqrt(2 * 65536) should be ~362.
    // isqrt_fp returns floor, so it should be 362.
    let val = 2 * 65536u64;
    let result = isqrt_fp(val);
    // 362^2 = 131044, 363^2 = 131769. val = 131072. So floor = 362.
    assert_eq!(result, 362);
}

#[test]
fn linear_to_idx_clamps_at_4095() {
    assert_eq!(linear_to_idx(0), 0);
    assert_eq!(linear_to_idx(16), 1); // 16 >> 4 = 1
    assert_eq!(linear_to_idx(65535), 4095);
    assert_eq!(linear_to_idx(100000), 4095); // clamped
}

// ============================================================================
// 7. Blit operations
// ============================================================================

#[test]
fn blit_copies_pixels() {
    let mut dst_buf = [0u8; 4 * 4 * 4]; // 4x4 destination
    let mut dst = make_surface(&mut dst_buf, 4, 4);

    // 2x2 red source.
    let src_color = Color::rgb(255, 0, 0);
    let encoded = src_color.encode(PixelFormat::Bgra8888);
    let mut src_buf = [0u8; 4 * 2 * 2];
    for px in 0..4 {
        let off = px * 4;
        src_buf[off..off + 4].copy_from_slice(&encoded);
    }

    dst.blit(&src_buf, 2, 2, 8, 1, 1);

    assert_eq!(dst.get_pixel(1, 1), Some(src_color));
    assert_eq!(dst.get_pixel(2, 2), Some(src_color));
    // Outside blit region should be untouched.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn blit_clips_to_dst_bounds() {
    let mut dst_buf = [0u8; 4 * 2 * 2]; // 2x2 destination
    let mut dst = make_surface(&mut dst_buf, 2, 2);

    // 4x4 source.
    let src_buf = [0xFFu8; 4 * 4 * 4];
    dst.blit(&src_buf, 4, 4, 16, 0, 0);
    // Should not panic, and only the 2x2 region should be written.
}

#[test]
fn blit_out_of_bounds_dst_is_noop() {
    let mut dst_buf = [0u8; 4 * 2 * 2];
    let mut dst = make_surface(&mut dst_buf, 2, 2);
    let src_buf = [0xFFu8; 16];
    // dst_x >= width.
    dst.blit(&src_buf, 2, 2, 8, 5, 0);
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

// ============================================================================
// 8. Gradient PRNG
// ============================================================================

#[test]
fn xorshift32_deterministic() {
    let mut a = Xorshift32::new(42);
    let mut b = Xorshift32::new(42);
    for _ in 0..100 {
        assert_eq!(a.next(), b.next());
    }
}

#[test]
fn xorshift32_never_zero() {
    // Zero seed should be corrected.
    let mut rng = Xorshift32::new(0);
    // First output should not be zero.
    assert_ne!(rng.next(), 0);
}

#[test]
fn xorshift32_noise_in_range() {
    let mut rng = Xorshift32::new(1234);
    for _ in 0..1000 {
        let val = rng.noise(3);
        assert!(
            (-3..=3).contains(&val),
            "noise(3) produced {val}, expected [-3, 3]"
        );
    }
}

// ============================================================================
// 9. Box blur helpers
// ============================================================================

#[test]
fn box_blur_widths_small_sigma() {
    // sigma < 0.5 should return [1, 1, 1].
    let h = box_blur_widths(0.1);
    assert_eq!(h, [1, 1, 1]);
}

#[test]
fn box_blur_widths_reasonable_sigma() {
    // For sigma=4, widths should be non-trivial and small.
    let h = box_blur_widths(4.0);
    for &half in &h {
        assert!(half > 0, "half-width should be > 0 for sigma=4");
        assert!(half < 20, "half-width should be reasonable");
    }
}

#[test]
fn box_blur_pad_grows_with_sigma() {
    let pad_small = box_blur_pad(2.0);
    let pad_large = box_blur_pad(8.0);
    assert!(
        pad_large > pad_small,
        "larger sigma should produce larger padding"
    );
}

// ============================================================================
// 10. Blur kernel computation
// ============================================================================

#[test]
fn compute_kernel_radius_zero() {
    let mut kernel = [0u32; MAX_KERNEL_DIAMETER];
    let diameter = compute_kernel(&mut kernel, 0, 256);
    assert_eq!(diameter, 1);
    assert_eq!(kernel[0], 65536); // all weight on center
}

#[test]
fn compute_kernel_sums_to_65536() {
    let mut kernel = [0u32; MAX_KERNEL_DIAMETER];
    for radius in 1..=MAX_CPU_BLUR_RADIUS {
        let sigma_fp = radius * 64; // reasonable sigma
        let diameter = compute_kernel(&mut kernel, radius, sigma_fp);
        let sum: u64 = kernel[..diameter].iter().map(|&w| w as u64).sum();
        assert_eq!(
            sum, 65536,
            "kernel sum should be 65536 for radius={radius}, got {sum}"
        );
    }
}

#[test]
fn compute_kernel_is_symmetric() {
    let mut kernel = [0u32; MAX_KERNEL_DIAMETER];
    let diameter = compute_kernel(&mut kernel, 4, 512);
    for i in 0..diameter / 2 {
        assert_eq!(
            kernel[i],
            kernel[diameter - 1 - i],
            "kernel should be symmetric: index {i} vs {}",
            diameter - 1 - i
        );
    }
}

#[test]
fn compute_kernel_center_is_largest() {
    let mut kernel = [0u32; MAX_KERNEL_DIAMETER];
    let diameter = compute_kernel(&mut kernel, 4, 512);
    let center = diameter / 2;
    for i in 0..diameter {
        assert!(
            kernel[center] >= kernel[i],
            "center weight should be >= all others"
        );
    }
}

// ============================================================================
// 11. Palette constants accessible
// ============================================================================

#[test]
fn palette_constants_exist() {
    // Verify a few palette constants are reachable and have expected properties.
    assert_eq!(BG_BASE, Color::rgb(0x20, 0x20, 0x20));
    assert_eq!(PAGE_BG, Color::rgb(255, 255, 255));
    assert_eq!(CHROME_BG.a, 0); // transparent
    assert_eq!(CURSOR_FILL, Color::WHITE);
    assert_eq!(CURSOR_OUTLINE, Color::BLACK);
}

// ============================================================================
// 12. Gamma table sanity
// ============================================================================

#[test]
fn srgb_to_linear_boundary_values() {
    assert_eq!(SRGB_TO_LINEAR[0], 0);
    assert_eq!(SRGB_TO_LINEAR[255], 65535);
    // Monotonically increasing.
    for i in 1..256 {
        assert!(
            SRGB_TO_LINEAR[i] >= SRGB_TO_LINEAR[i - 1],
            "SRGB_TO_LINEAR should be monotonically increasing at index {i}"
        );
    }
}

#[test]
fn linear_to_srgb_boundary_values() {
    assert_eq!(LINEAR_TO_SRGB[0], 0);
    assert_eq!(LINEAR_TO_SRGB[4095], 255);
    // Monotonically non-decreasing.
    for i in 1..4096 {
        assert!(
            LINEAR_TO_SRGB[i] >= LINEAR_TO_SRGB[i - 1],
            "LINEAR_TO_SRGB should be monotonically non-decreasing at index {i}"
        );
    }
}

#[test]
fn srgb_roundtrip_approximate() {
    // Converting sRGB -> linear -> sRGB should be close to identity.
    for srgb in 0..=255u8 {
        let linear = SRGB_TO_LINEAR[srgb as usize];
        let idx = linear_to_idx(linear as u32);
        let back = LINEAR_TO_SRGB[idx];
        // Allow +/- 1 for quantization.
        let diff = if back > srgb {
            back - srgb
        } else {
            srgb - back
        };
        assert!(
            diff <= 1,
            "sRGB roundtrip: {srgb} -> linear {linear} -> idx {idx} -> {back} (diff {diff})"
        );
    }
}

// ============================================================================
// 13. Coverage map drawing
// ============================================================================

#[test]
fn draw_coverage_full_coverage_opaque() {
    let mut buf = [0u8; 4 * 4 * 4]; // 4x4
    let mut s = make_surface(&mut buf, 4, 4);
    let color = Color::rgb(255, 0, 0);

    // 2x2 coverage map, all 255 (full coverage).
    let cov = [255u8; 4];
    s.draw_coverage(1, 1, &cov, 2, 2, color);

    // Covered pixels should have the color.
    assert_eq!(s.get_pixel(1, 1), Some(color));
    assert_eq!(s.get_pixel(2, 2), Some(color));
}

#[test]
fn draw_coverage_zero_coverage_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.clear(Color::rgb(100, 100, 100));

    let mut before = [0u8; 4 * 4 * 4];
    before.copy_from_slice(s.data);

    let cov = [0u8; 4];
    s.draw_coverage(1, 1, &cov, 2, 2, Color::rgb(255, 0, 0));
    assert_eq!(s.data, &before[..]);
}

#[test]
fn draw_coverage_transparent_color_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.clear(Color::rgb(100, 100, 100));

    let mut before = [0u8; 4 * 4 * 4];
    before.copy_from_slice(s.data);

    let cov = [255u8; 4];
    s.draw_coverage(0, 0, &cov, 2, 2, Color::TRANSPARENT);
    assert_eq!(s.data, &before[..]);
}

#[test]
fn draw_coverage_negative_offset_clips() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    let color = Color::rgb(0, 255, 0);

    // 4x4 coverage, placed at (-2, -2): only bottom-right 2x2 should be visible.
    let cov = [255u8; 16];
    s.draw_coverage(-2, -2, &cov, 4, 4, color);

    assert_eq!(s.get_pixel(0, 0), Some(color));
    assert_eq!(s.get_pixel(1, 1), Some(color));
    // (2, 0) should not be covered (coverage map doesn't extend that far).
    assert_eq!(s.get_pixel(2, 0), Some(Color::rgba(0, 0, 0, 0)));
}
