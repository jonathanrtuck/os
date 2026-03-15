//! Drawing primitives for pixel buffers.
//!
//! Pure library — no allocations, no syscalls, no hardware access. Operates on
//! borrowed pixel buffers. All drawing operations clip to surface bounds; out-of-
//! range coordinates are silently ignored (no panics).
//!
//! # Usage
//!
//! ```text
//! let mut buf = [0u8; 320 * 240 * 4];
//! let mut surface = Surface {
//!     data: &mut buf,
//!     width: 320,
//!     height: 240,
//!     stride: 320 * 4,
//!     format: PixelFormat::Bgra8888,
//! };
//! surface.clear(Color::rgb(30, 30, 30));
//! surface.fill_rect(10, 10, 100, 50, Color::rgb(220, 80, 80));
//! ```

#![no_std]

extern crate alloc;
extern crate shaping;

pub use protocol::DirtyRect;

include!("cache.rs");
include!("gamma_tables.rs");
include!("palette.rs");
include!("png.rs");
include!("rasterizer.rs");
include!("svg.rs");
include!("truetype.rs");

// ---------------------------------------------------------------------------
// Stem darkening — non-linear coverage boost for thin strokes
// ---------------------------------------------------------------------------

/// Tunable boost constant for stem darkening. Higher values produce heavier
/// strokes. Reasonable range: 40–120. Applied after rasterization and subpixel
/// downsampling via a 256-entry lookup table.
pub const STEM_DARKENING_BOOST: u32 = 70;
/// Pre-computed lookup table for stem darkening.
///
/// Formula: `darkened = cov + STEM_DARKENING_BOOST * (255 - cov) / 255`
///
/// Properties:
/// - LUT[0] = 0 (zero coverage stays zero)
/// - LUT[255] = 255 (full coverage stays full)
/// - LUT[c] > c for all c in 1..=251 (strict boost)
/// - LUT[c] = c for c in 252..=254 (boost rounds to zero at high coverage)
/// - Monotonically non-decreasing
///
/// Applied equally to all 3 subpixel channels (R, G, B) after the FIR
/// color-fringe filter in the rasterizer.
pub const STEM_DARKENING_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let boost = STEM_DARKENING_BOOST;
    // LUT[0] = 0: zero coverage stays zero (no phantom pixels).
    // LUT[255] = 255: full coverage stays full.
    // LUT[1..254]: boosted via formula.
    let mut i = 1u32;

    while i < 256 {
        let darkened = i + boost * (255 - i) / 255;

        lut[i as usize] = if darkened > 255 { 255 } else { darkened as u8 };
        i += 1;
    }
    lut
};

const GLYPH_MAX_W: usize = 48;
const GLYPH_MAX_H: usize = 48;
/// Number of printable ASCII glyphs cached (0x20..=0x7E).
const ASCII_CACHE_COUNT: usize = 95;
/// Per-glyph coverage buffer size. Must accommodate the intermediate
/// oversampled raster (GLYPH_MAX_W * OVERSAMPLE_X * GLYPH_MAX_H) since
/// rasterize() uses the same buffer for the oversampled coverage map before
/// downsampling in-place. With subpixel rendering, the final coverage is
/// 3 bytes per pixel (RGB), so GLYPH_MAX_W * 3 * GLYPH_MAX_H for the output.
/// We need the max of (oversampled intermediate, 3-channel output).
/// Oversampled = GLYPH_MAX_W * 6 * GLYPH_MAX_H = 48*6*48 = 13824.
/// 3-channel output = GLYPH_MAX_W * 3 * GLYPH_MAX_H = 48*3*48 = 6912.
/// So the oversampled intermediate is always larger.
const GLYPH_BUF_SIZE: usize = GLYPH_MAX_W * OVERSAMPLE_X as usize * GLYPH_MAX_H;

/// Pre-rasterized metrics for one cached glyph.
#[derive(Clone, Copy)]
pub struct CachedGlyph {
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: u32,
    buf_offset: usize,
}
/// A color in canonical RGBA order. Converted to the target pixel format
/// at the point of writing — callers always work in RGBA regardless of the
/// underlying buffer format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
/// Fixed-size glyph cache for printable ASCII (0x20–0x7E).
/// Coverage maps are stored in a single contiguous buffer.
/// Total size: ~1.3 MiB (95 glyphs × 13 824 bytes coverage + metadata).
/// Each glyph buffer is GLYPH_MAX_W × OVERSAMPLE_X × GLYPH_MAX_H = 48×6×48 = 13 824 bytes
/// to accommodate the 6× oversampled intermediate raster used by subpixel rendering.
pub struct GlyphCache {
    glyphs: [CachedGlyph; ASCII_CACHE_COUNT],
    coverage: [u8; ASCII_CACHE_COUNT * GLYPH_BUF_SIZE],
    pub line_height: u32,
    /// Distance from top of line to baseline, in pixels. Derived from hhea ascent.
    pub ascent: u32,
    /// Distance from baseline to bottom of line, in pixels. Derived from hhea descent.
    /// Stored as a positive value (descent below baseline).
    pub descent: u32,
    /// The font size in pixels used to rasterize this cache (for kerning scaling).
    pub size_px: u32,
}
/// A mutable view into a pixel buffer.
///
/// Does not own the backing memory — the caller provides a mutable byte slice
/// from whatever source (DMA buffer, stack allocation, heap). The surface
/// borrows the slice for its lifetime.
///
/// Stride may exceed `width * bytes_per_pixel` if rows are padded.
pub struct Surface<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}
/// Fixed-pitch text layout engine.
///
/// Computes line breaks (hard newlines + soft wrap at max width), cursor
/// mapping (byte offset to/from pixel coordinates), and combined layout+draw.
/// Pure computation — no allocations, no side effects.
pub struct TextLayout {
    pub char_width: u32,
    pub line_height: u32,
    pub max_width: u32,
}

/// Pixel byte ordering within each pixel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// Blue, Green, Red, Alpha — 8 bits each. Used by virtio-gpu 2D.
    Bgra8888,
}

impl Color {
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    /// Decode from pixel bytes in the given format.
    fn decode(bytes: &[u8], format: PixelFormat) -> Self {
        match format {
            PixelFormat::Bgra8888 => Color {
                r: bytes[2],
                g: bytes[1],
                b: bytes[0],
                a: bytes[3],
            },
        }
    }
    /// Encode to pixel bytes in the given format.
    fn encode(self, format: PixelFormat) -> [u8; 4] {
        match format {
            PixelFormat::Bgra8888 => [self.b, self.g, self.r, self.a],
        }
    }

