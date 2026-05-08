//! Coverage-map rendering for anti-aliased glyphs.
//!
//! Draws 1-byte-per-pixel grayscale coverage maps (from font rasterizers)
//! onto BGRA surfaces with sRGB gamma-correct blending.

use crate::{Color, LINEAR_TO_SRGB, SRGB_TO_LINEAR, Surface, div255, linear_to_idx};

impl<'a> Surface<'a> {
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
        let end_row = if cov_h < surf_h - yi {
            cov_h
        } else {
            surf_h - yi
        };
        let start_col = if xi < 0 { -xi } else { 0 };
        let end_col = if cov_w < surf_w - xi {
            cov_w
        } else {
            surf_w - xi
        };

        if start_row >= end_row || start_col >= end_col || end_row <= 0 || end_col <= 0 {
            return;
        }

        assert!(
            self.is_valid(),
            "Surface invariant violated in draw_coverage"
        );

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
                let alpha = div255(color_a * cov as u32);

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
                    let out_r_lin = div255(dst_r_lin * inv_a + src_r_lin * alpha);
                    let out_g_lin = div255(dst_g_lin * inv_a + src_g_lin * alpha);
                    let out_b_lin = div255(dst_b_lin * inv_a + src_b_lin * alpha);

                    // Convert back to sRGB.
                    let out_r = LINEAR_TO_SRGB[linear_to_idx(out_r_lin)];
                    let out_g = LINEAR_TO_SRGB[linear_to_idx(out_g_lin)];
                    let out_b = LINEAR_TO_SRGB[linear_to_idx(out_b_lin)];

                    // Alpha compositing.
                    let out_a = dst_a_byte as u32 + div255(alpha * (255 - dst_a_byte as u32));
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
}
