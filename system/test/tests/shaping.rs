//! Tests for the shaping library (HarfRust integration).
//!
//! Validates that text shaping works correctly: glyph production, ligatures,
//! kerning, and OpenType feature control.

use shaping::{shape, Feature, ShapedGlyph};

const NUNITO_SANS: &[u8] = include_bytes!("../../share/nunito-sans.ttf");
const NUNITO_SANS_VARIABLE: &[u8] = include_bytes!("../../share/nunito-sans-variable.ttf");
const SOURCE_CODE_PRO: &[u8] = include_bytes!("../../share/source-code-pro.ttf");

// ---------------------------------------------------------------------------
// VAL-SHAPE-001: Basic Latin text shaping
// ---------------------------------------------------------------------------

#[test]
fn shape_hello_world_glyph_count() {
    // "Hello World" is 11 characters (including space). For basic Latin text
    // with no ligatures, each character should produce one glyph.
    let glyphs = shape(NUNITO_SANS, "Hello World", &[]);
    assert!(
        !glyphs.is_empty(),
        "shaping should produce glyphs for non-empty text"
    );
    // Nunito Sans static may or may not have ligatures for this text.
    // At minimum we expect at least 10 glyphs for 11 characters.
    assert!(
        glyphs.len() >= 10,
        "expected at least 10 glyphs for 'Hello World', got {}",
        glyphs.len()
    );
}

#[test]
fn shape_hello_world_nonzero_advances() {
    let glyphs = shape(NUNITO_SANS, "Hello World", &[]);
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
    let glyphs = shape(NUNITO_SANS, "", &[]);
    assert!(
        glyphs.is_empty(),
        "empty string should produce 0 glyphs, got {}",
        glyphs.len()
    );
}

// ---------------------------------------------------------------------------
// VAL-SHAPE-002: Ligature production
// ---------------------------------------------------------------------------
// Font: Variable Nunito Sans (nunito-sans-variable.ttf) — a variable OpenType
// font with GSUB ligature tables, including standard "fi" and "fl" ligatures.

#[test]
fn shape_ligature_fi_fewer_glyphs() {
    // "fi" (2 chars) should produce fewer than 2 glyphs when the font's GSUB
    // ligature tables are active (default shaping enables standard ligatures).
    let liga_on = vec!["+liga".parse::<Feature>().unwrap()];
    let glyphs = shape(NUNITO_SANS_VARIABLE, "fi", &liga_on);
    assert!(
        glyphs.len() < 2,
        "Variable Nunito Sans: 'fi' with +liga should produce fewer than 2 glyphs \
         (ligature substitution), got {} glyphs",
        glyphs.len()
    );
}

#[test]
fn shape_ligature_fl_fewer_glyphs() {
    // "fl" (2 chars) should also produce fewer than 2 glyphs via GSUB ligature.
    let liga_on = vec!["+liga".parse::<Feature>().unwrap()];
    let glyphs = shape(NUNITO_SANS_VARIABLE, "fl", &liga_on);
    assert!(
        glyphs.len() < 2,
        "Variable Nunito Sans: 'fl' with +liga should produce fewer than 2 glyphs \
         (ligature substitution), got {} glyphs",
        glyphs.len()
    );
}

#[test]
fn shape_ligature_disabled_no_merge() {
    // With ligatures explicitly disabled, "fi" should produce exactly 2 glyphs.
    let liga_off = vec!["-liga".parse::<Feature>().unwrap()];
    let glyphs = shape(NUNITO_SANS_VARIABLE, "fi", &liga_off);
    assert_eq!(
        glyphs.len(),
        2,
        "Variable Nunito Sans: 'fi' with -liga should produce 2 glyphs (no ligature), got {}",
        glyphs.len()
    );
}

// ---------------------------------------------------------------------------
// VAL-SHAPE-003: Kerning application
// ---------------------------------------------------------------------------