    /// Porter-Duff source-over: composite `self` on top of `dst`.
    ///
    /// sRGB gamma-correct blending: colors are converted to linear light before
    /// compositing, then converted back to sRGB. This produces perceptually
    /// correct results (especially for translucent overlays and text).
    ///
    /// Straight (non-premultiplied) alpha, integer math only. Returns the
    /// blended color. Fast-paths for fully opaque or fully transparent source.
    pub fn blend_over(self, dst: Color) -> Color {
        if self.a == 255 {
            return self;
        }
        if self.a == 0 {
            return dst;
        }

        let sa = self.a as u32;
        let da = dst.a as u32;
        let inv_sa = 255 - sa;
        // out_a = src_a + dst_a * (1 - src_a / 255)
        let out_a = sa + da * inv_sa / 255;

        if out_a == 0 {
            return Color::TRANSPARENT;
        }

        // Convert to linear space for color blending.
        let src_r_lin = SRGB_TO_LINEAR[self.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[self.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[self.b as usize] as u32;
        let dst_r_lin = SRGB_TO_LINEAR[dst.r as usize] as u32;
        let dst_g_lin = SRGB_TO_LINEAR[dst.g as usize] as u32;
        let dst_b_lin = SRGB_TO_LINEAR[dst.b as usize] as u32;
        // out_c = (src_c * src_a + dst_c * dst_a * (1 - src_a / 255)) / out_a
        // Computed in linear space.
        let r_lin = (src_r_lin * sa + dst_r_lin * da * inv_sa / 255) / out_a;
        let g_lin = (src_g_lin * sa + dst_g_lin * da * inv_sa / 255) / out_a;
        let b_lin = (src_b_lin * sa + dst_b_lin * da * inv_sa / 255) / out_a;

        // Convert back to sRGB (table is indexed by linear >> 4).
        Color {
            r: LINEAR_TO_SRGB[linear_to_idx(r_lin)],
            g: LINEAR_TO_SRGB[linear_to_idx(g_lin)],
            b: LINEAR_TO_SRGB[linear_to_idx(b_lin)],
            a: if out_a > 255 { 255 } else { out_a as u8 },
        }
    }
    /// Decode a Color from a BGRA8888 byte slice (at least 4 bytes).
    pub fn decode_from_bgra(bytes: &[u8]) -> Self {
        Color {
            b: bytes[0],
            g: bytes[1],
            r: bytes[2],
            a: bytes[3],
        }
    }
    /// Opaque color from RGB components.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
    /// Color with explicit alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
}
impl GlyphCache {
    /// Get cached glyph data for a character (must be 0x20..=0x7E).
    ///
    /// Returns 3-channel (RGB) subpixel coverage: 3 bytes per pixel
    /// (R, G, B coverage), stored row-major. Total length = width * height * 3.
    pub fn get(&self, ch: u8) -> Option<(&CachedGlyph, &[u8])> {
        if ch < 0x20 || ch > 0x7E {
            return None;
        }

        let idx = (ch - 0x20) as usize;
        let g = &self.glyphs[idx];
        let len = (g.width * g.height) as usize * 3; // 3 channels (RGB)
        let cov = &self.coverage[g.buf_offset..g.buf_offset + len];

        Some((g, cov))
    }
    /// Rasterize all printable ASCII glyphs into this cache in place.
    ///
    /// Uses the shaping library's rasterizer (read-fonts for outline extraction,
    /// scanline algorithm for coverage generation). The `font_data` is raw font
    /// file bytes.
    ///
    /// With subpixel rendering, the rasterizer writes 3-channel (RGB)
    /// coverage: width × height × 3 bytes per glyph. The GLYPH_BUF_SIZE
    /// accommodates the oversampled intermediate (which is always larger).
    pub fn populate(&mut self, font_data: &[u8], size_px: u32) {
        use shaping::rasterize;

        // Extract font metrics via shaping's rasterize module.
        let metrics = match rasterize::font_metrics(font_data) {
            Some(m) => m,
            None => return,
        };

        let upem = metrics.units_per_em;
        let asc_fu = metrics.ascent as i32;
        let desc_fu = metrics.descent as i32; // negative
        let gap_fu = metrics.line_gap as i32;
        let ascent_px = scale_fu_ceil(asc_fu, size_px, upem);
        let descent_px = scale_fu_ceil(-desc_fu, size_px, upem);
        let gap_px = scale_fu(gap_fu, size_px, upem);
        let gap_px = if gap_px < 0 { 0 } else { gap_px as u32 };

        self.ascent = ascent_px as u32;
        self.descent = descent_px as u32;
        self.size_px = size_px;
        self.line_height = self.ascent + self.descent + gap_px;

        let mut scratch = rasterize::RasterScratch::zeroed();

        for i in 0..ASCII_CACHE_COUNT {
            let codepoint = (0x20u8 + i as u8) as char;
            // Look up glyph ID for this codepoint via shaping's rasterize module.
            let glyph_id = match rasterize::glyph_id_for_char(font_data, codepoint) {
                Some(id) => id,
                None => continue,
            };
            let buf_offset = i * GLYPH_BUF_SIZE;
            let buf = &mut self.coverage[buf_offset..buf_offset + GLYPH_BUF_SIZE];
            let mut raster = rasterize::RasterBuffer {
                data: buf,
                width: GLYPH_MAX_W as u32,
                height: GLYPH_MAX_H as u32,
            };

            if let Some(m) = rasterize::rasterize(
                font_data,
                glyph_id,
                size_px as u16,
                &mut raster,
                &mut scratch,
            ) {
                self.glyphs[i] = CachedGlyph {
                    width: m.width,
                    height: m.height,
                    bearing_x: m.bearing_x,
                    bearing_y: m.bearing_y,
                    advance: m.advance,
                    buf_offset,
                };
            }
        }
    }
    /// Zero-initialize the cache. The struct is ~1.3 MiB -- callers with
    /// limited stack should allocate on the heap first, then call `populate`.
    pub const fn zeroed() -> Self {
        GlyphCache {
            glyphs: [CachedGlyph {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                advance: 0,
                buf_offset: 0,
            }; ASCII_CACHE_COUNT],
            coverage: [0u8; ASCII_CACHE_COUNT * GLYPH_BUF_SIZE],
            line_height: 0,
            ascent: 0,
            descent: 0,
            size_px: 0,
        }
    }
}
impl PixelFormat {
    /// Number of bytes per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Bgra8888 => 4,
        }
    }
}
impl<'a> Surface<'a> {
    /// Byte offset for pixel (x, y), or `None` if out of bounds.
    fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let offset = (y * self.stride + x * self.format.bytes_per_pixel()) as usize;
        let bpp = self.format.bytes_per_pixel() as usize;

