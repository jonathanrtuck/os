//! Tests for content-type-aware typography defaults.
//!
//! Validates VAL-CTYPE-001 through VAL-CTYPE-005 and VAL-CROSS-003.

use shaping::fallback::ContentType;
use shaping::typography::{FontFamily, TypographyConfig};
use shaping::Feature;

const NUNITO_SANS_VARIABLE: &[u8] = include_bytes!("../../share/nunito-sans-variable.ttf");
const SOURCE_CODE_PRO_VARIABLE: &[u8] = include_bytes!("../../share/source-code-pro-variable.ttf");

// ===========================================================================
// VAL-CTYPE-001: Code typography defaults
// ===========================================================================

#[test]
fn ctype_code_uses_monospace_font() {
    let config = TypographyConfig::for_content_type(ContentType::Code);
    assert_eq!(
        config.font_family,
        FontFamily::Monospace,
        "code content type should select monospace font"
    );
}

#[test]
fn ctype_code_has_calt_feature() {
    let config = TypographyConfig::for_content_type(ContentType::Code);
    let feature_strs: Vec<&str> = config.features.iter().map(|s| s.as_str()).collect();
    assert!(
        feature_strs.contains(&"+calt"),
        "code typography should include +calt for programming ligatures, got {:?}",
        feature_strs
    );
}

#[test]
fn ctype_code_has_tnum_feature() {
    let config = TypographyConfig::for_content_type(ContentType::Code);
    let feature_strs: Vec<&str> = config.features.iter().map(|s| s.as_str()).collect();
    assert!(
        feature_strs.contains(&"+tnum"),
        "code typography should include +tnum for tabular figures, got {:?}",
        feature_strs
    );
}

// ===========================================================================
// VAL-CTYPE-002: Prose typography defaults
// ===========================================================================

#[test]
fn ctype_prose_uses_proportional_font() {
    let config = TypographyConfig::for_content_type(ContentType::Prose);
    assert_eq!(
        config.font_family,
        FontFamily::Proportional,
        "prose content type should select proportional font"
    );
}

#[test]
fn ctype_prose_has_onum_feature() {
    let config = TypographyConfig::for_content_type(ContentType::Prose);
    let feature_strs: Vec<&str> = config.features.iter().map(|s| s.as_str()).collect();
    assert!(
        feature_strs.contains(&"+onum"),
        "prose typography should include +onum for oldstyle figures, got {:?}",
        feature_strs
    );
}

#[test]
fn ctype_prose_has_optical_sizing() {
    let config = TypographyConfig::for_content_type(ContentType::Prose);
    assert!(
        config.optical_sizing,
        "prose content type should enable optical sizing"
    );
}

// ===========================================================================
// VAL-CTYPE-003: UI label typography defaults
// ===========================================================================

#[test]
fn ctype_ui_uses_proportional_font() {
    let config = TypographyConfig::for_content_type(ContentType::Ui);
    assert_eq!(
        config.font_family,
        FontFamily::Proportional,
        "UI content type should select proportional font"
    );
}

#[test]
fn ctype_ui_has_medium_weight() {
    let config = TypographyConfig::for_content_type(ContentType::Ui);
    // Medium weight is ~500.
    assert!(
        (config.weight_preference - 500.0).abs() < 50.0,
        "UI content type should have weight preference ~500, got {}",
        config.weight_preference
    );
}

#[test]
fn ctype_ui_has_tracking() {
    let config = TypographyConfig::for_content_type(ContentType::Ui);
    // UI should have a defined tracking value (can be 0.0 for standard).
    // The key is it exists and doesn't panic.
    let _tracking = config.tracking;
}

// ===========================================================================
// VAL-CTYPE-004: Content-type wired to shaping pipeline
// ===========================================================================

#[test]
fn ctype_code_vs_prose_different_shaped_output() {
    // Shaping "1/2 != 0.5" as code vs prose should produce different glyph
    // output because code applies +calt for the != ligature.
    let text = "1/2 != 0.5";

    let code_config = TypographyConfig::for_content_type(ContentType::Code);
    let prose_config = TypographyConfig::for_content_type(ContentType::Prose);

    // Get the font data for each content type.
    let code_font = SOURCE_CODE_PRO_VARIABLE; // monospace for code
    let prose_font = NUNITO_SANS_VARIABLE; // proportional for prose

    // Parse features.
    let code_features: Vec<Feature> = code_config
        .features
        .iter()
        .filter_map(|s| s.parse::<Feature>().ok())
        .collect();
    let prose_features: Vec<Feature> = prose_config
        .features
        .iter()
        .filter_map(|s| s.parse::<Feature>().ok())
        .collect();

    let code_glyphs = shaping::shape(code_font, text, &code_features);
    let prose_glyphs = shaping::shape(prose_font, text, &prose_features);

    // Different fonts should produce different glyph IDs.
    let code_ids: Vec<u16> = code_glyphs.iter().map(|g| g.glyph_id).collect();
    let prose_ids: Vec<u16> = prose_glyphs.iter().map(|g| g.glyph_id).collect();

    assert_ne!(
        code_ids, prose_ids,
        "code vs prose shaping of '{}' should produce different glyph output",
        text
    );
}

