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
#[cfg(target_arch = "aarch64")]
include!("neon.rs");
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

/// Resampling method for scaled or transformed blits.
///
/// The API is parameterized so new methods (e.g., Lanczos) can be added
/// without changing call sites — callers pass the enum variant and the
/// implementation dispatches internally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResamplingMethod {
    /// Bilinear interpolation: samples 4 surrounding pixels and blends
    /// based on fractional position. Good balance of quality and speed
    /// for most scaling/rotation operations.
    Bilinear,
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

                #[cfg(target_arch = "aarch64")]
                {
                    // NEON path: process 4 pixels at a time for the
                    // semi-transparent case, with per-pixel alpha handling.
                    let chunks = copy_w / 4;
                    let tail_start = chunks * 4;

                    for chunk in 0..chunks {
                        let base = (chunk * 4 * bpp) as usize;
                        let sp = src_row_ptr.add(base);
                        let dp = dst_row_ptr.add(base);

                        // Check if all 4 source pixels in this chunk have
                        // the same alpha (common fast paths).
                        let a0 = core::ptr::read(sp.add(3));
                        let a1 = core::ptr::read(sp.add(7));
                        let a2 = core::ptr::read(sp.add(11));
                        let a3 = core::ptr::read(sp.add(15));

                        if a0 == 0 && a1 == 0 && a2 == 0 && a3 == 0 {
                            // All transparent: skip.
                            continue;
                        }

                        if a0 == 255 && a1 == 255 && a2 == 255 && a3 == 255 {
                            // All opaque: direct copy.
                            core::ptr::copy_nonoverlapping(sp, dp, 16);
                            continue;
                        }

                        // Mixed alpha: use NEON blend for any semi-transparent
                        // pixels, but handle fully opaque/transparent per-pixel.
                        let has_semi = (a0 > 0 && a0 < 255)
                            || (a1 > 0 && a1 < 255)
                            || (a2 > 0 && a2 < 255)
                            || (a3 > 0 && a3 < 255);

                        if has_semi {
                            // SAFETY: sp points to 16 readable bytes (4 src
                            // pixels), dp points to 16 writable bytes (4 dst
                            // pixels). Both are within the clipped bounds.
                            neon_blend_4px(sp, dp, &SRGB_TO_LINEAR, &LINEAR_TO_SRGB);
                        } else {
                            // All pixels are either 0 or 255 — no semi-transparent.
                            blit_blend_scalar_4px(sp, dp, bpp);
                        }
                    }

                    // Handle tail pixels with scalar code.
                    for col in tail_start..copy_w {
                        let offset = (col * bpp) as usize;
                        let sp = src_row_ptr.add(offset);
                        let dp = dst_row_ptr.add(offset);
                        blit_blend_scalar_1px(sp, dp, self.format);
                    }
                }

                #[cfg(not(target_arch = "aarch64"))]
                {
                    for col in 0..copy_w {
                        let offset = (col * bpp) as usize;
                        let sp = src_row_ptr.add(offset);
                        let dp = dst_row_ptr.add(offset);
                        blit_blend_scalar_1px(sp, dp, self.format);
                    }
                }
            }
        }
    }
    /// Fill the entire surface with a solid color.
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }
    /// Draw a 1-byte-per-pixel grayscale coverage map (anti-aliased glyph)
    /// at position (x, y) in the given color. The coverage data has 1 byte
    /// per pixel, stored row-major: `coverage[row * cov_width + col]`.
    ///
    /// The single coverage value is applied uniformly to R, G, B channels
    /// (no per-channel independent modulation). This produces smooth
    /// grayscale anti-aliased text without color fringing.
    ///
    /// Blending is performed in linear light (sRGB gamma-correct).
    /// `x` and `y` can be negative. Clips to surface bounds.
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
        let cov_total = (cov_width as usize) * (cov_height as usize);
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
                let cov = coverage[(row * cov_width + col) as usize];

                // Skip zero coverage.
                if cov == 0 {
                    continue;
                }

                let px = (x + col as i32) as u32;
                let pixel_off = row_base + (px * 4) as usize;

                // Effective alpha: color.a * coverage / 255 (uniform for all channels).
                let alpha = div255(color_a * cov as u32 + 127);

                // Fast path: full coverage + opaque color.
                if alpha >= 255 {
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

                    // Blend uniformly in linear space (same alpha for all channels).
                    let inv_a = 255 - alpha;
                    let out_r_lin = div255(dst_r_lin * inv_a + src_r_lin * alpha + 127);
                    let out_g_lin = div255(dst_g_lin * inv_a + src_g_lin * alpha + 127);
                    let out_b_lin = div255(dst_b_lin * inv_a + src_b_lin * alpha + 127);

                    // Convert back to sRGB.
                    let out_r = LINEAR_TO_SRGB[linear_to_idx(out_r_lin)];
                    let out_g = LINEAR_TO_SRGB[linear_to_idx(out_g_lin)];
                    let out_b = LINEAR_TO_SRGB[linear_to_idx(out_b_lin)];

                    // Alpha compositing.
                    let out_a = dst_a_byte as u32
                        + div255(alpha * (255 - dst_a_byte as u32));
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
    /// Draw an anti-aliased line using Wu's algorithm.
    ///
    /// Axis-aligned lines (horizontal or vertical) are drawn pixel-perfect
    /// with no anti-aliasing fringe. Diagonal lines use coverage-based
    /// sub-pixel blending for smooth edges. The algorithm produces two
    /// pixels per step along the major axis with complementary coverage
    /// values, resulting in a visually consistent 1px line width across
    /// all angles.
    ///
    /// Blending uses gamma-correct sRGB compositing via the existing LUT
    /// infrastructure. Clips to surface bounds; out-of-range coordinates
    /// are silently ignored.
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        // Single point.
        if x0 == x1 && y0 == y1 {
            if x0 >= 0 && y0 >= 0 {
                self.set_pixel(x0 as u32, y0 as u32, color);
            }
            return;
        }

        // Axis-aligned lines: pixel-perfect, no AA fringe.
        if y0 == y1 {
            // Horizontal line.
            let (lx, rx) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            for x in lx..=rx {
                if x >= 0 && y0 >= 0 {
                    self.set_pixel(x as u32, y0 as u32, color);
                }
            }
            return;
        }
        if x0 == x1 {
            // Vertical line.
            let (ty, by) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            for y in ty..=by {
                if x0 >= 0 && y >= 0 {
                    self.set_pixel(x0 as u32, y as u32, color);
                }
            }
            return;
        }

        // Wu's anti-aliased line algorithm.
        // We work in 8.8 fixed-point for the gradient's fractional part.
        let mut ax0 = x0;
        let mut ay0 = y0;
        let mut ax1 = x1;
        let mut ay1 = y1;

        let steep = abs(ay1 - ay0) > abs(ax1 - ax0);

        // If steep, swap x and y so we always iterate along the longer axis.
        if steep {
            core::mem::swap(&mut ax0, &mut ay0);
            core::mem::swap(&mut ax1, &mut ay1);
        }

        // Ensure we draw from left to right.
        if ax0 > ax1 {
            core::mem::swap(&mut ax0, &mut ax1);
            core::mem::swap(&mut ay0, &mut ay1);
        }

        let dx = ax1 - ax0;
        let dy = ay1 - ay0;

        // Gradient in 8.8 fixed-point: dy/dx scaled by 256.
        // dx is guaranteed > 0 here (we ensured ax0 < ax1 and handled ax0==ax1).
        let gradient_fp = if dx == 0 {
            256i32 // Should not happen, but safe fallback.
        } else {
            (dy * 256) / dx
        };

        // Check for perfect 45-degree lines: gradient is exactly ±256 (±1.0).
        // These pass through pixel centers, so no AA is needed.
        if gradient_fp == 256 || gradient_fp == -256 {
            // 45-degree line: draw solid pixels along the diagonal.
            let sy: i32 = if dy > 0 { 1 } else { -1 };
            let mut cy = ay0;
            for cx in ax0..=ax1 {
                if steep {
                    if cy >= 0 && cx >= 0 {
                        self.set_pixel(cy as u32, cx as u32, color);
                    }
                } else if cx >= 0 && cy >= 0 {
                    self.set_pixel(cx as u32, cy as u32, color);
                }
                cy += sy;
            }
            return;
        }

        // First endpoint.
        self.wu_endpoint(ax0, ay0, steep, color);

        // Last endpoint.
        self.wu_endpoint(ax1, ay1, steep, color);

        // y-intercept in 8.8 fixed-point, starting after the first endpoint.
        // The y value at x=ax0 is ay0 (pixel center). At x=ax0+1, y = ay0 + gradient.
        let mut y_fp = ay0 * 256 + 128 + gradient_fp;

        // Main loop: iterate along the major axis between the two endpoints.
        for x in (ax0 + 1)..ax1 {
            let y_int = y_fp >> 8; // Integer part (floor, but arithmetic shift for negative).
            let y_int = if y_fp < 0 {
                // Arithmetic right shift for negative values.
                (y_fp - 255) / 256
            } else {
                y_fp / 256
            };
            let frac = ((y_fp - y_int * 256) & 0xFF) as u32; // Fractional part 0..255.

            // Two pixels at this x: one at y_int, one at y_int+1.
            // Coverage: pixel at y_int gets (255 - frac), pixel at y_int+1 gets frac.
            let cov_lo = (255 - frac) as u8;
            let cov_hi = frac as u8;

            if steep {
                self.wu_plot(y_int as i32, x, color, cov_lo);
                self.wu_plot((y_int + 1) as i32, x, color, cov_hi);
            } else {
                self.wu_plot(x, y_int as i32, color, cov_lo);
                self.wu_plot(x, (y_int + 1) as i32, color, cov_hi);
            }

            y_fp += gradient_fp;
        }
    }

    /// Plot a single pixel with coverage-weighted alpha blending (Wu's AA helper).
    ///
    /// `coverage` is 0..255 where 255 = fully covered. Skips if coverage is 0
    /// or coordinates are out of bounds.
    fn wu_plot(&mut self, x: i32, y: i32, color: Color, coverage: u8) {
        if coverage == 0 || x < 0 || y < 0 {
            return;
        }
        let ux = x as u32;
        let uy = y as u32;
        if ux >= self.width || uy >= self.height {
            return;
        }

        if coverage == 255 {
            // Fully covered: use source-over blend (handles color.a < 255).
            self.blend_pixel(ux, uy, color);
            return;
        }

        // Effective alpha = color.a * coverage / 255.
        let eff_a = div255(color.a as u32 * coverage as u32);
        if eff_a == 0 {
            return;
        }

        let aa_color = Color::rgba(color.r, color.g, color.b, eff_a as u8);
        self.blend_pixel(ux, uy, aa_color);
    }

    /// Draw an endpoint pixel for Wu's algorithm.
    fn wu_endpoint(&mut self, x: i32, y: i32, steep: bool, color: Color) {
        if steep {
            if y >= 0 && x >= 0 {
                self.blend_pixel(y as u32, x as u32, color);
            }
        } else if x >= 0 && y >= 0 {
            self.blend_pixel(x as u32, y as u32, color);
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

                #[cfg(target_arch = "aarch64")]
                {
                    // SAFETY: row_ptr points to pixel_count contiguous u32
                    // slots within the surface buffer. Bounds verified above.
                    neon_fill_row(row_ptr, pixel_count, pixel_u32);
                }

                #[cfg(not(target_arch = "aarch64"))]
                {
                    for i in 0..pixel_count {
                        core::ptr::write(row_ptr.add(i), pixel_u32);
                    }
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

                #[cfg(target_arch = "aarch64")]
                {
                    let chunks = pixel_count / 4;
                    let tail_start = chunks * 4;

                    for chunk in 0..chunks {
                        let p = row_ptr.add(chunk * 16);
                        // SAFETY: p points to 16 writable bytes (4 dst pixels)
                        // within the clipped surface bounds.
                        neon_blend_const_4px(
                            p,
                            src_r_lin as u16,
                            src_g_lin as u16,
                            src_b_lin as u16,
                            sa as u16,
                            inv_sa as u16,
                            &SRGB_TO_LINEAR,
                            &LINEAR_TO_SRGB,
                        );
                    }

                    // Scalar tail.
                    for i in tail_start..pixel_count {
                        let p = row_ptr.add(i * 4);
                        fill_rect_blend_scalar_1px(
                            p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa,
                        );
                    }
                }

                #[cfg(not(target_arch = "aarch64"))]
                {
                    for i in 0..pixel_count {
                        let p = row_ptr.add(i * 4);
                        fill_rect_blend_scalar_1px(
                            p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa,
                        );
                    }
                }
            }
        }
    }
    /// Fill a rounded rectangle with a solid opaque color. Clips to surface bounds.
    ///
    /// Uses SDF-based approach: for each pixel in the corner arc regions,
    /// computes signed distance to the rounded corner and derives coverage
    /// (0.0–1.0) for anti-aliasing. Interior rows use `fill_rect` for speed.
    ///
    /// `radius` is clamped to `min(w, h) / 2`. Zero radius delegates to `fill_rect`.
    /// Anti-aliased edge pixels use gamma-correct sRGB blending.
    pub fn fill_rounded_rect(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        radius: u32,
        color: Color,
    ) {
        if w == 0 || h == 0 {
            return;
        }

        // Clamp radius to half the smallest dimension.
        let max_r = min(w, h) / 2;
        let r = min(radius, max_r);

        // Zero radius: delegate to fill_rect (no overhead).
        if r == 0 {
            self.fill_rect(x, y, w, h, color);
            return;
        }

        // Interior rows (between top and bottom arcs): fill_rect fast path.
        if h > 2 * r {
            self.fill_rect(x, y + r, w, h - 2 * r, color);
        }

        // Corner arc rows: top r rows and bottom r rows.
        // For each row in the arc region, compute the horizontal extent
        // of the rounded rect and fill with per-pixel AA at the edges.
        let encoded = color.encode(self.format);
        let pixel_u32 = u32::from_ne_bytes(encoded);
        let bpp = self.format.bytes_per_pixel();
        let stride = self.stride;
        let surf_w = self.width;
        let surf_h = self.height;
        let ptr = self.data.as_mut_ptr();

        // Pre-convert color to linear for AA blending.
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;

        // Process top and bottom arc rows.
        for arc_row in 0..r {
            // Distance from arc row center to the circle center (at radius r from edge).
            // dy = r - arc_row - 0.5 (distance from pixel center to circle center y).
            // We use fixed-point: dy_fp = (r * 256) - (arc_row * 256) - 128
            let dy_fp: i64 = (r as i64 * 256) - (arc_row as i64 * 256) - 128;
            let dy_sq = (dy_fp * dy_fp) as u64;
            let r_sq = (r as u64 * 256) * (r as u64 * 256);

            // The arc at this row defines x extent: x_arc = sqrt(r² - dy²)
            // This tells us how far the arc extends horizontally from the corner center.
            let x_arc_sq = if r_sq > dy_sq { r_sq - dy_sq } else { 0 };
            let x_arc_fp = isqrt_fp(x_arc_sq); // in 8.8 fixed point

            // Process both top row and bottom row.
            let rows: [u32; 2] = [y + arc_row, y + h - 1 - arc_row];
            for &py in &rows {
                if py >= surf_h {
                    continue;
                }

                // Left corner: center is at (x + r, py_center). Arc extends x_arc left.
                // The solid interior starts at x + r - floor(x_arc) and extends to x + w - r + floor(x_arc).
                let x_arc_int = (x_arc_fp >> 8) as u32;
                let x_arc_frac = (x_arc_fp & 0xFF) as u32; // 0..255

                // Left edge pixel: partial coverage.
                let left_solid = x + r - x_arc_int;
                let right_solid = x + w - r + x_arc_int;

                // Left AA pixel (if in bounds).
                if left_solid > 0 && x_arc_frac > 0 {
                    let lx = left_solid - 1;
                    if lx >= x && lx < surf_w {
                        // Coverage is x_arc_frac / 256.
                        let cov = x_arc_frac;
                        // SAFETY: lx < surf_w and py < surf_h (checked above).
                        // Pixel offset is within the surface data bounds.
                        unsafe {
                            rounded_rect_write_aa_pixel(
                                ptr, lx, py, stride, bpp,
                                src_r_lin, src_g_lin, src_b_lin, color.a as u32, cov,
                            );
                        }
                    }
                }

                // Right AA pixel (if in bounds).
                if right_solid < x + w && x_arc_frac > 0 {
                    let rx = right_solid;
                    if rx < surf_w {
                        let cov = x_arc_frac;
                        // SAFETY: rx < surf_w and py < surf_h (checked above).
                        unsafe {
                            rounded_rect_write_aa_pixel(
                                ptr, rx, py, stride, bpp,
                                src_r_lin, src_g_lin, src_b_lin, color.a as u32, cov,
                            );
                        }
                    }
                }

                // Solid interior pixels for this arc row.
                let fill_x0 = if left_solid < x { x } else { left_solid };
                let fill_x1 = if right_solid > x + w { x + w } else { right_solid };

                if fill_x0 < fill_x1 {
                    let clipped_x0 = if fill_x0 >= surf_w { continue } else { fill_x0 };
                    let clipped_x1 = min(fill_x1, surf_w);
                    let count = (clipped_x1 - clipped_x0) as usize;

                    if count > 0 {
                        let row_offset = (py * stride + clipped_x0 * bpp) as usize;
                        // SAFETY: py < surf_h, clipped_x0..clipped_x1 within [0, surf_w).
                        // row_offset + count * 4 <= surf_h * stride <= data.len().
                        unsafe {
                            let row_ptr = ptr.add(row_offset) as *mut u32;
                            #[cfg(target_arch = "aarch64")]
                            {
                                neon_fill_row(row_ptr, count, pixel_u32);
                            }
                            #[cfg(not(target_arch = "aarch64"))]
                            {
                                for i in 0..count {
                                    core::ptr::write(row_ptr.add(i), pixel_u32);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fill a rounded rectangle with alpha-blended color. Clips to surface bounds.
    ///
    /// Each destination pixel is blended using source-over. Corner pixels use
    /// per-pixel coverage derived from the SDF for anti-aliasing, combined with
    /// the source alpha. Interior rows use `fill_rect_blend` fast path.
    ///
    /// `radius` is clamped to `min(w, h) / 2`. Zero radius delegates to
    /// `fill_rect_blend`. Opaque colors fast-path to `fill_rounded_rect`.
    pub fn fill_rounded_rect_blend(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        radius: u32,
        color: Color,
    ) {
        if color.a == 255 {
            self.fill_rounded_rect(x, y, w, h, radius, color);
            return;
        }
        if color.a == 0 || w == 0 || h == 0 {
            return;
        }

        // Clamp radius.
        let max_r = min(w, h) / 2;
        let r = min(radius, max_r);

        if r == 0 {
            self.fill_rect_blend(x, y, w, h, color);
            return;
        }

        // Interior rows: fill_rect_blend fast path.
        if h > 2 * r {
            self.fill_rect_blend(x, y + r, w, h - 2 * r, color);
        }

        // Pre-convert source color for blending.
        let sa = color.a as u32;
        let inv_sa = 255 - sa;
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;
        let bpp = self.format.bytes_per_pixel();
        let stride = self.stride;
        let surf_w = self.width;
        let surf_h = self.height;
        let ptr = self.data.as_mut_ptr();

        for arc_row in 0..r {
            let dy_fp: i64 = (r as i64 * 256) - (arc_row as i64 * 256) - 128;
            let dy_sq = (dy_fp * dy_fp) as u64;
            let r_sq = (r as u64 * 256) * (r as u64 * 256);
            let x_arc_sq = if r_sq > dy_sq { r_sq - dy_sq } else { 0 };
            let x_arc_fp = isqrt_fp(x_arc_sq);

            let rows: [u32; 2] = [y + arc_row, y + h - 1 - arc_row];
            for &py in &rows {
                if py >= surf_h {
                    continue;
                }

                let x_arc_int = (x_arc_fp >> 8) as u32;
                let x_arc_frac = (x_arc_fp & 0xFF) as u32;

                let left_solid = x + r - x_arc_int;
                let right_solid = x + w - r + x_arc_int;

                // Left AA pixel.
                if left_solid > 0 && x_arc_frac > 0 {
                    let lx = left_solid - 1;
                    if lx >= x && lx < surf_w {
                        // Effective alpha = color.a * coverage / 256.
                        let eff_a = (sa * x_arc_frac) >> 8;
                        if eff_a > 0 {
                            let eff_inv_a = 255 - eff_a;
                            // SAFETY: lx < surf_w, py < surf_h.
                            unsafe {
                                let p = ptr.add((py * stride + lx * bpp) as usize);
                                fill_rect_blend_scalar_1px(
                                    p, src_r_lin, src_g_lin, src_b_lin, eff_a, eff_inv_a,
                                );
                            }
                        }
                    }
                }

                // Right AA pixel.
                if right_solid < x + w && x_arc_frac > 0 {
                    let rx = right_solid;
                    if rx < surf_w {
                        let eff_a = (sa * x_arc_frac) >> 8;
                        if eff_a > 0 {
                            let eff_inv_a = 255 - eff_a;
                            // SAFETY: rx < surf_w, py < surf_h.
                            unsafe {
                                let p = ptr.add((py * stride + rx * bpp) as usize);
                                fill_rect_blend_scalar_1px(
                                    p, src_r_lin, src_g_lin, src_b_lin, eff_a, eff_inv_a,
                                );
                            }
                        }
                    }
                }

                // Solid interior pixels — blend with full source alpha.
                let fill_x0 = if left_solid < x { x } else { left_solid };
                let fill_x1 = if right_solid > x + w { x + w } else { right_solid };

                if fill_x0 < fill_x1 {
                    let clipped_x0 = if fill_x0 >= surf_w { continue } else { fill_x0 };
                    let clipped_x1 = min(fill_x1, surf_w);
                    let count = (clipped_x1 - clipped_x0) as usize;

                    if count > 0 {
                        let row_offset = (py * stride + clipped_x0 * bpp) as usize;
                        // SAFETY: py < surf_h, clipped_x0..clipped_x1 within [0, surf_w).
                        unsafe {
                            let row_ptr = ptr.add(row_offset);
                            #[cfg(target_arch = "aarch64")]
                            {
                                let chunks = count / 4;
                                let tail_start = chunks * 4;
                                for chunk in 0..chunks {
                                    let p = row_ptr.add(chunk * 16);
                                    neon_blend_const_4px(
                                        p,
                                        src_r_lin as u16,
                                        src_g_lin as u16,
                                        src_b_lin as u16,
                                        sa as u16,
                                        inv_sa as u16,
                                        &SRGB_TO_LINEAR,
                                        &LINEAR_TO_SRGB,
                                    );
                                }
                                for i in tail_start..count {
                                    let p = row_ptr.add(i * 4);
                                    fill_rect_blend_scalar_1px(
                                        p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa,
                                    );
                                }
                            }
                            #[cfg(not(target_arch = "aarch64"))]
                            {
                                for i in 0..count {
                                    let p = row_ptr.add(i * 4);
                                    fill_rect_blend_scalar_1px(
                                        p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa,
                                    );
                                }
                            }
                        }
                    }
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

    /// Blit source pixels onto this surface with per-pixel alpha blending,
    /// modulated by a global opacity (0–255).
    ///
    /// Each source pixel's alpha is multiplied by `opacity / 255` before
    /// compositing over the destination. This implements group opacity:
    /// the source buffer contains a fully-composited subtree, and we
    /// composite the entire buffer at a reduced opacity.
    ///
    /// sRGB gamma-correct blending is used throughout, matching the
    /// existing `blit_blend` and `blend_over` behaviour.
    ///
    /// `opacity == 255` is equivalent to `blit_blend`.
    /// `opacity == 0` is a no-op (no pixels modified).
    pub fn blit_blend_with_opacity(
        &mut self,
        src_data: &[u8],
        src_width: u32,
        src_height: u32,
        src_stride: u32,
        dst_x: u32,
        dst_y: u32,
        opacity: u8,
    ) {
        if opacity == 0 {
            return;
        }
        if opacity == 255 {
            self.blit_blend(src_data, src_width, src_height, src_stride, dst_x, dst_y);
            return;
        }
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
        let opa = opacity as u32;

        for row in 0..copy_h {
            let src_row_off = (row * src_stride) as usize;
            let dst_row_off = ((dst_y + row) * dst_stride + dst_x * bpp) as usize;
            let row_bytes = (copy_w * bpp) as usize;

            if src_row_off + row_bytes > src_data.len() {
                continue;
            }

            for col in 0..copy_w {
                let src_off = src_row_off + (col * bpp) as usize;
                let dst_off = dst_row_off + (col * bpp) as usize;

                if dst_off + 4 > self.data.len() {
                    continue;
                }

                // Source pixel (BGRA).
                let src_b = src_data[src_off];
                let src_g = src_data[src_off + 1];
                let src_r = src_data[src_off + 2];
                let src_a = src_data[src_off + 3];

                if src_a == 0 {
                    continue;
                }

                // Modulate source alpha by group opacity.
                let effective_a = div255(src_a as u32 * opa) as u8;
                if effective_a == 0 {
                    continue;
                }

                let src_color = Color {
                    r: src_r,
                    g: src_g,
                    b: src_b,
                    a: effective_a,
                };

                // Read destination pixel.
                let dst_b = self.data[dst_off];
                let dst_g = self.data[dst_off + 1];
                let dst_r = self.data[dst_off + 2];
                let dst_a = self.data[dst_off + 3];
                let dst_color = Color {
                    r: dst_r,
                    g: dst_g,
                    b: dst_b,
                    a: dst_a,
                };

                // sRGB-correct blend.
                let blended = src_color.blend_over(dst_color);

                self.data[dst_off] = blended.b;
                self.data[dst_off + 1] = blended.g;
                self.data[dst_off + 2] = blended.r;
                self.data[dst_off + 3] = blended.a;
            }
        }
    }

    /// Blit a source buffer to this surface with a 2D affine inverse transform
    /// and bilinear interpolation. For each destination pixel in the region
    /// `(dst_x, dst_y, dst_w, dst_h)`, the inverse transform maps back to
    /// source coordinates. Bilinear sampling produces anti-aliased results
    /// for rotated, scaled, and skewed content.
    ///
    /// `inv_a..inv_ty` are the 6 elements of the **inverse** affine matrix.
    /// The caller computes the inverse of the forward transform so that for
    /// each destination pixel `(dx, dy)`:
    ///   `src_x = inv_a * dx + inv_c * dy + inv_tx`
    ///   `src_y = inv_b * dx + inv_d * dy + inv_ty`
    ///
    /// `opacity` modulates the source alpha (255 = fully opaque).
    ///
    /// Source pixels outside `[0, src_width) × [0, src_height)` are treated
    /// as transparent (no contribution).
    ///
    /// Wrapper that defaults to `ResamplingMethod::Bilinear`. Use
    /// [`blit_blend_bilinear`] for explicit method selection.
    pub fn blit_transformed_bilinear(
        &mut self,
        src_data: &[u8],
        src_width: u32,
        src_height: u32,
        src_stride: u32,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
        inv_a: f32,
        inv_b: f32,
        inv_c: f32,
        inv_d: f32,
        inv_tx: f32,
        inv_ty: f32,
        opacity: u8,
    ) {
        if opacity == 0 || dst_w == 0 || dst_h == 0 {
            return;
        }

        let fb_w = self.width as i32;
        let fb_h = self.height as i32;
        let sw = src_width as i32;
        let sh = src_height as i32;
        let fb_stride = self.stride;

        for row in 0..dst_h {
            let dy = dst_y + row as i32;
            if dy < 0 || dy >= fb_h {
                continue;
            }
            let fb_row_off = (dy as u32 * fb_stride) as usize;

            for col in 0..dst_w {
                let dx = dst_x + col as i32;
                if dx < 0 || dx >= fb_w {
                    continue;
                }

                // Map destination pixel to source coordinates.
                let sx_f = inv_a * col as f32 + inv_c * row as f32 + inv_tx;
                let sy_f = inv_b * col as f32 + inv_d * row as f32 + inv_ty;

                // Bilinear interpolation: sample the 4 surrounding source pixels.
                let sx_floor = if sx_f >= 0.0 { sx_f as i32 } else { sx_f as i32 - 1 };
                let sy_floor = if sy_f >= 0.0 { sy_f as i32 } else { sy_f as i32 - 1 };

                // Skip if completely outside source bounds.
                if sx_floor + 1 < 0 || sx_floor >= sw || sy_floor + 1 < 0 || sy_floor >= sh {
                    continue;
                }

                // Fractional parts (0..1 range, stored as 0..256 fixed-point).
                let fx = ((sx_f - sx_floor as f32) * 256.0) as u32;
                let fy = ((sy_f - sy_floor as f32) * 256.0) as u32;
                let fx = if fx > 256 { 256 } else { fx };
                let fy = if fy > 256 { 256 } else { fy };

                // Sample 4 source pixels (clamp to transparent for out-of-bounds).
                let p00 = sample_src(src_data, src_stride, sw, sh, sx_floor, sy_floor);
                let p10 = sample_src(src_data, src_stride, sw, sh, sx_floor + 1, sy_floor);
                let p01 = sample_src(src_data, src_stride, sw, sh, sx_floor, sy_floor + 1);
                let p11 = sample_src(src_data, src_stride, sw, sh, sx_floor + 1, sy_floor + 1);

                // Bilinear blend in BGRA space.
                let inv_fx = 256 - fx;
                let inv_fy = 256 - fy;

                // Interpolate top row: lerp(p00, p10, fx).
                let top_b = (p00.0 as u32 * inv_fx + p10.0 as u32 * fx) >> 8;
                let top_g = (p00.1 as u32 * inv_fx + p10.1 as u32 * fx) >> 8;
                let top_r = (p00.2 as u32 * inv_fx + p10.2 as u32 * fx) >> 8;
                let top_a = (p00.3 as u32 * inv_fx + p10.3 as u32 * fx) >> 8;

                // Interpolate bottom row: lerp(p01, p11, fx).
                let bot_b = (p01.0 as u32 * inv_fx + p11.0 as u32 * fx) >> 8;
                let bot_g = (p01.1 as u32 * inv_fx + p11.1 as u32 * fx) >> 8;
                let bot_r = (p01.2 as u32 * inv_fx + p11.2 as u32 * fx) >> 8;
                let bot_a = (p01.3 as u32 * inv_fx + p11.3 as u32 * fx) >> 8;

                // Interpolate columns: lerp(top, bot, fy).
                let fin_b = ((top_b * inv_fy + bot_b * fy) >> 8) as u8;
                let fin_g = ((top_g * inv_fy + bot_g * fy) >> 8) as u8;
                let fin_r = ((top_r * inv_fy + bot_r * fy) >> 8) as u8;
                let mut fin_a = ((top_a * inv_fy + bot_a * fy) >> 8) as u8;

                if fin_a == 0 {
                    continue;
                }

                // Apply group opacity.
                if opacity < 255 {
                    fin_a = div255(fin_a as u32 * opacity as u32) as u8;
                    if fin_a == 0 {
                        continue;
                    }
                }

                // Composite over destination using sRGB-correct blending.
                let fb_off = fb_row_off + (dx as usize * 4);
                if fb_off + 4 > self.data.len() {
                    continue;
                }

                let src_color = Color { r: fin_r, g: fin_g, b: fin_b, a: fin_a };
                let dst_b = self.data[fb_off];
                let dst_g = self.data[fb_off + 1];
                let dst_r = self.data[fb_off + 2];
                let dst_a = self.data[fb_off + 3];
                let dst_color = Color { r: dst_r, g: dst_g, b: dst_b, a: dst_a };

                let blended = src_color.blend_over(dst_color);

                self.data[fb_off] = blended.b;
                self.data[fb_off + 1] = blended.g;
                self.data[fb_off + 2] = blended.r;
                self.data[fb_off + 3] = blended.a;
            }
        }
    }

    /// Blit a source buffer to this surface with a 2D affine inverse transform
    /// and configurable resampling method.
    ///
    /// This is the primary interface for scaled/transformed blits. The
    /// `_method` parameter selects the resampling algorithm. Currently only
    /// `Bilinear` is implemented; `Lanczos` can be added later without
    /// changing call sites.
    ///
    /// Parameters are identical to [`blit_transformed_bilinear`] plus the
    /// resampling method selector.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_blend_bilinear(
        &mut self,
        src_data: &[u8],
        src_width: u32,
        src_height: u32,
        src_stride: u32,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
        inv_a: f32,
        inv_b: f32,
        inv_c: f32,
        inv_d: f32,
        inv_tx: f32,
        inv_ty: f32,
        opacity: u8,
        _method: ResamplingMethod,
    ) {
        // Dispatch to the appropriate implementation based on method.
        // Currently only Bilinear is supported; future variants
        // (e.g., Lanczos) would branch here.
        match _method {
            ResamplingMethod::Bilinear => {
                self.blit_transformed_bilinear(
                    src_data, src_width, src_height, src_stride,
                    dst_x, dst_y, dst_w, dst_h,
                    inv_a, inv_b, inv_c, inv_d, inv_tx, inv_ty,
                    opacity,
                );
            }
        }
    }
}

/// Sample a source pixel at integer coordinates. Returns (B, G, R, A).
/// Out-of-bounds pixels return transparent (0, 0, 0, 0).
#[inline]
fn sample_src(data: &[u8], stride: u32, w: i32, h: i32, x: i32, y: i32) -> (u8, u8, u8, u8) {
    if x < 0 || x >= w || y < 0 || y >= h {
        return (0, 0, 0, 0);
    }
    let off = (y as u32 * stride + x as u32 * 4) as usize;
    if off + 4 > data.len() {
        return (0, 0, 0, 0);
    }
    (data[off], data[off + 1], data[off + 2], data[off + 3])
}

/// Scalar fill_rect_blend for a single destination pixel (unsafe helper).
///
/// # Safety
///
/// `p` must point to 4 readable and writable bytes (destination BGRA pixel).
#[inline(always)]
unsafe fn fill_rect_blend_scalar_1px(
    p: *mut u8,
    src_r_lin: u32,
    src_g_lin: u32,
    src_b_lin: u32,
    sa: u32,
    inv_sa: u32,
) {
    // Read destination BGRA pixel.
    let dst_b = core::ptr::read(p);
    let dst_g = core::ptr::read(p.add(1));
    let dst_r = core::ptr::read(p.add(2));
    let dst_a = core::ptr::read(p.add(3));

    let da = dst_a as u32;
    let da_eff = div255(da * inv_sa);
    let out_a = sa + da_eff;

    if out_a == 0 {
        return;
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

/// Scalar blit-blend for a single pixel (unsafe helper).
///
/// # Safety
///
/// `sp` must point to 4 readable bytes (source BGRA pixel).
/// `dp` must point to 4 readable and writable bytes (destination BGRA pixel).
#[inline(always)]
unsafe fn blit_blend_scalar_1px(sp: *const u8, dp: *mut u8, format: PixelFormat) {
    let src_a = core::ptr::read(sp.add(3));

    if src_a == 0 {
        return;
    }

    if src_a == 255 {
        // Opaque: direct copy (4 bytes).
        core::ptr::copy_nonoverlapping(sp, dp, 4);
        return;
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
    let encoded = blended.encode(format);

    core::ptr::write(dp, encoded[0]);
    core::ptr::write(dp.add(1), encoded[1]);
    core::ptr::write(dp.add(2), encoded[2]);
    core::ptr::write(dp.add(3), encoded[3]);
}

/// Scalar blit-blend for 4 pixels where all are either fully opaque or
/// fully transparent (no semi-transparent pixels).
///
/// # Safety
///
/// `sp` must point to 16 readable bytes (4 source BGRA pixels).
/// `dp` must point to 16 readable and writable bytes (4 destination BGRA pixels).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn blit_blend_scalar_4px(sp: *const u8, dp: *mut u8, bpp: u32) {
    for i in 0..4u32 {
        let offset = (i * bpp) as usize;
        let src_a = core::ptr::read(sp.add(offset + 3));

        if src_a == 0 {
            continue;
        }

        // Must be 255 since has_semi was false.
        core::ptr::copy_nonoverlapping(sp.add(offset), dp.add(offset), 4);
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

/// Integer square root of a 64-bit value in 8.8 fixed-point.
///
/// Given `x` in 16.16 fixed-point (i.e., the value `n * 256 * n * 256` where
/// `n` is in 8.8 fixed-point), returns `sqrt(x)` in 8.8 fixed-point.
/// Uses binary search with bit-at-a-time refinement. Never panics.
fn isqrt_fp(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut result: u64 = 0;
    let mut bit: u64 = 1u64 << 30; // Start from highest reasonable bit.

    // Find the highest bit position for square root.
    while bit > x {
        bit >>= 2;
    }

    while bit != 0 {
        let candidate = result + bit;
        if x >= candidate * candidate {
            result = candidate;
        }
        bit >>= 1;
    }

    result
}

/// Write a single anti-aliased pixel for a rounded rectangle corner.
///
/// Blends the shape color onto the existing destination using coverage-weighted
/// gamma-correct sRGB blending.
///
/// `cov` is coverage in 0..256 (8-bit fraction, where 256 = fully covered).
///
/// # Safety
///
/// The pixel at (px, py) must be within the surface bounds. `ptr` is the
/// surface data pointer, and `py * stride + px * bpp` must be a valid offset.
#[inline(always)]
unsafe fn rounded_rect_write_aa_pixel(
    ptr: *mut u8,
    px: u32,
    py: u32,
    stride: u32,
    bpp: u32,
    src_r_lin: u32,
    src_g_lin: u32,
    src_b_lin: u32,
    src_a: u32,
    cov: u32,
) {
    let offset = (py * stride + px * bpp) as usize;
    let p = ptr.add(offset);

    // Effective alpha = src_a * cov / 256.
    let eff_a = (src_a * cov) >> 8;
    if eff_a == 0 {
        return;
    }
    if eff_a >= 255 {
        // Fully covered and fully opaque: write solid pixel.
        let color = Color {
            r: LINEAR_TO_SRGB[linear_to_idx(src_r_lin)],
            g: LINEAR_TO_SRGB[linear_to_idx(src_g_lin)],
            b: LINEAR_TO_SRGB[linear_to_idx(src_b_lin)],
            a: 255,
        };
        let encoded = color.encode(PixelFormat::Bgra8888);
        core::ptr::write(p as *mut u32, u32::from_ne_bytes(encoded));
        return;
    }

    let eff_inv_a = 255 - eff_a;
    fill_rect_blend_scalar_1px(p, src_r_lin, src_g_lin, src_b_lin, eff_a, eff_inv_a);
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
// Gaussian blur — separable two-pass convolution
// ---------------------------------------------------------------------------

/// Maximum blur radius for the CPU path. Larger radii are clamped.
pub const MAX_CPU_BLUR_RADIUS: u32 = 16;

/// Maximum kernel diameter (2 * MAX_CPU_BLUR_RADIUS + 1).
pub const MAX_KERNEL_DIAMETER: usize = (2 * MAX_CPU_BLUR_RADIUS + 1) as usize;

/// An immutable view into a pixel buffer (source for blur).
pub struct ReadSurface<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

/// Strategy for blur operations. Trait-based interface accommodates a future
/// GPU path — callers program against the trait, implementations are leaf
/// nodes that can be swapped.
pub trait BlurStrategy {
    /// Apply Gaussian blur from `src` into `dst`.
    ///
    /// `tmp` is scratch space for the intermediate horizontal pass.
    /// Must be at least `dst.stride * dst.height` bytes.
    ///
    /// `radius` is the blur kernel half-width (capped by implementation).
    /// `sigma_fp` is the Gaussian spread in 8.8 fixed-point (256 = sigma 1.0).
    fn blur(
        &self,
        src: &ReadSurface,
        dst: &mut Surface,
        tmp: &mut [u8],
        radius: u32,
        sigma_fp: u32,
    );
}

/// CPU implementation of Gaussian blur using two-pass separable convolution.
///
/// Pass 1 (horizontal): convolve each row of `src` into `tmp`.
/// Pass 2 (vertical): convolve each column of `tmp` into `dst`.
///
/// Edge handling: clamp at surface boundaries.
/// On aarch64, the inner loop uses NEON SIMD to process 4 pixels at a time.
pub struct CpuBlur;

impl BlurStrategy for CpuBlur {
    fn blur(
        &self,
        src: &ReadSurface,
        dst: &mut Surface,
        tmp: &mut [u8],
        radius: u32,
        sigma_fp: u32,
    ) {
        blur_surface(src, dst, tmp, radius, sigma_fp);
    }
}

/// Compute 1D Gaussian kernel weights in 16.16 fixed-point.
///
/// Returns the number of entries written (always `2 * effective_radius + 1`).
/// `kernel` must have at least `MAX_KERNEL_DIAMETER` entries.
/// Weights are normalized so they sum to 65536 (1.0 in 16.16 FP).
///
/// Uses integer-only Gaussian approximation: `exp(-x²/(2σ²))` is approximated
/// via a piecewise polynomial in fixed-point.
pub fn compute_kernel(kernel: &mut [u32; MAX_KERNEL_DIAMETER], radius: u32, sigma_fp: u32) -> usize {
    let r = if radius > MAX_CPU_BLUR_RADIUS {
        MAX_CPU_BLUR_RADIUS
    } else {
        radius
    };
    let diameter = (2 * r + 1) as usize;

    if r == 0 {
        kernel[0] = 65536;
        return 1;
    }

    // sigma_fp is 8.8 FP. Convert sigma² to 16.16 FP for computation.
    // sigma² = (sigma_fp / 256)² = sigma_fp² / 65536
    // In 16.16 FP: sigma²_fp16 = sigma_fp² * 65536 / 65536 = sigma_fp²
    // But we need 2*sigma² in a form we can divide by.
    // 2σ² in 16.16 FP = 2 * sigma_fp * sigma_fp / 256 (since sigma_fp is 8.8)
    let sigma_fp64 = sigma_fp as u64;
    let two_sigma_sq_fp = 2 * sigma_fp64 * sigma_fp64; // in 16.16 FP (via 8.8 * 8.8 = 16.16)

    if two_sigma_sq_fp == 0 {
        // sigma ≈ 0: all weight on center.
        for w in kernel[..diameter].iter_mut() {
            *w = 0;
        }
        kernel[r as usize] = 65536;
        return diameter;
    }

    // Compute raw weights using integer approximation of Gaussian.
    // g(x) = exp(-x² / (2σ²))
    // We approximate: weight(x) = 2^16 * exp(-x² * 2^16 / two_sigma_sq_fp)
    // Using the identity: exp(-t) ≈ (1 - t/n)^n for small t.
    // Instead, use a lookup-based approach with a quadratic decay:
    // g(x) ∝ max(0, two_sigma_sq_fp - x² * 256) for a simple bell curve,
    // but this doesn't match Gaussian well. Instead, use iterative exp approx.
    //
    // Practical approach: compute exp(-x²/(2σ²)) using a fixed-point
    // Taylor-like approximation. For x²/(2σ²) = t:
    //   exp(-t) ≈ 1/(1 + t + t²/2 + t³/6) (Padé-like)
    //   or simpler: exp(-t) ≈ 256/(256 + t*256) for small t
    //
    // More accurate: use the fact that exp(-t) for integer t can be
    // computed via repeated squaring of exp(-1).
    //
    // Simplest correct approach: compute weights as integers proportional
    // to the Gaussian, then normalize.
    let mut raw = [0u64; MAX_KERNEL_DIAMETER];
    let mut sum: u64 = 0;

    for i in 0..diameter {
        let x = i as i64 - r as i64;
        let x_sq = (x * x) as u64;

        // t = x² * 65536 / two_sigma_sq_fp (in 16.16 FP)
        // We want exp(-x²/(2σ²)).
        // x²/(2σ²) = x_sq * 65536 / two_sigma_sq_fp (converting to same FP scale)
        //
        // Compute exp(-t) where t = x_sq * 65536 / two_sigma_sq_fp
        // Using integer exp approximation: exp(-t) ≈ 2^24 / (2^24 + t * k)
        // where k is chosen to match the Gaussian.
        //
        // Better: use the series exp(-t) = 1 - t + t²/2 - t³/6 + ...
        // but with fixed-point. For t in 0..~radius²/(2σ²):
        // At radius=16, sigma=1: t = 128, which is very large.
        //
        // Most practical: use a 2^(-t * log2(e)) style computation.
        // exp(-t) = 2^(-t/ln2) = 2^(-t * 1.4427)
        //
        // Simplest correct integer-only Gaussian:
        // Weight = round(65536 * exp(-x²/(2σ²)))
        // We compute this with enough precision using only integers.

        // Numerator: x² << 24 (for precision)
        // Denominator: 2σ² in 8.8*8.8 = 16.16 scale
        // Quotient: x²/(2σ²) in 8.8 FP (shifted by 24-16=8)
        let t_fp8 = if two_sigma_sq_fp > 0 {
            (x_sq << 24) / two_sigma_sq_fp // result in ~8.8 FP
        } else {
            u64::MAX
        };

        // exp(-t) approximation using (1024 - t)^6 / 1024^6 style decay,
        // or more accurately, use the rational approximation:
        // exp(-t) ≈ (1 + t/2)^-2 is a decent Padé[0/2].
        // Better: exp(-t) ≈ 1/(1 + t + t²/2) (3-term Taylor denominator).
        //
        // With t in 8.8 FP (t_fp8), where 256 = 1.0:
        // denom = 256 + t_fp8 + (t_fp8 * t_fp8) / (2 * 256)
        // weight = 256 * 65536 / denom  (yields weight in 16.16 FP)
        if t_fp8 > 8 * 256 {
            // exp(-8) ≈ 0.00034 — negligible, set to 0.
            raw[i] = 0;
        } else {
            let t = t_fp8;
            let t_sq = t * t;
            // Denominator in 8.8 FP: 1 + t + t²/2 + t³/6
            // = 256 + t + t²/512 + t³/393216
            let denom = 256u64 + t + t_sq / 512 + (t_sq * t) / (512 * 768);
            if denom > 0 {
                raw[i] = (256u64 * 65536) / denom;
            }
        }
        sum += raw[i];
    }

    // Normalize to sum = 65536.
    if sum == 0 {
        kernel[r as usize] = 65536;
        return diameter;
    }

    let mut normalized_sum: u64 = 0;
    for i in 0..diameter {
        kernel[i] = ((raw[i] * 65536 + sum / 2) / sum) as u32;
        normalized_sum += kernel[i] as u64;
    }

    // Adjust center to ensure exact sum = 65536.
    let diff = 65536i64 - normalized_sum as i64;
    kernel[r as usize] = (kernel[r as usize] as i64 + diff) as u32;

    diameter
}

/// Apply Gaussian blur from `src` into `dst` using two-pass separable convolution.
///
/// `tmp` is scratch space for the intermediate horizontal pass.
/// Must be at least `dst.stride * dst.height` bytes.
///
/// `radius` is clamped to `MAX_CPU_BLUR_RADIUS` (16).
/// `sigma_fp` is Gaussian spread in 8.8 fixed-point (256 = sigma 1.0).
///
/// If `radius == 0`, copies `src` to `dst` (identity).
pub fn blur_surface(
    src: &ReadSurface,
    dst: &mut Surface,
    tmp: &mut [u8],
    radius: u32,
    sigma_fp: u32,
) {
    let r = if radius > MAX_CPU_BLUR_RADIUS {
        MAX_CPU_BLUR_RADIUS
    } else {
        radius
    };

    let w = src.width;
    let h = src.height;

    if w == 0 || h == 0 {
        return;
    }

    // Identity: radius=0 means no blur.
    if r == 0 {
        // Copy src to dst.
        let bpp = src.format.bytes_per_pixel();
        let row_bytes = (w * bpp) as usize;
        for y in 0..h {
            let src_off = (y * src.stride) as usize;
            let dst_off = (y * dst.stride) as usize;
            if src_off + row_bytes <= src.data.len() && dst_off + row_bytes <= dst.data.len() {
                dst.data[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&src.data[src_off..src_off + row_bytes]);
            }
        }
        return;
    }

    // Compute kernel.
    let mut kernel = [0u32; MAX_KERNEL_DIAMETER];
    let diameter = compute_kernel(&mut kernel, r, sigma_fp);
    let kernel_slice = &kernel[..diameter];

    // Ensure tmp is large enough.
    let dst_stride = dst.stride;
    let needed = (dst_stride * h) as usize;
    if tmp.len() < needed {
        return;
    }

    // Pass 1: horizontal blur from src → tmp.
    blur_horizontal(src.data, tmp, w, h, src.stride, dst_stride, r, kernel_slice);

    // Pass 2: vertical blur from tmp → dst.
    blur_vertical(tmp, dst.data, w, h, dst_stride, dst_stride, r, kernel_slice);
}

/// Apply Gaussian blur using only the scalar (non-NEON) code path.
/// Used for testing NEON correctness.
pub fn blur_surface_scalar(
    src: &ReadSurface,
    dst: &mut Surface,
    tmp: &mut [u8],
    radius: u32,
    sigma_fp: u32,
) {
    let r = if radius > MAX_CPU_BLUR_RADIUS {
        MAX_CPU_BLUR_RADIUS
    } else {
        radius
    };

    let w = src.width;
    let h = src.height;

    if w == 0 || h == 0 {
        return;
    }

    if r == 0 {
        let bpp = src.format.bytes_per_pixel();
        let row_bytes = (w * bpp) as usize;
        for y in 0..h {
            let src_off = (y * src.stride) as usize;
            let dst_off = (y * dst.stride) as usize;
            if src_off + row_bytes <= src.data.len() && dst_off + row_bytes <= dst.data.len() {
                dst.data[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&src.data[src_off..src_off + row_bytes]);
            }
        }
        return;
    }

    let mut kernel = [0u32; MAX_KERNEL_DIAMETER];
    let diameter = compute_kernel(&mut kernel, r, sigma_fp);
    let kernel_slice = &kernel[..diameter];

    let dst_stride = dst.stride;
    let needed = (dst_stride * h) as usize;
    if tmp.len() < needed {
        return;
    }

    blur_horizontal_scalar(src.data, tmp, w, h, src.stride, dst_stride, r, kernel_slice);
    blur_vertical_scalar(tmp, dst.data, w, h, dst_stride, dst_stride, r, kernel_slice);
}

/// Horizontal blur pass (scalar implementation).
fn blur_horizontal_scalar(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    src_stride: u32,
    dst_stride: u32,
    radius: u32,
    kernel: &[u32],
) {
    let r = radius as i32;
    let bpp = 4u32;

    for y in 0..height {
        let src_row = (y * src_stride) as usize;
        let dst_row = (y * dst_stride) as usize;

        for x in 0..width {
            let mut sum_b: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_r: u64 = 0;
            let mut sum_a: u64 = 0;

            for k in -r..=r {
                let sx = if x as i32 + k < 0 {
                    0u32
                } else if (x as i32 + k) >= width as i32 {
                    width - 1
                } else {
                    (x as i32 + k) as u32
                };

                let src_off = src_row + (sx * bpp) as usize;
                let w = kernel[(k + r) as usize] as u64;

                sum_b += src[src_off] as u64 * w;
                sum_g += src[src_off + 1] as u64 * w;
                sum_r += src[src_off + 2] as u64 * w;
                sum_a += src[src_off + 3] as u64 * w;
            }

            let dst_off = dst_row + (x * bpp) as usize;
            // Weights sum to 65536, so divide by 65536 (>> 16) with rounding.
            dst[dst_off] = ((sum_b + 32768) >> 16) as u8;
            dst[dst_off + 1] = ((sum_g + 32768) >> 16) as u8;
            dst[dst_off + 2] = ((sum_r + 32768) >> 16) as u8;
            dst[dst_off + 3] = ((sum_a + 32768) >> 16) as u8;
        }
    }
}

/// Vertical blur pass (scalar implementation).
fn blur_vertical_scalar(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    src_stride: u32,
    dst_stride: u32,
    radius: u32,
    kernel: &[u32],
) {
    let r = radius as i32;
    let bpp = 4u32;

    for y in 0..height {
        let dst_row = (y * dst_stride) as usize;

        for x in 0..width {
            let mut sum_b: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_r: u64 = 0;
            let mut sum_a: u64 = 0;

            for k in -r..=r {
                let sy = if y as i32 + k < 0 {
                    0u32
                } else if (y as i32 + k) >= height as i32 {
                    height - 1
                } else {
                    (y as i32 + k) as u32
                };

                let src_off = (sy * src_stride + x * bpp) as usize;
                let w = kernel[(k + r) as usize] as u64;

                sum_b += src[src_off] as u64 * w;
                sum_g += src[src_off + 1] as u64 * w;
                sum_r += src[src_off + 2] as u64 * w;
                sum_a += src[src_off + 3] as u64 * w;
            }

            let dst_off = dst_row + (x * bpp) as usize;
            dst[dst_off] = ((sum_b + 32768) >> 16) as u8;
            dst[dst_off + 1] = ((sum_g + 32768) >> 16) as u8;
            dst[dst_off + 2] = ((sum_r + 32768) >> 16) as u8;
            dst[dst_off + 3] = ((sum_a + 32768) >> 16) as u8;
        }
    }
}

/// Horizontal blur pass — dispatches to NEON or scalar.
fn blur_horizontal(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    src_stride: u32,
    dst_stride: u32,
    radius: u32,
    kernel: &[u32],
) {
    #[cfg(target_arch = "aarch64")]
    {
        blur_horizontal_neon(src, dst, width, height, src_stride, dst_stride, radius, kernel);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        blur_horizontal_scalar(src, dst, width, height, src_stride, dst_stride, radius, kernel);
    }
}

/// Vertical blur pass — dispatches to NEON or scalar.
fn blur_vertical(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    src_stride: u32,
    dst_stride: u32,
    radius: u32,
    kernel: &[u32],
) {
    #[cfg(target_arch = "aarch64")]
    {
        blur_vertical_neon(src, dst, width, height, src_stride, dst_stride, radius, kernel);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        blur_vertical_scalar(src, dst, width, height, src_stride, dst_stride, radius, kernel);
    }
}