        if offset + bpp <= self.data.len() {
            Some(offset)
        } else {
            None
        }
    }

    /// Blend a single pixel using source-over compositing.
    ///
    /// Reads the existing pixel, blends `color` on top, writes back. No-op
    /// if out of bounds or if `color` is fully transparent.
    pub fn blend_pixel(&mut self, x: u32, y: u32, color: Color) {
        if color.a == 255 {
            self.set_pixel(x, y, color);

            return;
        }
        if color.a == 0 {
            return;
        }

        if let Some(dst) = self.get_pixel(x, y) {
            self.set_pixel(x, y, color.blend_over(dst));
        }
    }
    /// Copy pixels from a source buffer onto this surface at (dst_x, dst_y).
    ///
    /// Clips to both source and destination bounds. Assumes source uses the
    /// same pixel format as this surface. Rows are copied via `copy_from_slice`
    /// for efficiency.
    pub fn blit(
        &mut self,
        src_data: &[u8],
        src_width: u32,
        src_height: u32,
        src_stride: u32,
        dst_x: u32,
        dst_y: u32,
    ) {
        if dst_x >= self.width || dst_y >= self.height {
            return;
        }

        let copy_w = min(src_width, self.width - dst_x);
        let copy_h = min(src_height, self.height - dst_y);
        let bpp = self.format.bytes_per_pixel() as usize;
        let row_bytes = copy_w as usize * bpp;

        for row in 0..copy_h {
            let src_off = (row * src_stride) as usize;
            let dst_off =
                ((dst_y + row) * self.stride + dst_x * self.format.bytes_per_pixel()) as usize;

            if src_off + row_bytes <= src_data.len() && dst_off + row_bytes <= self.data.len() {
                self.data[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&src_data[src_off..src_off + row_bytes]);
            }
        }
    }
    /// Blit source pixels onto this surface with per-pixel alpha blending.
    ///
    /// Each source pixel is composited over the destination using source-over.
    /// Fully transparent source pixels are skipped; fully opaque pixels
    /// overwrite without reading the destination (fast path).
    pub fn blit_blend(
        &mut self,
        src_data: &[u8],
        src_width: u32,
        src_height: u32,
        src_stride: u32,
        dst_x: u32,
        dst_y: u32,
    ) {
        if dst_x >= self.width || dst_y >= self.height {
            return;
        }

        let copy_w = min(src_width, self.width - dst_x);
        let copy_h = min(src_height, self.height - dst_y);
        let bpp = self.format.bytes_per_pixel() as usize;

        for row in 0..copy_h {
            for col in 0..copy_w {
                let src_off = (row * src_stride + col * self.format.bytes_per_pixel()) as usize;

                if src_off + bpp <= src_data.len() {
                    let src_color = Color::decode(&src_data[src_off..src_off + bpp], self.format);

                    if src_color.a == 255 {
                        self.set_pixel(dst_x + col, dst_y + row, src_color);
                    } else if src_color.a > 0 {
                        self.blend_pixel(dst_x + col, dst_y + row, src_color);
                    }
                }
            }
        }
    }
    /// Fill the entire surface with a solid color.
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }
    /// Draw a 3-channel subpixel coverage map (anti-aliased glyph) at
    /// position (x, y) in the given color. The coverage data has 3 bytes
    /// per pixel (R, G, B coverage), stored row-major:
    /// `coverage[(row * cov_width + col) * 3 + channel]` where channel
    /// 0=R, 1=G, 2=B.
    ///
    /// Each channel's coverage independently modulates the corresponding
    /// color channel's alpha, enabling LCD subpixel rendering (RGB sub-pixel
    /// order). This produces crisper text than greyscale antialiasing by
    /// exploiting the separate R, G, B sub-pixels of LCD displays.
    ///
    /// Blending is performed in linear light (sRGB gamma-correct) per
    /// channel. `x` and `y` can be negative. Clips to surface bounds.
    pub fn draw_coverage(
        &mut self,
        x: i32,
        y: i32,
        coverage: &[u8],
        cov_width: u32,
        cov_height: u32,
        color: Color,
    ) {
        // Pre-convert source color to linear space.
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;

        for row in 0..cov_height {
            for col in 0..cov_width {
                let base = ((row * cov_width + col) * 3) as usize;

                if base + 2 >= coverage.len() {
                    return;
                }

                let cov_r = coverage[base];
                let cov_g = coverage[base + 1];
                let cov_b = coverage[base + 2];

                // Skip if all channels are zero.
                if cov_r == 0 && cov_g == 0 && cov_b == 0 {
                    continue;
                }

                let px = x + col as i32;
                let py = y + row as i32;

                if px < 0 || py < 0 {
                    continue;
                }

                let ux = px as u32;
                let uy = py as u32;
                // Per-channel effective alpha: color.a * channel_coverage / 255.
                let alpha_r = (color.a as u32 * cov_r as u32 + 127) / 255;
                let alpha_g = (color.a as u32 * cov_g as u32 + 127) / 255;
                let alpha_b = (color.a as u32 * cov_b as u32 + 127) / 255;

                // Fast path: all channels full coverage + opaque color.
                if alpha_r >= 255 && alpha_g >= 255 && alpha_b >= 255 {
                    self.set_pixel(ux, uy, color);

                    continue;
                }

                if let Some(dst) = self.get_pixel(ux, uy) {
                    // Convert destination to linear space.
                    let dst_r_lin = SRGB_TO_LINEAR[dst.r as usize] as u32;
                    let dst_g_lin = SRGB_TO_LINEAR[dst.g as usize] as u32;
                    let dst_b_lin = SRGB_TO_LINEAR[dst.b as usize] as u32;
                    // Blend each channel independently in linear space.
                    let inv_r = 255 - alpha_r;
                    let inv_g = 255 - alpha_g;
                    let inv_b = 255 - alpha_b;
                    let out_r_lin = (dst_r_lin * inv_r + src_r_lin * alpha_r + 127) / 255;
                    let out_g_lin = (dst_g_lin * inv_g + src_g_lin * alpha_g + 127) / 255;
                    let out_b_lin = (dst_b_lin * inv_b + src_b_lin * alpha_b + 127) / 255;
                    // Convert back to sRGB.
                    let out_r = LINEAR_TO_SRGB[linear_to_idx(out_r_lin)];
                    let out_g = LINEAR_TO_SRGB[linear_to_idx(out_g_lin)];
                    let out_b = LINEAR_TO_SRGB[linear_to_idx(out_b_lin)];
                    // Alpha: use max channel alpha for the output alpha.
                    let max_alpha = if alpha_r > alpha_g { alpha_r } else { alpha_g };
                    let max_alpha = if alpha_b > max_alpha {
                        alpha_b
                    } else {
                        max_alpha
                    };
                    let out_a = dst.a as u32 + max_alpha * (255 - dst.a as u32) / 255;

                    self.set_pixel(
                        ux,
                        uy,
                        Color {
                            r: out_r,
                            g: out_g,
                            b: out_b,
                            a: if out_a > 255 { 255 } else { out_a as u8 },
                        },
                    );
                }
            }
        }
    }
    /// Draw a horizontal line. Clips to surface bounds.
    pub fn draw_hline(&mut self, x: u32, y: u32, w: u32, color: Color) {
        self.fill_rect(x, y, w, 1, color);
    }
    /// Draw a line using Bresenham's algorithm. Clips per-pixel.
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        let dx = abs(x1 - x0);
        let dy = abs(y1 - y0);
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;
        let mut cx = x0;
        let mut cy = y0;

        loop {
            // set_pixel clips negative/out-of-range via u32 conversion.
            if cx >= 0 && cy >= 0 {
                self.set_pixel(cx as u32, cy as u32, color);
            }
            if cx == x1 && cy == y1 {
                break;
            }

            let e2 = 2 * err;

            if e2 > -dy {
                err -= dy;
                cx += sx;
            }
            if e2 < dx {
                err += dx;
                cy += sy;
            }
        }
    }
    /// Draw a rectangle outline (1px border). Clips to surface bounds.
    ///
    /// The border is drawn inside the given bounds (the filled area is
    /// x..x+w, y..y+h including the border pixels).
    pub fn draw_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        if w == 0 || h == 0 {
            return;
        }

        // Top and bottom edges.
        self.draw_hline(x, y, w, color);

        if h > 1 {
            self.draw_hline(x, y + h - 1, w, color);
        }
        // Left and right edges (excluding corners already drawn).
        if h > 2 {
            self.draw_vline(x, y + 1, h - 2, color);

            if w > 1 {
                self.draw_vline(x + w - 1, y + 1, h - 2, color);
            }
        }
    }
    /// Draw a vertical line. Clips to surface bounds.
    pub fn draw_vline(&mut self, x: u32, y: u32, h: u32, color: Color) {
        self.fill_rect(x, y, 1, h, color);
    }
    /// Fill a rectangle with a solid color. Clips to surface bounds.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let x2 = min(x.saturating_add(w), self.width);
        let y2 = min(y.saturating_add(h), self.height);
        let pixel_count = (x2 - x) as usize;

        if pixel_count == 0 {
            return;
        }

        let encoded = color.encode(self.format);
        let pixel_u32 = u32::from_ne_bytes(encoded);
        let bpp = self.format.bytes_per_pixel();
        let ptr = self.data.as_mut_ptr();

        for row in y..y2 {
            let row_offset = (row * self.stride + x * bpp) as usize;

            // SAFETY: bounds checked above — x..x2 is within width, row is
            // within height, and stride * height <= data.len().
            unsafe {
                let row_ptr = ptr.add(row_offset) as *mut u32;

                for i in 0..pixel_count {
                    core::ptr::write(row_ptr.add(i), pixel_u32);
                }
            }
        }
    }
    /// Fill a rectangle with a vertical gradient from `color_top` to `color_bottom`.
    ///
    /// Each row linearly interpolates RGBA between the two colors. Row 0 gets
    /// `color_top`, row h-1 gets `color_bottom`. Clips to surface bounds.
    /// Integer math only. Useful for drop-shadow falloff effects.
    pub fn fill_gradient_v(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        color_top: Color,
        color_bottom: Color,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }

        let x2 = min(x.saturating_add(w), self.width);
        let y2 = min(y.saturating_add(h), self.height);

        // For h=1, just fill with color_top.
        if h == 1 {
            self.fill_rect(x, y, x2 - x, 1, color_top);

            return;
        }

        let denom = (h - 1) as u32;

        for row in y..y2 {
            let t = (row - y) as u32; // 0..h-1

            // Linearly interpolate each channel: c = top + (bottom - top) * t / denom.
            let r =
                (color_top.r as u32 * (denom - t) + color_bottom.r as u32 * t + denom / 2) / denom;
            let g =
                (color_top.g as u32 * (denom - t) + color_bottom.g as u32 * t + denom / 2) / denom;
            let b =
                (color_top.b as u32 * (denom - t) + color_bottom.b as u32 * t + denom / 2) / denom;
            let a =
                (color_top.a as u32 * (denom - t) + color_bottom.a as u32 * t + denom / 2) / denom;
            let row_color = Color {
                r: if r > 255 { 255 } else { r as u8 },
                g: if g > 255 { 255 } else { g as u8 },
                b: if b > 255 { 255 } else { b as u8 },
                a: if a > 255 { 255 } else { a as u8 },
            };

            self.fill_rect(x, row, x2 - x, 1, row_color);
        }
    }
    /// Fill a rectangle with alpha-blended color. Clips to surface bounds.
    ///
    /// Each destination pixel is blended with `color` using source-over.
    /// Opaque colors fast-path to `fill_rect`.
    pub fn fill_rect_blend(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        if color.a == 255 {
            self.fill_rect(x, y, w, h, color);

            return;
        }
        if color.a == 0 || w == 0 || h == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }

        let x2 = min(x.saturating_add(w), self.width);
        let y2 = min(y.saturating_add(h), self.height);

        for row in y..y2 {
            for col in x..x2 {
                self.blend_pixel(col, row, color);
            }
        }
    }
    /// Read a single pixel. Returns `None` if out of bounds.
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        if let Some(offset) = self.pixel_offset(x, y) {
            let bpp = self.format.bytes_per_pixel() as usize;

            Some(Color::decode(&self.data[offset..offset + bpp], self.format))
        } else {
            None
        }
    }
    /// Write a single pixel. No-op if out of bounds.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if let Some(offset) = self.pixel_offset(x, y) {
            let encoded = color.encode(self.format);
            let bpp = self.format.bytes_per_pixel() as usize;

            self.data[offset..offset + bpp].copy_from_slice(&encoded[..bpp]);
        }
    }
}
impl TextLayout {
    fn cols(&self) -> usize {
        if self.char_width == 0 {
            return 0;
        }

        (self.max_width / self.char_width) as usize
    }