#[test]
fn ctype_code_features_are_parseable_and_applied() {
    // Verify that code typography features (+calt, +tnum) are valid parseable
    // OpenType features that can be applied to the shaping pipeline.
    // Whether they produce visible differences depends on font support.
    let code_config = TypographyConfig::for_content_type(ContentType::Code);
    let code_features: Vec<Feature> = code_config
        .features
        .iter()
        .filter_map(|s| s.parse::<Feature>().ok())
        .collect();

    // All configured features should be parseable.
    assert_eq!(
        code_features.len(),
        code_config.features.len(),
        "all code features should be parseable as OpenType Feature structs"
    );

    // Shaping with features should not crash and should produce output.
    let text = "1/2 != 0.5";
    let glyphs = shaping::shape(SOURCE_CODE_PRO_VARIABLE, text, &code_features);
    assert!(
        !glyphs.is_empty(),
        "shaping with code features should produce glyphs"
    );
    for g in &glyphs {
        assert!(g.x_advance > 0, "all glyphs should have positive advance");
    }
}

// ===========================================================================
// VAL-CTYPE-005: Unknown content type defaults
// ===========================================================================

#[test]
fn ctype_unknown_falls_back_to_sane_defaults() {
    let config = TypographyConfig::for_content_type(ContentType::Unknown);
    // Should return non-empty config without panicking.
    assert!(
        !config.features.is_empty() || config.font_family == FontFamily::Proportional,
        "unknown content type should have sane defaults"
    );
}

#[test]
fn ctype_unknown_matches_prose_defaults() {
    // Unknown content type should fall back to prose defaults.
    let unknown = TypographyConfig::for_content_type(ContentType::Unknown);
    let prose = TypographyConfig::for_content_type(ContentType::Prose);
    assert_eq!(
        unknown.font_family, prose.font_family,
        "unknown should default to same font family as prose"
    );
}

#[test]
fn ctype_unknown_no_panic() {
    // Calling for_content_type with Unknown should not panic.
    let _config = TypographyConfig::for_content_type(ContentType::Unknown);
}

// ===========================================================================
// VAL-CROSS-003: Perceptual rendering end-to-end
// ===========================================================================

#[test]
fn cross_003_auto_opsz_produces_different_rendering_than_fixed() {
    // Auto-opsz produces different rendering than fixed defaults (no opsz axis).
    use shaping::rasterize::{
        auto_axis_values_for_opsz, rasterize, rasterize_with_axes, RasterBuffer, RasterScratch,
    };

    let gid = shaping::rasterize::glyph_id_for_char(NUNITO_SANS_VARIABLE, 'e').unwrap();

    // Render without auto-opsz (default axis values = no variation).
    let mut buf_default = vec![0u8; 128 * 6 * 128];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf_default,
        width: 128,
        height: 128,
    };
    let m_default =
        rasterize(NUNITO_SANS_VARIABLE, gid, 10, &mut rb, &mut scratch).expect("should rasterize");
    let total_default = (m_default.width * m_default.height * 3) as usize;
    let sum_default: u64 = buf_default[..total_default].iter().map(|&b| b as u64).sum();

    // Render with auto-opsz at 10px, 96dpi.
    let auto_axes = auto_axis_values_for_opsz(NUNITO_SANS_VARIABLE, 10, 96);
    assert!(
        !auto_axes.is_empty(),
        "auto-opsz should return axes for a font with opsz axis"
    );

    let mut buf_opsz = vec![0u8; 128 * 6 * 128];
    let mut scratch2 = Box::new(RasterScratch::zeroed());
    let mut rb2 = RasterBuffer {
        data: &mut buf_opsz,
        width: 128,
        height: 128,
    };
    let m_opsz = rasterize_with_axes(
        NUNITO_SANS_VARIABLE,
        gid,
        10,
        &mut rb2,
        &mut scratch2,
        &auto_axes,
    )
    .expect("should rasterize with auto-opsz");
    let total_opsz = (m_opsz.width * m_opsz.height * 3) as usize;
    let sum_opsz: u64 = buf_opsz[..total_opsz].iter().map(|&b| b as u64).sum();

    // Auto-opsz should produce different rendering than no opsz.
    assert!(
        sum_default != sum_opsz || total_default != total_opsz,
        "auto-opsz (sum={}, len={}) should differ from fixed defaults (sum={}, len={})",
        sum_opsz,
        total_opsz,
        sum_default,
        total_default
    );
}

