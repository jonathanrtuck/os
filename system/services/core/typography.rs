//! Content-type-aware typography defaults.
//!
//! Maps content types (code, prose, UI, unknown) to typographic settings:
//! font family, OpenType feature flags, weight preference, tracking,
//! optical sizing, and variable font axis overrides.
//!
//! The OS natively understands content types (settled decision #5). These
//! defaults let the rendering pipeline produce intelligent typographic
//! output without explicit configuration from editors.
//!
//! Primary font: **Recursive Variable** — a single font with MONO axis
//! (0=proportional sans, 1=monospace) and CASL axis (0=linear, 1=casual).
//! Content type drives axis values, not font selection.

use alloc::vec::Vec;

use fonts::rasterize::AxisValue;

use super::fallback::ContentType;

/// Font family preference for a content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFamily {
    /// Fixed-width font (Recursive MONO=1).
    Monospace,
    /// Variable-width font (Recursive MONO=0).
    Proportional,
}

/// Typography configuration for a content type.
///
/// Determines font selection, OpenType features, weight, tracking, and
/// perceptual rendering options. Editors can override these defaults.
#[derive(Debug, Clone)]
pub struct TypographyConfig {
    /// Preferred font family (monospace or proportional).
    pub font_family: FontFamily,

    /// OpenType feature tags to enable during shaping (e.g., b"calt", b"tnum").
    /// Fixed-capacity array avoids heap allocation for 4-byte tags.
    pub features: &'static [&'static [u8; 4]],

    /// Preferred font weight (in CSS-like units: 100–900).
    ///
    /// 400 = Regular, 500 = Medium, 700 = Bold.
    /// For variable fonts with a `wght` axis, this value is used as the
    /// base weight before any perceptual corrections (dark mode, etc.).
    pub weight_preference: f32,

    /// Letter-spacing adjustment in font units (0.0 = standard tracking).
    pub tracking: f32,

    /// Whether automatic optical sizing should be applied.
    pub optical_sizing: bool,

    /// Explicit variable font axis overrides for this content type.
    ///
    /// For Recursive: MONO=1 for code, MONO=0 for prose/UI, CASL for
    /// casual vs linear style. These are passed to the rasterizer and
    /// merged with any automatic axis values (opsz, wght correction).
    pub axis_overrides: Vec<AxisValue>,
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

    /// Code typography: monospace (MONO=1), linear (CASL=0), programming
    /// ligatures, tabular figures.
    fn code_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Monospace,
            features: &[b"calt", b"tnum"],
            weight_preference: 400.0,
            tracking: 0.0,
            optical_sizing: false,
            axis_overrides: alloc::vec![
                AxisValue {
                    tag: *b"MONO",
                    value: 1.0
                }, // monospace
                AxisValue {
                    tag: *b"CASL",
                    value: 0.0
                }, // linear (clean)
            ],
        }
    }

    /// Prose typography: proportional (MONO=0), linear (CASL=0).
    fn prose_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Proportional,
            features: &[b"onum"],
            weight_preference: 400.0,
            tracking: 0.0,
            optical_sizing: false, // Recursive has no opsz axis
            axis_overrides: alloc::vec![
                AxisValue {
                    tag: *b"MONO",
                    value: 0.0
                }, // proportional sans
                AxisValue {
                    tag: *b"CASL",
                    value: 0.0
                }, // linear
            ],
        }
    }

    /// UI label typography: proportional, medium weight.
    fn ui_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Proportional,
            features: &[],
            weight_preference: 500.0,
            tracking: 0.0,
            optical_sizing: false,
            axis_overrides: alloc::vec![
                AxisValue {
                    tag: *b"MONO",
                    value: 0.0
                }, // proportional sans
                AxisValue {
                    tag: *b"CASL",
                    value: 0.0
                }, // linear
            ],
        }
    }
}