    /// Return the visual line number (0-based) for a given byte offset.
    /// Uses the same wrapping rules as `layout_lines` and `byte_to_xy`.
    pub fn byte_to_visual_line(&self, text: &[u8], offset: usize) -> u32 {
        let cols = self.cols();

        if cols == 0 || text.is_empty() {
            return 0;
        }

        let target = if offset > text.len() {
            text.len()
        } else {
            offset
        };
        let mut col = 0usize;
        let mut row = 0u32;

        for (i, &byte) in text.iter().enumerate() {
            if i == target {
                return row;
            }

            if byte == b'\n' {
                row += 1;
                col = 0;

                continue;
            }

            if col >= cols {
                row += 1;
                col = 0;
            }

            col += 1;
        }

        // offset == text.len()
        row
    }
    /// Map a byte offset to pixel coordinates relative to the text origin.
    pub fn byte_to_xy(&self, text: &[u8], offset: usize) -> (u32, u32) {
        let cols = self.cols();

        if cols == 0 || text.is_empty() {
            return (0, 0);
        }

        let target = if offset > text.len() {
            text.len()
        } else {
            offset
        };
        let mut col = 0usize;
        let mut row = 0u32;

        for (i, &byte) in text.iter().enumerate() {
            if i == target {
                return (col as u32 * self.char_width, row * self.line_height);
            }

            if byte == b'\n' {
                row += 1;
                col = 0;

                continue;
            }

            if col >= cols {
                row += 1;
                col = 0;
            }

            col += 1;
        }

        // offset == text.len(): cursor at end.
        (col as u32 * self.char_width, row * self.line_height)
    }
    /// Layout and draw text using pre-rasterized TrueType glyphs.
    /// Anti-aliased rendering via coverage maps. Same interface as `draw`.
    pub fn draw_tt(
        &self,
        fb: &mut Surface,
        text: &[u8],
        origin_x: u32,
        origin_y: u32,
        cursor_offset: usize,
        cache: &GlyphCache,
        text_color: Color,
        cursor_color: Color,
        max_y: u32,
    ) -> (u32, u32) {
        self.draw_tt_sel(
            fb,
            text,
            origin_x,
            origin_y,
            cursor_offset,
            cache,
            text_color,
            cursor_color,
            max_y,
            0,
            0,
            Color::TRANSPARENT,
        )
    }
    /// Layout and draw text with optional selection highlight.
    ///
    /// `sel_start` and `sel_end` define the selected byte range (half-open).
    /// When `sel_start < sel_end`, characters in that range are rendered with
    /// `sel_color` as a background highlight. When `sel_start == sel_end`
    /// (or both are 0), no selection highlight is drawn.
    ///
    /// The selection is normalized internally: if `sel_start > sel_end`, they
    /// are swapped so the highlight always covers the correct range regardless
    /// of anchor vs cursor ordering.
    ///
    /// Equivalent to `draw_tt_sel_scroll` with `scroll_offset = 0`.
    pub fn draw_tt_sel(
        &self,
        fb: &mut Surface,
        text: &[u8],
        origin_x: u32,
        origin_y: u32,
        cursor_offset: usize,
        cache: &GlyphCache,
        text_color: Color,
        cursor_color: Color,
        max_y: u32,
        sel_start: usize,
        sel_end: usize,
        sel_color: Color,
    ) -> (u32, u32) {
        self.draw_tt_sel_scroll(
            fb,
            text,
            origin_x,
            origin_y,
            cursor_offset,
            cache,
            text_color,
            cursor_color,
            max_y,
            sel_start,
            sel_end,
            sel_color,
            0,
        )
    }
    /// Layout and draw text with selection and vertical scrolling.
    ///
    /// `scroll_offset` is the number of visual lines to skip at the top.
    /// Lines above the scroll offset are not drawn. The cursor and selection
    /// positions are adjusted so that line `scroll_offset` appears at
    /// `origin_y`. Content below `max_y` is clipped as before.
    ///
    /// Returns `(cursor_x, cursor_y)` in surface coordinates (accounting for
    /// scroll). If the cursor is above the viewport, returns `(origin_x, 0)`.
    pub fn draw_tt_sel_scroll(
        &self,
        fb: &mut Surface,
        text: &[u8],
        origin_x: u32,
        origin_y: u32,
        cursor_offset: usize,
        cache: &GlyphCache,
        text_color: Color,
        cursor_color: Color,
        max_y: u32,
        sel_start: usize,
        sel_end: usize,
        sel_color: Color,
        scroll_offset: u32,
    ) -> (u32, u32) {
        let cols = self.cols();
        let mut col = 0usize;
        let mut row = 0u32;
        let mut cursor_x = origin_x;
        let mut cursor_y = origin_y;
        let baseline_offset = cache.ascent;
        // Normalize selection range.
        let (s_lo, s_hi) = if sel_start <= sel_end {
            (sel_start, sel_end)
        } else {
            (sel_end, sel_start)
        };
        let has_selection = s_lo < s_hi;

        for (i, &byte) in text.iter().enumerate() {
            // Compute the visual Y for this row, accounting for scroll.
            // If this row is above the scroll offset, it's not drawn.
            let visual_row = row as i32 - scroll_offset as i32;

            if i == cursor_offset {
                if visual_row >= 0 {
                    cursor_x = origin_x + col as u32 * self.char_width;
                    cursor_y = origin_y + visual_row as u32 * self.line_height;
                } else {
                    // Cursor is above viewport (shouldn't happen with auto-scroll,
                    // but handle gracefully).
                    cursor_x = origin_x;
                    cursor_y = 0;
                }
            }

            if byte == b'\n' {
                col = 0;
                row += 1;

                continue;
            }

            if cols > 0 && col >= cols {
                col = 0;
                row += 1;

                // Recompute visual_row after wrap.
                let visual_row = row as i32 - scroll_offset as i32;

                if visual_row >= 0 {
                    let py = origin_y + visual_row as u32 * self.line_height;

                    if py > max_y {
                        break;
                    }
                }
            }

            // Only draw if this row is within the visible viewport.
            let visual_row = row as i32 - scroll_offset as i32;

            if visual_row >= 0 {
                let py = origin_y + visual_row as u32 * self.line_height;

                if py > max_y {
                    break;
                }

                // Draw selection highlight behind the character if selected.
                if has_selection && i >= s_lo && i < s_hi && sel_color.a > 0 {
                    let hx = origin_x + col as u32 * self.char_width;

                    fb.fill_rect_blend(hx, py, self.char_width, cache.line_height, sel_color);
                }

                if let Some((glyph, coverage)) = cache.get(byte) {
                    if glyph.width > 0 && glyph.height > 0 {
                        let gx =
                            origin_x as i32 + col as i32 * self.char_width as i32 + glyph.bearing_x;
                        let gy = py as i32 + baseline_offset as i32 - glyph.bearing_y;

                        fb.draw_coverage(gx, gy, coverage, glyph.width, glyph.height, text_color);
                    }
                }
            }

            col += 1;
        }

        // Cursor at end of text (but not when cursor is disabled via usize::MAX).
        if cursor_offset != usize::MAX && cursor_offset >= text.len() {
            let visual_row = row as i32 - scroll_offset as i32;

            if visual_row >= 0 {
                let py = origin_y + visual_row as u32 * self.line_height;

                cursor_x = origin_x + col as u32 * self.char_width;
                cursor_y = py;
            } else {
                cursor_x = origin_x;
                cursor_y = 0;
            }
        }

        // Draw cursor: thin bar (no cursor when disabled or there's a visible selection).
        if cursor_offset != usize::MAX
            && !has_selection
            && cursor_y >= origin_y
            && cursor_y <= max_y
        {
            fb.fill_rect(cursor_x, cursor_y, 2, cache.line_height, cursor_color);
        }

        (cursor_x, cursor_y)
    }
    /// Like `draw_tt_sel_scroll`, but only renders visual lines in the range
    /// `[first_vis_line, last_vis_line]` (inclusive, viewport-relative — i.e.,
    /// visual line 0 is the first visible line after `scroll_offset`).
    ///
    /// The caller is responsible for clearing those lines in the surface
    /// before calling this method (alpha blending is additive).
    ///
    /// Returns `(cursor_x, cursor_y)` as in `draw_tt_sel_scroll`.
    pub fn draw_tt_sel_scroll_lines(
        &self,
        fb: &mut Surface,
        text: &[u8],
        origin_x: u32,
        origin_y: u32,
        cursor_offset: usize,
        cache: &GlyphCache,
        text_color: Color,
        cursor_color: Color,
        max_y: u32,
        sel_start: usize,
        sel_end: usize,
        sel_color: Color,
        scroll_offset: u32,
        first_vis_line: u32,
        last_vis_line: u32,
    ) -> (u32, u32) {
        let cols = self.cols();
        let mut col = 0usize;
        let mut row = 0u32;
        let mut cursor_x = origin_x;
        let mut cursor_y = origin_y;
        let baseline_offset = cache.ascent;
        // Normalize selection range.
        let (s_lo, s_hi) = if sel_start <= sel_end {
            (sel_start, sel_end)
        } else {
            (sel_end, sel_start)
        };
        let has_selection = s_lo < s_hi;

        for (i, &byte) in text.iter().enumerate() {
            let visual_row = row as i32 - scroll_offset as i32;

            if i == cursor_offset {
                if visual_row >= 0 {
                    cursor_x = origin_x + col as u32 * self.char_width;
                    cursor_y = origin_y + visual_row as u32 * self.line_height;
                } else {
                    cursor_x = origin_x;
                    cursor_y = 0;
                }
            }

            if byte == b'\n' {
                col = 0;
                row += 1;
                continue;
            }

            if cols > 0 && col >= cols {
                col = 0;
                row += 1;

                let visual_row = row as i32 - scroll_offset as i32;

                if visual_row >= 0 {
                    let py = origin_y + visual_row as u32 * self.line_height;

                    if py > max_y {
                        break;
                    }
                }
            }

            let visual_row = row as i32 - scroll_offset as i32;

            if visual_row >= 0 {
                let vis = visual_row as u32;
                let py = origin_y + vis * self.line_height;

                if py > max_y {
                    break;
                }

                // Only render if this visual line is within the requested range.
                if vis >= first_vis_line && vis <= last_vis_line {
                    // Draw selection highlight behind the character if selected.
                    if has_selection && i >= s_lo && i < s_hi && sel_color.a > 0 {
                        let hx = origin_x + col as u32 * self.char_width;

                        fb.fill_rect_blend(hx, py, self.char_width, cache.line_height, sel_color);
                    }

                    if let Some((glyph, coverage)) = cache.get(byte) {
                        if glyph.width > 0 && glyph.height > 0 {
                            let gx = origin_x as i32
                                + col as i32 * self.char_width as i32
                                + glyph.bearing_x;
                            let gy = py as i32 + baseline_offset as i32 - glyph.bearing_y;

                            fb.draw_coverage(
                                gx,
                                gy,
                                coverage,
                                glyph.width,
                                glyph.height,
                                text_color,
                            );
                        }
                    }
                }
            }

            col += 1;
        }

        // Cursor at end of text (not when disabled via usize::MAX).
        if cursor_offset != usize::MAX && cursor_offset >= text.len() {
            let visual_row = row as i32 - scroll_offset as i32;

            if visual_row >= 0 {
                let py = origin_y + visual_row as u32 * self.line_height;

                cursor_x = origin_x + col as u32 * self.char_width;
                cursor_y = py;
            } else {
                cursor_x = origin_x;
                cursor_y = 0;
            }
        }

        // Draw cursor only if enabled and within the requested line range.
        if cursor_offset != usize::MAX
            && !has_selection
            && cursor_y >= origin_y
            && cursor_y <= max_y
        {
            let cursor_vis_line = (cursor_y - origin_y) / self.line_height;

            if cursor_vis_line >= first_vis_line && cursor_vis_line <= last_vis_line {
                fb.fill_rect(cursor_x, cursor_y, 2, cache.line_height, cursor_color);
            }
        }

        (cursor_x, cursor_y)
    }
    /// Count the total number of visual lines in the text.
    /// Empty text returns 0. A single line of text returns 1.
    pub fn total_visual_lines(&self, text: &[u8]) -> u32 {
        if text.is_empty() {
            return 0;
        }

        let mut count = 0u32;

        self.layout_lines(text, |_, _, _| {
            count += 1;
        });

        count
    }
    /// Compute the scroll offset (in visual lines) needed to keep the cursor
    /// visible within a viewport of `viewport_lines` visual lines.
    ///
    /// - If the cursor is above the current viewport, scrolls up.
    /// - If the cursor is below the current viewport, scrolls down.
    /// - Otherwise returns the current scroll unchanged.
    pub fn scroll_for_cursor(
        &self,
        text: &[u8],
        cursor_offset: usize,
        current_scroll: u32,
        viewport_lines: u32,
    ) -> u32 {
        if viewport_lines == 0 {
            return 0;
        }

        let cursor_line = self.byte_to_visual_line(text, cursor_offset);

        // If cursor is above the visible region, scroll up to show it.
        if cursor_line < current_scroll {
            return cursor_line;
        }

        // If cursor is below the visible region, scroll down.
        let last_visible = current_scroll + viewport_lines - 1;

        if cursor_line > last_visible {
            return cursor_line - (viewport_lines - 1);
        }

        current_scroll
    }
    /// Walk text and call `f(line_start, line_end, visual_row)` for each
    /// visual line. Handles hard newlines and soft wrap at max_width.
    pub fn layout_lines(&self, text: &[u8], mut f: impl FnMut(usize, usize, u32)) {
        if text.is_empty() {
            return;
        }

        let cols = self.cols();

        if cols == 0 {
            return;
        }

        let mut row = 0u32;
        let mut line_start = 0usize;
        let mut col = 0usize;

        for (i, &byte) in text.iter().enumerate() {
            if byte == b'\n' {
                f(line_start, i, row);

                row += 1;
                line_start = i + 1;
                col = 0;

                continue;
            }
            if col >= cols {
                f(line_start, i, row);

                row += 1;
                line_start = i;
                col = 0;
            }

            col += 1;
        }

        // Emit final line (may be empty for trailing newline).
        f(line_start, text.len(), row);
    }
    /// Map pixel coordinates to a byte offset (hit testing). Coordinates
    /// are relative to the text origin. Rounds to nearest character boundary.
    pub fn xy_to_byte(&self, text: &[u8], x: u32, y: u32) -> usize {
        let cols = self.cols();

        if cols == 0 || text.is_empty() {
            return 0;
        }

        let target_row = y / self.line_height;
        let half_char = self.char_width / 2;
        let target_col = (x + half_char) / self.char_width;
        let mut col = 0usize;
        let mut row = 0u32;

        for (i, &byte) in text.iter().enumerate() {
            if byte == b'\n' {
                if row == target_row {
                    return i; // click past line end -> snap to newline
                }

                row += 1;
                col = 0;

                continue;
            }

            if col >= cols {
                row += 1;
                col = 0;
            }

            if row == target_row && col >= target_col as usize {
                return i;
            }
            if row > target_row {
                return i;
            }

            col += 1;
        }

        text.len()
    }
}

