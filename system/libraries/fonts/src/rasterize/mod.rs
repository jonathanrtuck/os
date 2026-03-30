//! Glyph rasterizer — converts glyph outlines to coverage maps.
//!
//! Uses read-fonts for glyph outline extraction, then runs the analytic area
//! coverage rasterizer (bezier flattening + exact signed-area trapezoid
//! coverage per pixel).
//!
//! Output is 1 byte per pixel (grayscale coverage). No subpixel (LCD) rendering.
//!
//! Font-unit metrics (FontMetrics, AxisValue, etc.) live in `crate::metrics`.
//! This module contains only pixel-denominated types and rasterization functions.

pub mod embolden;
mod gvar;
pub mod hvar;
mod metrics;
pub(crate) mod outline;
pub(crate) mod scale;
mod scanline;

// Re-export pixel types from metrics.
// From embolden
pub use embolden::{compute_dilation, embolden_outline};
// From gvar
pub use gvar::rasterize_with_axes;
pub use metrics::{GlyphMetrics, RasterBuffer};
// From outline
pub use outline::{GlyphOutline, GlyphPoint};
// Re-export crate-visible items used by other modules in the crate.
pub(crate) use scale::{scale_fu, scale_fu_ceil};
// From scanline
pub use scanline::{rasterize, RasterScratch};
