//! Drawing primitives for pixel buffers.
//!
//! Pure library — no syscalls, no hardware access. Operates on borrowed pixel
//! buffers. All drawing operations clip to surface bounds; out-of-range
//! coordinates are silently ignored (no panics).
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

include!("gamma_tables.rs");
include!("palette.rs");
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
        // div255 is exact for da * inv_sa ∈ 0..=65025.
        let da_eff = div255(da * inv_sa);
        let out_a = sa + da_eff;

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
        // out_c = (src_c * src_a + dst_c * dst_a_eff) / out_a
        // da_eff = div255(dst_a * inv_src_a) is precomputed above.
        let r_lin = (src_r_lin * sa + dst_r_lin * da_eff) / out_a;
        let g_lin = (src_g_lin * sa + dst_g_lin * da_eff) / out_a;
        let b_lin = (src_b_lin * sa + dst_b_lin * da_eff) / out_a;

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

        if copy_w == 0 || copy_h == 0 {
            return;
        }

        let bpp = self.format.bytes_per_pixel();
        let dst_stride = self.stride;
        let row_bytes = (copy_w * bpp) as usize;
        let dst_ptr = self.data.as_mut_ptr();

        for row in 0..copy_h {
            let src_row_off = (row * src_stride) as usize;
            let dst_row_off = ((dst_y + row) * dst_stride + dst_x * bpp) as usize;

            // Bounds check for source row.
            if src_row_off + row_bytes > src_data.len() {
                continue;
            }

            // Fast-path: check if all source pixels in this row are opaque.
            let mut all_opaque = true;
            for col in 0..copy_w {
                if src_data[src_row_off + (col * bpp + 3) as usize] != 255 {
                    all_opaque = false;
                    break;
                }
            }

            if all_opaque {
                // SAFETY: copy_w/copy_h clipped to min(src, dst) dimensions.
                // src_row_off + row_bytes <= src_data.len() (checked above).
                // dst_row_off + row_bytes = (dst_y + row) * stride + (dst_x + copy_w) * 4
                //   <= height * stride <= data.len() because dst_y + row < height
                //   and dst_x + copy_w <= width.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_data.as_ptr().add(src_row_off),
                        dst_ptr.add(dst_row_off),
                        row_bytes,
                    );
                }
                continue;
            }

            // SAFETY: copy_w/copy_h clipped to min(src, dst) dimensions.
            // All pixel offsets within src_row_off..src_row_off + row_bytes
            // (src bounds checked above) and dst_row_off..dst_row_off + row_bytes
            // (dst bounds guaranteed by clipping: dst_y + row < height,
            // dst_x + col < width, stride * height <= data.len()).
            unsafe {
                let src_row_ptr = src_data.as_ptr().add(src_row_off);
                let dst_row_ptr = dst_ptr.add(dst_row_off);

                for col in 0..copy_w {
                    let offset = (col * bpp) as usize;
                    let sp = src_row_ptr.add(offset);
                    let dp = dst_row_ptr.add(offset);

                    // Read source BGRA pixel.
                    let src_a = core::ptr::read(sp.add(3));

                    if src_a == 0 {
                        continue;
                    }

                    if src_a == 255 {
                        // Opaque: direct copy (4 bytes).
                        core::ptr::copy_nonoverlapping(sp, dp, 4);
                        continue;
                    }

                    // Semi-transparent: read both pixels and blend.
                    let src_color = Color {
                        b: core::ptr::read(sp),
                        g: core::ptr::read(sp.add(1)),
                        r: core::ptr::read(sp.add(2)),
                        a: src_a,
                    };
                    let dst_color = Color {
                        b: core::ptr::read(dp),
                        g: core::ptr::read(dp.add(1)),
                        r: core::ptr::read(dp.add(2)),
                        a: core::ptr::read(dp.add(3)),
                    };
                    let blended = src_color.blend_over(dst_color);
                    let encoded = blended.encode(self.format);

                    core::ptr::write(dp, encoded[0]);
                    core::ptr::write(dp.add(1), encoded[1]);
                    core::ptr::write(dp.add(2), encoded[2]);
                    core::ptr::write(dp.add(3), encoded[3]);
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
        if cov_width == 0 || cov_height == 0 || color.a == 0 {
            return;
        }

        // Upfront coverage buffer size check.
        let cov_total = (cov_width as usize) * (cov_height as usize) * 3;
        if coverage.len() < cov_total {
            return;
        }

        // Pre-convert source color to linear space (loop-invariant).
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;
        let color_a = color.a as u32;
        let encoded = color.encode(self.format);
        let encoded_u32 = u32::from_ne_bytes(encoded);

        // Pre-clip: compute visible range of the coverage buffer against
        // surface bounds, handling negative x/y offsets. Uses i64 to avoid
        // overflow when surface dimensions are added to large negative coords.
        let xi = x as i64;
        let yi = y as i64;
        let surf_w = self.width as i64;
        let surf_h = self.height as i64;
        let cov_w = cov_width as i64;
        let cov_h = cov_height as i64;

        let start_row = if yi < 0 { -yi } else { 0 };
        let end_row = if cov_h < surf_h - yi { cov_h } else { surf_h - yi };
        let start_col = if xi < 0 { -xi } else { 0 };
        let end_col = if cov_w < surf_w - xi { cov_w } else { surf_w - xi };

        if start_row >= end_row || start_col >= end_col || end_row <= 0 || end_col <= 0 {
            return;
        }

        let start_row = start_row as u32;
        let end_row = end_row as u32;
        let start_col = start_col as u32;
        let end_col = end_col as u32;
        let stride = self.stride;
        let ptr = self.data.as_mut_ptr();

        for row in start_row..end_row {
            let py = (y + row as i32) as u32;
            let row_base = (py * stride) as usize;

            for col in start_col..end_col {
                let base = ((row * cov_width + col) * 3) as usize;
                let cov_r = coverage[base];
                let cov_g = coverage[base + 1];
                let cov_b = coverage[base + 2];

                // Skip if all channels are zero.
                if cov_r == 0 && cov_g == 0 && cov_b == 0 {
                    continue;
                }

                let px = (x + col as i32) as u32;
                let pixel_off = row_base + (px * 4) as usize;

                // Per-channel effective alpha: color.a * channel_coverage / 255.
                let alpha_r = div255(color_a * cov_r as u32 + 127);
                let alpha_g = div255(color_a * cov_g as u32 + 127);
                let alpha_b = div255(color_a * cov_b as u32 + 127);

                // Fast path: all channels full coverage + opaque color.
                if alpha_r >= 255 && alpha_g >= 255 && alpha_b >= 255 {
                    // SAFETY: coords are pre-clipped to [0..width, 0..height];
                    // pixel_off = py * stride + px * 4 where py < height and
                    // px < width, so pixel_off + 4 <= height * stride <= data.len().
                    unsafe {
                        core::ptr::write((ptr.add(pixel_off)) as *mut u32, encoded_u32);
                    }

                    continue;
                }

                // SAFETY: coords are pre-clipped to [0..width, 0..height];
                // pixel_off = py * stride + px * 4 where py < height and
                // px < width, so pixel_off + 4 <= height * stride <= data.len().
                unsafe {
                    let p = ptr.add(pixel_off);

                    // Read BGRA destination pixel.
                    let dst_b = core::ptr::read(p);
                    let dst_g_byte = core::ptr::read(p.add(1));
                    let dst_r_byte = core::ptr::read(p.add(2));
                    let dst_a_byte = core::ptr::read(p.add(3));

                    // Convert destination to linear space.
                    let dst_r_lin = SRGB_TO_LINEAR[dst_r_byte as usize] as u32;
                    let dst_g_lin = SRGB_TO_LINEAR[dst_g_byte as usize] as u32;
                    let dst_b_lin = SRGB_TO_LINEAR[dst_b as usize] as u32;

                    // Blend each channel independently in linear space.
                    let inv_r = 255 - alpha_r;
                    let inv_g = 255 - alpha_g;
                    let inv_b = 255 - alpha_b;
                    let out_r_lin = div255(dst_r_lin * inv_r + src_r_lin * alpha_r + 127);
                    let out_g_lin = div255(dst_g_lin * inv_g + src_g_lin * alpha_g + 127);
                    let out_b_lin = div255(dst_b_lin * inv_b + src_b_lin * alpha_b + 127);

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
                    let out_a = dst_a_byte as u32
                        + div255(max_alpha * (255 - dst_a_byte as u32));
                    let out_a = if out_a > 255 { 255u8 } else { out_a as u8 };

                    // Write BGRA pixel.
                    core::ptr::write(p, out_b);
                    core::ptr::write(p.add(1), out_g);
                    core::ptr::write(p.add(2), out_r);
                    core::ptr::write(p.add(3), out_a);
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
        let pixel_count = (x2 - x) as usize;

        if pixel_count == 0 {
            return;
        }

        // Hoist src color linear conversion outside all loops.
        let sa = color.a as u32;
        let inv_sa = 255 - sa;
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;
        let bpp = self.format.bytes_per_pixel();
        let stride = self.stride;
        let ptr = self.data.as_mut_ptr();

        for row in y..y2 {
            let row_offset = (row * stride + x * bpp) as usize;

            // SAFETY: x/y clipped to surface bounds; x..x2 within width,
            // row within y..y2 < height. stride * height <= data.len().
            unsafe {
                let row_ptr = ptr.add(row_offset);

                for i in 0..pixel_count {
                    let p = row_ptr.add(i * 4);

                    // Read destination BGRA pixel.
                    let dst_b = core::ptr::read(p);
                    let dst_g = core::ptr::read(p.add(1));
                    let dst_r = core::ptr::read(p.add(2));
                    let dst_a = core::ptr::read(p.add(3));

                    let da = dst_a as u32;
                    let da_eff = div255(da * inv_sa);
                    let out_a = sa + da_eff;

                    if out_a == 0 {
                        continue;
                    }

                    // Convert destination to linear space.
                    let dst_r_lin = SRGB_TO_LINEAR[dst_r as usize] as u32;
                    let dst_g_lin = SRGB_TO_LINEAR[dst_g as usize] as u32;
                    let dst_b_lin = SRGB_TO_LINEAR[dst_b as usize] as u32;

                    let r_lin = (src_r_lin * sa + dst_r_lin * da_eff) / out_a;
                    let g_lin = (src_g_lin * sa + dst_g_lin * da_eff) / out_a;
                    let b_lin = (src_b_lin * sa + dst_b_lin * da_eff) / out_a;

                    let out_r = LINEAR_TO_SRGB[linear_to_idx(r_lin)];
                    let out_g = LINEAR_TO_SRGB[linear_to_idx(g_lin)];
                    let out_b = LINEAR_TO_SRGB[linear_to_idx(b_lin)];
                    let out_a_u8 = if out_a > 255 { 255u8 } else { out_a as u8 };

                    // Write BGRA pixel.
                    core::ptr::write(p, out_b);
                    core::ptr::write(p.add(1), out_g);
                    core::ptr::write(p.add(2), out_r);
                    core::ptr::write(p.add(3), out_a_u8);
                }
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
pub fn linear_to_idx(v: u32) -> usize {
    let idx = v >> 4;

    if idx > 4095 {
        4095
    } else {
        idx as usize
    }
}
/// Fast integer divide-by-255: exact for 0..=65025, ±1 for larger values.
///
/// Replaces the expensive `x / 255` in alpha-blending hot paths. The identity
/// `(x + 1 + (x >> 8)) >> 8 == x / 255` holds for all u32 values in the
/// 0..=65025 range used by alpha blending (255 × 255 = 65025).
#[inline(always)]
pub fn div255(x: u32) -> u32 {
    (x + 1 + (x >> 8)) >> 8
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


