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
include!("rasterizer.rs");
include!("truetype.rs");

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

    /// Opaque color from RGB components.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
    /// Color with explicit alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

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

        // out_c = (src_c * src_a + dst_c * dst_a * (1 - src_a / 255)) / out_a
        let r = (self.r as u32 * sa + dst.r as u32 * da * inv_sa / 255) / out_a;
        let g = (self.g as u32 * sa + dst.g as u32 * da * inv_sa / 255) / out_a;
        let b = (self.b as u32 * sa + dst.b as u32 * da * inv_sa / 255) / out_a;

        Color {
            r: if r > 255 { 255 } else { r as u8 },
            g: if g > 255 { 255 } else { g as u8 },
            b: if b > 255 { 255 } else { b as u8 },
            a: if out_a > 255 { 255 } else { out_a as u8 },
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
    /// Draw a coverage map (anti-aliased glyph) at position (x, y) in the
    /// given color. Each byte in the coverage map modulates the color's alpha.
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

                let alpha = (color.a as u32 * cov as u32 / 255) as u8;
                let c = Color::rgba(color.r, color.g, color.b, alpha);

                self.blend_pixel(px as u32, py as u32, c);
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
fn min(a: u32, b: u32) -> u32 {
    if a < b {
        a
    } else {
        b
    }
}