#[test]
fn shape_kerning_av_tighter_than_sum() {
    // "AV" is a classic kerning pair. With Nunito Sans, the total advance
    // when shaped together should be less than the sum of individual advances
    // (kerning pulls them closer together).
    let av_glyphs = shape(NUNITO_SANS, "AV", &[]);
    assert_eq!(av_glyphs.len(), 2, "AV should produce 2 glyphs");

    let total_advance: i32 = av_glyphs.iter().map(|g| g.x_advance).sum();

    let a_glyphs = shape(NUNITO_SANS, "A", &[]);
    let v_glyphs = shape(NUNITO_SANS, "V", &[]);
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
fn shape_feature_liga_on_off_differs() {
    // When ligatures are enabled vs disabled, text containing ligature-eligible
    // sequences should produce different glyph output.
    let liga_on = vec!["+liga".parse::<Feature>().unwrap()];
    let liga_off = vec!["-liga".parse::<Feature>().unwrap()];

    let text = "difficult office";
    let glyphs_on = shape(NUNITO_SANS, text, &liga_on);
    let glyphs_off = shape(NUNITO_SANS, text, &liga_off);

    // With ligatures disabled, every character maps to one glyph.
    // With ligatures enabled, fi/ffi ligatures may reduce glyph count.
    // At minimum, the glyph ID arrays should differ.
    let ids_on: Vec<u16> = glyphs_on.iter().map(|g| g.glyph_id).collect();
    let ids_off: Vec<u16> = glyphs_off.iter().map(|g| g.glyph_id).collect();

    assert_ne!(
        ids_on, ids_off,
        "liga on vs off should produce different glyph IDs for '{}'",
        text
    );
}

// ---------------------------------------------------------------------------
// VAL-E2E-001: Proportional shaping confirms different widths
// ---------------------------------------------------------------------------

#[test]
fn shape_proportional_w_vs_i() {
    // Proportional font should give different advance widths for 'W' and 'i'.
    let w_glyphs = shape(NUNITO_SANS, "W", &[]);
    let i_glyphs = shape(NUNITO_SANS, "i", &[]);
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
    // Source Code Pro is monospace — all Latin glyphs should have the same advance.
    let glyphs = shape(SOURCE_CODE_PRO, "iiiWWW", &[]);
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
fn shape_source_code_pro_basic() {
    let glyphs = shape(SOURCE_CODE_PRO, "Hello", &[]);
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
// Variable font files and constants
// ---------------------------------------------------------------------------

const SOURCE_CODE_PRO_VARIABLE: &[u8] =
    include_bytes!("../../share/source-code-pro-variable.ttf");

// ---------------------------------------------------------------------------
// VAL-VARFONT-001: Variable Nunito Sans axis detection
// ---------------------------------------------------------------------------

#[test]
fn varfont_nunito_sans_has_four_axes() {
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
    assert_eq!(
        axes.len(),
        4,
        "variable Nunito Sans should have 4 axes, got {}",
        axes.len()
    );
}

#[test]
fn varfont_nunito_sans_opsz_axis() {
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
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
fn varfont_nunito_sans_wght_axis() {
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
    let wght = axes.iter().find(|a| &a.tag == b"wght");
    assert!(wght.is_some(), "should have wght axis");
    let wght = wght.unwrap();
    assert!(
        wght.min_value < wght.max_value,
        "wght min ({}) must be < max ({})",
        wght.min_value,
        wght.max_value
    );
    // Typical weight range: 100–900 or similar.
    assert!(
        wght.min_value >= 100.0 && wght.max_value <= 1000.0,
        "wght range [{}, {}] seems unreasonable",
        wght.min_value,
        wght.max_value
    );
}

#[test]
fn varfont_nunito_sans_wdth_axis() {
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
    let wdth = axes.iter().find(|a| &a.tag == b"wdth");
    assert!(wdth.is_some(), "should have wdth axis");
    let wdth = wdth.unwrap();
    assert!(
        wdth.min_value < wdth.max_value,
        "wdth min ({}) must be < max ({})",
        wdth.min_value,
        wdth.max_value
    );
}

#[test]
fn varfont_nunito_sans_ytlc_axis() {
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
    let ytlc = axes.iter().find(|a| &a.tag == b"YTLC");
    assert!(ytlc.is_some(), "should have YTLC axis");
    let ytlc = ytlc.unwrap();
    assert!(
        ytlc.min_value < ytlc.max_value,
        "YTLC min ({}) must be < max ({})",
        ytlc.min_value,
        ytlc.max_value
    );
}

#[test]
fn varfont_nunito_sans_axis_tags_exact() {
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
    let tags: Vec<[u8; 4]> = axes.iter().map(|a| a.tag).collect();
    assert!(
        tags.contains(b"opsz"),
        "missing opsz axis in {:?}",
        tags
    );
    assert!(
        tags.contains(b"wght"),
        "missing wght axis in {:?}",
        tags
    );
    assert!(
        tags.contains(b"wdth"),
        "missing wdth axis in {:?}",
        tags
    );
    assert!(
        tags.contains(b"YTLC"),
        "missing YTLC axis in {:?}",
        tags
    );
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-004: Variable Source Code Pro axis detection
// ---------------------------------------------------------------------------

#[test]
fn varfont_source_code_pro_has_wght_axis() {
    let axes = shaping::rasterize::font_axes(SOURCE_CODE_PRO_VARIABLE);
    assert!(
        !axes.is_empty(),
        "variable Source Code Pro should have at least one axis"
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
fn varfont_source_code_pro_wght_range_valid() {
    let axes = shaping::rasterize::font_axes(SOURCE_CODE_PRO_VARIABLE);
    let wght = axes.iter().find(|a| &a.tag == b"wght").unwrap();
    // Source Code Pro variable has weights from ~200 to ~900.
    assert!(
        wght.min_value >= 100.0,
        "wght min {} too low",
        wght.min_value
    );
    assert!(
        wght.max_value <= 1000.0,
        "wght max {} too high",
        wght.max_value
    );
}

// ---------------------------------------------------------------------------
// Non-variable fonts return no axes
// ---------------------------------------------------------------------------

#[test]
fn varfont_static_font_returns_no_axes() {
    let axes = shaping::rasterize::font_axes(SOURCE_CODE_PRO);
    assert!(
        axes.is_empty(),
        "static Source Code Pro should have 0 axes, got {}",
        axes.len()
    );
}

#[test]
fn varfont_empty_data_returns_no_axes() {
    let axes = shaping::rasterize::font_axes(&[]);
    assert!(axes.is_empty(), "empty data should have 0 axes");
}

// ---------------------------------------------------------------------------
// Variable fonts parse via shaping library (basic shaping works)
// ---------------------------------------------------------------------------

#[test]
fn varfont_nunito_sans_variable_shapes_text() {
    let glyphs = shape(NUNITO_SANS_VARIABLE, "Hello", &[]);
    assert!(
        glyphs.len() >= 5,
        "variable Nunito Sans should shape 'Hello' to >= 5 glyphs, got {}",
        glyphs.len()
    );
    for g in &glyphs {
        assert!(g.x_advance > 0, "all advances should be > 0");
    }
}

#[test]
fn varfont_source_code_pro_variable_shapes_text() {
    let glyphs = shape(SOURCE_CODE_PRO_VARIABLE, "Hello", &[]);
    assert!(
        glyphs.len() >= 5,
        "variable Source Code Pro should shape 'Hello' to >= 5 glyphs, got {}",
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
    use shaping::rasterize::{AxisValue, RasterBuffer, RasterScratch};

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
    let metrics = shaping::rasterize::rasterize_with_axes(
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
    shaping::rasterize::glyph_id_for_char(font_data, ch)
        .expect("should find glyph for character")
}

#[test]
fn varfont_wght_400_vs_700_different_coverage() {
    // VAL-VARFONT-002: wght=400 vs wght=700 produces measurably different coverage.
    let gid = glyph_for_char(NUNITO_SANS_VARIABLE, 'H');
    let cov_400 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 400.0);
    let cov_700 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 700.0);

    assert_ne!(
        cov_400, cov_700,
        "coverage at wght=400 ({}) must differ from wght=700 ({})",
        cov_400, cov_700
    );
}

#[test]
fn varfont_wght_700_heavier_than_400() {
    // VAL-CROSS-002: Coverage sum at wght=700 > coverage sum at wght=400.
    let gid = glyph_for_char(NUNITO_SANS_VARIABLE, 'H');
    let cov_400 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 400.0);
    let cov_700 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 700.0);

    assert!(
        cov_700 > cov_400,
        "wght=700 coverage ({}) must be > wght=400 coverage ({})",
        cov_700, cov_400
    );
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-003: Interpolation at intermediate axis values
// ---------------------------------------------------------------------------

#[test]
fn varfont_wght_550_differs_from_400_and_700() {
    // VAL-VARFONT-003: wght=550 differs from both 400 and 700 by >5% of pixels.
    let gid = glyph_for_char(NUNITO_SANS_VARIABLE, 'H');
    let cov_400 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 400.0);
    let cov_550 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 550.0);
    let cov_700 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 24, 700.0);

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
        cov_550, cov_400, pct_400
    );
    assert!(
        pct_700 > 5.0,
        "wght=550 coverage ({}) must differ from wght=700 ({}) by >5%, got {:.1}%",
        cov_550, cov_700, pct_700
    );
}

// ---------------------------------------------------------------------------
// VAL-VARFONT-002: Out-of-range axis value clamped without panic
// ---------------------------------------------------------------------------

#[test]
fn varfont_out_of_range_wght_clamped_no_panic() {
    // Out-of-range axis value (wght=2000) is clamped to font's max without panic.
    let gid = glyph_for_char(NUNITO_SANS_VARIABLE, 'A');
    // Should not panic — just clamp to max.
    let cov = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 18, 2000.0);
    assert!(cov > 0, "clamped out-of-range weight should still produce coverage");

    // Also test underflow (wght=0, below min).
    let cov_low = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 18, 0.0);
    assert!(
        cov_low > 0,
        "clamped underflow weight should still produce coverage"
    );
}

