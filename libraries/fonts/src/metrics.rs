//! Font metrics — font-unit API for layout, presenter, and other consumers
//! above the render boundary.
//!
//! Everything in this module operates in **font units** or is unitless.
//! No pixel concepts exist here. Pixel-denominated types (`GlyphMetrics`,
//! `RasterBuffer`) and rasterization functions live in `crate::rasterize`.

use read_fonts::{FontRef, TableProvider, tables::cmap::Cmap};

// ---------------------------------------------------------------------------
// Font metrics
// ---------------------------------------------------------------------------

/// Basic font metrics extracted from the hhea, head, and OS/2 tables.
///
/// All values are in font units.
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

/// Compute the caret skew factor from a font's hhea table.
///
/// Returns the horizontal shear factor for italic carets: negative for
/// right-leaning italic, zero for upright. Derived from the font's
/// `caretSlopeRise` and `caretSlopeRun` fields.
///
/// The returned value is `tan(angle)`.
pub fn caret_skew(font_data: &[u8]) -> f32 {
    let font = match FontRef::new(font_data) {
        Ok(f) => f,
        Err(_) => return 0.0,
    };
    let hhea = match font.hhea() {
        Ok(h) => h,
        Err(_) => return 0.0,
    };
    let rise = hhea.caret_slope_rise();
    let run = hhea.caret_slope_run();
    if rise == 0 {
        return 0.0;
    }
    -(run as f32) / (rise as f32)
}

// ---------------------------------------------------------------------------
// Glyph lookup and horizontal metrics
// ---------------------------------------------------------------------------

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
///
/// Both values are in font units.
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

/// Returns the horizontal advance width for a glyph, adjusted for variation axes.
///
/// Tries HVAR first (fast per-glyph delta lookup). Falls back to the plain
/// hmtx advance when no axes are specified or the font has no HVAR table.
///
/// Returns font units.
pub fn glyph_h_advance_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    axes: &[AxisValue],
) -> Option<i32> {
    crate::rasterize::hvar::advance_with_delta(font_data, glyph_id, axes)
}

// ---------------------------------------------------------------------------
// Variable font axis types and helpers
// ---------------------------------------------------------------------------

