//! Affine-transformed blits with bilinear interpolation.

use crate::{
    div255, linear_to_idx, Color, ResamplingMethod, Surface, LINEAR_TO_SRGB, SRGB_TO_LINEAR,
};

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

impl<'a> Surface<'a> {
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
    /// Source pixels outside `[0, src_width) x [0, src_height)` are treated
    /// as transparent (no contribution).
    ///
    /// Wrapper that defaults to `ResamplingMethod::Bilinear`. Use
    /// [`blit_blend_bilinear`] for explicit method selection.
    /// NOTE: Bilinear interpolation blends in sRGB space while the rest
    /// of the pipeline is gamma-correct. May cause banding/color shifts
    /// on rotated or scaled content (review 7.16).
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
                let sx_floor = if sx_f >= 0.0 {
                    sx_f as i32
                } else {
                    sx_f as i32 - 1
                };
                let sy_floor = if sy_f >= 0.0 {
                    sy_f as i32
                } else {
                    sy_f as i32 - 1
                };

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
                // Bilinear blend in linear light space (gamma-correct).
                let inv_fx = 256 - fx;
                let inv_fy = 256 - fy;
                // Linearize B, G, R channels of all 4 samples via SRGB_TO_LINEAR.
                // Alpha is already linear -- interpolated directly in 0-255 space.
                let p00_b = SRGB_TO_LINEAR[p00.0 as usize] as u32;
                let p00_g = SRGB_TO_LINEAR[p00.1 as usize] as u32;
                let p00_r = SRGB_TO_LINEAR[p00.2 as usize] as u32;
                let p10_b = SRGB_TO_LINEAR[p10.0 as usize] as u32;
                let p10_g = SRGB_TO_LINEAR[p10.1 as usize] as u32;
                let p10_r = SRGB_TO_LINEAR[p10.2 as usize] as u32;
                let p01_b = SRGB_TO_LINEAR[p01.0 as usize] as u32;
                let p01_g = SRGB_TO_LINEAR[p01.1 as usize] as u32;
                let p01_r = SRGB_TO_LINEAR[p01.2 as usize] as u32;
                let p11_b = SRGB_TO_LINEAR[p11.0 as usize] as u32;
                let p11_g = SRGB_TO_LINEAR[p11.1 as usize] as u32;
                let p11_r = SRGB_TO_LINEAR[p11.2 as usize] as u32;
                // Interpolate top row in linear space: lerp(p00, p10, fx).
                let top_b = (p00_b * inv_fx + p10_b * fx) >> 8;
                let top_g = (p00_g * inv_fx + p10_g * fx) >> 8;
                let top_r = (p00_r * inv_fx + p10_r * fx) >> 8;
                let top_a = (p00.3 as u32 * inv_fx + p10.3 as u32 * fx) >> 8;
                // Interpolate bottom row in linear space: lerp(p01, p11, fx).
                let bot_b = (p01_b * inv_fx + p11_b * fx) >> 8;
                let bot_g = (p01_g * inv_fx + p11_g * fx) >> 8;
                let bot_r = (p01_r * inv_fx + p11_r * fx) >> 8;
                let bot_a = (p01.3 as u32 * inv_fx + p11.3 as u32 * fx) >> 8;
                // Interpolate columns in linear space: lerp(top, bot, fy).
                let lin_b = (top_b * inv_fy + bot_b * fy) >> 8;
                let lin_g = (top_g * inv_fy + bot_g * fy) >> 8;
                let lin_r = (top_r * inv_fy + bot_r * fy) >> 8;
                let mut fin_a = ((top_a * inv_fy + bot_a * fy) >> 8) as u8;
                // Convert back from linear to sRGB.
                let fin_b = LINEAR_TO_SRGB[linear_to_idx(lin_b)];
                let fin_g = LINEAR_TO_SRGB[linear_to_idx(lin_g)];
                let fin_r = LINEAR_TO_SRGB[linear_to_idx(lin_r)];

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

                let src_color = Color {
                    r: fin_r,
                    g: fin_g,
                    b: fin_b,
                    a: fin_a,
                };
                let dst_b = self.data[fb_off];
                let dst_g = self.data[fb_off + 1];
                let dst_r = self.data[fb_off + 2];
                let dst_a = self.data[fb_off + 3];
                let dst_color = Color {
                    r: dst_r,
                    g: dst_g,
                    b: dst_b,
                    a: dst_a,
                };
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
                    src_data, src_width, src_height, src_stride, dst_x, dst_y, dst_w, dst_h, inv_a,
                    inv_b, inv_c, inv_d, inv_tx, inv_ty, opacity,
                );
            }
        }
    }
}
