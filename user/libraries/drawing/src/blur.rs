//! Gaussian blur via separable two-pass convolution.
//!
//! Provides a `BlurStrategy` trait for GPU/CPU dispatch, a `CpuBlur`
//! implementation, and the underlying kernel computation and blur passes.

use crate::Surface;

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
    pub format: crate::PixelFormat,
}

/// Strategy for blur operations. Trait-based interface accommodates a future
/// GPU path -- callers program against the trait, implementations are leaf
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
/// Uses integer-only Gaussian approximation: `exp(-x^2/(2sigma^2))` is approximated
/// via a piecewise polynomial in fixed-point.
pub fn compute_kernel(
    kernel: &mut [u32; MAX_KERNEL_DIAMETER],
    radius: u32,
    sigma_fp: u32,
) -> usize {
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

    // sigma_fp is 8.8 FP. Convert sigma^2 to 16.16 FP for computation.
    // sigma^2 = (sigma_fp / 256)^2 = sigma_fp^2 / 65536
    // In 16.16 FP: sigma^2_fp16 = sigma_fp^2 * 65536 / 65536 = sigma_fp^2
    // But we need 2*sigma^2 in a form we can divide by.
    // 2sigma^2 in 16.16 FP = 2 * sigma_fp * sigma_fp / 256 (since sigma_fp is 8.8)
    let sigma_fp64 = sigma_fp as u64;
    let two_sigma_sq_fp = 2 * sigma_fp64 * sigma_fp64; // in 16.16 FP (via 8.8 * 8.8 = 16.16)

    if two_sigma_sq_fp == 0 {
        // sigma ~ 0: all weight on center.
        for w in kernel[..diameter].iter_mut() {
            *w = 0;
        }

        kernel[r as usize] = 65536;

        return diameter;
    }

    // Compute raw weights using integer approximation of Gaussian.
    // g(x) = exp(-x^2 / (2sigma^2))
    // We approximate: weight(x) = 2^16 * exp(-x^2 * 2^16 / two_sigma_sq_fp)
    // Using the identity: exp(-t) ~ (1 - t/n)^n for small t.
    // Instead, use a lookup-based approach with a quadratic decay:
    // g(x) ~ max(0, two_sigma_sq_fp - x^2 * 256) for a simple bell curve,
    // but this doesn't match Gaussian well. Instead, use iterative exp approx.
    //
    // Practical approach: compute exp(-x^2/(2sigma^2)) using a fixed-point
    // Taylor-like approximation. For x^2/(2sigma^2) = t:
    //   exp(-t) ~ 1/(1 + t + t^2/2 + t^3/6) (Pade-like)
    //   or simpler: exp(-t) ~ 256/(256 + t*256) for small t
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
        // t = x^2 * 65536 / two_sigma_sq_fp (in 16.16 FP)
        // We want exp(-x^2/(2sigma^2)).
        // x^2/(2sigma^2) = x_sq * 65536 / two_sigma_sq_fp (converting to same FP scale)
        //
        // Compute exp(-t) where t = x_sq * 65536 / two_sigma_sq_fp
        // Using integer exp approximation: exp(-t) ~ 2^24 / (2^24 + t * k)
        // where k is chosen to match the Gaussian.
        //
        // Better: use the series exp(-t) = 1 - t + t^2/2 - t^3/6 + ...
        // but with fixed-point. For t in 0..~radius^2/(2sigma^2):
        // At radius=16, sigma=1: t = 128, which is very large.
        //
        // Most practical: use a 2^(-t * log2(e)) style computation.
        // exp(-t) = 2^(-t/ln2) = 2^(-t * 1.4427)
        //
        // Simplest correct integer-only Gaussian:
        // Weight = round(65536 * exp(-x^2/(2sigma^2)))
        // We compute this with enough precision using only integers.

        // Numerator: x^2 << 24 (for precision)
        // Denominator: 2sigma^2 in 8.8*8.8 = 16.16 scale
        // Quotient: x^2/(2sigma^2) in 8.8 FP (shifted by 24-16=8)
        let t_fp8 = if two_sigma_sq_fp > 0 {
            (x_sq << 24) / two_sigma_sq_fp // result in ~8.8 FP
        } else {
            u64::MAX
        };

        // exp(-t) approximation using (1024 - t)^6 / 1024^6 style decay,
        // or more accurately, use the rational approximation:
        // exp(-t) ~ (1 + t/2)^-2 is a decent Pade[0/2].
        // Better: exp(-t) ~ 1/(1 + t + t^2/2) (3-term Taylor denominator).
        //
        // With t in 8.8 FP (t_fp8), where 256 = 1.0:
        // denom = 256 + t_fp8 + (t_fp8 * t_fp8) / (2 * 256)
        // weight = 256 * 65536 / denom  (yields weight in 16.16 FP)
        if t_fp8 > 8 * 256 {
            // exp(-8) ~ 0.00034 -- negligible, set to 0.
            raw[i] = 0;
        } else {
            let t = t_fp8;
            let t_sq = t * t;
            // Denominator in 8.8 FP: 1 + t + t^2/2 + t^3/6
            // = 256 + t + t^2/512 + t^3/393216
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

    // Pass 1: horizontal blur from src -> tmp.
    blur_horizontal(src.data, tmp, w, h, src.stride, dst_stride, r, kernel_slice);
    // Pass 2: vertical blur from tmp -> dst.
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
pub(crate) fn blur_horizontal_scalar(
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
pub(crate) fn blur_vertical_scalar(
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

/// Horizontal blur pass -- dispatches to NEON or scalar.
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
        crate::blur_horizontal_scalar_4x(
            src, dst, width, height, src_stride, dst_stride, radius, kernel,
        );
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        blur_horizontal_scalar(
            src, dst, width, height, src_stride, dst_stride, radius, kernel,
        );
    }
}

/// Vertical blur pass -- dispatches to NEON or scalar.
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
        crate::blur_vertical_neon(
            src, dst, width, height, src_stride, dst_stride, radius, kernel,
        );
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        blur_vertical_scalar(
            src, dst, width, height, src_stride, dst_stride, radius, kernel,
        );
    }
}
