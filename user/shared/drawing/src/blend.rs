//! Porter-Duff source-over compositing and scalar blend helpers.
//!
//! All blending is done in linear light (sRGB gamma-correct) using the
//! lookup tables in `gamma_tables.rs`. Integer math only.

use crate::{div255, linear_to_idx, Color, PixelFormat, Surface, LINEAR_TO_SRGB, SRGB_TO_LINEAR};

impl Color {
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
        // div255 is exact for da * inv_sa in 0..=65025.
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
}

/// Scalar fill_rect_blend for a single destination pixel (unsafe helper).
///
/// # Safety
///
/// `p` must point to 4 readable and writable bytes (destination BGRA pixel).
#[inline(always)]
pub(crate) unsafe fn fill_rect_blend_scalar_1px(
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
pub(crate) unsafe fn blit_blend_scalar_1px(sp: *const u8, dp: *mut u8, format: PixelFormat) {
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
pub(crate) unsafe fn blit_blend_scalar_4px(sp: *const u8, dp: *mut u8, bpp: u32) {
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
pub(crate) unsafe fn rounded_rect_write_aa_pixel(
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
