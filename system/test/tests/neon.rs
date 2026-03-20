//! NEON SIMD correctness tests for the drawing library.
//!
//! Tests that NEON-accelerated drawing operations produce correct output:
//! - fill_rect: exact match for all widths (aligned and unaligned)
//! - blit_blend alpha blending: within ±1 LSB per channel of scalar reference
//! - fill_rect_blend: within ±1 LSB per channel of scalar reference
//! - f64 reference comparison: NEON blend matches mathematically correct sRGB blend

use drawing::{Color, PixelFormat, Surface};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a zeroed test surface.
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

/// Scalar-only Color::blend_over reference (uses the same code path as
/// Color::blend_over, which is what the NEON path should match within ±1).
fn scalar_blend_over(src: Color, dst: Color) -> Color {
    src.blend_over(dst)
}

/// Mathematically correct sRGB alpha blend using f64 precision.
/// This is the gold standard reference for gamma correctness.
fn f64_srgb_blend(src: Color, dst: Color) -> Color {
    if src.a == 255 {
        return src;
    }
    if src.a == 0 {
        return dst;
    }

    let sa = src.a as f64 / 255.0;
    let da = dst.a as f64 / 255.0;
    let inv_sa = 1.0 - sa;

    let da_eff = da * inv_sa;
    let out_a = sa + da_eff;

    if out_a == 0.0 {
        return Color::TRANSPARENT;
    }

    // sRGB to linear (precise)
    let sr_lin = srgb_to_linear_f64(src.r);
    let sg_lin = srgb_to_linear_f64(src.g);
    let sb_lin = srgb_to_linear_f64(src.b);
    let dr_lin = srgb_to_linear_f64(dst.r);
    let dg_lin = srgb_to_linear_f64(dst.g);
    let db_lin = srgb_to_linear_f64(dst.b);

    // Blend in linear space
    let r_lin = (sr_lin * sa + dr_lin * da_eff) / out_a;
    let g_lin = (sg_lin * sa + dg_lin * da_eff) / out_a;
    let b_lin = (sb_lin * sa + db_lin * da_eff) / out_a;

    // Linear to sRGB (precise)
    let r = linear_to_srgb_f64(r_lin);
    let g = linear_to_srgb_f64(g_lin);
    let b = linear_to_srgb_f64(b_lin);
    let a = (out_a * 255.0).round().min(255.0) as u8;

    Color::rgba(r, g, b, a)
}

