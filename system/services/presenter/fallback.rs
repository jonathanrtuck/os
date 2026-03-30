//! Font fallback chain — tries fonts in order until a valid glyph is found.
//!
//! When the primary font lacks a glyph for a codepoint (shaping produces
//! glyph_id 0 / .notdef), the fallback chain tries the next font. This
//! continues until a valid glyph (id > 0) is found or the chain is exhausted.
//!
//! Content-type-aware: different content types (code, prose) select different
//! primary fonts. Code uses monospace primary with proportional fallback;
//! prose uses proportional primary with monospace fallback.

use alloc::vec::Vec;

use fonts::{metrics::AxisValue, shape, Feature, ShapedGlyph};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Content type for content-type-aware font selection and typography defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Code content — primary=monospace, fallback=proportional.
    Code,
    /// Prose content — primary=proportional, fallback=monospace.
    Prose,
    /// UI labels — primary=proportional, medium weight, tight tracking.
    Ui,
    /// Unknown / unrecognized content type — falls back to prose defaults.
    Unknown,
}

/// A shaped glyph with metadata about which font in the fallback chain
/// provided it.
#[derive(Debug, Clone, Copy)]
pub struct FallbackGlyph {
    /// The shaped glyph data (glyph ID, advances, offsets, cluster).
    pub glyph: ShapedGlyph,
    /// Index into the fallback chain identifying which font was used.
    /// 0 = primary font, 1 = first fallback, etc.
    pub font_index: u8,
}

/// An ordered list of font data references for glyph fallback.
///
/// When shaping text, the chain tries each font in order. For each character
/// that produces glyph_id 0 (.notdef) from the primary font, subsequent fonts
/// are tried until a valid glyph is found or the chain is exhausted.
pub struct FallbackChain<'a> {
    /// Ordered font data references. Index 0 is the primary font.
    fonts: Vec<&'a [u8]>,
}

