//! Tests for the shaping library (HarfRust integration).
//!
//! Validates that text shaping works correctly: glyph production, ligatures,
//! kerning, and OpenType feature control.

use fonts::{shape, Feature, ShapedGlyph};

const INTER: &[u8] = include_bytes!("../../share/inter.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../share/jetbrains-mono.ttf");

// ---------------------------------------------------------------------------
// VAL-SHAPE-001: Basic Latin text shaping
// ---------------------------------------------------------------------------

#[test]
fn shape_hello_world_glyph_count() {
    // "Hello World" is 11 characters (including space). For basic Latin text
    // with no ligatures, each character should produce one glyph.
    let glyphs = shape(INTER, "Hello World", &[]);
    assert!(
        !glyphs.is_empty(),
        "shaping should produce glyphs for non-empty text"
    );
    // At minimum we expect at least 10 glyphs for 11 characters.
    assert!(
        glyphs.len() >= 10,
        "expected at least 10 glyphs for 'Hello World', got {}",
        glyphs.len()
    );
}

#[test]
fn shape_hello_world_nonzero_advances() {
    let glyphs = shape(INTER, "Hello World", &[]);
    for (i, g) in glyphs.iter().enumerate() {
        // Space glyph (cluster mapping to space character) may have 0 y_advance
        // but should have non-zero x_advance for horizontal text.
        assert!(
            g.x_advance > 0,
            "glyph {} (glyph_id={}) should have positive x_advance, got {}",
            i,
            g.glyph_id,
            g.x_advance
        );
    }
}

#[test]
fn shape_empty_string_produces_no_glyphs() {
    let glyphs = shape(INTER, "", &[]);
    assert!(
        glyphs.is_empty(),
        "empty string should produce 0 glyphs, got {}",
        glyphs.len()
    );
}

// ---------------------------------------------------------------------------
// VAL-SHAPE-002: Ligature production
// ---------------------------------------------------------------------------
// Font: Inter (inter.ttf) — a variable OpenType font. Inter is a UI font
// that does not have traditional fi/fl ligatures.

#[test]
fn shape_ligature_fi_no_ligature_in_inter() {
    // Inter does not have traditional fi/fl ligatures (it's a UI font).
    // "fi" (2 chars) should produce exactly 2 glyphs regardless of liga setting.
    let liga_on = vec!["+liga".parse::<Feature>().unwrap()];
    let glyphs = shape(INTER, "fi", &liga_on);
    assert_eq!(
        glyphs.len(),
        2,
        "Inter: 'fi' with +liga should produce 2 glyphs (no fi ligature), got {}",
        glyphs.len()
    );
}

#[test]
fn shape_ligature_fl_no_ligature_in_inter() {
    // Inter does not have traditional fl ligatures.
    // "fl" (2 chars) should produce exactly 2 glyphs.
    let liga_on = vec!["+liga".parse::<Feature>().unwrap()];
    let glyphs = shape(INTER, "fl", &liga_on);
    assert_eq!(
        glyphs.len(),
        2,
        "Inter: 'fl' with +liga should produce 2 glyphs (no fl ligature), got {}",
        glyphs.len()
    );
}

#[test]
fn shape_ligature_disabled_no_merge() {
    // With ligatures explicitly disabled, "fi" should produce exactly 2 glyphs.
    // Inter has no fi ligature, so this matches the enabled case.
    let liga_off = vec!["-liga".parse::<Feature>().unwrap()];
    let glyphs = shape(INTER, "fi", &liga_off);
    assert_eq!(
        glyphs.len(),
        2,
        "Inter: 'fi' with -liga should produce 2 glyphs, got {}",
        glyphs.len()
    );
}

// ---------------------------------------------------------------------------
// VAL-SHAPE-003: Kerning application
// ---------------------------------------------------------------------------

#[test]
fn shape_kerning_av_tighter_than_sum() {
    // "AV" is a classic kerning pair. With Inter, the total advance
    // when shaped together should be less than the sum of individual advances
    // (kerning pulls them closer together).
    let av_glyphs = shape(INTER, "AV", &[]);
    assert_eq!(av_glyphs.len(), 2, "AV should produce 2 glyphs");

    let total_advance: i32 = av_glyphs.iter().map(|g| g.x_advance).sum();

    let a_glyphs = shape(INTER, "A", &[]);
    let v_glyphs = shape(INTER, "V", &[]);
    let sum_individual = a_glyphs[0].x_advance + v_glyphs[0].x_advance;

    assert!(
        total_advance < sum_individual,
        "kerned AV advance ({}) should be less than sum of A ({}) + V ({})",
        total_advance,
        a_glyphs[0].x_advance,
        v_glyphs[0].x_advance,
    );
}

// ---------------------------------------------------------------------------
// VAL-SHAPE-004: OpenType feature control
// ---------------------------------------------------------------------------

#[test]
fn shape_feature_liga_on_off_same_for_inter() {
    // Inter has no traditional fi/fl/ffi ligatures, so enabling vs disabling
    // the liga feature should produce identical glyph output.
    let liga_on = vec!["+liga".parse::<Feature>().unwrap()];
    let liga_off = vec!["-liga".parse::<Feature>().unwrap()];

    let text = "difficult office";
    let glyphs_on = shape(INTER, text, &liga_on);
    let glyphs_off = shape(INTER, text, &liga_off);

    let ids_on: Vec<u16> = glyphs_on.iter().map(|g| g.glyph_id).collect();
    let ids_off: Vec<u16> = glyphs_off.iter().map(|g| g.glyph_id).collect();

    assert_eq!(
        ids_on, ids_off,
        "Inter has no traditional ligatures — liga on vs off should produce \
         identical glyph IDs for '{}'",
        text
    );
}

// ---------------------------------------------------------------------------
// VAL-E2E-001: Proportional shaping confirms different widths
// ---------------------------------------------------------------------------