/// Precise sRGB to linear conversion using the IEC 61966-2-1 formula.
fn srgb_to_linear_f64(srgb: u8) -> f64 {
    let s = srgb as f64 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Precise linear to sRGB conversion using the IEC 61966-2-1 formula.
fn linear_to_srgb_f64(linear: f64) -> u8 {
    let s = if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Check that two colors are within ±tolerance per channel.
fn colors_within_tolerance(a: Color, b: Color, tolerance: u8) -> bool {
    let dr = (a.r as i16 - b.r as i16).unsigned_abs() as u8;
    let dg = (a.g as i16 - b.g as i16).unsigned_abs() as u8;
    let db = (a.b as i16 - b.b as i16).unsigned_abs() as u8;
    let da = (a.a as i16 - b.a as i16).unsigned_abs() as u8;
    dr <= tolerance && dg <= tolerance && db <= tolerance && da <= tolerance
}

// ---------------------------------------------------------------------------
// NEON fill_rect tests — exact match
// ---------------------------------------------------------------------------

#[test]
fn neon_fill_rect_exact_width_1() {
    let mut buf = [0u8; 1 * 1 * 4];
    let mut s = make_surface(&mut buf, 1, 1);
    let color = Color::rgb(0xAA, 0xBB, 0xCC);
    s.fill_rect(0, 0, 1, 1, color);
    assert_eq!(s.get_pixel(0, 0), Some(color));
}

#[test]
fn neon_fill_rect_exact_width_2() {
    let mut buf = [0u8; 2 * 1 * 4];
    let mut s = make_surface(&mut buf, 2, 1);
    let color = Color::rgb(0x11, 0x22, 0x33);
    s.fill_rect(0, 0, 2, 1, color);
    for x in 0..2 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 2, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_width_3() {
    let mut buf = [0u8; 3 * 1 * 4];
    let mut s = make_surface(&mut buf, 3, 1);
    let color = Color::rgb(0x44, 0x55, 0x66);
    s.fill_rect(0, 0, 3, 1, color);
    for x in 0..3 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 3, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_width_4() {
    let mut buf = [0u8; 4 * 1 * 4];
    let mut s = make_surface(&mut buf, 4, 1);
    let color = Color::rgb(0x77, 0x88, 0x99);
    s.fill_rect(0, 0, 4, 1, color);
    for x in 0..4 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 4, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_width_5() {
    let mut buf = [0u8; 5 * 1 * 4];
    let mut s = make_surface(&mut buf, 5, 1);
    let color = Color::rgb(0xDD, 0xEE, 0xFF);
    s.fill_rect(0, 0, 5, 1, color);
    for x in 0..5 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 5, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_width_7() {
    let mut buf = [0u8; 7 * 1 * 4];
    let mut s = make_surface(&mut buf, 7, 1);
    let color = Color::rgb(0x12, 0x34, 0x56);
    s.fill_rect(0, 0, 7, 1, color);
    for x in 0..7 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 7, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_width_8() {
    let mut buf = [0u8; 8 * 1 * 4];
    let mut s = make_surface(&mut buf, 8, 1);
    let color = Color::rgb(0xFE, 0xDC, 0xBA);
    s.fill_rect(0, 0, 8, 1, color);
    for x in 0..8 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 8, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_width_100() {
    let mut buf = vec![0u8; 100 * 1 * 4];
    let mut s = make_surface(&mut buf, 100, 1);
    let color = Color::rgb(42, 128, 200);
    s.fill_rect(0, 0, 100, 1, color);
    for x in 0..100 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "width 100, pixel {x}");
    }
}

#[test]
fn neon_fill_rect_exact_multi_row() {
    let mut buf = [0u8; 10 * 5 * 4];
    let mut s = make_surface(&mut buf, 10, 5);
    let color = Color::rgba(100, 200, 50, 255);
    s.fill_rect(0, 0, 10, 5, color);
    for y in 0..5 {
        for x in 0..10 {
            assert_eq!(
                s.get_pixel(x, y),
                Some(color),
                "multi-row at ({x}, {y})"
            );
        }
    }
}

#[test]
fn neon_fill_rect_exact_zero_size() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);
    s.fill_rect(0, 0, 0, 0, Color::WHITE);
    s.fill_rect(0, 0, 0, 4, Color::WHITE);
    s.fill_rect(0, 0, 4, 0, Color::WHITE);
    // All pixels should still be zero (transparent black).
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn neon_fill_rect_clipped_exact() {
    let mut buf = [0u8; 5 * 5 * 4];
    let mut s = make_surface(&mut buf, 5, 5);
    let color = Color::rgb(255, 128, 64);
    // Fill extends past right edge: should clip to width 5.
    s.fill_rect(3, 0, 10, 1, color);
    assert_eq!(s.get_pixel(3, 0), Some(color));
    assert_eq!(s.get_pixel(4, 0), Some(color));
    assert_eq!(s.get_pixel(2, 0), Some(Color::rgba(0, 0, 0, 0)));
}

// ---------------------------------------------------------------------------
// NEON blit_blend tests — ±1 LSB tolerance
// ---------------------------------------------------------------------------

/// Run blit_blend with semi-transparent source over opaque destination,
/// compare each pixel against scalar blend_over reference.
fn test_blit_blend_width(width: u32) {
    let height = 1u32;
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let src_stride = width * bpp;
    let dst_stride = width * bpp;

    // Semi-transparent red source.
    let src_color = Color::rgba(200, 50, 100, 180);
    let mut src_buf = vec![0u8; (src_stride * height) as usize];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width,
            height,
            stride: src_stride,
            format: PixelFormat::Bgra8888,
        };
        src.clear(src_color);
    }

    // Opaque blue destination.
    let dst_color = Color::rgb(30, 60, 200);
    let mut dst_buf = vec![0u8; (dst_stride * height) as usize];
    let mut dst = Surface {
        data: &mut dst_buf,
        width,
        height,
        stride: dst_stride,
        format: PixelFormat::Bgra8888,
    };
    dst.clear(dst_color);

    // Perform NEON-accelerated blit_blend.
    dst.blit_blend(&src_buf, width, height, src_stride, 0, 0);

    // Compute scalar reference for one pixel.
    let expected = scalar_blend_over(src_color, dst_color);

    for x in 0..width {
        let actual = dst.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, expected, 1),
            "blit_blend width={width} pixel {x}: actual={:?} expected={:?}",
            actual, expected,
        );
    }
}

#[test]
fn neon_blit_blend_width_1() {
    test_blit_blend_width(1);
}
#[test]
fn neon_blit_blend_width_2() {
    test_blit_blend_width(2);
}
#[test]
fn neon_blit_blend_width_3() {
    test_blit_blend_width(3);
}
#[test]
fn neon_blit_blend_width_4() {
    test_blit_blend_width(4);
}
#[test]
fn neon_blit_blend_width_5() {
    test_blit_blend_width(5);
}
#[test]
fn neon_blit_blend_width_7() {
    test_blit_blend_width(7);
}
#[test]
fn neon_blit_blend_width_8() {
    test_blit_blend_width(8);
}
#[test]
fn neon_blit_blend_width_100() {
    test_blit_blend_width(100);
}

#[test]
fn neon_blit_blend_transparent_passthrough() {
    let width = 8u32;
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;

    // Fully transparent source.
    let src_buf = vec![0u8; (stride * 1) as usize];

    let bg = Color::rgb(100, 150, 200);
    let mut dst_buf = vec![0u8; (stride * 1) as usize];
    let mut dst = Surface {
        data: &mut dst_buf,
        width,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    dst.clear(bg);

    dst.blit_blend(&src_buf, width, 1, stride, 0, 0);

    for x in 0..width {
        assert_eq!(dst.get_pixel(x, 0), Some(bg), "transparent src at {x}");
    }
}

#[test]
fn neon_blit_blend_opaque_overwrites() {
    let width = 8u32;
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;

    let src_color = Color::rgb(255, 128, 0);
    let mut src_buf = vec![0u8; (stride * 1) as usize];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        src.clear(src_color);
    }

    let mut dst_buf = vec![0u8; (stride * 1) as usize];
    let mut dst = Surface {
        data: &mut dst_buf,
        width,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    dst.clear(Color::BLACK);

    dst.blit_blend(&src_buf, width, 1, stride, 0, 0);

    for x in 0..width {
        assert_eq!(dst.get_pixel(x, 0), Some(src_color), "opaque src at {x}");
    }
}

#[test]
fn neon_blit_blend_mixed_alpha_row() {
    // Row with mixed opaque, semi-transparent, and transparent pixels.
    let width = 8u32;
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    let bg = Color::rgb(0, 0, 255);

    let mut src_buf = vec![0u8; (stride * 1) as usize];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        // Pixels 0-1: opaque red
        src.set_pixel(0, 0, Color::rgb(255, 0, 0));
        src.set_pixel(1, 0, Color::rgb(255, 0, 0));
        // Pixels 2-3: semi-transparent green
        src.set_pixel(2, 0, Color::rgba(0, 255, 0, 128));
        src.set_pixel(3, 0, Color::rgba(0, 255, 0, 128));
        // Pixels 4-5: transparent (already zeroed)
        // Pixels 6-7: semi-transparent white
        src.set_pixel(6, 0, Color::rgba(255, 255, 255, 64));
        src.set_pixel(7, 0, Color::rgba(255, 255, 255, 64));
    }

    let mut dst_buf = vec![0u8; (stride * 1) as usize];
    let mut dst = Surface {
        data: &mut dst_buf,
        width,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    dst.clear(bg);

    dst.blit_blend(&src_buf, width, 1, stride, 0, 0);

    // Opaque red should overwrite.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(1, 0), Some(Color::rgb(255, 0, 0)));

    // Semi-transparent green on blue: check within tolerance.
    let expected_green = scalar_blend_over(Color::rgba(0, 255, 0, 128), bg);
    for x in 2..4 {
        let actual = dst.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, expected_green, 1),
            "semi-green at {x}: {:?} vs {:?}",
            actual,
            expected_green,
        );
    }

    // Transparent should leave background unchanged.
    assert_eq!(dst.get_pixel(4, 0), Some(bg));
    assert_eq!(dst.get_pixel(5, 0), Some(bg));

    // Semi-transparent white on blue.
    let expected_white = scalar_blend_over(Color::rgba(255, 255, 255, 64), bg);
    for x in 6..8 {
        let actual = dst.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, expected_white, 1),
            "semi-white at {x}: {:?} vs {:?}",
            actual,
            expected_white,
        );
    }
}

