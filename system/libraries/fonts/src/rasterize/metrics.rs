//! Font metric helpers and public types for the rasterizer.
//!
//! Provides font-level metric extraction (ascent, descent, line gap),
//! glyph-level metrics (advance, bearing), cmap lookup, variable font
//! axis enumeration, and the public output types (GlyphMetrics, RasterBuffer).

use read_fonts::{tables::cmap::Cmap, FontRef, TableProvider};

// ---------------------------------------------------------------------------
// Font metric helpers
// ---------------------------------------------------------------------------

/// Basic font metrics extracted from the hhea, head, and OS/2 tables.
pub struct FontMetrics {
    pub units_per_em: u16,
    /// hhea ascent (positive above baseline, in font units).
    pub ascent: i16,
    /// hhea descent (negative below baseline, in font units).
    pub descent: i16,
    /// hhea line gap (in font units).
    pub line_gap: i16,
    /// OS/2 sCapHeight (height of capital H above baseline, font units). 0 if unavailable.
    pub cap_height: i16,
}

/// Extract basic font metrics from raw font data.
pub fn font_metrics(font_data: &[u8]) -> Option<FontMetrics> {
    let font = FontRef::new(font_data).ok()?;
    let head = font.head().ok()?;
    let hhea = font.hhea().ok()?;

    // Cap height from OS/2 table (version 2+). Falls back to 0.
    let cap_height = font
        .os2()
        .ok()
        .and_then(|os2| os2.s_cap_height())
        .unwrap_or(0) as i16;

    Some(FontMetrics {
        units_per_em: head.units_per_em(),
        ascent: hhea.ascender().to_i16(),
        descent: hhea.descender().to_i16(),
        line_gap: hhea.line_gap().to_i16(),
        cap_height,
    })
}

/// Look up the glyph ID for a Unicode codepoint using the font's cmap table.
pub fn glyph_id_for_char(font_data: &[u8], codepoint: char) -> Option<u16> {
    let font = FontRef::new(font_data).ok()?;
    let cmap = font.cmap().ok()?;

    cmap_lookup(&cmap, codepoint as u32)
}

/// Look up a codepoint in the cmap table, trying all supported subtables.
fn cmap_lookup(cmap: &Cmap, codepoint: u32) -> Option<u16> {
    for record in cmap.encoding_records() {
        if let Ok(subtable) = record.subtable(cmap.offset_data()) {
            if let Some(gid) = subtable.map_codepoint(codepoint) {
                let id = gid.to_u32() as u16;
                if id > 0 {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Get horizontal metrics for a glyph: (advance_width, left_side_bearing).
pub fn glyph_h_metrics(font_data: &[u8], glyph_id: u16) -> Option<(u16, i16)> {
    let font = FontRef::new(font_data).ok()?;
    let hmtx = font.hmtx().ok()?;
    let hhea = font.hhea().ok()?;
    let num_h_metrics = hhea.number_of_h_metrics();

    if (glyph_id as u16) < num_h_metrics {
        let metrics = hmtx.h_metrics();
        let m = metrics.get(glyph_id as usize)?;
        Some((m.advance.get(), m.side_bearing.get()))
    } else {
        let metrics = hmtx.h_metrics();
        let last = metrics.get(num_h_metrics as usize - 1)?;
        let advance = last.advance.get();
        let lsb_data = hmtx.left_side_bearings();
        let lsb_idx = (glyph_id as usize).checked_sub(num_h_metrics as usize)?;
        let lsb = lsb_data.get(lsb_idx).map(|v| v.get()).unwrap_or(0);
        Some((advance, lsb))
    }
}

// ---------------------------------------------------------------------------
// Variable font axis helpers
// ---------------------------------------------------------------------------

/// Information about a single variation axis in a variable font.
#[derive(Debug, Clone)]
pub struct FontAxis {
    /// 4-byte axis tag (e.g. b"wght", b"opsz", b"wdth", b"YTLC").
    pub tag: [u8; 4],
    /// Minimum axis value.
    pub min_value: f32,
    /// Default axis value.
    pub default_value: f32,
    /// Maximum axis value.
    pub max_value: f32,
}

/// A user-specified axis value for variable font rendering (e.g., wght=700).
#[derive(Debug, Clone, Copy)]
pub struct AxisValue {
    /// 4-byte axis tag (e.g. b"wght", b"opsz").
    pub tag: [u8; 4],
    /// Desired axis value in design-space units.
    pub value: f32,
}

/// Parse all variation axes from a variable font.
///
/// Returns an empty `Vec` for non-variable fonts or parse failure.
pub fn font_axes(font_data: &[u8]) -> alloc::vec::Vec<FontAxis> {
    let font = match FontRef::new(font_data) {
        Ok(f) => f,
        Err(_) => return alloc::vec::Vec::new(),
    };
    let fvar = match font.fvar() {
        Ok(f) => f,
        Err(_) => return alloc::vec::Vec::new(),
    };
    let axes = match fvar.axes() {
        Ok(a) => a,
        Err(_) => return alloc::vec::Vec::new(),
    };
    axes.iter()
        .map(|axis| FontAxis {
            tag: axis.axis_tag().into_bytes(),
            min_value: axis.min_value().to_f32(),
            default_value: axis.default_value().to_f32(),
            max_value: axis.max_value().to_f32(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public output types
// ---------------------------------------------------------------------------

/// Metrics for a single rasterized glyph.
#[derive(Clone, Copy, Debug)]
pub struct GlyphMetrics {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal offset from pen position to left edge of bitmap.
    pub bearing_x: i32,
    /// Vertical offset from baseline to top edge of bitmap (positive = up).
    pub bearing_y: i32,
    /// Horizontal advance to next glyph in pixels.
    pub advance: u32,
}

/// Caller-provided buffer for rasterization output (1 byte per pixel coverage).
pub struct RasterBuffer<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}

/// Returns the horizontal advance width for a glyph, adjusted for variation axes.
///
/// Tries HVAR first (fast per-glyph delta lookup). Falls back to the plain
/// hmtx advance when no axes are specified or the font has no HVAR table.
pub fn glyph_h_advance_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    axes: &[AxisValue],
) -> Option<i32> {
    super::hvar::advance_with_delta(font_data, glyph_id, axes)
}

/// Compute a deterministic hash of axis values for use as a glyph cache key component.
///
/// The hash is computed from the axis tags and values. An empty axis values
/// slice produces hash 0.
pub fn axis_values_hash(axis_values: &[AxisValue]) -> u32 {
    if axis_values.is_empty() {
        return 0;
    }
    // Simple FNV-1a-like hash.
    let mut h: u32 = 0x811c_9dc5;
    for av in axis_values {
        for &b in &av.tag {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        let bits = av.value.to_bits();
        for shift in [0, 8, 16, 24] {
            h ^= (bits >> shift) as u32 & 0xFF;
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}
