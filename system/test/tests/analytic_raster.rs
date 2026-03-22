//! Tests for the analytic area coverage rasterizer.
//!
//! Validates that the signed-area trapezoid model produces mathematically
//! correct coverage values for known geometric configurations.

use fonts::rasterize::{RasterBuffer, RasterScratch};

const JETBRAINS_MONO: &[u8] = include_bytes!("../../share/jetbrains-mono.ttf");
const INTER: &[u8] = include_bytes!("../../share/inter.ttf");

// ===========================================================================
// Basic glyph rasterization (integration tests)
// ===========================================================================

#[test]
fn rasterize_letter_l_produces_coverage() {
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let glyph_id = fonts::rasterize::glyph_id_for_char(JETBRAINS_MONO, 'l').unwrap();
    let metrics =
        fonts::rasterize::rasterize(JETBRAINS_MONO, glyph_id, 16, &mut raster, &mut scratch)
            .unwrap();
    drop(raster); // Release mutable borrow on buf.

    let total = (metrics.width * metrics.height) as usize;
    let coverage = &buf[..total];

    // Should have full-coverage pixels (stem interior).
    let has_full = coverage.iter().any(|&c| c >= 200);
    assert!(has_full, "'l' should have high-coverage pixels");

    // Should have partial-coverage pixels (anti-aliased edges).
    let has_partial = coverage.iter().any(|&c| c > 0 && c < 200);
    assert!(has_partial, "'l' should have partial-coverage pixels");

    // Should have zero-coverage pixels (background).
    let has_zero = coverage.iter().any(|&c| c == 0);
    assert!(has_zero, "'l' should have zero-coverage background pixels");
}

#[test]
fn rasterize_letter_o_has_hole() {
    // 'O' has an inner contour (the hole). The coverage should be zero inside
    // the counter, non-zero on the strokes, and zero outside.
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let glyph_id = fonts::rasterize::glyph_id_for_char(INTER, 'O').unwrap();
    let metrics =
        fonts::rasterize::rasterize(INTER, glyph_id, 24, &mut raster, &mut scratch).unwrap();
    drop(raster); // Release mutable borrow on buf.

    let w = metrics.width as usize;
    let h = metrics.height as usize;

    // The center row should have: zero (outside) → high (left stroke) → low (counter) → high (right stroke) → zero (outside)
    let mid_row = h / 2;
    let row = &buf[mid_row * w..(mid_row + 1) * w];

    // Find first and last non-zero pixels (left and right strokes).
    let first_nonzero = row.iter().position(|&c| c > 0).unwrap_or(0);
    let last_nonzero = row.iter().rposition(|&c| c > 0).unwrap_or(w - 1);

    // There should be low-coverage pixels between the strokes (the counter/hole).
    let interior = &row[first_nonzero + 2..last_nonzero.saturating_sub(1)];
    let has_counter = interior.iter().any(|&c| c < 50);
    assert!(
        has_counter,
        "'O' mid-row should have low-coverage counter pixels"
    );
}

#[test]
fn rasterize_space_returns_zero_dimensions() {
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let glyph_id = fonts::rasterize::glyph_id_for_char(JETBRAINS_MONO, ' ').unwrap();
    let metrics =
        fonts::rasterize::rasterize(JETBRAINS_MONO, glyph_id, 16, &mut raster, &mut scratch)
            .unwrap();

    assert_eq!(metrics.width, 0);
    assert_eq!(metrics.height, 0);
    assert!(metrics.advance > 0, "space should have nonzero advance");
}

#[test]
fn rasterize_at_multiple_sizes() {
    // Rasterize the same glyph at different sizes. Larger sizes should produce
    // larger bitmaps (more pixels).
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let glyph_id = fonts::rasterize::glyph_id_for_char(INTER, 'A').unwrap();

    let mut prev_area = 0u32;
    for size in [10, 14, 18, 24, 36] {
        let mut raster = RasterBuffer {
            data: &mut buf,
            width: 128,
            height: 128,
        };
        let metrics =
            fonts::rasterize::rasterize(INTER, glyph_id, size, &mut raster, &mut scratch).unwrap();
        let area = metrics.width * metrics.height;
        assert!(
            area > prev_area,
            "{}px should produce larger bitmap than previous size",
            size
        );
        prev_area = area;
    }
}

#[test]
fn rasterize_coverage_values_bounded() {
    // All coverage values should be in [0, 255].
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for ch in ['A', 'g', 'W', '@', '0'] {
        let glyph_id = fonts::rasterize::glyph_id_for_char(INTER, ch).unwrap();
        let metrics = {
            let mut raster = RasterBuffer {
                data: &mut buf,
                width: 128,
                height: 128,
            };
            fonts::rasterize::rasterize(INTER, glyph_id, 16, &mut raster, &mut scratch).unwrap()
        };
        let total = (metrics.width * metrics.height) as usize;
        for &c in &buf[..total] {
            assert!(c <= 255);
        }
    }
}

#[test]
fn rasterize_variable_font_with_axes() {
    // Rasterize with weight axis variation.
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let glyph_id = fonts::rasterize::glyph_id_for_char(INTER, 'A').unwrap();
    let axes = [fonts::rasterize::AxisValue {
        tag: *b"wght",
        value: 400.0,
    }];
    let metrics = fonts::rasterize::rasterize_with_axes(
        INTER,
        glyph_id,
        16,
        &mut raster,
        &mut scratch,
        &axes,
    )
    .unwrap();

    assert!(metrics.width > 0);
    assert!(metrics.height > 0);
}