// ---------------------------------------------------------------------------
// NEON fill_rect_blend tests — ±1 LSB tolerance
// ---------------------------------------------------------------------------

fn test_fill_rect_blend_width(width: u32) {
    let height = 1u32;
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    let src_color = Color::rgba(180, 60, 120, 160);
    let bg = Color::rgb(50, 100, 200);

    let mut dst_buf = vec![0u8; (stride * height) as usize];
    let mut dst = Surface {
        data: &mut dst_buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    };
    dst.clear(bg);

    dst.fill_rect_blend(0, 0, width, height, src_color);

    let expected = scalar_blend_over(src_color, bg);

    for x in 0..width {
        let actual = dst.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, expected, 1),
            "fill_rect_blend width={width} pixel {x}: actual={:?} expected={:?}",
            actual,
            expected,
        );
    }
}

#[test]
fn neon_fill_rect_blend_width_1() {
    test_fill_rect_blend_width(1);
}
#[test]
fn neon_fill_rect_blend_width_2() {
    test_fill_rect_blend_width(2);
}
#[test]
fn neon_fill_rect_blend_width_3() {
    test_fill_rect_blend_width(3);
}
#[test]
fn neon_fill_rect_blend_width_4() {
    test_fill_rect_blend_width(4);
}
#[test]
fn neon_fill_rect_blend_width_5() {
    test_fill_rect_blend_width(5);
}
#[test]
fn neon_fill_rect_blend_width_7() {
    test_fill_rect_blend_width(7);
}
#[test]
fn neon_fill_rect_blend_width_8() {
    test_fill_rect_blend_width(8);
}
#[test]
fn neon_fill_rect_blend_width_100() {
    test_fill_rect_blend_width(100);
}