fn abs(x: i32) -> i32 {
    if x < 0 {
        -x
    } else {
        x
    }
}

// ---------------------------------------------------------------------------
// Xorshift32 PRNG — deterministic noise generation
// ---------------------------------------------------------------------------

/// A simple 32-bit xorshift PRNG for deterministic noise generation.
/// Period is 2^32 - 1. State must never be zero.
pub struct Xorshift32 {
    pub state: u32,
}

impl Xorshift32 {
    /// Create a new PRNG with the given seed. Seed must not be zero.
    pub const fn new(seed: u32) -> Self {
        // Ensure seed is never zero (xorshift32 has a zero fixed-point).
        let s = if seed == 0 { 0x12345678 } else { seed };

        Xorshift32 { state: s }
    }

    /// Generate the next pseudo-random u32 value.
    pub fn next(&mut self) -> u32 {
        let mut x = self.state;

        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;

        x
    }
    /// Generate a random value in the range [-amplitude, +amplitude] (inclusive).
    /// Uses integer math only. `amplitude` should be small (e.g., 2–4).
    pub fn noise(&mut self, amplitude: u32) -> i32 {
        let range = amplitude * 2 + 1; // e.g., amplitude=3 → range=7
        let val = self.next();

        // Map to [0, range) then shift to [-amplitude, +amplitude].
        (val % range) as i32 - amplitude as i32
    }
}