/// Information about a single variation axis in a variable font.
#[derive(Debug, Clone)]
pub struct FontAxis {
    /// 4-byte axis tag (e.g. b"wght", b"opsz", b"wdth").
    pub tag: [u8; 4],
    pub min_value: f32,
    pub default_value: f32,
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

/// Compute a deterministic hash of axis values for use as a glyph cache key.
///
/// An empty axis values slice produces hash 0.
pub fn axis_values_hash(axis_values: &[AxisValue]) -> u32 {
    if axis_values.is_empty() {
        return 0;
    }
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

// ---------------------------------------------------------------------------
// Automatic optical sizing
// ---------------------------------------------------------------------------

/// Compute the optical size value from a rendered pixel size and display DPI.
///
/// Converts rendered pixel size to an equivalent point size for the font's
/// optical size axis.
pub fn compute_optical_size(font_size_px: u16, dpi: u16) -> f32 {
    if dpi == 0 {
        return font_size_px as f32;
    }
    font_size_px as f32 * 72.0 / dpi as f32
}

/// Compute automatic optical size axis values for a font.
///
/// If the font has an `opsz` variation axis, returns an `AxisValue` array
/// with the optical size set to the computed value (clamped to the font's
/// declared opsz range). Returns empty for fonts without `opsz`.
pub fn auto_axis_values_for_opsz(
    font_data: &[u8],
    font_size_px: u16,
    dpi: u16,
) -> alloc::vec::Vec<AxisValue> {
    let axes = font_axes(font_data);
    let opsz_axis = match axes.iter().find(|a| &a.tag == b"opsz") {
        Some(a) => a,
        None => return alloc::vec::Vec::new(),
    };

    let raw_opsz = compute_optical_size(font_size_px, dpi);

    let clamped = if raw_opsz < opsz_axis.min_value {
        opsz_axis.min_value
    } else if raw_opsz > opsz_axis.max_value {
        opsz_axis.max_value
    } else {
        raw_opsz
    };

    alloc::vec![AxisValue {
        tag: *b"opsz",
        value: clamped,
    }]
}

// ---------------------------------------------------------------------------
// Dark mode weight correction
// ---------------------------------------------------------------------------

/// sRGB-to-linear lookup table (256 entries, scaled to u16 0-65535).
const SRGB_TO_LINEAR_LUT: [u16; 256] = {
    let mut lut = [0u16; 256];
    let mut i = 0u32;
    while i < 256 {
        let s = i as f64 / 255.0;
        let linear = if s <= 0.04045 {
            s / 12.92
        } else {
            let base = (s + 0.055) / 1.055;
            let base_sq = base * base;
            let mut t = base;
            let mut iter = 0;
            while iter < 50 {
                let t2 = t * t;
                let t4 = t2 * t2;
                if t4 < 1e-15 {
                    break;
                }
                let t_new = (4.0 * t + base_sq / t4) / 5.0;
                let diff = t_new - t;
                if diff < 1e-12 && diff > -1e-12 {
                    break;
                }
                t = t_new;
                iter += 1;
            }
            base_sq * t
        };
        let scaled = (linear * 65535.0 + 0.5) as u32;
        lut[i as usize] = if scaled > 65535 { 65535 } else { scaled as u16 };
        i += 1;
    }
    lut
};

fn srgb_to_linear(value: u8) -> f32 {
    SRGB_TO_LINEAR_LUT[value as usize] as f32 / 65535.0
}

fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);
    0.2126 * rl + 0.7152 * gl + 0.0722 * bl
}

/// Compute the weight correction factor for dark mode text rendering.
///
/// Returns a factor in [0.85, 1.0]. Multiply the font's base weight by
/// this to compensate for the irradiation illusion (light text on dark
/// backgrounds appears heavier).
pub fn weight_correction_factor(fg_r: u8, fg_g: u8, fg_b: u8, bg_r: u8, bg_g: u8, bg_b: u8) -> f32 {
    let fg_lum = relative_luminance(fg_r, fg_g, fg_b);
    let bg_lum = relative_luminance(bg_r, bg_g, bg_b);

    if fg_lum <= bg_lum {
        return 1.0;
    }

    let contrast = (fg_lum + 0.05) / (bg_lum + 0.05);
    let normalized = (contrast - 1.0) / 20.0;
    let clamped = if normalized < 0.0 {
        0.0
    } else if normalized > 1.0 {
        1.0
    } else {
        normalized
    };

    1.0 - 0.15 * clamped
}

/// Compute automatic weight correction axis values for dark mode rendering.
///
/// For a variable font with a `wght` axis, returns the corrected weight.
/// Returns empty for fonts without `wght` or when no correction is needed.
pub fn auto_weight_correction_axes(
    font_data: &[u8],
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
) -> alloc::vec::Vec<AxisValue> {
    let axes = font_axes(font_data);
    let wght_axis = match axes.iter().find(|a| &a.tag == b"wght") {
        Some(a) => a,
        None => return alloc::vec::Vec::new(),
    };

    let factor = weight_correction_factor(fg_r, fg_g, fg_b, bg_r, bg_g, bg_b);

    if (factor - 1.0).abs() < f32::EPSILON {
        return alloc::vec::Vec::new();
    }

    let adjusted = wght_axis.default_value * factor;

    let clamped = if adjusted < wght_axis.min_value {
        wght_axis.min_value
    } else if adjusted > wght_axis.max_value {
        wght_axis.max_value
    } else {
        adjusted
    };

    alloc::vec![AxisValue {
        tag: *b"wght",
        value: clamped,
    }]
}
