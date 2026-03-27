//! Tests for HVAR (Horizontal Metrics Variation) table support.
//!
//! Validates that variable font advance widths adjust correctly with
//! variation axes, using read-fonts' HVAR table parser.

use fonts::rasterize::{glyph_h_metrics, glyph_id_for_char, hvar, AxisValue};

const INTER: &[u8] = include_bytes!("../../share/inter.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../share/jetbrains-mono.ttf");

// ---------------------------------------------------------------------------
// HVAR table presence
// ---------------------------------------------------------------------------

#[test]
fn inter_has_hvar_table() {
    assert!(hvar::has_hvar(INTER));
}

// ---------------------------------------------------------------------------
// Advance width deltas
// ---------------------------------------------------------------------------

#[test]
fn bold_advance_wider_than_regular() {
    let gid = glyph_id_for_char(INTER, 'M').unwrap();
    let (default_advance, _) = glyph_h_metrics(INTER, gid).unwrap();
    let bold_axes = [AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];
    let bold_advance = hvar::advance_with_delta(INTER, gid, &bold_axes).unwrap();
    assert!(
        bold_advance > default_advance as i32,
        "bold advance ({}) should be greater than default ({})",
        bold_advance,
        default_advance
    );
}

#[test]
fn default_weight_has_zero_delta() {
    let gid = glyph_id_for_char(INTER, 'A').unwrap();
    let (default_advance, _) = glyph_h_metrics(INTER, gid).unwrap();
    let axes = [AxisValue {
        tag: *b"wght",
        value: 400.0,
    }];
    let adjusted = hvar::advance_with_delta(INTER, gid, &axes).unwrap();
    assert_eq!(adjusted, default_advance as i32);
}

// ---------------------------------------------------------------------------
// Graceful handling of fonts without HVAR
// ---------------------------------------------------------------------------

#[test]
fn monospace_font_graceful() {
    // JetBrains Mono may not have HVAR — should not crash.
    let gid = glyph_id_for_char(JETBRAINS_MONO, 'A').unwrap_or(0);
    let _ = hvar::advance_with_delta(JETBRAINS_MONO, gid, &[]);
}

// ---------------------------------------------------------------------------
// Wrapper function fallback
// ---------------------------------------------------------------------------

#[test]
fn wrapper_falls_back_for_no_axes() {
    let gid = glyph_id_for_char(INTER, 'A').unwrap();
    let (default_advance, _) = glyph_h_metrics(INTER, gid).unwrap();
    let result = fonts::rasterize::glyph_h_advance_with_axes(INTER, gid, &[]).unwrap();
    assert_eq!(result, default_advance as i32);
}

#[test]
fn wrapper_uses_hvar_for_bold() {
    let gid = glyph_id_for_char(INTER, 'M').unwrap();
    let (default_advance, _) = glyph_h_metrics(INTER, gid).unwrap();
    let bold_axes = [AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];
    let result = fonts::rasterize::glyph_h_advance_with_axes(INTER, gid, &bold_axes).unwrap();
    assert!(
        result > default_advance as i32,
        "wrapper bold advance ({}) should be greater than default ({})",
        result,
        default_advance
    );
}

#[test]
fn wrapper_returns_some_for_font_without_hvar() {
    // Even without HVAR, wrapper should return the default hmtx advance.
    let gid = glyph_id_for_char(JETBRAINS_MONO, 'A').unwrap_or(0);
    if gid > 0 {
        let result = fonts::rasterize::glyph_h_advance_with_axes(JETBRAINS_MONO, gid, &[]);
        assert!(result.is_some());
    }
}