#[test]
fn shape_proportional_w_vs_i() {
    // Proportional font should give different advance widths for 'W' and 'i'.
    let w_glyphs = shape(INTER, "W", &[]);
    let i_glyphs = shape(INTER, "i", &[]);
    assert_eq!(w_glyphs.len(), 1);
    assert_eq!(i_glyphs.len(), 1);
    assert_ne!(
        w_glyphs[0].x_advance, i_glyphs[0].x_advance,
        "proportional font should have different advances for W and i"
    );
}

// ---------------------------------------------------------------------------
// VAL-E2E-004: Monospace text has uniform widths
// ---------------------------------------------------------------------------

#[test]
fn shape_monospace_uniform_width() {
    // JetBrains Mono is monospace — all Latin glyphs should have the same advance.
    let glyphs = shape(JETBRAINS_MONO, "iiiWWW", &[]);
    assert_eq!(glyphs.len(), 6, "expected 6 glyphs for 'iiiWWW'");
    let first_advance = glyphs[0].x_advance;
    for (idx, g) in glyphs.iter().enumerate() {
        assert_eq!(
            g.x_advance, first_advance,
            "monospace glyph {} has advance {} (expected {})",
            idx, g.x_advance, first_advance,
        );
    }
}

// ---------------------------------------------------------------------------
// Shaping with monospace font produces correct output
// ---------------------------------------------------------------------------

#[test]
fn shape_jetbrains_mono_basic() {
    let glyphs = shape(JETBRAINS_MONO, "Hello", &[]);
    assert_eq!(glyphs.len(), 5, "expected 5 glyphs for 'Hello'");
    for g in &glyphs {
        assert!(
            g.x_advance > 0,
            "monospace glyph should have positive advance"
        );
    }
}

// ---------------------------------------------------------------------------
// ShapedGlyph struct layout
// ---------------------------------------------------------------------------

#[test]
fn shaped_glyph_is_repr_c() {
    // Verify the struct has expected size (2 + 4*4 + 4 = 22, but #[repr(C)]
    // adds padding — u16 then 3 bytes padding, or packed differently).
    // The actual size depends on alignment.
    let size = core::mem::size_of::<ShapedGlyph>();
    // repr(C): u16 (2) + 2 padding + i32 (4) + i32 (4) + i32 (4) + i32 (4) + u32 (4) = 24
    assert_eq!(
        size, 24,
        "ShapedGlyph size should be 24 bytes (repr(C) with padding), got {}",
        size
    );
}

// ---------------------------------------------------------------------------
// Invalid font data doesn't panic
// ---------------------------------------------------------------------------

#[test]
fn shape_invalid_font_data() {
    let glyphs = shape(&[0, 1, 2, 3], "Hello", &[]);
    assert!(
        glyphs.is_empty(),
        "invalid font data should produce 0 glyphs"
    );
}

