//! Content-type-aware typography defaults.
//!
//! Maps content types (code, prose, UI, unknown) to typographic settings:
//! font family, OpenType feature flags, weight preference, tracking, and
//! optical sizing.
//!
//! The OS natively understands content types (settled decision #5). These
//! defaults let the rendering pipeline produce intelligent typographic
//! output without explicit configuration from editors.
//!
//! Font families:
//! - **JetBrains Mono** — monospace (code, editor)
//! - **Inter** — sans-serif (UI labels, chrome)
//! - **Source Serif 4** — serif (prose, body text)
//!
//! Each font is a separate static file. Content type selects the font
//! family, not variable font axis values.

use super::fallback::ContentType;

/// Font family preference for a content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFamily {
    /// Fixed-width font (JetBrains Mono).
    Monospace,
    /// Variable-width sans-serif (Inter).
    Sans,
    /// Variable-width serif (Source Serif 4).
    Serif,
}

/// Typography configuration for a content type.
///
/// Determines font selection, OpenType features, weight, tracking, and
/// perceptual rendering options. Editors can override these defaults.
#[derive(Debug, Clone)]
pub struct TypographyConfig {
    /// Preferred font family.
    pub font_family: FontFamily,

    /// OpenType feature tags to enable during shaping (e.g., b"calt", b"tnum").
    /// Fixed-capacity array avoids heap allocation for 4-byte tags.
    pub features: &'static [&'static [u8; 4]],

    /// Preferred font weight (in CSS-like units: 100–900).
    ///
    /// 400 = Regular, 500 = Medium, 700 = Bold.
    pub weight_preference: f32,

    /// Letter-spacing adjustment in font units (0.0 = standard tracking).
    pub tracking: f32,

    /// Whether automatic optical sizing should be applied.
    pub optical_sizing: bool,
}

impl TypographyConfig {
    /// Get typography defaults for a content type.
    pub fn for_content_type(content_type: ContentType) -> Self {
        match content_type {
            ContentType::Code => Self::code_defaults(),
            ContentType::Prose => Self::prose_defaults(),
            ContentType::Ui => Self::ui_defaults(),
            ContentType::Unknown => Self::prose_defaults(),
        }
    }

    /// Code typography: JetBrains Mono, programming ligatures, tabular figures.
    fn code_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Monospace,
            features: &[b"calt", b"tnum"],
            weight_preference: 400.0,
            tracking: 0.0,
            optical_sizing: false,
        }
    }

    /// Prose typography: Source Serif 4, oldstyle figures.
    fn prose_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Serif,
            features: &[b"onum"],
            weight_preference: 400.0,
            tracking: 0.0,
            optical_sizing: false,
        }
    }

    /// UI label typography: Inter, medium weight.
    fn ui_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Sans,
            features: &[],
            weight_preference: 500.0,
            tracking: 0.0,
            optical_sizing: false,
        }
    }
}