// ---------------------------------------------------------------------------
// Radial gradient with noise — background generation
// ---------------------------------------------------------------------------

/// Clamp an i32 to [0, 255] and return as u8.
fn clamp_u8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}
/// Convert a linear light value (0–65535 u32) to a LINEAR_TO_SRGB table index.
/// The table has 4096 entries; index is `value >> 4`, clamped to 4095.
fn linear_to_idx(v: u32) -> usize {
    let idx = v >> 4;

    if idx > 4095 {
        4095
    } else {
        idx as usize
    }
}
fn min(a: u32, b: u32) -> u32 {
    if a < b {
        a
    } else {
        b
    }
}

/// 4×4 Bayer ordered-dither threshold matrix (0–15).
///
/// Each entry represents a threshold at which a fractional color value
/// rounds UP instead of down. By distributing these thresholds in a
/// structured 4×4 pattern, quantization bands are broken into an
/// imperceptible stipple — far superior to random noise for gradient
/// banding elimination.
///
/// The matrix is indexed as `BAYER4[y & 3][x & 3]`.
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Compute the gradient parameters needed for radial gradient rendering.
/// Returns (cx, cy, max_dist_sq).
fn gradient_params(w: u32, h: u32) -> (u32, u32, u64) {
    let cx = w / 2;
    let cy = h / 2;
    let max_dx = if cx > w - cx - 1 { cx } else { w - cx - 1 };
    let max_dy = if cy > h - cy - 1 { cy } else { h - cy - 1 };
    let max_dist_sq = (max_dx as u64) * (max_dx as u64) + (max_dy as u64) * (max_dy as u64);
    let max_dist_sq = if max_dist_sq == 0 { 1 } else { max_dist_sq };

    (cx, cy, max_dist_sq)
}

/// Compute a single dithered gradient pixel at coordinates (x, y)
/// within a surface of dimensions (w, h), with center at (cx, cy)
/// and max squared distance `max_dist_sq`.
///
/// Uses Bayer 4×4 ordered dithering: the continuous gradient value
/// (in 20.12 fixed-point) is offset by a Bayer threshold before
/// truncating to 8-bit, so the rounding boundary varies per pixel in
/// a structured pattern that breaks up quantization bands.
fn gradient_pixel(
    x: u32,
    y: u32,
    cx: u32,
    cy: u32,
    max_dist_sq: u64,
    center_color: Color,
    edge_color: Color,
) -> Color {
    let dx = if x >= cx { x - cx } else { cx - x };
    let dy = if y >= cy { y - cy } else { cy - y };
    let dist_sq = (dx as u64) * (dx as u64) + (dy as u64) * (dy as u64);
    // Interpolation factor in 20.12 fixed-point (0..255 << 12 range).
    // t_fp = dist_sq * (255 << 12) / max_dist_sq.
    let t_fp = ((dist_sq * (255 << 12)) / max_dist_sq) as u32;
    let t_fp = if t_fp > (255 << 12) { 255 << 12 } else { t_fp };
    let inv_t_fp = (255 << 12) - t_fp;
    // Interpolate each channel in fixed-point: result has 12 fraction bits.
    let base_r_fp =
        (center_color.r as u32 * inv_t_fp + edge_color.r as u32 * t_fp + (127 << 12)) / 255;
    let base_g_fp =
        (center_color.g as u32 * inv_t_fp + edge_color.g as u32 * t_fp + (127 << 12)) / 255;
    let base_b_fp =
        (center_color.b as u32 * inv_t_fp + edge_color.b as u32 * t_fp + (127 << 12)) / 255;
    // Apply Bayer dither: add threshold * (1 << 12) / 16 to the
    // fixed-point value before truncating.
    // threshold is 0..15, so dither_offset ranges from 0..(15/16 of
    // one integer unit), i.e., 0..3840 in fixed-point.
    let threshold = BAYER4[(y & 3) as usize][(x & 3) as usize] as u32;
    let dither_offset = (threshold << 12) / 16; // 0..3840

    // Add dither offset then truncate (shift right by 12).
    let r = clamp_u8(((base_r_fp + dither_offset) >> 12) as i32);
    let g = clamp_u8(((base_g_fp + dither_offset) >> 12) as i32);
    let b = clamp_u8(((base_b_fp + dither_offset) >> 12) as i32);

    Color::rgb(r, g, b)
}