#[test]
fn varfont_wght_2000_equals_max() {
    // Out-of-range wght=2000 should produce same result as max weight.
    let gid = glyph_for_char(NUNITO_SANS_VARIABLE, 'H');
    let axes = shaping::rasterize::font_axes(NUNITO_SANS_VARIABLE);
    let wght = axes.iter().find(|a| &a.tag == b"wght").unwrap();
    let cov_2000 = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 18, 2000.0);
    let cov_max = rasterize_at_weight(NUNITO_SANS_VARIABLE, gid, 18, wght.max_value);
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
    use drawing::{LruCachedGlyph, LruGlyphCache};

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
    let axes_400: &[shaping::rasterize::AxisValue] = &[shaping::rasterize::AxisValue {
        tag: *b"wght",
        value: 400.0,
    }];
    let axes_700: &[shaping::rasterize::AxisValue] = &[shaping::rasterize::AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];

    let hash_400 = drawing::axis_values_hash(axes_400);
    let hash_700 = drawing::axis_values_hash(axes_700);

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
    use drawing::{LruCachedGlyph, LruGlyphCache};

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
    let axes_700: &[shaping::rasterize::AxisValue] = &[shaping::rasterize::AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];
    let hash_700 = drawing::axis_values_hash(axes_700);
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
    use shaping::rasterize::AxisValue;

    let axes = [AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];
    let glyphs = shaping::shape_with_variations(NUNITO_SANS_VARIABLE, "Hello", &[], &axes);
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
    let glyphs_default = shape(NUNITO_SANS_VARIABLE, "Hello", &[]);
    let glyphs_empty = shaping::shape_with_variations(NUNITO_SANS_VARIABLE, "Hello", &[], &[]);

    assert_eq!(
        glyphs_default.len(),
        glyphs_empty.len(),
        "empty variations should match default shaping"
    );
    for (a, b) in glyphs_default.iter().zip(glyphs_empty.iter()) {
        assert_eq!(a.glyph_id, b.glyph_id);
    }
}