impl<'a> FallbackChain<'a> {
    /// Create a new fallback chain from an ordered list of font data references.
    ///
    /// The first font is the primary; subsequent fonts are fallbacks tried in order.
    pub fn new(fonts: &[&'a [u8]]) -> Self {
        FallbackChain {
            fonts: fonts.to_vec(),
        }
    }

    /// Create a content-type-aware fallback chain.
    ///
    /// - `Code` → primary=monospace, fallback=proportional
    /// - `Prose` → primary=proportional, fallback=monospace
    ///
    /// `mono_font` is the monospace font data, `prop_font` is the proportional
    /// font data.
    pub fn for_content_type(
        content_type: ContentType,
        mono_font: &'a [u8],
        prop_font: &'a [u8],
    ) -> Self {
        match content_type {
            ContentType::Code => FallbackChain {
                fonts: alloc::vec![mono_font, prop_font],
            },
            ContentType::Prose | ContentType::Ui | ContentType::Unknown => FallbackChain {
                fonts: alloc::vec![prop_font, mono_font],
            },
        }
    }

    /// Shape a string of text using the fallback chain.
    ///
    /// For each character, shaping is first attempted with the primary font.
    /// If any glyph has glyph_id 0 (.notdef), that character is re-shaped
    /// with subsequent fonts in the chain until a valid glyph is found.
    ///
    /// Returns a `Vec<FallbackGlyph>` with one entry per output glyph,
    /// including the font_index indicating which font provided each glyph.
    ///
    /// When all fonts are exhausted for a character, .notdef (glyph_id 0)
    /// is returned and shaping continues for subsequent characters.
    // NOTE: Shapes full text with each fallback font — O(fonts × text_length).
    // Acceptable for current use (short UI labels). For large documents,
    // optimize to only reshape runs with missing glyphs.
    pub fn shape(&self, text: &str, features: &[Feature]) -> Vec<FallbackGlyph> {
        if text.is_empty() || self.fonts.is_empty() {
            return Vec::new();
        }

        // First, shape the entire text with the primary font.
        let primary_glyphs = shape(self.fonts[0], text, features);

        // If there's only one font, just annotate with font_index=0.
        if self.fonts.len() == 1 {
            return primary_glyphs
                .into_iter()
                .map(|g| FallbackGlyph {
                    glyph: g,
                    font_index: 0,
                })
                .collect();
        }

        // Check if any glyphs need fallback (glyph_id == 0).
        let needs_fallback = primary_glyphs.iter().any(|g| g.glyph_id == 0);

        if !needs_fallback {
            // All glyphs resolved by primary font — no fallback needed.
            return primary_glyphs
                .into_iter()
                .map(|g| FallbackGlyph {
                    glyph: g,
                    font_index: 0,
                })
                .collect();
        }

        // Need fallback: for each character that produced .notdef, try
        // subsequent fonts. We use cluster indices to map back to characters.
        let mut result: Vec<FallbackGlyph> = primary_glyphs
            .into_iter()
            .map(|g| FallbackGlyph {
                glyph: g,
                font_index: 0,
            })
            .collect();

        // Try each fallback font for .notdef glyphs.
        for (font_idx, &font_data) in self.fonts.iter().enumerate().skip(1) {
            // Check if there are still unresolved glyphs.
            let still_needs_fallback = result.iter().any(|fg| fg.glyph.glyph_id == 0);
            if !still_needs_fallback {
                break;
            }

            // Shape the full text with this fallback font.
            let fallback_glyphs = shape(font_data, text, features);

            // For each .notdef glyph in the result, try to use the fallback's
            // glyph at the same cluster position.
            for fg in result.iter_mut() {
                if fg.glyph.glyph_id == 0 {
                    // Find a matching glyph from the fallback by cluster index.
                    if let Some(replacement) = fallback_glyphs
                        .iter()
                        .find(|g| g.cluster == fg.glyph.cluster && g.glyph_id > 0)
                    {
                        fg.glyph = *replacement;
                        fg.font_index = font_idx as u8;
                    }
                }
            }
        }

        result
    }

    /// Shape a single character using the fallback chain.
    ///
    /// Tries each font in order until a valid glyph (glyph_id > 0) is found.
    /// Returns the glyph and the font index that provided it.
    ///
    /// If no font has the glyph, returns .notdef from the last font tried
    /// (or glyph_id 0 with font_index 0 if the chain is empty).
    pub fn shape_char(&self, ch: char) -> (ShapedGlyph, u8) {
        let notdef = ShapedGlyph {
            glyph_id: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
            cluster: 0,
        };

        if self.fonts.is_empty() {
            return (notdef, 0);
        }

        let mut buf = [0u8; 4];
        let text = ch.encode_utf8(&mut buf);

        for (idx, &font_data) in self.fonts.iter().enumerate() {
            let glyphs = shape(font_data, text, &[]);
            if let Some(g) = glyphs.first() {
                if g.glyph_id > 0 {
                    return (*g, idx as u8);
                }
            }
        }

        // Chain exhausted — return .notdef.
        // Try to get at least the .notdef glyph from the primary font.
        let glyphs = shape(self.fonts[0], text, &[]);
        if let Some(g) = glyphs.first() {
            return (*g, 0);
        }

        (notdef, 0)
    }

    /// Number of fonts in the chain.
    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    /// Whether the chain is empty (no fonts).
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Font identifier hash — for cache key differentiation
// ---------------------------------------------------------------------------

/// Compute a hash that combines a font index with axis values.
///
/// This ensures that the glyph cache distinguishes between:
/// - The same glyph ID from different fonts in a fallback chain
/// - The same glyph ID at different variable font axis values
///
/// The `font_index` is the position in the fallback chain (0 = primary,
/// 1 = first fallback, etc.). Axis values are hashed as before.
pub fn font_identifier_hash(font_index: u8, axis_values: &[AxisValue]) -> u32 {
    let style_id = fonts::metrics::axis_values_hash(axis_values);

    // Combine font_index with style_id using FNV-1a mixing.
    let mut h: u32 = 0x811c_9dc5;
    // Mix in font_index.
    h ^= font_index as u32;
    h = h.wrapping_mul(0x0100_0193);
    // Mix in style_id.
    h ^= style_id;
    h = h.wrapping_mul(0x0100_0193);

    h
}
