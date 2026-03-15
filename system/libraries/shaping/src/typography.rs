//! Content-type-aware typography defaults.
//!
//! Maps content types (code, prose, UI, unknown) to typographic settings:
//! font family, OpenType feature flags, weight preference, tracking, and
//! whether automatic optical sizing should be applied.
//!
//! The OS natively understands content types (settled decision #5). These
//! defaults let the rendering pipeline produce intelligent typographic
//! output without explicit configuration from editors.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fallback::ContentType;

/// Font family preference for a content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFamily {
    /// Fixed-width font (e.g., Source Code Pro).
    Monospace,
    /// Variable-width font (e.g., Nunito Sans).
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

    /// OpenType feature flags to enable during shaping.
    ///
    /// Stored as parseable strings (e.g., "+calt", "+tnum", "+onum").
    /// The shaping pipeline parses these into `Feature` structs.
    pub features: Vec<String>,

    /// Preferred font weight (in CSS-like units: 100–900).
    ///
    /// 400 = Regular, 500 = Medium, 700 = Bold.
    /// For variable fonts with a `wght` axis, this value is used as the
    /// base weight before any perceptual corrections (dark mode, etc.).
    pub weight_preference: f32,

    /// Letter-spacing adjustment in font units (0.0 = standard tracking).
    ///
    /// Positive values increase spacing, negative values tighten.
    pub tracking: f32,

    /// Whether automatic optical sizing should be applied.
    ///
    /// When true, the pipeline automatically sets the `opsz` axis value
    /// to match the rendered pixel size (for fonts with an opsz axis).
    pub optical_sizing: bool,
}

impl TypographyConfig {
    /// Get typography defaults for a content type.
    ///
    /// Returns a fully populated `TypographyConfig` with sane defaults.
    /// Unknown content types fall back to prose defaults without panic.
    pub fn for_content_type(content_type: ContentType) -> Self {
        match content_type {
            ContentType::Code => Self::code_defaults(),
            ContentType::Prose => Self::prose_defaults(),
            ContentType::Ui => Self::ui_defaults(),
            ContentType::Unknown => Self::prose_defaults(),
        }
    }

    /// Code typography: monospace, programming ligatures, tabular figures.
    fn code_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Monospace,
            features: alloc::vec![
                String::from("+calt"), // contextual alternates (programming ligatures: !=, =>, ->)
                String::from("+tnum"), // tabular figures (aligned number columns)
            ],
            weight_preference: 400.0, // Regular weight
            tracking: 0.0,            // Standard tracking for monospace
            optical_sizing: false,     // Monospace fonts rarely have opsz axis
        }
    }

    /// Prose typography: proportional, optical sizing, oldstyle figures.
    fn prose_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Proportional,
            features: alloc::vec![
                String::from("+onum"), // oldstyle figures (harmonize with lowercase text)
            ],
            weight_preference: 400.0, // Regular weight
            tracking: 0.0,            // Standard tracking
            optical_sizing: true,     // Auto-adjust opsz for rendered size
        }
    }

    /// UI label typography: proportional, medium weight, standard tracking.
    fn ui_defaults() -> Self {
        TypographyConfig {
            font_family: FontFamily::Proportional,
            features: alloc::vec![],
            weight_preference: 500.0, // Medium weight (functional, not decorative)
            tracking: 0.0,            // Standard tracking for UI
            optical_sizing: false,    // UI labels are typically fixed-size
        }
    }
}