/// Fill a surface with a radial gradient from `center_color` (at the center
/// of the surface) to `edge_color` (at the corners), using ordered dithering
/// to eliminate gradient banding.
///
/// The gradient uses an approximation of Euclidean distance (no sqrt, no FP):
/// `d² = dx² + dy²` is compared to `max_d²` to interpolate linearly.
///
/// Banding is eliminated via a 4×4 Bayer ordered-dither matrix: the
/// continuous gradient value is offset by a structured threshold before
/// truncating to 8-bit, so quantization bands break into an imperceptible
/// stipple pattern. The dither is fully deterministic and depends only on
/// pixel coordinates — no PRNG state.
///
/// `_noise_amplitude` and `_prng_seed` are accepted for API compatibility
/// but ignored; dithering replaces random noise entirely.
///
/// All math is integer only (no floating point).
pub fn fill_radial_gradient_noise(
    surf: &mut Surface,
    center_color: Color,
    edge_color: Color,
    _noise_amplitude: u32,
    _prng_seed: u32,
) {
    let w = surf.width;
    let h = surf.height;

    if w == 0 || h == 0 {
        return;
    }

    let (cx, cy, max_dist_sq) = gradient_params(w, h);
    let bpp = surf.format.bytes_per_pixel();

    for y in 0..h {
        for x in 0..w {
            let color = gradient_pixel(x, y, cx, cy, max_dist_sq, center_color, edge_color);
            let offset = (y * surf.stride + x * bpp) as usize;
            let encoded = color.encode(surf.format);
            let end = offset + bpp as usize;

            if end <= surf.data.len() {
                surf.data[offset..end].copy_from_slice(&encoded[..bpp as usize]);
            }
        }
    }
}

