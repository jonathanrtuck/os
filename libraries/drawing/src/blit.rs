//! Blit operations: copy and alpha-blend source buffers onto surfaces.

#[cfg(target_arch = "aarch64")]
use crate::blend::blit_blend_scalar_4px;
use crate::{Color, SRGB_TO_LINEAR, Surface, blend::blit_blend_scalar_1px, div255, min};

impl<'a> Surface<'a> {
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

        assert!(self.is_valid(), "Surface invariant violated in blit_blend");

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
                            crate::neon_blend_4px(sp, dp, &SRGB_TO_LINEAR, &crate::LINEAR_TO_SRGB);
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

    /// Blit source pixels onto this surface with per-pixel alpha blending,
    /// modulated by a global opacity (0-255).
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
}