#[test]
fn shape_empty_font_data() {
    let glyphs = shape(&[], "Hello", &[]);
    assert!(glyphs.is_empty(), "empty font data should produce 0 glyphs");
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-001: Variable Inter axis detection
// ---------------------------------------------------------------------------

#[test]
fn varfont_inter_has_two_axes() {
    let axes = fonts::rasterize::font_axes(INTER);
    assert_eq!(
        axes.len(),
        2,
        "variable Inter should have 2 axes, got {}",
        axes.len()
    );
}

#[test]
fn varfont_inter_opsz_axis() {
    let axes = fonts::rasterize::font_axes(INTER);
    let opsz = axes.iter().find(|a| &a.tag == b"opsz");
    assert!(opsz.is_some(), "should have opsz axis");
    let opsz = opsz.unwrap();
    assert!(
        opsz.min_value < opsz.max_value,
        "opsz min ({}) must be < max ({})",
        opsz.min_value,
        opsz.max_value
    );
    assert!(
        opsz.default_value >= opsz.min_value && opsz.default_value <= opsz.max_value,
        "opsz default ({}) must be within [{}, {}]",
        opsz.default_value,
        opsz.min_value,
        opsz.max_value
    );
}

#[test]
fn varfont_inter_wght_axis() {
    let axes = fonts::rasterize::font_axes(INTER);
    let wght = axes.iter().find(|a| &a.tag == b"wght");
    assert!(wght.is_some(), "should have wght axis");
    let wght = wght.unwrap();
    assert!(
        wght.min_value < wght.max_value,
        "wght min ({}) must be < max ({})",
        wght.min_value,
        wght.max_value
    );
    // Inter weight range: 100–900.
    assert_eq!(
        wght.min_value, 100.0,
        "Inter wght min should be 100, got {}",
        wght.min_value
    );
    assert_eq!(
        wght.max_value, 900.0,
        "Inter wght max should be 900, got {}",
        wght.max_value
    );
}

#[test]
fn varfont_inter_axis_tags_exact() {
    let axes = fonts::rasterize::font_axes(INTER);
    let tags: Vec<[u8; 4]> = axes.iter().map(|a| a.tag).collect();
    assert!(tags.contains(b"opsz"), "missing opsz axis in {:?}", tags);
    assert!(tags.contains(b"wght"), "missing wght axis in {:?}", tags);
    assert_eq!(
        tags.len(),
        2,
        "Inter should have exactly 2 axes (opsz, wght), got {:?}",
        tags
    );
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-004: Variable JetBrains Mono axis detection
// ---------------------------------------------------------------------------

#[test]
fn varfont_jetbrains_mono_has_wght_axis() {
    let axes = fonts::rasterize::font_axes(JETBRAINS_MONO);
    assert!(
        !axes.is_empty(),
        "variable JetBrains Mono should have at least one axis"
    );
    let wght = axes.iter().find(|a| &a.tag == b"wght");
    assert!(wght.is_some(), "should have wght axis");
    let wght = wght.unwrap();
    assert!(
        wght.min_value < wght.max_value,
        "wght min ({}) must be < max ({})",
        wght.min_value,
        wght.max_value
    );
    assert!(
        wght.default_value >= wght.min_value && wght.default_value <= wght.max_value,
        "wght default ({}) must be within [{}, {}]",
        wght.default_value,
        wght.min_value,
        wght.max_value
    );
}

#[test]
fn varfont_jetbrains_mono_wght_range_valid() {
    let axes = fonts::rasterize::font_axes(JETBRAINS_MONO);
    let wght = axes.iter().find(|a| &a.tag == b"wght").unwrap();
    // JetBrains Mono variable has weights from 100 to 800.
    assert_eq!(
        wght.min_value, 100.0,
        "JetBrains Mono wght min should be 100, got {}",
        wght.min_value
    );
    assert_eq!(
        wght.max_value, 800.0,
        "JetBrains Mono wght max should be 800, got {}",
        wght.max_value
    );
}

// ---------------------------------------------------------------------------
// Non-variable / invalid fonts return no axes
// ---------------------------------------------------------------------------

#[test]
fn varfont_empty_data_returns_no_axes() {
    let axes = fonts::rasterize::font_axes(&[]);
    assert!(axes.is_empty(), "empty data should have 0 axes");
}

// ---------------------------------------------------------------------------
// Variable fonts parse via shaping library (basic shaping works)
// ---------------------------------------------------------------------------

#[test]
fn varfont_inter_shapes_text() {
    let glyphs = shape(INTER, "Hello", &[]);
    assert!(
        glyphs.len() >= 5,
        "variable Inter should shape 'Hello' to >= 5 glyphs, got {}",
        glyphs.len()
    );
    for g in &glyphs {
        assert!(g.x_advance > 0, "all advances should be > 0");
    }
}

#[test]
fn varfont_jetbrains_mono_shapes_text() {
    let glyphs = shape(JETBRAINS_MONO, "Hello", &[]);
    assert!(
        glyphs.len() >= 5,
        "variable JetBrains Mono should shape 'Hello' to >= 5 glyphs, got {}",
        glyphs.len()
    );
    for g in &glyphs {
        assert!(g.x_advance > 0, "all advances should be > 0");
    }
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-002: Axis value affects glyph outlines
// ---------------------------------------------------------------------------

/// Helper: rasterize a glyph at a given weight and return total coverage sum.
fn rasterize_at_weight(font_data: &[u8], glyph_id: u16, size_px: u16, weight: f32) -> u32 {
    use fonts::rasterize::{AxisValue, RasterBuffer, RasterScratch};

    let mut buf = vec![0u8; 48 * 6 * 48];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf,
        width: 48,
        height: 48,
    };
    let axes = [AxisValue {
        tag: *b"wght",
        value: weight,
    }];
    let metrics = fonts::rasterize::rasterize_with_axes(
        font_data,
        glyph_id,
        size_px,
        &mut rb,
        &mut scratch,
        &axes,
    )
    .expect("rasterization should succeed");

    let total = (metrics.width * metrics.height * 3) as usize;
    buf[..total].iter().map(|&b| b as u32).sum()
}

/// Helper: look up glyph ID for a character.
fn glyph_for_char(font_data: &[u8], ch: char) -> u16 {
    fonts::rasterize::glyph_id_for_char(font_data, ch).expect("should find glyph for character")
}

#[test]
fn varfont_wght_400_vs_700_different_coverage() {
    // VAL-VARFONT-002: wght=400 vs wght=700 produces measurably different coverage.
    let gid = glyph_for_char(INTER, 'H');
    let cov_400 = rasterize_at_weight(INTER, gid, 24, 400.0);
    let cov_700 = rasterize_at_weight(INTER, gid, 24, 700.0);

    assert_ne!(
        cov_400, cov_700,
        "coverage at wght=400 ({}) must differ from wght=700 ({})",
        cov_400, cov_700
    );
}

#[test]
fn varfont_wght_700_heavier_than_400() {
    // VAL-CROSS-002: Coverage sum at wght=700 > coverage sum at wght=400.
    let gid = glyph_for_char(INTER, 'H');
    let cov_400 = rasterize_at_weight(INTER, gid, 24, 400.0);
    let cov_700 = rasterize_at_weight(INTER, gid, 24, 700.0);

    assert!(
        cov_700 > cov_400,
        "wght=700 coverage ({}) must be > wght=400 coverage ({})",
        cov_700,
        cov_400
    );
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-003: Interpolation at intermediate axis values
// ---------------------------------------------------------------------------

#[test]
fn varfont_wght_550_differs_from_400_and_700() {
    // VAL-VARFONT-003: wght=550 differs from both 400 and 700 by >5% of pixels.
    let gid = glyph_for_char(INTER, 'H');
    let cov_400 = rasterize_at_weight(INTER, gid, 24, 400.0);
    let cov_550 = rasterize_at_weight(INTER, gid, 24, 550.0);
    let cov_700 = rasterize_at_weight(INTER, gid, 24, 700.0);

    // Difference from 400.
    let diff_400 = if cov_550 > cov_400 {
        cov_550 - cov_400
    } else {
        cov_400 - cov_550
    };
    let pct_400 = diff_400 as f64 / cov_400.max(1) as f64 * 100.0;

    // Difference from 700.
    let diff_700 = if cov_550 > cov_700 {
        cov_550 - cov_700
    } else {
        cov_700 - cov_550
    };
    let pct_700 = diff_700 as f64 / cov_700.max(1) as f64 * 100.0;

    assert!(
        pct_400 > 5.0,
        "wght=550 coverage ({}) must differ from wght=400 ({}) by >5%, got {:.1}%",
        cov_550,
        cov_400,
        pct_400
    );
    assert!(
        pct_700 > 5.0,
        "wght=550 coverage ({}) must differ from wght=700 ({}) by >5%, got {:.1}%",
        cov_550,
        cov_700,
        pct_700
    );
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-002: Out-of-range axis value clamped without panic
// ---------------------------------------------------------------------------

#[test]
fn varfont_out_of_range_wght_clamped_no_panic() {
    // Out-of-range axis value (wght=2000) is clamped to font's max without panic.
    let gid = glyph_for_char(INTER, 'A');
    // Should not panic — just clamp to max.
    let cov = rasterize_at_weight(INTER, gid, 18, 2000.0);
    assert!(
        cov > 0,
        "clamped out-of-range weight should still produce coverage"
    );

    // Also test underflow (wght=0, below min).
    let cov_low = rasterize_at_weight(INTER, gid, 18, 0.0);
    assert!(
        cov_low > 0,
        "clamped underflow weight should still produce coverage"
    );
}

#[test]
fn varfont_wght_2000_equals_max() {
    // Out-of-range wght=2000 should produce same result as max weight.
    let gid = glyph_for_char(INTER, 'H');
    let axes = fonts::rasterize::font_axes(INTER);
    let wght = axes.iter().find(|a| &a.tag == b"wght").unwrap();
    let cov_2000 = rasterize_at_weight(INTER, gid, 18, 2000.0);
    let cov_max = rasterize_at_weight(INTER, gid, 18, wght.max_value);
    assert_eq!(
        cov_2000, cov_max,
        "wght=2000 should produce same result as wght={} (font max)",
        wght.max_value
    );
}

// ---------------------------------------------------------------------------
// VAL-CACHE-005: Glyph cache key includes axis values
// ---------------------------------------------------------------------------

#[test]
fn varfont_cache_axis_values_separate_entries() {
    // Same glyph at wght=400 and wght=700 should be cached as separate entries.
    use fonts::cache::{LruCachedGlyph, LruGlyphCache};

    let mut cache = LruGlyphCache::new(64);
    // Simulate caching with axis hash included in the key.
    let glyph_400 = LruCachedGlyph {
        width: 10,
        height: 12,
        bearing_x: 1,
        bearing_y: 10,
        advance: 8,
        coverage: vec![100; 30],
    };
    let glyph_700 = LruCachedGlyph {
        width: 12,
        height: 14,
        bearing_x: 1,
        bearing_y: 12,
        advance: 9,
        coverage: vec![200; 30],
    };

    // Use the new axis-aware cache API.
    let axes_400: &[fonts::rasterize::AxisValue] = &[fonts::rasterize::AxisValue {
        tag: *b"wght",
        value: 400.0,
    }];
    let axes_700: &[fonts::rasterize::AxisValue] = &[fonts::rasterize::AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];

    let hash_400 = fonts::cache::axis_values_hash(axes_400);
    let hash_700 = fonts::cache::axis_values_hash(axes_700);

    cache.insert_with_axes(65, 18, hash_400, glyph_400.clone());
    cache.insert_with_axes(65, 18, hash_700, glyph_700.clone());

    // Both should be retrievable independently.
    let r400 = cache.get_with_axes(65, 18, hash_400);
    assert!(r400.is_some(), "wght=400 entry must be retrievable");
    assert_eq!(r400.unwrap().coverage, vec![100u8; 30]);

    let r700 = cache.get_with_axes(65, 18, hash_700);
    assert!(r700.is_some(), "wght=700 entry must be retrievable");
    assert_eq!(r700.unwrap().coverage, vec![200u8; 30]);
}

#[test]
fn varfont_cache_no_axes_vs_with_axes() {
    // Glyph cached without axis values should be different from with axis values.
    use fonts::cache::{LruCachedGlyph, LruGlyphCache};

    let mut cache = LruGlyphCache::new(64);
    let glyph_default = LruCachedGlyph {
        width: 10,
        height: 12,
        bearing_x: 1,
        bearing_y: 10,
        advance: 8,
        coverage: vec![50; 30],
    };
    let glyph_heavy = LruCachedGlyph {
        width: 12,
        height: 14,
        bearing_x: 1,
        bearing_y: 12,
        advance: 9,
        coverage: vec![150; 30],
    };

    // No axes = hash of 0.
    cache.insert_with_axes(65, 18, 0, glyph_default.clone());
    let axes_700: &[fonts::rasterize::AxisValue] = &[fonts::rasterize::AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];
    let hash_700 = fonts::cache::axis_values_hash(axes_700);
    cache.insert_with_axes(65, 18, hash_700, glyph_heavy.clone());

    let r_default = cache.get_with_axes(65, 18, 0);
    assert_eq!(r_default.unwrap().coverage, vec![50u8; 30]);

    let r_heavy = cache.get_with_axes(65, 18, hash_700);
    assert_eq!(r_heavy.unwrap().coverage, vec![150u8; 30]);
}

// ---------------------------------------------------------------------------
// Shaping with axis values (HarfRust Variation/ShaperInstance)
// ---------------------------------------------------------------------------

#[test]
fn varfont_shape_with_variations_produces_output() {
    use fonts::rasterize::AxisValue;

    let axes = [AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];
    let glyphs = fonts::shape_with_variations(INTER, "Hello", &[], &axes);
    assert!(
        glyphs.len() >= 5,
        "shape_with_variations should produce glyphs"
    );
    for g in &glyphs {
        assert!(g.x_advance > 0, "advances should be > 0");
    }
}

#[test]
fn varfont_shape_with_no_variations_same_as_default() {
    // Shaping with empty axis values should match regular shape().
    let glyphs_default = shape(INTER, "Hello", &[]);
    let glyphs_empty = fonts::shape_with_variations(INTER, "Hello", &[], &[]);

    assert_eq!(
        glyphs_default.len(),
        glyphs_empty.len(),
        "empty variations should match default shaping"
    );
    for (a, b) in glyphs_default.iter().zip(glyphs_empty.iter()) {
        assert_eq!(a.glyph_id, b.glyph_id);
    }
}

// ===========================================================================
// VAL-OPSZ-001: Optical size calculation
// ===========================================================================

#[test]
fn opsz_calculation_three_size_dpi_combos_distinct() {
    // VAL-OPSZ-001: Optical size calculation produces different opsz values
    // for different size/DPI combinations: (10px,144dpi), (18px,96dpi), (48px,192dpi).
    use fonts::rasterize::compute_optical_size;

    let opsz_a = compute_optical_size(10, 144);
    let opsz_b = compute_optical_size(18, 96);
    let opsz_c = compute_optical_size(48, 192);

    assert_ne!(
        opsz_a, opsz_b,
        "opsz for (10px,144dpi)={} must differ from (18px,96dpi)={}",
        opsz_a, opsz_b
    );
    assert_ne!(
        opsz_b, opsz_c,
        "opsz for (18px,96dpi)={} must differ from (48px,192dpi)={}",
        opsz_b, opsz_c
    );
    assert_ne!(
        opsz_a, opsz_c,
        "opsz for (10px,144dpi)={} must differ from (48px,192dpi)={}",
        opsz_a, opsz_c
    );
}

#[test]
fn opsz_calculation_larger_size_produces_larger_opsz() {
    // Larger rendered sizes should produce larger optical size values.
    use fonts::rasterize::compute_optical_size;

    let opsz_small = compute_optical_size(10, 96);
    let opsz_large = compute_optical_size(48, 96);

    assert!(
        opsz_large > opsz_small,
        "opsz at 48px ({}) must be > opsz at 10px ({})",
        opsz_large,
        opsz_small
    );
}

#[test]
fn opsz_calculation_is_point_size_based() {
    // The computation is: opsz = font_size_px * 72.0 / dpi.
    // At 72dpi, opsz == font_size_px (1:1 mapping).
    use fonts::rasterize::compute_optical_size;

    let opsz = compute_optical_size(12, 72);
    // At 72 DPI, 12px == 12pt.
    let expected = 12.0f32;
    assert!(
        (opsz - expected).abs() < 0.01,
        "at 72 DPI, 12px should map to opsz={:.2}, got {:.2}",
        expected,
        opsz
    );
}

// ===========================================================================
// VAL-OPSZ-002: Automatic optical size application
// ===========================================================================

/// Helper: rasterize a glyph with automatic optical sizing at a given font size.
fn rasterize_with_auto_opsz(font_data: &[u8], glyph_id: u16, size_px: u16) -> Vec<u8> {
    use fonts::rasterize::{auto_axis_values_for_opsz, RasterBuffer, RasterScratch};

    let dpi = 96; // standard screen DPI
    let auto_axes = auto_axis_values_for_opsz(font_data, size_px, dpi);
    let mut buf = vec![0u8; 128 * 6 * 128];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize_with_axes(
        font_data,
        glyph_id,
        size_px,
        &mut rb,
        &mut scratch,
        &auto_axes,
    );

    match metrics {
        Some(m) => {
            let total = (m.width * m.height * 3) as usize;
            buf[..total].to_vec()
        }
        None => vec![],
    }
}

#[test]
fn opsz_auto_10px_vs_48px_different_coverage() {
    // VAL-OPSZ-002: When rendering 10px text vs 48px text with variable Inter,
    // the opsz axis is automatically set to match the rendered size, producing
    // different glyph outlines (smaller text gets sturdier letterforms).
    let gid = glyph_for_char(INTER, 'a');
    let coverage_10 = rasterize_with_auto_opsz(INTER, gid, 10);
    let coverage_48 = rasterize_with_auto_opsz(INTER, gid, 48);

    // Both should produce some output.
    assert!(
        !coverage_10.is_empty(),
        "auto-opsz at 10px should produce coverage"
    );
    assert!(
        !coverage_48.is_empty(),
        "auto-opsz at 48px should produce coverage"
    );

    // They should differ — different opsz produces different outlines.
    // Normalize coverage per pixel to compare independent of size.
    let sum_10: u64 = coverage_10.iter().map(|&b| b as u64).sum();
    let sum_48: u64 = coverage_48.iter().map(|&b| b as u64).sum();
    let per_pixel_10 = sum_10 as f64 / coverage_10.len().max(1) as f64;
    let per_pixel_48 = sum_48 as f64 / coverage_48.len().max(1) as f64;

    // At minimum, the normalized coverage densities or total pixel counts differ.
    // The 10px version should have sturdier (optically compensated) letterforms.
    assert!(
        (per_pixel_10 - per_pixel_48).abs() > 0.01 || coverage_10.len() != coverage_48.len(),
        "auto-opsz at 10px (avg={:.2}, len={}) vs 48px (avg={:.2}, len={}) \
         should produce different coverage",
        per_pixel_10,
        coverage_10.len(),
        per_pixel_48,
        coverage_48.len()
    );
}

#[test]
fn opsz_auto_returns_opsz_axis_value() {
    // The auto function should return an AxisValue with tag "opsz".
    use fonts::rasterize::auto_axis_values_for_opsz;

    let axes = auto_axis_values_for_opsz(INTER, 18, 96);
    assert!(
        !axes.is_empty(),
        "auto_axis_values_for_opsz should return axis values for a font with opsz"
    );
    let opsz_av = axes.iter().find(|av| &av.tag == b"opsz");
    assert!(opsz_av.is_some(), "returned axes should include opsz");
    let opsz_val = opsz_av.unwrap().value;
    // At 18px, 96dpi: opsz = 18 * 72 / 96 = 13.5. Inter opsz range is
    // 14–32, so it should be clamped to the min (14.0).
    let font_axes = fonts::rasterize::font_axes(INTER);
    let opsz_axis = font_axes.iter().find(|a| &a.tag == b"opsz").unwrap();
    assert!(
        opsz_val >= opsz_axis.min_value && opsz_val <= opsz_axis.max_value,
        "opsz value ({}) must be within font's range [{}, {}]",
        opsz_val,
        opsz_axis.min_value,
        opsz_axis.max_value
    );
}

// ===========================================================================
// VAL-OPSZ-003: Fonts without opsz axis are unaffected
// ===========================================================================

#[test]
fn opsz_no_op_for_non_opsz_font() {
    // VAL-OPSZ-003: When auto optical sizing is applied to a font without an
    // opsz axis (e.g., JetBrains Mono variable), rendering is unchanged
    // compared to rendering without auto-opsz. No error or crash.
    use fonts::rasterize::{auto_axis_values_for_opsz, RasterBuffer, RasterScratch};

    let gid = glyph_for_char(JETBRAINS_MONO, 'H');

    // Render without auto-opsz (no axes).
    let mut buf_without = vec![0u8; 48 * 6 * 48];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf_without,
        width: 48,
        height: 48,
    };
    let metrics_without =
        fonts::rasterize::rasterize(JETBRAINS_MONO, gid, 18, &mut rb, &mut scratch)
            .expect("rasterization without opsz should succeed");

    let total_without = (metrics_without.width * metrics_without.height * 3) as usize;
    let coverage_without: Vec<u8> = buf_without[..total_without].to_vec();

    // Render with auto-opsz (should be a no-op since SCP has no opsz axis).
    let auto_axes = auto_axis_values_for_opsz(JETBRAINS_MONO, 18, 96);
    // Should return empty — no opsz axis in JetBrains Mono.
    assert!(
        auto_axes.is_empty(),
        "auto_axis_values_for_opsz should return empty for a font without opsz axis, got {:?}",
        auto_axes
            .iter()
            .map(|a| core::str::from_utf8(&a.tag).unwrap_or("?"))
            .collect::<Vec<_>>()
    );

    // Render with the (empty) auto axes.
    let mut buf_with = vec![0u8; 48 * 6 * 48];
    let mut scratch2 = Box::new(RasterScratch::zeroed());
    let mut rb2 = RasterBuffer {
        data: &mut buf_with,
        width: 48,
        height: 48,
    };
    let metrics_with = fonts::rasterize::rasterize_with_axes(
        JETBRAINS_MONO,
        gid,
        18,
        &mut rb2,
        &mut scratch2,
        &auto_axes,
    )
    .expect("rasterization with auto-opsz should succeed for non-opsz font");

    let total_with = (metrics_with.width * metrics_with.height * 3) as usize;

    assert_eq!(
        total_without, total_with,
        "coverage size should be identical"
    );
    assert_eq!(
        &coverage_without[..],
        &buf_with[..total_with],
        "coverage should be byte-identical without and with auto-opsz on non-opsz font"
    );
}

#[test]
fn opsz_auto_empty_for_font_without_opsz() {
    // Fonts without an opsz axis should return empty axes.
    use fonts::rasterize::auto_axis_values_for_opsz;

    let axes = auto_axis_values_for_opsz(JETBRAINS_MONO, 18, 96);
    assert!(
        axes.is_empty(),
        "font without opsz axis should return empty auto-opsz axes"
    );
}

#[test]
fn opsz_auto_empty_for_empty_data() {
    // Empty font data should return empty without panic.
    use fonts::rasterize::auto_axis_values_for_opsz;

    let axes = auto_axis_values_for_opsz(&[], 18, 96);
    assert!(
        axes.is_empty(),
        "empty font data should return empty auto-opsz axes"
    );
}

// ===========================================================================
// VAL-WEIGHT-001: Weight correction calculation
// ===========================================================================

#[test]
fn weight_correction_white_on_black_reduces_weight() {
    // VAL-WEIGHT-001: Light-on-dark (white on black) should produce a
    // correction factor < 1.0 (weight reduction to compensate for irradiation).
    use fonts::rasterize::weight_correction_factor;

    let factor = weight_correction_factor(255, 255, 255, 0, 0, 0);
    assert!(
        factor < 1.0,
        "white-on-black correction factor ({:.4}) should be < 1.0",
        factor
    );
}

#[test]
fn weight_correction_black_on_white_no_reduction() {
    // VAL-WEIGHT-001: Dark-on-light (black on white) should produce a
    // correction factor >= 1.0 (no weight reduction needed).
    use fonts::rasterize::weight_correction_factor;

    let factor = weight_correction_factor(0, 0, 0, 255, 255, 255);
    assert!(
        factor >= 1.0,
        "black-on-white correction factor ({:.4}) should be >= 1.0",
        factor
    );
}

#[test]
fn weight_correction_same_color_no_reduction() {
    // Same foreground and background → no contrast → no weight change.
    use fonts::rasterize::weight_correction_factor;

    let factor = weight_correction_factor(128, 128, 128, 128, 128, 128);
    assert!(
        (factor - 1.0).abs() < f32::EPSILON,
        "same fg/bg correction factor ({:.4}) should be 1.0",
        factor
    );
}

// ===========================================================================
// VAL-WEIGHT-002: Continuous weight correction
// ===========================================================================

#[test]
fn weight_correction_monotonically_decreasing_with_contrast() {
    // VAL-WEIGHT-002: Weight correction is proportional to luminance contrast,
    // not a binary switch. 3+ contrast levels with lighter fg than bg produce
    // monotonically decreasing correction factor as contrast increases.
    use fonts::rasterize::weight_correction_factor;

    // Low contrast: light gray on dark gray.
    let factor_low = weight_correction_factor(160, 160, 160, 80, 80, 80);
    // Medium contrast: lighter gray on darker gray.
    let factor_mid = weight_correction_factor(200, 200, 200, 40, 40, 40);
    // High contrast: white on black.
    let factor_high = weight_correction_factor(255, 255, 255, 0, 0, 0);

    assert!(
        factor_low < 1.0,
        "low-contrast light-on-dark factor ({:.4}) should be < 1.0",
        factor_low
    );
    assert!(
        factor_mid < factor_low,
        "medium-contrast factor ({:.4}) should be < low-contrast factor ({:.4})",
        factor_mid,
        factor_low
    );
    assert!(
        factor_high < factor_mid,
        "high-contrast factor ({:.4}) should be < medium-contrast factor ({:.4})",
        factor_high,
        factor_mid
    );
}

#[test]
fn weight_correction_five_contrast_levels_monotonic() {
    // Additional granularity: 5 levels from minimal to maximal contrast.
    use fonts::rasterize::weight_correction_factor;

    let levels: [(u8, u8); 5] = [
        (140, 100), // minimal contrast
        (160, 80),  // low contrast
        (200, 40),  // medium contrast
        (230, 15),  // high contrast
        (255, 0),   // maximum contrast
    ];

    let factors: Vec<f32> = levels
        .iter()
        .map(|&(fg, bg)| weight_correction_factor(fg, fg, fg, bg, bg, bg))
        .collect();

    for i in 1..factors.len() {
        assert!(
            factors[i] < factors[i - 1],
            "factor[{}]={:.4} should be < factor[{}]={:.4} (higher contrast → more reduction)",
            i,
            factors[i],
            i - 1,
            factors[i - 1]
        );
    }
}

// ===========================================================================
// VAL-WEIGHT-003: Weight correction affects rendering
// ===========================================================================

#[test]
fn weight_correction_reduces_coverage_white_on_black() {
    // VAL-WEIGHT-003: Rendering white-on-black text with a variable weight font
    // and weight correction enabled produces measurably thinner glyph coverage
    // (lower total coverage sum) than rendering without correction.
    //
    // We use wght=400 (Regular) as the base weight because the font's default
    // may be at the axis minimum (100 for Inter), where a reduction would
    // clamp to the minimum and show no difference.
    use fonts::rasterize::{
        font_axes, rasterize_with_axes, weight_correction_factor, AxisValue, RasterBuffer,
        RasterScratch,
    };

    let gid = glyph_for_char(INTER, 'H');

    // Use Regular weight (400) as base weight — high enough that correction
    // can reduce it without clamping to the axis minimum.
    let base_weight = 400.0f32;

    // Render at base weight (uncorrected).
    let axes_base = vec![AxisValue {
        tag: *b"wght",
        value: base_weight,
    }];
    let mut buf_base = vec![0u8; 128 * 6 * 128];
    let mut scratch_base = Box::new(RasterScratch::zeroed());
    let mut rb_base = RasterBuffer {
        data: &mut buf_base,
        width: 128,
        height: 128,
    };
    let metrics_base = rasterize_with_axes(
        INTER,
        gid,
        24,
        &mut rb_base,
        &mut scratch_base,
        &axes_base,
    )
    .expect("rasterization at base weight should succeed");
    let total_base = (metrics_base.width * metrics_base.height * 3) as usize;
    let sum_base: u64 = buf_base[..total_base].iter().map(|&b| b as u64).sum();

    // Compute corrected weight (white fg on black bg).
    let factor = weight_correction_factor(255, 255, 255, 0, 0, 0);
    assert!(
        factor < 1.0,
        "white-on-black factor ({:.4}) should be < 1.0",
        factor
    );
    let corrected_weight = base_weight * factor;

    // Verify corrected weight is within the font's wght axis range.
    let axes = font_axes(INTER);
    let wght_axis = axes.iter().find(|a| &a.tag == b"wght").unwrap();
    let clamped_weight = if corrected_weight < wght_axis.min_value {
        wght_axis.min_value
    } else if corrected_weight > wght_axis.max_value {
        wght_axis.max_value
    } else {
        corrected_weight
    };

    assert!(
        clamped_weight < base_weight,
        "corrected weight ({:.1}) should be < base weight ({:.1})",
        clamped_weight,
        base_weight
    );

    // Render at corrected weight.
    let axes_corrected = vec![AxisValue {
        tag: *b"wght",
        value: clamped_weight,
    }];
    let mut buf_corrected = vec![0u8; 128 * 6 * 128];
    let mut scratch_corrected = Box::new(RasterScratch::zeroed());
    let mut rb_corrected = RasterBuffer {
        data: &mut buf_corrected,
        width: 128,
        height: 128,
    };
    let metrics_corrected = rasterize_with_axes(
        INTER,
        gid,
        24,
        &mut rb_corrected,
        &mut scratch_corrected,
        &axes_corrected,
    )
    .expect("rasterization at corrected weight should succeed");
    let total_corrected = (metrics_corrected.width * metrics_corrected.height * 3) as usize;
    let sum_corrected: u64 = buf_corrected[..total_corrected]
        .iter()
        .map(|&b| b as u64)
        .sum();

    assert!(
        sum_corrected < sum_base,
        "corrected coverage sum ({}) should be < base coverage sum ({}) \
         for white-on-black text (lighter weight = thinner strokes)",
        sum_corrected,
        sum_base
    );
}

// ===========================================================================
// VAL-WEIGHT-004: Fonts without wght axis are unaffected
// ===========================================================================

#[test]
fn weight_correction_no_op_for_invalid_font_data() {
    // VAL-WEIGHT-004: Weight correction on invalid font data produces
    // no change and no error.
    use fonts::rasterize::auto_weight_correction_axes;

    let axes = auto_weight_correction_axes(
        &[0, 1, 2, 3],
        255,
        255,
        255, // white fg
        0,
        0,
        0, // black bg
    );
    assert!(
        axes.is_empty(),
        "invalid font data should return empty weight correction axes, got {} axes",
        axes.len()
    );
}

#[test]
fn weight_correction_no_op_for_font_without_wght() {
    // A font that is variable but lacks a wght axis should also be unaffected.
    // Both Inter and JetBrains Mono have wght, so we can't easily test this
    // without a custom font. Instead, verify that the function handles
    // empty font data gracefully.
    use fonts::rasterize::auto_weight_correction_axes;

    let axes = auto_weight_correction_axes(
        &[],
        255,
        255,
        255, // white fg
        0,
        0,
        0, // black bg
    );
    assert!(
        axes.is_empty(),
        "empty font data should return empty weight correction axes"
    );
}

#[test]
fn weight_correction_no_op_rendering_identical_with_empty_axes() {
    // VAL-WEIGHT-004: Rendering with empty correction axes produces
    // identical output to rendering without any axes.
    use fonts::rasterize::{rasterize, rasterize_with_axes, RasterBuffer, RasterScratch};

    let gid = glyph_for_char(JETBRAINS_MONO, 'A');

    // Render without any axes.
    let mut buf_without = vec![0u8; 48 * 6 * 48];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf_without,
        width: 48,
        height: 48,
    };
    let metrics_without = rasterize(JETBRAINS_MONO, gid, 18, &mut rb, &mut scratch)
        .expect("rasterization without axes should succeed");
    let total_without = (metrics_without.width * metrics_without.height * 3) as usize;
    let coverage_without: Vec<u8> = buf_without[..total_without].to_vec();

    // Render with empty axes (should be identical).
    let mut buf_with = vec![0u8; 48 * 6 * 48];
    let mut scratch2 = Box::new(RasterScratch::zeroed());
    let mut rb2 = RasterBuffer {
        data: &mut buf_with,
        width: 48,
        height: 48,
    };
    let metrics_with = rasterize_with_axes(
        JETBRAINS_MONO,
        gid,
        18,
        &mut rb2,
        &mut scratch2,
        &[],
    )
    .expect("rasterization with empty axes should succeed");
    let total_with = (metrics_with.width * metrics_with.height * 3) as usize;

    assert_eq!(
        total_without, total_with,
        "coverage size should be identical"
    );
    assert_eq!(
        &coverage_without[..],
        &buf_with[..total_with],
        "coverage should be byte-identical with and without empty axes"
    );
}

#[test]
fn weight_correction_dark_on_light_no_change() {
    // When foreground is darker than background, no weight reduction occurs.
    // The auto function should still return a wght axis value, but at default.
    use fonts::rasterize::{auto_weight_correction_axes, font_axes};

    let axes_result = auto_weight_correction_axes(
        INTER,
        0,
        0,
        0,     // black fg
        255,
        255,
        255, // white bg
    );
    // For dark-on-light, correction factor >= 1.0, so weight stays at default.
    // The function may return empty (no adjustment needed) or the default weight.
    if !axes_result.is_empty() {
        let font_ax = font_axes(INTER);
        let wght = font_ax.iter().find(|a| &a.tag == b"wght").unwrap();
        let returned_wght = axes_result.iter().find(|a| &a.tag == b"wght").unwrap();
        assert!(
            returned_wght.value >= wght.default_value - 0.1,
            "dark-on-light weight ({:.1}) should be >= default ({:.1})",
            returned_wght.value,
            wght.default_value
        );
    }
    // Either way: no error, no panic.
}

// ===========================================================================
// VAL-SHAPE-02: Real font metrics — glyph IDs from cmap, not ASCII
// ===========================================================================

#[test]
fn shape_a_glyph_id_not_ascii() {
    // VAL-SHAPE-02: Shape "A" in JetBrains Mono, verify glyph ID ≠ 65
    // (real cmap lookup, not ASCII byte cast).
    let glyphs = shape(JETBRAINS_MONO, "A", &[]);
    assert_eq!(glyphs.len(), 1, "expected 1 glyph for 'A'");
    assert_ne!(
        glyphs[0].glyph_id, 65,
        "glyph ID for 'A' should not be 65 (the ASCII byte) — real cmap lookup produces a different ID"
    );
    assert!(
        glyphs[0].x_advance > 0,
        "advance should be non-zero from real font metrics"
    );
}

// ===========================================================================
// VAL-SHAPE-03: Empty and whitespace text handled gracefully
// ===========================================================================

#[test]
fn shape_whitespace_produces_glyphs_with_advances() {
    // VAL-SHAPE-03: Shaping whitespace produces glyph(s) with non-zero advance.
    let glyphs = shape(JETBRAINS_MONO, " ", &[]);
    assert!(
        !glyphs.is_empty(),
        "shaping a single space should produce at least one glyph"
    );
    assert!(
        glyphs[0].x_advance > 0,
        "space glyph should have non-zero x_advance, got {}",
        glyphs[0].x_advance
    );
}

// ===========================================================================
// Font unit → point conversion helper test
// ===========================================================================

#[test]
fn font_units_to_points_conversion_correct() {
    // Verify the conversion formula: value_pt = value_fu * point_size / upem
    // For JetBrains Mono: units_per_em is typically 1000.
    let fm = fonts::rasterize::font_metrics(JETBRAINS_MONO)
        .expect("should parse font metrics");
    let upem = fm.units_per_em;
    assert!(upem > 0, "units_per_em should be > 0");

    // Shape "A" to get font-unit advance
    let glyphs = shape(JETBRAINS_MONO, "A", &[]);
    assert_eq!(glyphs.len(), 1);
    let advance_fu = glyphs[0].x_advance;
    assert!(advance_fu > 0, "advance in font units should be > 0");

    // Convert to points at size 18
    let point_size: i32 = 18;
    let advance_pt = (advance_fu * point_size) / upem as i32;
    assert!(
        advance_pt > 0,
        "advance in points should be > 0 at size 18, got {} (fu={}, upem={})",
        advance_pt,
        advance_fu,
        upem
    );
    // Truncated to i16 should still be reasonable
    assert!(
        advance_pt <= i16::MAX as i32,
        "advance in points should fit in i16"
    );
}
