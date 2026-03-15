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

pub mod fallback;
pub mod rasterize;
pub mod typography;

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

/// Shape text with variable font axis settings.
///
/// Like `shape()`, but also applies axis variations (e.g., wght=700) via
/// HarfRust's `Variation`/`ShaperInstance` API. Axis values are specified
/// as `AxisValue` structs from the rasterize module.
///
/// # Arguments
///
/// * `font_data` — raw font file bytes (TrueType/OpenType variable font)
/// * `text` — input text to shape
/// * `features` — OpenType feature settings
/// * `axis_values` — variable font axis settings (e.g., wght=700)
///
/// Returns an empty vec for empty input, unparseable fonts, or if axis
/// values are empty (delegates to `shape()` for efficiency).
pub fn shape_with_variations(
    font_data: &[u8],
    text: &str,
    features: &[Feature],
    axis_values: &[rasterize::AxisValue],
) -> Vec<ShapedGlyph> {
    if text.is_empty() {
        return Vec::new();
    }
    if axis_values.is_empty() {
        return shape(font_data, text, features);
    }

    let font = match harfrust::FontRef::from_index(font_data, 0) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    // Build variation strings for HarfRust.
    let variations: Vec<harfrust::Variation> = axis_values
        .iter()
        .filter_map(|av| {
            // Format as "tag=value" string and parse.
            let tag_str = core::str::from_utf8(&av.tag).ok()?;
            let mut buf = [0u8; 32];
            let s = format_axis_setting(&mut buf, tag_str, av.value)?;
            s.parse::<harfrust::Variation>().ok()
        })
        .collect();

    let data = harfrust::ShaperData::new(&font);
    let instance = harfrust::ShaperInstance::from_variations(&font, &variations);
    let mut buffer = harfrust::UnicodeBuffer::new();

    buffer.push_str(text);
    buffer.guess_segment_properties();

    let shaper = data.shaper(&font).instance(Some(&instance)).build();
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

/// Format an axis setting as "tag=value" into a stack buffer.
/// Returns `Some(&str)` on success, `None` if the buffer is too small.
fn format_axis_setting<'a>(buf: &'a mut [u8], tag: &str, value: f32) -> Option<&'a str> {
    use core::fmt::Write;

    struct BufWriter<'b> {
        buf: &'b mut [u8],
        pos: usize,
    }

    impl<'b> Write for BufWriter<'b> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            if self.pos + bytes.len() > self.buf.len() {
                return Err(core::fmt::Error);
            }
            self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
            self.pos += bytes.len();
            Ok(())
        }
    }

    let mut w = BufWriter { buf, pos: 0 };
    // Format as integer if value is a whole number, otherwise as decimal.
    // Check if value is a whole number without using f32::floor (not available in no_std).
    let truncated = value as i32;
    if value == truncated as f32 {
        write!(w, "{}={}", tag, truncated).ok()?;
    } else {
        write!(w, "{}={}", tag, value).ok()?;
    }
    core::str::from_utf8(&w.buf[..w.pos]).ok()
}
