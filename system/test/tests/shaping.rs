//! Tests for the shaping library (HarfRust integration).
//!
//! Validates that text shaping works correctly: glyph production, ligatures,
//! kerning, and OpenType feature control.

use shaping::{shape, Feature, ShapedGlyph};

const NUNITO_SANS: &[u8] = include_bytes!("../../share/nunito-sans.ttf");
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
        assert!(g.x_advance > 0, "monospace glyph should have positive advance");
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
    assert!(
        glyphs.is_empty(),
        "empty font data should produce 0 glyphs"
    );
}
