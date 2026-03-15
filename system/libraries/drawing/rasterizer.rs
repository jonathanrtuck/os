// Oversampling constants for glyph rasterization.
//
// Glyph rasterization now happens via fonts::rasterize which has its own
// scanline rasterizer using read-fonts for outline extraction. This file
// retains only the oversampling constants used by GLYPH_BUF_SIZE (in lib.rs)
// and by the SVG rasterizer (svg.rs).

/// Horizontal oversampling factor for anti-aliasing.
/// Rasterise at OVERSAMPLE_X × width, then downsample into per-channel
/// (R, G, B) subpixel coverage. 6 = 3 subpixels × 2× oversampling each.
pub const OVERSAMPLE_X: i32 = 6;
/// Vertical oversampling factor for anti-aliasing.
pub const OVERSAMPLE_Y: i32 = 8;