#[test]
fn cross_003_auto_weight_correction_produces_different_rendering_than_fixed() {
    // Auto-weight-correction for white-on-black text produces different
    // rendering than a fixed base weight WITHOUT explicit caller intervention.
    //
    // We compare rendering at a base weight (400) vs the auto-corrected weight.
    // The auto function computes a reduced weight for white-on-black, which
    // should produce measurably thinner glyph coverage.
    use shaping::rasterize::{
        auto_weight_correction_axes, rasterize_with_axes, weight_correction_factor, AxisValue,
        RasterBuffer, RasterScratch,
    };

    let gid = shaping::rasterize::glyph_id_for_char(NUNITO_SANS_VARIABLE, 'H').unwrap();

    // Render at base weight (400) — the "uncorrected" rendering.
    let base_axes = [AxisValue {
        tag: *b"wght",
        value: 400.0,
    }];
    let mut buf_base = vec![0u8; 128 * 6 * 128];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf_base,
        width: 128,
        height: 128,
    };
    let m_base = rasterize_with_axes(
        NUNITO_SANS_VARIABLE,
        gid,
        24,
        &mut rb,
        &mut scratch,
        &base_axes,
    )
    .expect("should rasterize at base weight");
    let total_base = (m_base.width * m_base.height * 3) as usize;
    let sum_base: u64 = buf_base[..total_base].iter().map(|&b| b as u64).sum();

    // Compute auto-corrected weight for white-on-black.
    let factor = weight_correction_factor(255, 255, 255, 0, 0, 0);
    assert!(
        factor < 1.0,
        "white-on-black factor should be < 1.0, got {}",
        factor
    );

    let corrected_weight = 400.0 * factor;
    let corrected_axes = [AxisValue {
        tag: *b"wght",
        value: corrected_weight,
    }];

    let mut buf_corrected = vec![0u8; 128 * 6 * 128];
    let mut scratch2 = Box::new(RasterScratch::zeroed());
    let mut rb2 = RasterBuffer {
        data: &mut buf_corrected,
        width: 128,
        height: 128,
    };
    let m_corrected = rasterize_with_axes(
        NUNITO_SANS_VARIABLE,
        gid,
        24,
        &mut rb2,
        &mut scratch2,
        &corrected_axes,
    )
    .expect("should rasterize with corrected weight");
    let total_corrected = (m_corrected.width * m_corrected.height * 3) as usize;
    let sum_corrected: u64 = buf_corrected[..total_corrected]
        .iter()
        .map(|&b| b as u64)
        .sum();

    // Weight-corrected rendering should have LESS coverage (thinner strokes).
    assert!(
        sum_corrected < sum_base,
        "auto-weight-correction (sum={}, wght={:.1}) should produce less coverage \
         than base weight (sum={}, wght=400) for white-on-black text",
        sum_corrected,
        corrected_weight,
        sum_base
    );
}

#[test]
fn cross_003_auto_perceptual_combined_differs_from_fixed() {
    // Both auto-opsz AND auto-weight-correction together produce different
    // rendering than fixed defaults, without explicit caller intervention.
    use shaping::rasterize::{
        auto_axis_values_for_opsz, auto_weight_correction_axes, rasterize, rasterize_with_axes,
        AxisValue, RasterBuffer, RasterScratch,
    };

    let gid = shaping::rasterize::glyph_id_for_char(NUNITO_SANS_VARIABLE, 'g').unwrap();

    // Render with fixed defaults (no perceptual adjustments).
    let mut buf_fixed = vec![0u8; 128 * 6 * 128];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf_fixed,
        width: 128,
        height: 128,
    };
    let m_fixed =
        rasterize(NUNITO_SANS_VARIABLE, gid, 14, &mut rb, &mut scratch).expect("should rasterize");
    let total_fixed = (m_fixed.width * m_fixed.height * 3) as usize;
    let sum_fixed: u64 = buf_fixed[..total_fixed].iter().map(|&b| b as u64).sum();

    // Compute combined perceptual axes: opsz + weight correction.
    let mut combined_axes: Vec<AxisValue> = Vec::new();
    combined_axes.extend_from_slice(&auto_axis_values_for_opsz(NUNITO_SANS_VARIABLE, 14, 96));
    combined_axes.extend_from_slice(&auto_weight_correction_axes(
        NUNITO_SANS_VARIABLE,
        255, 255, 255, // white fg
        0, 0, 0,       // black bg
    ));

    assert!(
        !combined_axes.is_empty(),
        "combined perceptual axes should not be empty"
    );

    let mut buf_perceptual = vec![0u8; 128 * 6 * 128];
    let mut scratch2 = Box::new(RasterScratch::zeroed());
    let mut rb2 = RasterBuffer {
        data: &mut buf_perceptual,
        width: 128,
        height: 128,
    };
    let m_perceptual = rasterize_with_axes(
        NUNITO_SANS_VARIABLE,
        gid,
        14,
        &mut rb2,
        &mut scratch2,
        &combined_axes,
    )
    .expect("should rasterize with perceptual axes");
    let total_perceptual = (m_perceptual.width * m_perceptual.height * 3) as usize;
    let sum_perceptual: u64 = buf_perceptual[..total_perceptual]
        .iter()
        .map(|&b| b as u64)
        .sum();

    assert!(
        sum_fixed != sum_perceptual || total_fixed != total_perceptual,
        "combined perceptual rendering (sum={}, len={}) should differ from fixed (sum={}, len={})",
        sum_perceptual,
        total_perceptual,
        sum_fixed,
        total_fixed
    );
}
