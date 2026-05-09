//! HVAR (Horizontal Metrics Variation) table support.
//!
//! Provides variation-aware advance widths using the `read-fonts` crate's
//! HVAR parser. Bold Inter at weight 700 has wider glyphs than regular
//! weight 400 — this module computes the correct advance for any axis
//! position by reading the HVAR delta.

use read_fonts::{FontRef, TableProvider};

use super::gvar::build_normalized_coords;
use crate::metrics::{AxisValue, glyph_h_metrics};

/// Returns `true` if the font contains an HVAR table.
pub fn has_hvar(font_data: &[u8]) -> bool {
    let font = match FontRef::new(font_data) {
        Ok(f) => f,
        Err(_) => return false,
    };

    font.hvar().is_ok()
}

/// Returns the default hmtx advance plus the HVAR delta for the given axes.
///
/// Returns `None` if the font cannot be parsed or the glyph ID is invalid.
/// If the font has no HVAR table or the axes are empty, returns the plain
/// hmtx advance with no delta applied.
pub fn advance_with_delta(font_data: &[u8], glyph_id: u16, axes: &[AxisValue]) -> Option<i32> {
    let (default_advance, _lsb) = glyph_h_metrics(font_data, glyph_id)?;
    let default_i32 = default_advance as i32;

    // No axes specified — return the default advance.
    if axes.is_empty() {
        return Some(default_i32);
    }

    let font = FontRef::new(font_data).ok()?;
    let hvar = match font.hvar() {
        Ok(h) => h,
        // No HVAR table — return default advance without delta.
        Err(_) => return Some(default_i32),
    };

    let coords = build_normalized_coords(font_data, axes);

    if coords.is_empty() || coords.iter().all(|c| c.to_f32().abs() < f32::EPSILON) {
        return Some(default_i32);
    }

    let gid = read_fonts::types::GlyphId::new(glyph_id as u32);
    let delta = match hvar.advance_width_delta(gid, &coords) {
        Ok(d) => d.to_i32(),
        // Delta lookup failed — return default advance.
        Err(_) => 0,
    };

    Some((default_i32 + delta).max(0))
}