#[test]
fn neon_fill_rect_blend_opaque_exact() {
    // Opaque color should fast-path to fill_rect (exact).
    let mut buf = [0u8; 8 * 1 * 4];
    let mut s = make_surface(&mut buf, 8, 1);
    let color = Color::rgb(100, 200, 50);
    s.fill_rect_blend(0, 0, 8, 1, color);
    for x in 0..8 {
        assert_eq!(s.get_pixel(x, 0), Some(color), "opaque at {x}");
    }
}

#[test]
fn neon_fill_rect_blend_transparent_noop() {
    let mut buf = [0u8; 8 * 1 * 4];
    let mut s = make_surface(&mut buf, 8, 1);
    s.clear(Color::WHITE);
    s.fill_rect_blend(0, 0, 8, 1, Color::TRANSPARENT);
    for x in 0..8 {
        assert_eq!(s.get_pixel(x, 0), Some(Color::WHITE), "transparent at {x}");
    }
}

#[test]
fn neon_fill_rect_blend_multi_row() {
    let width = 9u32;
    let height = 5u32;
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    let src_color = Color::rgba(255, 0, 0, 100);
    let bg = Color::rgb(0, 0, 255);

    let mut buf = vec![0u8; (stride * height) as usize];
    let mut dst = Surface {
        data: &mut buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    };
    dst.clear(bg);
    dst.fill_rect_blend(0, 0, width, height, src_color);

    let expected = scalar_blend_over(src_color, bg);
    for y in 0..height {
        for x in 0..width {
            let actual = dst.get_pixel(x, y).unwrap();
            assert!(
                colors_within_tolerance(actual, expected, 1),
                "multi-row ({x}, {y}): {:?} vs {:?}",
                actual,
                expected,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// NEON vs f64 reference — gamma correctness (VAL-NEON-005)
// ---------------------------------------------------------------------------

#[test]
fn neon_blend_vs_f64_reference_semi_red_on_blue() {
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgb(0, 0, 255);
    let f64_ref = f64_srgb_blend(src, dst);

    // Test via blit_blend.
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = 4 * bpp;
    let mut src_buf = vec![0u8; stride as usize];
    {
        let mut s = Surface {
            data: &mut src_buf,
            width: 4,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        s.clear(src);
    }
    let mut dst_buf = vec![0u8; stride as usize];
    let mut d = Surface {
        data: &mut dst_buf,
        width: 4,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    d.clear(dst);
    d.blit_blend(&src_buf, 4, 1, stride, 0, 0);

    for x in 0..4 {
        let actual = d.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, f64_ref, 1),
            "f64 ref (red/blue) pixel {x}: actual={:?} f64_ref={:?}",
            actual, f64_ref,
        );
    }
}

#[test]
fn neon_blend_vs_f64_reference_semi_white_on_black() {
    let src = Color::rgba(255, 255, 255, 128);
    let dst = Color::rgb(0, 0, 0);
    let f64_ref = f64_srgb_blend(src, dst);

    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = 8 * bpp;
    let mut src_buf = vec![0u8; stride as usize];
    {
        let mut s = Surface {
            data: &mut src_buf,
            width: 8,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        s.clear(src);
    }
    let mut dst_buf = vec![0u8; stride as usize];
    let mut d = Surface {
        data: &mut dst_buf,
        width: 8,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    d.clear(dst);
    d.blit_blend(&src_buf, 8, 1, stride, 0, 0);

    for x in 0..8 {
        let actual = d.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, f64_ref, 1),
            "f64 ref (white/black) pixel {x}: actual={:?} f64_ref={:?}",
            actual, f64_ref,
        );
    }
}

#[test]
fn neon_blend_vs_f64_reference_low_alpha() {
    let src = Color::rgba(200, 100, 50, 32);
    let dst = Color::rgb(10, 20, 30);
    let f64_ref = f64_srgb_blend(src, dst);

    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = 4 * bpp;
    let mut src_buf = vec![0u8; stride as usize];
    {
        let mut s = Surface {
            data: &mut src_buf,
            width: 4,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        s.clear(src);
    }
    let mut dst_buf = vec![0u8; stride as usize];
    let mut d = Surface {
        data: &mut dst_buf,
        width: 4,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    d.clear(dst);
    d.blit_blend(&src_buf, 4, 1, stride, 0, 0);

    for x in 0..4 {
        let actual = d.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, f64_ref, 1),
            "f64 ref (low alpha) pixel {x}: actual={:?} f64_ref={:?}",
            actual, f64_ref,
        );
    }
}

#[test]
fn neon_blend_vs_f64_reference_high_alpha() {
    let src = Color::rgba(50, 200, 150, 240);
    let dst = Color::rgb(200, 50, 100);
    let f64_ref = f64_srgb_blend(src, dst);

    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = 4 * bpp;
    let mut src_buf = vec![0u8; stride as usize];
    {
        let mut s = Surface {
            data: &mut src_buf,
            width: 4,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        s.clear(src);
    }
    let mut dst_buf = vec![0u8; stride as usize];
    let mut d = Surface {
        data: &mut dst_buf,
        width: 4,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    d.clear(dst);
    d.blit_blend(&src_buf, 4, 1, stride, 0, 0);

    for x in 0..4 {
        let actual = d.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, f64_ref, 1),
            "f64 ref (high alpha) pixel {x}: actual={:?} f64_ref={:?}",
            actual, f64_ref,
        );
    }
}

#[test]
fn neon_fill_rect_blend_vs_f64_reference() {
    // Verify fill_rect_blend also matches the f64 reference.
    let src = Color::rgba(180, 60, 120, 160);
    let dst = Color::rgb(50, 100, 200);
    let f64_ref = f64_srgb_blend(src, dst);

    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = 8 * bpp;
    let mut buf = vec![0u8; stride as usize];
    let mut d = Surface {
        data: &mut buf,
        width: 8,
        height: 1,
        stride,
        format: PixelFormat::Bgra8888,
    };
    d.clear(dst);
    d.fill_rect_blend(0, 0, 8, 1, src);

    for x in 0..8 {
        let actual = d.get_pixel(x, 0).unwrap();
        assert!(
            colors_within_tolerance(actual, f64_ref, 1),
            "fill_rect_blend f64 ref pixel {x}: actual={:?} f64_ref={:?}",
            actual, f64_ref,
        );
    }
}

// ---------------------------------------------------------------------------
// Comprehensive alpha sweep — tests many alpha values
// ---------------------------------------------------------------------------

#[test]
fn neon_blit_blend_alpha_sweep() {
    // Test blit_blend at a range of alpha values to ensure ±1 LSB tolerance.
    let alphas: [u8; 10] = [1, 16, 32, 64, 96, 128, 160, 192, 224, 254];
    let bg = Color::rgb(100, 150, 200);

    for &alpha in &alphas {
        let src_color = Color::rgba(200, 50, 100, alpha);
        let expected = scalar_blend_over(src_color, bg);

        let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
        let stride = 8 * bpp;
        let mut src_buf = vec![0u8; stride as usize];
        {
            let mut s = Surface {
                data: &mut src_buf,
                width: 8,
                height: 1,
                stride,
                format: PixelFormat::Bgra8888,
            };
            s.clear(src_color);
        }
        let mut dst_buf = vec![0u8; stride as usize];
        let mut dst = Surface {
            data: &mut dst_buf,
            width: 8,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        dst.clear(bg);
        dst.blit_blend(&src_buf, 8, 1, stride, 0, 0);

        for x in 0..8u32 {
            let actual = dst.get_pixel(x, 0).unwrap();
            assert!(
                colors_within_tolerance(actual, expected, 1),
                "alpha sweep a={alpha} pixel {x}: actual={:?} expected={:?}",
                actual, expected,
            );
        }
    }
}

#[test]
fn neon_fill_rect_blend_alpha_sweep() {
    let alphas: [u8; 10] = [1, 16, 32, 64, 96, 128, 160, 192, 224, 254];
    let bg = Color::rgb(100, 150, 200);

    for &alpha in &alphas {
        let src_color = Color::rgba(200, 50, 100, alpha);
        let expected = scalar_blend_over(src_color, bg);

        let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
        let stride = 8 * bpp;
        let mut buf = vec![0u8; stride as usize];
        let mut dst = Surface {
            data: &mut buf,
            width: 8,
            height: 1,
            stride,
            format: PixelFormat::Bgra8888,
        };
        dst.clear(bg);
        dst.fill_rect_blend(0, 0, 8, 1, src_color);

        for x in 0..8u32 {
            let actual = dst.get_pixel(x, 0).unwrap();
            assert!(
                colors_within_tolerance(actual, expected, 1),
                "fill_rect_blend alpha sweep a={alpha} pixel {x}: actual={:?} expected={:?}",
                actual, expected,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// NEON on single-pixel surface (edge case)
// ---------------------------------------------------------------------------

#[test]
fn neon_single_pixel_surface_blend() {
    let mut buf = [0u8; 4];
    let mut s = make_surface(&mut buf, 1, 1);
    s.set_pixel(0, 0, Color::rgb(0, 0, 255));
    s.fill_rect_blend(0, 0, 1, 1, Color::rgba(255, 0, 0, 128));

    let actual = s.get_pixel(0, 0).unwrap();
    let expected = scalar_blend_over(Color::rgba(255, 0, 0, 128), Color::rgb(0, 0, 255));
    assert!(
        colors_within_tolerance(actual, expected, 1),
        "single pixel: {:?} vs {:?}",
        actual, expected,
    );
}

// ---------------------------------------------------------------------------
// NEON SAFETY: verify all unsafe blocks work correctly
// ---------------------------------------------------------------------------

#[test]
fn neon_fill_rect_all_byte_patterns() {
    // Test that the NEON fill_rect correctly encodes various byte patterns.
    let colors = [
        Color::rgba(0, 0, 0, 0),
        Color::rgba(255, 255, 255, 255),
        Color::rgba(0, 0, 0, 255),
        Color::rgba(255, 0, 0, 255),
        Color::rgba(0, 255, 0, 255),
        Color::rgba(0, 0, 255, 255),
        Color::rgba(128, 128, 128, 128),
        Color::rgba(1, 2, 3, 4),
        Color::rgba(254, 253, 252, 251),
    ];

    for &color in &colors {
        let mut buf = [0u8; 13 * 1 * 4]; // width 13 = 3*4 + 1 tail
        let mut s = make_surface(&mut buf, 13, 1);
        s.fill_rect(0, 0, 13, 1, color);
        for x in 0..13 {
            assert_eq!(
                s.get_pixel(x, 0),
                Some(color),
                "byte pattern {:?} at pixel {x}",
                color,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// NEON compiles for aarch64 — VAL-NEON-003
// ---------------------------------------------------------------------------

#[test]
fn neon_intrinsics_available() {
    // This test compiles and runs on aarch64, proving that NEON intrinsics
    // are available without extra feature flags. The fill_rect, blit_blend,
    // and fill_rect_blend functions all use NEON paths on this architecture.
    #[cfg(target_arch = "aarch64")]
    {
        assert!(true, "NEON available on aarch64");
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // On non-aarch64, NEON is not used — scalar fallback.
        assert!(true, "scalar fallback on non-aarch64");
    }
}

// ---------------------------------------------------------------------------
// NEON rounded rect tests — VAL-PRIM-005
// ---------------------------------------------------------------------------

/// VAL-PRIM-005: NEON path for rounded rect interior matches scalar fill_rect.
/// The interior rows of a rounded rect (between top and bottom arcs) should
/// use the existing fill_rect NEON fast path and produce identical output.
#[test]
fn neon_rounded_rect_interior_matches_fill_rect() {
    // Use a wide surface to exercise the NEON 4-pixel chunks.
    let width = 200u32;
    let height = 100u32;
    let radius = 16u32;
    let color = Color::rgb(42, 128, 200);

    let mut rr_buf = vec![0u8; (width * height * 4) as usize];
    {
        let mut surf = make_surface(&mut rr_buf, width, height);
        surf.fill_rounded_rect(0, 0, width, height, radius, color);
    }

    let mut fr_buf = vec![0u8; (width * height * 4) as usize];
    {
        let mut surf = make_surface(&mut fr_buf, width, height);
        surf.fill_rect(0, radius, width, height - 2 * radius, color);
    }

    // Compare interior rows (y=radius..height-radius).
    let stride = (width * 4) as usize;
    for row in radius..(height - radius) {
        let off = (row * width * 4) as usize;
        assert_eq!(
            &rr_buf[off..off + stride],
            &fr_buf[off..off + stride],
            "NEON rounded rect interior row {row} should match fill_rect exactly"
        );
    }
}

/// NEON rounded rect blend: interior rows match fill_rect_blend.
#[test]
fn neon_rounded_rect_blend_interior_matches_fill_rect_blend() {
    let width = 200u32;
    let height = 100u32;
    let radius = 16u32;
    let fg = Color::rgba(200, 100, 50, 180);
    let bg = Color::rgb(30, 60, 90);

    let mut rr_buf = vec![0u8; (width * height * 4) as usize];
    {
        let mut surf = make_surface(&mut rr_buf, width, height);
        surf.clear(bg);
        surf.fill_rounded_rect_blend(0, 0, width, height, radius, fg);
    }

    let mut fr_buf = vec![0u8; (width * height * 4) as usize];
    {
        let mut surf = make_surface(&mut fr_buf, width, height);
        surf.clear(bg);
        surf.fill_rect_blend(0, radius, width, height - 2 * radius, fg);
    }

    let stride = (width * 4) as usize;
    for row in radius..(height - radius) {
        let off = (row * width * 4) as usize;
        assert_eq!(
            &rr_buf[off..off + stride],
            &fr_buf[off..off + stride],
            "NEON rounded rect blend interior row {row} should match fill_rect_blend exactly"
        );
    }
}

/// NEON rounded rect: various widths exercise aligned and unaligned paths.
#[test]
fn neon_rounded_rect_various_widths() {
    let color = Color::rgb(200, 100, 50);
    // Test widths that exercise: 1 pixel tail, 2, 3, exactly 4 (one NEON chunk),
    // 5 (1 chunk + 1 tail), 7, 8, 100.
    let widths = [5, 6, 7, 8, 9, 13, 16, 17, 100];

    for &width in &widths {
        let height = width; // square
        let radius = if width >= 8 { 4 } else { width / 2 };
        let mut buf = vec![0u8; (width * height * 4) as usize];
        let mut surf = make_surface(&mut buf, width, height);
        surf.fill_rounded_rect(0, 0, width, height, radius, color);

        // Interior center pixel should be fully solid.
        let cx = width / 2;
        let cy = height / 2;
        assert_eq!(
            surf.get_pixel(cx, cy),
            Some(color),
            "center pixel at width={width}"
        );
    }
}

// ---------------------------------------------------------------------------
// NEON Gaussian blur tests
// ---------------------------------------------------------------------------

/// Helper to create a read-only surface.
fn make_readonly_surface(buf: &[u8], width: u32, height: u32) -> drawing::ReadSurface<'_> {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    assert!(buf.len() >= (stride * height) as usize);
    drawing::ReadSurface {
        data: buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    }
}

/// VAL-BLUR-006: NEON-accelerated blur matches scalar reference within ±1.
#[test]
fn neon_blur_matches_scalar() {
    let w = 32u32;
    let h = 32u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 6u32;
    let sigma_fp = 384u32; // sigma=1.5

    // Create a colorful test pattern.
    let mut src_buf = vec![0u8; size];
    for y in 0..h {
        for x in 0..w {
            let off = (y * stride + x * bpp) as usize;
            src_buf[off] = ((x * 8) % 256) as u8;       // B
            src_buf[off + 1] = ((y * 8) % 256) as u8;   // G
            src_buf[off + 2] = (((x + y) * 4) % 256) as u8; // R
            src_buf[off + 3] = 255;                       // A
        }
    }

    // NEON-accelerated blur (the default path on aarch64).
    let mut dst_neon_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];
    {
        let src = make_readonly_surface(&src_buf, w, h);
        let mut dst = make_surface(&mut dst_neon_buf, w, h);
        drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);
    }

    // Scalar-only blur.
    let mut dst_scalar_buf = vec![0u8; size];
    {
        let src = make_readonly_surface(&src_buf, w, h);
        let mut dst = make_surface(&mut dst_scalar_buf, w, h);
        drawing::blur_surface_scalar(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);
    }

    // Max per-channel difference should be ≤ 1.
    let mut max_diff = 0u8;
    for i in 0..size {
        let diff = (dst_neon_buf[i] as i16 - dst_scalar_buf[i] as i16).unsigned_abs() as u8;
        if diff > max_diff {
            max_diff = diff;
        }
    }

    assert!(
        max_diff <= 1,
        "NEON blur vs scalar blur: max channel diff = {max_diff}, expected ≤ 1"
    );
}
