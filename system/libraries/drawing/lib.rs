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

include!("font_data.rs");
include!("gamma_tables.rs");
include!("png.rs");
include!("rasterizer.rs");
include!("truetype.rs");

const GLYPH_FIRST: u8 = 0x20;
const GLYPH_LAST: u8 = 0x7E;
const GLYPH_COUNT: usize = (GLYPH_LAST - GLYPH_FIRST + 1) as usize; // 95
const GLYPH_MAX_W: usize = 48;
const GLYPH_MAX_H: usize = 48;
/// Per-glyph coverage buffer size. Must accommodate the intermediate
/// oversampled raster (GLYPH_MAX_W * OVERSAMPLE_X * GLYPH_MAX_H) since
/// rasterize() uses the same buffer for the oversampled coverage map before
/// downsampling in-place.
const GLYPH_BUF_SIZE: usize = GLYPH_MAX_W * OVERSAMPLE_X as usize * GLYPH_MAX_H;

/// Built-in 8×16 VGA-style bitmap font covering printable ASCII (0x20–0x7E).
pub const FONT_8X16: BitmapFont = BitmapFont {
    glyph_width: 8,
    glyph_height: 16,
    data: &FONT_8X16_DATA,
    first: 0x20,
    last: 0x7E,
};

/// An embedded monospace bitmap font (1 bit per pixel).
///
/// Each glyph is `glyph_height` bytes — one byte per scanline row, MSB is the
/// leftmost pixel. Covers a contiguous range of ASCII codepoints.
pub struct BitmapFont {
    /// Glyph cell width in pixels.
    pub glyph_width: u32,
    /// Glyph cell height in pixels.
    pub glyph_height: u32,
    data: &'static [u8],
    first: u8,
    last: u8,
}
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
/// Total size: ~220 KiB (95 glyphs * 48*48 bytes coverage + metadata).
pub struct GlyphCache {
    glyphs: [CachedGlyph; GLYPH_COUNT],
    coverage: [u8; GLYPH_COUNT * GLYPH_BUF_SIZE],
    pub line_height: u32,
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

impl BitmapFont {
    /// Return the bitmap rows for a character, or `None` if outside the font.
    ///
    /// The returned slice is `glyph_height` bytes. Each byte is one scanline
    /// row (MSB = leftmost pixel).
    pub fn glyph(&self, ch: char) -> Option<&[u8]> {
        let c = ch as u32;

        if c < self.first as u32 || c > self.last as u32 {
            return None;
        }

        let idx = (c - self.first as u32) as usize;
        let bpg = self.glyph_height as usize;
        let start = idx * bpg;
        let end = start + bpg;

        if end <= self.data.len() {
            Some(&self.data[start..end])
        } else {
            None
        }
    }
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
    pub fn get(&self, ch: u8) -> Option<(&CachedGlyph, &[u8])> {
        if ch < GLYPH_FIRST || ch > GLYPH_LAST {
            return None;
        }

        let idx = (ch - GLYPH_FIRST) as usize;
        let g = &self.glyphs[idx];
        let len = (g.width * g.height) as usize;
        let cov = &self.coverage[g.buf_offset..g.buf_offset + len];

        Some((g, cov))
    }
    /// Rasterize all printable ASCII glyphs into this cache in place.
    /// Caller provides scratch space (~60 KiB) to avoid stack overflow.
    pub fn populate(&mut self, font: &TrueTypeFont, size_px: u32, scratch: &mut RasterScratch) {
        self.line_height = size_px + size_px / 4;

        for i in 0..GLYPH_COUNT {
            let ch = (GLYPH_FIRST + i as u8) as char;
            let buf_offset = i * GLYPH_BUF_SIZE;
            let buf = &mut self.coverage[buf_offset..buf_offset + GLYPH_BUF_SIZE];
            let mut raster = RasterBuffer {
                data: buf,
                width: GLYPH_MAX_W as u32,
                height: GLYPH_MAX_H as u32,
            };

            if let Some(m) = font.rasterize(ch, size_px, &mut raster, &mut *scratch) {
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
    /// Zero-initialize the cache. The struct is ~220 KiB -- callers with
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
            }; GLYPH_COUNT],
            coverage: [0u8; GLYPH_COUNT * GLYPH_BUF_SIZE],
            line_height: 0,
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
    /// Draw a coverage map (anti-aliased glyph) at position (x, y) in the
    /// given color. Each byte in the coverage map modulates the color's alpha.
    ///
    /// Blending is performed in linear light (sRGB gamma-correct): destination
    /// pixels are converted to linear space, blended with the coverage-modulated
    /// source color, then converted back to sRGB. This produces perceptually
    /// correct stroke weights (fixes the "wispy text" problem where thin strokes
    /// appear too light with naive linear-in-sRGB blending).
    ///
    /// `x` and `y` can be negative (glyph bearings may position the bitmap
    /// outside the pen origin). Clips to surface bounds.
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
                let idx = (row * cov_width + col) as usize;

                if idx >= coverage.len() {
                    return;
                }

                let cov = coverage[idx];

                if cov == 0 {
                    continue;
                }

                let px = x + col as i32;
                let py = y + row as i32;

                if px < 0 || py < 0 {
                    continue;
                }

                let ux = px as u32;
                let uy = py as u32;

                // Effective alpha: color.a * coverage / 255.
                let alpha = (color.a as u32 * cov as u32 + 127) / 255;

                if alpha >= 255 {
                    // Full coverage + opaque color: just write the source.
                    self.set_pixel(ux, uy, color);
                    continue;
                }

                if let Some(dst) = self.get_pixel(ux, uy) {
                    // Convert destination to linear space.
                    let dst_r_lin = SRGB_TO_LINEAR[dst.r as usize] as u32;
                    let dst_g_lin = SRGB_TO_LINEAR[dst.g as usize] as u32;
                    let dst_b_lin = SRGB_TO_LINEAR[dst.b as usize] as u32;

                    // Blend in linear space: out = dst * (1 - alpha/255) + src * alpha/255.
                    let inv_alpha = 255 - alpha;
                    let out_r_lin = (dst_r_lin * inv_alpha + src_r_lin * alpha + 127) / 255;
                    let out_g_lin = (dst_g_lin * inv_alpha + src_g_lin * alpha + 127) / 255;
                    let out_b_lin = (dst_b_lin * inv_alpha + src_b_lin * alpha + 127) / 255;

                    // Convert back to sRGB (table is indexed by linear >> 4).
                    let out_r = LINEAR_TO_SRGB[linear_to_idx(out_r_lin)];
                    let out_g = LINEAR_TO_SRGB[linear_to_idx(out_g_lin)];
                    let out_b = LINEAR_TO_SRGB[linear_to_idx(out_b_lin)];

                    // Alpha: blend destination alpha in sRGB space (alpha is perceptually
                    // uniform already). For opaque destinations this is always 255.
                    let out_a = dst.a as u32 + alpha * (255 - dst.a as u32) / 255;

                    self.set_pixel(ux, uy, Color {
                        r: out_r,
                        g: out_g,
                        b: out_b,
                        a: if out_a > 255 { 255 } else { out_a as u8 },
                    });
                }
            }
        }
    }
    /// Draw a horizontal line. Clips to surface bounds.
    pub fn draw_hline(&mut self, x: u32, y: u32, w: u32, color: Color) {
        self.fill_rect(x, y, w, 1, color);
    }
    /// Draw a single glyph at (x, y) in the given color.
    ///
    /// Only foreground pixels (bit = 1) are drawn; the background is left
    /// unchanged. Out-of-bounds pixels clip silently.
    pub fn draw_glyph(&mut self, x: u32, y: u32, ch: char, font: &BitmapFont, color: Color) {
        if let Some(glyph) = font.glyph(ch) {
            for row in 0..font.glyph_height {
                let byte = glyph[row as usize];

                for col in 0..font.glyph_width {
                    if byte & (0x80 >> col) != 0 {
                        if let (Some(px), Some(py)) = (x.checked_add(col), y.checked_add(row)) {
                            self.set_pixel(px, py, color);
                        }
                    }
                }
            }
        }
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
    /// Draw a string starting at (x, y). Returns the x position after the
    /// last glyph.
    ///
    /// Each character advances by `font.glyph_width` regardless of whether
    /// the glyph exists. Characters outside the font's range render as blanks.
    pub fn draw_text(
        &mut self,
        x: u32,
        y: u32,
        text: &str,
        font: &BitmapFont,
        color: Color,
    ) -> u32 {
        let mut cx = x;

        for ch in text.chars() {
            self.draw_glyph(cx, y, ch, font, color);
            cx = cx.saturating_add(font.glyph_width);
        }

        cx
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
            let r = (color_top.r as u32 * (denom - t) + color_bottom.r as u32 * t + denom / 2) / denom;
            let g = (color_top.g as u32 * (denom - t) + color_bottom.g as u32 * t + denom / 2) / denom;
            let b = (color_top.b as u32 * (denom - t) + color_bottom.b as u32 * t + denom / 2) / denom;
            let a = (color_top.a as u32 * (denom - t) + color_bottom.a as u32 * t + denom / 2) / denom;

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
    fn cols(&self) -> usize {
        if self.char_width == 0 {
            return 0;
        }

        (self.max_width / self.char_width) as usize
    }
    /// Layout and draw text onto a surface in one pass. Draws characters
    /// starting at (origin_x, origin_y), wrapping within max_width.
    /// Returns (cursor_x, cursor_y) for the given cursor byte offset.
    pub fn draw(
        &self,
        fb: &mut Surface,
        text: &[u8],
        origin_x: u32,
        origin_y: u32,
        cursor_offset: usize,
        font: &BitmapFont,
        text_color: Color,
        cursor_color: Color,
        max_y: u32,
    ) -> (u32, u32) {
        let cols = self.cols();
        let mut col = 0usize;
        let mut row = 0u32;
        let mut cursor_x = origin_x;
        let mut cursor_y = origin_y;

        for (i, &byte) in text.iter().enumerate() {
            let py = origin_y + row * self.line_height;

            if py > max_y {
                break;
            }

            if i == cursor_offset {
                cursor_x = origin_x + col as u32 * self.char_width;
                cursor_y = py;
            }

            if byte == b'\n' {
                col = 0;
                row += 1;

                continue;
            }

            if cols > 0 && col >= cols {
                col = 0;
                row += 1;

                let py = origin_y + row * self.line_height;

                if py > max_y {
                    break;
                }
            }

            if byte >= 0x20 && byte < 0x7F {
                let ch = [byte];
                let s = unsafe { core::str::from_utf8_unchecked(&ch) };

                fb.draw_text(
                    origin_x + col as u32 * self.char_width,
                    origin_y + row * self.line_height,
                    s,
                    font,
                    text_color,
                );
            }

            col += 1;
        }

        // Cursor at end of text.
        if cursor_offset >= text.len() {
            let py = origin_y + row * self.line_height;

            cursor_x = origin_x + col as u32 * self.char_width;
            cursor_y = py;
        }

        // Draw cursor.
        if cursor_y <= max_y {
            fb.fill_rect(
                cursor_x,
                cursor_y,
                self.char_width,
                font.glyph_height,
                cursor_color,
            );
        }

        (cursor_x, cursor_y)
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
        let cols = self.cols();
        let mut col = 0usize;
        let mut row = 0u32;
        let mut cursor_x = origin_x;
        let mut cursor_y = origin_y;
        let baseline_offset = cache.line_height * 3 / 4;

        for (i, &byte) in text.iter().enumerate() {
            let py = origin_y + row * self.line_height;

            if py > max_y {
                break;
            }

            if i == cursor_offset {
                cursor_x = origin_x + col as u32 * self.char_width;
                cursor_y = py;
            }

            if byte == b'\n' {
                col = 0;
                row += 1;

                continue;
            }

            if cols > 0 && col >= cols {
                col = 0;
                row += 1;

                let py = origin_y + row * self.line_height;

                if py > max_y {
                    break;
                }
            }

            if let Some((glyph, coverage)) = cache.get(byte) {
                if glyph.width > 0 && glyph.height > 0 {
                    let gx =
                        origin_x as i32 + col as i32 * self.char_width as i32 + glyph.bearing_x;
                    let gy = origin_y as i32
                        + row as i32 * self.line_height as i32
                        + baseline_offset as i32
                        - glyph.bearing_y;

                    fb.draw_coverage(gx, gy, coverage, glyph.width, glyph.height, text_color);
                }
            }

            col += 1;
        }

        if cursor_offset >= text.len() {
            let py = origin_y + row * self.line_height;

            cursor_x = origin_x + col as u32 * self.char_width;
            cursor_y = py;
        }

        if cursor_y <= max_y {
            fb.fill_rect(cursor_x, cursor_y, 2, cache.line_height, cursor_color);
        }

        (cursor_x, cursor_y)
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

// ---------------------------------------------------------------------------
// Proportional text rendering
// ---------------------------------------------------------------------------

/// Draw a byte string using per-glyph advance widths from a GlyphCache.
///
/// Unlike the monospace `draw_string` helper in the compositor, this function
/// uses each glyph's individual advance width for variable-pitch text layout.
/// If a codepoint has no cached glyph (outside 0x20..=0x7E), the pen advances
/// by the space glyph's advance width (fallback) without crashing.
///
/// Returns the final pen X position (total advance of all glyphs).
pub fn draw_proportional_string(
    fb: &mut Surface,
    x: u32,
    y: u32,
    text: &[u8],
    cache: &GlyphCache,
    color: Color,
) -> u32 {
    let baseline_y = y as i32 + (cache.line_height * 3 / 4) as i32;
    let mut cx = x as i32;
    // Fallback advance: use space glyph width (first cached glyph).
    let fallback_advance = match cache.get(b' ') {
        Some((g, _)) => g.advance,
        None => 8, // absolute fallback
    };

    for &byte in text {
        if let Some((glyph, coverage)) = cache.get(byte) {
            if glyph.width > 0 && glyph.height > 0 {
                let gx = cx + glyph.bearing_x;
                let gy = baseline_y - glyph.bearing_y;

                fb.draw_coverage(gx, gy, coverage, glyph.width, glyph.height, color);
            }

            cx += glyph.advance as i32;
        } else {
            // Missing glyph: advance by fallback width, don't crash.
            cx += fallback_advance as i32;
        }
    }

    if cx < 0 { 0 } else { cx as u32 }
}

fn abs(x: i32) -> i32 {
    if x < 0 {
        -x
    } else {
        x
    }
}
/// Convert a linear light value (0–65535 u32) to a LINEAR_TO_SRGB table index.
/// The table has 4096 entries; index is `value >> 4`, clamped to 4095.
fn linear_to_idx(v: u32) -> usize {
    let idx = v >> 4;

    if idx > 4095 { 4095 } else { idx as usize }
}
fn min(a: u32, b: u32) -> u32 {
    if a < b {
        a
    } else {
        b
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
///   20 = chrome (title bar, status bar)
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
        let src_offset = (src_y_start * s.surface.stride + src_x_start * s.surface.format.bytes_per_pixel()) as usize;

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

// ---------------------------------------------------------------------------
// Damage tracking — dirty rectangle management for partial GPU transfer
// ---------------------------------------------------------------------------

/// Maximum number of dirty rects that fit in a MSG_PRESENT payload.
/// Payload = 60 bytes. buffer_index (4) + rect_count (1) + pad (3) = 8.
/// Remaining = 52 bytes / 8 bytes per rect = 6 rects (conservative to fit).
/// Actually: 60 - 4 (buffer_index) - 4 (rect_count as u32) = 52 / 8 = 6.
pub const MAX_DIRTY_RECTS: usize = 7;

/// A rectangular region of pixels that has been modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

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

impl DirtyRect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// Compute the bounding box (union) of two rects.
    pub fn union(self, other: DirtyRect) -> DirtyRect {
        if self.w == 0 || self.h == 0 {
            return other;
        }
        if other.w == 0 || other.h == 0 {
            return self;
        }

        let x0 = if self.x < other.x { self.x } else { other.x };
        let y0 = if self.y < other.y { self.y } else { other.y };

        let self_x1 = self.x as u32 + self.w as u32;
        let other_x1 = other.x as u32 + other.w as u32;
        let x1 = if self_x1 > other_x1 { self_x1 } else { other_x1 };

        let self_y1 = self.y as u32 + self.h as u32;
        let other_y1 = other.y as u32 + other.h as u32;
        let y1 = if self_y1 > other_y1 { self_y1 } else { other_y1 };

        DirtyRect {
            x: x0,
            y: y0,
            w: (x1 - x0 as u32) as u16,
            h: (y1 - y0 as u32) as u16,
        }
    }

    /// Compute the union of a slice of rects. Returns a zero rect if empty.
    pub fn union_all(rects: &[DirtyRect]) -> DirtyRect {
        let mut result = DirtyRect::new(0, 0, 0, 0);
        for &r in rects {
            result = result.union(r);
        }
        result
    }
}

impl DamageTracker {
    /// Create a new damage tracker for the given framebuffer dimensions.
    pub const fn new(fb_width: u16, fb_height: u16) -> Self {
        Self {
            rects: [DirtyRect { x: 0, y: 0, w: 0, h: 0 }; MAX_DIRTY_RECTS],
            count: 0,
            full_screen: false,
            fb_width,
            fb_height,
        }
    }

    /// Reset the tracker for a new frame.
    pub fn reset(&mut self) {
        self.count = 0;
        self.full_screen = false;
    }

    /// Mark the entire framebuffer as dirty.
    pub fn mark_full_screen(&mut self) {
        self.full_screen = true;
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

    /// Get the dirty rects for this frame. Returns `None` if full-screen
    /// transfer is needed (either explicitly marked or overflow).
    pub fn dirty_rects(&self) -> Option<&[DirtyRect]> {
        if self.full_screen || self.count == 0 {
            None
        } else {
            Some(&self.rects[..self.count])
        }
    }

    /// Get the bounding box of all dirty rects, or full screen if needed.
    pub fn bounding_box(&self) -> DirtyRect {
        if self.full_screen || self.count == 0 {
            DirtyRect::new(0, 0, self.fb_width, self.fb_height)
        } else {
            DirtyRect::union_all(&self.rects[..self.count])
        }
    }
}
