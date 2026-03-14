//! Text shaping library wrapping HarfRust for OpenType shaping.
//!
//! Provides a simple API: font data + text + features → shaped glyph array.
//! All position values are in **font units** (units-per-em). Callers scale to
//! pixels via `pixel = font_unit * desired_px / units_per_em`.
//!
//! # no_std
//!
//! This library is `#![no_std]` with `extern crate alloc`. It compiles for
//! both the bare-metal target (`aarch64-unknown-none`) and the host target
//! (for testing).

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub use harfrust::Feature;

pub mod rasterize;

/// A single shaped glyph with positioning information.
///
/// All advance and offset values are in font units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ShapedGlyph {
    /// Glyph ID in the font (0 = .notdef).
    pub glyph_id: u16,
    /// Horizontal advance in font units.
    pub x_advance: i32,
    /// Vertical advance in font units.
    pub y_advance: i32,
    /// Horizontal offset from default position in font units.
    pub x_offset: i32,
    /// Vertical offset from default position in font units.
    pub y_offset: i32,
    /// Original character cluster index (maps back to input text).
    pub cluster: u32,
}

/// Shape a string of text using a font, producing positioned glyphs.
///
/// # Arguments
///
/// * `font_data` — raw font file bytes (TrueType/OpenType)
/// * `text` — input text to shape
/// * `features` — OpenType feature settings (e.g. `Feature::from_str("+liga")`)
///
/// # Returns
///
/// A `Vec<ShapedGlyph>` with one entry per output glyph. Returns an empty
/// vec for empty input or if the font cannot be parsed.
pub fn shape(font_data: &[u8], text: &str, features: &[Feature]) -> Vec<ShapedGlyph> {
    if text.is_empty() {
        return Vec::new();
    }

    let font = match harfrust::FontRef::from_index(font_data, 0) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let data = harfrust::ShaperData::new(&font);
    let mut buffer = harfrust::UnicodeBuffer::new();

    buffer.push_str(text);
    buffer.guess_segment_properties();

    let shaper = data.shaper(&font).build();
    let glyph_buffer = shaper.shape(buffer, features);
    let infos = glyph_buffer.glyph_infos();
    let positions = glyph_buffer.glyph_positions();

    infos
        .iter()
        .zip(positions.iter())
        .map(|(info, pos)| ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            x_advance: pos.x_advance,
            y_advance: pos.y_advance,
            x_offset: pos.x_offset,
            y_offset: pos.y_offset,
            cluster: info.cluster,
        })
        .collect()
}