/// Fill specific rows of a surface with a radial gradient using ordered
/// dithering. Produces pixels identical to `fill_radial_gradient_noise`
/// for the same coordinates — the dither pattern depends only on (x, y),
/// not on PRNG state.
///
/// `start_y` is the first row to fill (inclusive); `row_count` is the
/// number of rows. Rows outside the surface bounds are silently skipped.
pub fn fill_radial_gradient_rows(
    surf: &mut Surface,
    center_color: Color,
    edge_color: Color,
    start_y: u32,
    row_count: u32,
) {
    let w = surf.width;
    let h = surf.height;

    if w == 0 || h == 0 || row_count == 0 {
        return;
    }

    let (cx, cy, max_dist_sq) = gradient_params(w, h);
    let bpp = surf.format.bytes_per_pixel();
    let end_y = if start_y + row_count > h {
        h
    } else {
        start_y + row_count
    };

    for y in start_y..end_y {
        for x in 0..w {
            let color = gradient_pixel(x, y, cx, cy, max_dist_sq, center_color, edge_color);
            let offset = (y * surf.stride + x * bpp) as usize;
            let encoded = color.encode(surf.format);
            let end = offset + bpp as usize;

            if end <= surf.data.len() {
                surf.data[offset..end].copy_from_slice(&encoded[..bpp as usize]);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-surface compositing
// ---------------------------------------------------------------------------

/// A compositing surface: a pixel buffer with position, z-order, and visibility.
///
/// The compositor manages a set of these. On each frame, surfaces are composited
/// back-to-front (lowest z first) into the framebuffer using alpha blending.
///
/// Z-ordering convention (bottom to top):
///   0  = background
///   10 = content area
///   15 = shadows
///   20 = chrome (title bar)
pub struct CompositeSurface<'a> {
    pub surface: Surface<'a>,
    /// X position in framebuffer coordinates. Can be negative (partially offscreen).
    pub x: i32,
    /// Y position in framebuffer coordinates. Can be negative (partially offscreen).
    pub y: i32,
    /// Z-order: lower = further back. Composited in ascending z order.
    pub z: u16,
    /// Whether this surface participates in compositing.
    pub visible: bool,
}

/// Composite surfaces back-to-front onto a destination framebuffer.
///
/// Surfaces are sorted by z-order (ascending) and blitted with alpha blending.
/// Invisible surfaces are skipped. Surfaces may overlap and may extend outside
/// the destination bounds (clipped automatically by blit_blend).
///
/// The destination is NOT cleared — the caller should clear it beforehand if
/// needed, or include a full-screen background surface at z=0.
pub fn composite_surfaces(dst: &mut Surface, surfaces: &[&CompositeSurface]) {
    // Sort indices by z-order. We use a simple insertion sort since the number
    // of surfaces is small (typically 3-6).
    const MAX_SURFACES: usize = 16;

    let count = if surfaces.len() > MAX_SURFACES {
        MAX_SURFACES
    } else {
        surfaces.len()
    };
    let mut order: [usize; MAX_SURFACES] = [0; MAX_SURFACES];
    let mut i = 0;

    while i < count {
        order[i] = i;
        i += 1;
    }

    // Insertion sort by z-order.
    let mut j = 1;

    while j < count {
        let key = order[j];
        let key_z = surfaces[key].z;
        let mut k = j;

        while k > 0 && surfaces[order[k - 1]].z > key_z {
            order[k] = order[k - 1];
            k -= 1;
        }

        order[k] = key;
        j += 1;
    }

    // Composite back-to-front.
    let mut idx = 0;

    while idx < count {
        let s = surfaces[order[idx]];

        idx += 1;

        if !s.visible {
            continue;
        }

        // Handle negative offsets by computing source clip region.
        let src_x_start: u32 = if s.x < 0 { (-s.x) as u32 } else { 0 };
        let src_y_start: u32 = if s.y < 0 { (-s.y) as u32 } else { 0 };
        let dst_x: u32 = if s.x < 0 { 0 } else { s.x as u32 };
        let dst_y: u32 = if s.y < 0 { 0 } else { s.y as u32 };
        let src_w = s.surface.width;
        let src_h = s.surface.height;

        if src_x_start >= src_w || src_y_start >= src_h {
            continue; // Entirely off-screen to the left/top.
        }

        let visible_w = src_w - src_x_start;
        let visible_h = src_h - src_y_start;
        // Build a sub-region of the source data for blit_blend.
        // blit_blend takes src_data, src_width, src_height, src_stride.
        // We offset into the source buffer to skip the clipped rows/cols.
        let src_offset = (src_y_start * s.surface.stride
            + src_x_start * s.surface.format.bytes_per_pixel()) as usize;

        if src_offset < s.surface.data.len() {
            dst.blit_blend(
                &s.surface.data[src_offset..],
                visible_w,
                visible_h,
                s.surface.stride,
                dst_x,
                dst_y,
            );
        }
    }
}
/// Composite surfaces back-to-front onto a rectangular sub-region of the
/// destination framebuffer. Only pixels within `(rx, ry, rw, rh)` are
/// written. Surfaces are sorted by z-order as in `composite_surfaces`.
///
/// This is the damage-tracked variant: instead of re-compositing the entire
/// framebuffer, only the dirty region is updated.
pub fn composite_surfaces_rect(
    dst: &mut Surface,
    surfaces: &[&CompositeSurface],
    rx: u32,
    ry: u32,
    rw: u32,
    rh: u32,
) {
    if rw == 0 || rh == 0 {
        return;
    }

    // Sort indices by z-order (same insertion sort as composite_surfaces).
    const MAX_SURFACES: usize = 16;

    let count = if surfaces.len() > MAX_SURFACES {
        MAX_SURFACES
    } else {
        surfaces.len()
    };
    let mut order: [usize; MAX_SURFACES] = [0; MAX_SURFACES];
    let mut i = 0;

    while i < count {
        order[i] = i;
        i += 1;
    }

    let mut j = 1;

    while j < count {
        let key = order[j];
        let key_z = surfaces[key].z;
        let mut k = j;

        while k > 0 && surfaces[order[k - 1]].z > key_z {
            order[k] = order[k - 1];
            k -= 1;
        }

        order[k] = key;
        j += 1;
    }

    // Clamp the rect to destination bounds.
    let rx_end = min(rx + rw, dst.width);
    let ry_end = min(ry + rh, dst.height);

    if rx >= rx_end || ry >= ry_end {
        return;
    }

    // For each surface, composite only the intersection with the dirty rect.
    let mut idx = 0;
    while idx < count {
        let s = surfaces[order[idx]];

        idx += 1;

        if !s.visible {
            continue;
        }

        // Compute the region of the surface that overlaps the dirty rect in
        // framebuffer coordinates.
        let surf_fb_x0 = if s.x < 0 { 0i32 } else { s.x };
        let surf_fb_y0 = if s.y < 0 { 0i32 } else { s.y };
        let surf_fb_x1 = s.x + s.surface.width as i32;
        let surf_fb_y1 = s.y + s.surface.height as i32;
        // Intersect surface's FB region with the dirty rect.
        let ix0 = if surf_fb_x0 > rx as i32 {
            surf_fb_x0
        } else {
            rx as i32
        };
        let iy0 = if surf_fb_y0 > ry as i32 {
            surf_fb_y0
        } else {
            ry as i32
        };
        let ix1 = if surf_fb_x1 < rx_end as i32 {
            surf_fb_x1
        } else {
            rx_end as i32
        };
        let iy1 = if surf_fb_y1 < ry_end as i32 {
            surf_fb_y1
        } else {
            ry_end as i32
        };

        if ix0 >= ix1 || iy0 >= iy1 {
            continue; // No overlap.
        }

        // Compute source coordinates in the surface's local space.
        let src_x = (ix0 - s.x) as u32;
        let src_y = (iy0 - s.y) as u32;
        let blit_w = (ix1 - ix0) as u32;
        let blit_h = (iy1 - iy0) as u32;
        let src_offset =
            (src_y * s.surface.stride + src_x * s.surface.format.bytes_per_pixel()) as usize;

        if src_offset < s.surface.data.len() {
            dst.blit_blend(
                &s.surface.data[src_offset..],
                blit_w,
                blit_h,
                s.surface.stride,
                ix0 as u32,
                iy0 as u32,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Mouse cursor — procedural arrow cursor generation
// ---------------------------------------------------------------------------

/// Width of the procedural arrow cursor in pixels.
pub const CURSOR_W: u32 = 12;
/// Height of the procedural arrow cursor in pixels.
pub const CURSOR_H: u32 = 16;

/// Procedural arrow cursor bitmap: 1 = fill (white), 2 = outline (dark grey),
/// 0 = transparent. 12 wide × 16 tall, stored row-major.
///
/// Shape: classic arrow pointer pointing up-left.
const CURSOR_BITMAP: [u8; (CURSOR_W * CURSOR_H) as usize] = [
    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  0
    2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  1
    2, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  2
    2, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, //  3
    2, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, //  4
    2, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, //  5
    2, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, //  6
    2, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, //  7
    2, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, //  8
    2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, //  9
    2, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 0, // 10
    2, 1, 1, 2, 1, 1, 2, 0, 0, 0, 0, 0, // 11
    2, 1, 2, 0, 2, 1, 1, 2, 0, 0, 0, 0, // 12
    2, 2, 0, 0, 2, 1, 1, 2, 0, 0, 0, 0, // 13
    2, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0, 0, // 14
    0, 0, 0, 0, 0, 2, 2, 2, 0, 0, 0, 0, // 15
];

/// Render the procedural arrow cursor onto a BGRA8888 pixel buffer.
///
/// The buffer must be at least `CURSOR_W * CURSOR_H * 4` bytes.
/// Uses palette colors: CURSOR_FILL (white) for the fill, CURSOR_OUTLINE
/// (dark grey) for the outline, and transparent (alpha 0) elsewhere.
pub fn render_cursor(buf: &mut [u8]) {
    let stride = CURSOR_W * 4;
    let total = (CURSOR_W * CURSOR_H * 4) as usize;

    if buf.len() < total {
        return;
    }

    // Clear to fully transparent.
    let mut i = 0;

    while i < total {
        buf[i] = 0; // B
        buf[i + 1] = 0; // G
        buf[i + 2] = 0; // R
        buf[i + 3] = 0; // A
        i += 4;
    }

    let fill = CURSOR_FILL;
    let outline = CURSOR_OUTLINE;
    let mut y = 0u32;

    while y < CURSOR_H {
        let mut x = 0u32;

        while x < CURSOR_W {
            let idx = (y * CURSOR_W + x) as usize;
            let color = match CURSOR_BITMAP[idx] {
                1 => fill,
                2 => outline,
                _ => {
                    x += 1;
                    continue;
                }
            };
            let off = (y * stride + x * 4) as usize;
            let encoded = color.encode(PixelFormat::Bgra8888);

            buf[off] = encoded[0];
            buf[off + 1] = encoded[1];
            buf[off + 2] = encoded[2];
            buf[off + 3] = encoded[3];

            x += 1;
        }
        y += 1;
    }
}
/// Scale an absolute pointer coordinate from the [0, 32767] range to
/// [0, max_pixels). Uses integer math: `coord * max_pixels / 32768`.
/// The divisor is 32768 (not 32767) to ensure the result never equals
/// max_pixels (stays in [0, max_pixels-1]).
pub fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;

    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}

// ---------------------------------------------------------------------------
// Damage tracking — dirty rectangle management for partial GPU transfer
// ---------------------------------------------------------------------------

/// Maximum number of dirty rects that fit in a MSG_PRESENT payload.
/// Payload = 60 bytes. buffer_index (4) + rect_count (4) + pad (4) = 12.
/// Remaining = 48 bytes / 8 bytes per rect = 6 rects.
pub const MAX_DIRTY_RECTS: usize = 6;

/// Collects dirty rectangles during a render pass.
///
/// When the number of rects exceeds MAX_DIRTY_RECTS, the tracker
/// falls back to a single full-screen rect (signaled by `full_screen = true`).
pub struct DamageTracker {
    pub rects: [DirtyRect; MAX_DIRTY_RECTS],
    pub count: usize,
    pub full_screen: bool,
    fb_width: u16,
    fb_height: u16,
}

impl DamageTracker {
    /// Create a new damage tracker for the given framebuffer dimensions.
    pub const fn new(fb_width: u16, fb_height: u16) -> Self {
        Self {
            rects: [DirtyRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            }; MAX_DIRTY_RECTS],
            count: 0,
            full_screen: false,
            fb_width,
            fb_height,
        }
    }

    /// Add a dirty rectangle. If too many rects accumulate, falls back
    /// to full-screen damage.
    pub fn add(&mut self, x: u16, y: u16, w: u16, h: u16) {
        if self.full_screen || w == 0 || h == 0 {
            return;
        }

        if self.count >= MAX_DIRTY_RECTS {
            self.full_screen = true;

            return;
        }

        self.rects[self.count] = DirtyRect::new(x, y, w, h);
        self.count += 1;
    }
    /// Get the bounding box of all dirty rects, or full screen if needed.
    pub fn bounding_box(&self) -> DirtyRect {
        if self.full_screen || self.count == 0 {
            DirtyRect::new(0, 0, self.fb_width, self.fb_height)
        } else {
            DirtyRect::union_all(&self.rects[..self.count])
        }
    }
    /// Get the dirty rects for this frame. Returns `None` if full-screen
    /// transfer is needed (either explicitly marked or overflow).
    pub fn dirty_rects(&self) -> Option<&[DirtyRect]> {
        if self.full_screen || self.count == 0 {
            None
        } else {
            Some(&self.rects[..self.count])
        }
    }
    /// Mark the entire framebuffer as dirty.
    pub fn mark_full_screen(&mut self) {
        self.full_screen = true;
    }
    /// Reset the tracker for a new frame.
    pub fn reset(&mut self) {
        self.count = 0;
        self.full_screen = false;
    }
}
