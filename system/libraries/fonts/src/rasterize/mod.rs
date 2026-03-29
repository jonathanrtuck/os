//! Glyph rasterizer — converts glyph outlines to coverage maps using read-fonts.
//!
//! Uses read-fonts for glyph outline extraction and metrics, then runs the
//! analytic area coverage rasterizer (bezier flattening + exact signed-area
//! trapezoid coverage per pixel).
//!
//! Output is 1 byte per pixel (grayscale coverage). No subpixel (LCD) rendering.
//!
//! All math is integer/fixed-point. No floating point in the rasterizer itself.

pub mod embolden;
mod gvar;
pub mod hvar;
mod metrics;
mod optical;
pub(crate) mod outline;
pub(crate) mod scale;
mod scanline;

// Re-export the public API so `fonts::rasterize::*` paths remain unchanged.

// From embolden
pub use embolden::{compute_dilation, embolden_outline};
// From gvar
pub use gvar::rasterize_with_axes;
// From metrics
pub use metrics::{
    axis_values_hash, caret_skew, font_axes, font_metrics, glyph_h_advance_with_axes,
    glyph_h_metrics, glyph_id_for_char, AxisValue, FontAxis, FontMetrics, GlyphMetrics,
    RasterBuffer,
};
// From optical
pub use optical::{
    auto_axis_values_for_opsz, auto_weight_correction_axes, compute_optical_size,
    weight_correction_factor,
};
// From outline
pub use outline::{GlyphOutline, GlyphPoint};
// Re-export crate-visible items used by other modules in the crate.
pub(crate) use scale::{scale_fu, scale_fu_ceil};
// From scanline
pub use scanline::{rasterize, RasterScratch};
