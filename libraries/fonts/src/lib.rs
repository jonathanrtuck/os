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

pub mod cache;
pub mod metrics;
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
    axis_values: &[metrics::AxisValue],
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
pub fn format_axis_setting<'a>(buf: &'a mut [u8], tag: &str, value: f32) -> Option<&'a str> {
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

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec;

    use super::*;

    #[test]
    fn shape_empty_text() {
        assert!(shape(b"not a real font", "", &[]).is_empty());
    }

    #[test]
    fn shape_invalid_font() {
        assert!(shape(b"garbage", "hello", &[]).is_empty());
    }

    #[test]
    fn shape_empty_font_data() {
        assert!(shape(&[], "hello", &[]).is_empty());
    }

    #[test]
    fn shape_with_variations_empty_text() {
        assert!(shape_with_variations(b"garbage", "", &[], &[]).is_empty());
    }

    #[test]
    fn shape_with_variations_empty_axes_delegates() {
        assert!(shape_with_variations(b"garbage", "hello", &[], &[]).is_empty());
    }

    #[test]
    fn format_axis_setting_integer() {
        let mut buf = [0u8; 32];
        let s = format_axis_setting(&mut buf, "wght", 700.0).unwrap();

        assert_eq!(s, "wght=700");
    }

    #[test]
    fn format_axis_setting_fractional() {
        let mut buf = [0u8; 32];
        let s = format_axis_setting(&mut buf, "opsz", 14.5).unwrap();

        assert!(s.starts_with("opsz=14.5"));
    }

    #[test]
    fn format_axis_setting_buffer_too_small() {
        let mut buf = [0u8; 3];

        assert!(format_axis_setting(&mut buf, "wght", 700.0).is_none());
    }

    #[test]
    fn lru_cache_insert_and_get() {
        let mut cache = cache::LruGlyphCache::new(4);
        let glyph = cache::LruCachedGlyph {
            width: 10,
            height: 12,
            bearing_x: 1,
            bearing_y: 11,
            advance: 8,
            coverage: vec![128u8; 120],
        };

        cache.insert(42, 18, glyph.clone());

        assert_eq!(cache.len(), 1);

        let g = cache.get(42, 18).unwrap();

        assert_eq!(g.width, 10);
        assert_eq!(g.height, 12);
        assert_eq!(g.advance, 8);
    }

    #[test]
    fn lru_cache_miss() {
        let mut cache = cache::LruGlyphCache::new(4);

        assert!(cache.get(99, 18).is_none());
    }

    #[test]
    fn lru_cache_eviction() {
        let mut cache = cache::LruGlyphCache::new(2);
        let mk = |id| cache::LruCachedGlyph {
            width: id as u32,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            advance: 0,
            coverage: vec![0],
        };

        cache.insert(1, 18, mk(1));
        cache.insert(2, 18, mk(2));

        assert_eq!(cache.len(), 2);

        cache.insert(3, 18, mk(3));

        assert_eq!(cache.len(), 2);
        assert!(cache.get(1, 18).is_none());
        assert!(cache.get(2, 18).is_some());
        assert!(cache.get(3, 18).is_some());
    }

    #[test]
    fn lru_cache_access_promotes() {
        let mut cache = cache::LruGlyphCache::new(2);
        let mk = |id| cache::LruCachedGlyph {
            width: id as u32,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            advance: 0,
            coverage: vec![0],
        };

        cache.insert(1, 18, mk(1));
        cache.insert(2, 18, mk(2));
        cache.get(1, 18);
        cache.insert(3, 18, mk(3));

        assert!(cache.get(2, 18).is_none());
        assert!(cache.get(1, 18).is_some());
    }

    #[test]
    fn lru_cache_update_existing() {
        let mut cache = cache::LruGlyphCache::new(4);
        let glyph1 = cache::LruCachedGlyph {
            width: 10,
            height: 10,
            bearing_x: 0,
            bearing_y: 0,
            advance: 5,
            coverage: vec![0; 100],
        };
        let glyph2 = cache::LruCachedGlyph {
            width: 20,
            height: 20,
            bearing_x: 0,
            bearing_y: 0,
            advance: 10,
            coverage: vec![0; 400],
        };

        cache.insert(42, 18, glyph1);
        cache.insert(42, 18, glyph2);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(42, 18).unwrap().width, 20);
    }

    #[test]
    fn lru_cache_style_id_distinguishes() {
        let mut cache = cache::LruGlyphCache::new(4);
        let mk = |w| cache::LruCachedGlyph {
            width: w,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            advance: 0,
            coverage: vec![0],
        };

        cache.insert_with_axes(42, 18, 0, mk(10));
        cache.insert_with_axes(42, 18, 1, mk(20));

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get_with_axes(42, 18, 0).unwrap().width, 10);
        assert_eq!(cache.get_with_axes(42, 18, 1).unwrap().width, 20);
    }

    #[test]
    fn glyph_cache_zeroed() {
        let cache = cache::GlyphCache::zeroed();

        assert_eq!(cache.line_height, 0);
        assert_eq!(cache.ascent, 0);
        assert!(cache.get(65).is_none());
    }

    #[test]
    fn metrics_invalid_font() {
        assert!(metrics::font_metrics(b"not a font").is_none());
        assert!(metrics::font_metrics(&[]).is_none());
    }

    #[test]
    fn caret_skew_invalid_font() {
        assert_eq!(metrics::caret_skew(&[]), 0.0);
        assert_eq!(metrics::caret_skew(b"garbage"), 0.0);
    }

    #[test]
    fn glyph_id_invalid_font() {
        assert!(metrics::glyph_id_for_char(&[], 'A').is_none());
    }
}
