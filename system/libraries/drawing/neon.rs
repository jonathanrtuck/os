// NEON SIMD acceleration for drawing primitives.
//
// Provides 4-pixel-at-a-time processing for fill_rect and alpha blending.
// Uses `core::arch::aarch64` intrinsics. All functions are gated behind
// `#[cfg(target_arch = "aarch64")]`.

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// Fill `pixel_count` pixels starting at `row_ptr` with the constant `pixel_u32`.
///
/// Processes 4 pixels at a time using NEON `vst1q_u32`, then handles the
/// remaining tail with scalar writes.
///
/// # Safety
///
/// `row_ptr` must point to a valid region of at least `pixel_count * 4` bytes.
/// The region must be writable and not alias any other live references.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn neon_fill_row(row_ptr: *mut u32, pixel_count: usize, pixel_u32: u32) {
    // SAFETY: vdupq_n_u32 creates a 128-bit vector with all 4 lanes set to
    // the same value. This is a pure register operation with no memory access.
    let pixel_vec = vdupq_n_u32(pixel_u32);

    let chunks = pixel_count / 4;
    let tail = pixel_count % 4;

    // Process 4 pixels at a time.
    let mut ptr = row_ptr;
    for _ in 0..chunks {
        // SAFETY: caller guarantees row_ptr..row_ptr+pixel_count*4 is valid.
        // Each iteration writes 4 u32 values (16 bytes) at the current ptr.
        // We process at most `chunks * 4 <= pixel_count` pixels total.
        vst1q_u32(ptr, pixel_vec);
        ptr = ptr.add(4);
    }

    // Handle remaining pixels with scalar writes.
    for _ in 0..tail {
        // SAFETY: remaining pixels within the valid region.
        core::ptr::write(ptr, pixel_u32);
        ptr = ptr.add(1);
    }
}

/// Blend 4 source pixels over 4 destination pixels using sRGB-correct alpha
/// blending with NEON vector arithmetic.
///
/// Reads 4 src BGRA pixels and 4 dst BGRA pixels, performs per-channel
/// linear-space blending, and writes 4 result BGRA pixels. Uses scalar
/// SRGB_TO_LINEAR and LINEAR_TO_SRGB table lookups (NEON VTBL can't handle
/// 256-entry tables), with vector multiply/accumulate for the blend math.
///
/// # Safety
///
/// - `src_ptr` must point to at least 16 readable bytes (4 BGRA pixels).
/// - `dst_ptr` must point to at least 16 readable and writable bytes.
/// - Pointers must not alias.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn neon_blend_4px(
    src_ptr: *const u8,
    dst_ptr: *mut u8,
    srgb_to_linear: &[u16; 256],
    linear_to_srgb: &[u8; 4096],
) {
    // Read 4 source and 4 destination BGRA pixels, do scalar sRGB lookups,
    // pack into arrays for NEON loading.
    let mut src_r_lin = [0u16; 4];
    let mut src_g_lin = [0u16; 4];
    let mut src_b_lin = [0u16; 4];
    let mut src_a = [0u16; 4];

    let mut dst_r_lin = [0u16; 4];
    let mut dst_g_lin = [0u16; 4];
    let mut dst_b_lin = [0u16; 4];
    let mut dst_a = [0u16; 4];

    for i in 0..4 {
        let s = src_ptr.add(i * 4);
        let d = dst_ptr.add(i * 4);

        // SAFETY: src_ptr and dst_ptr guaranteed to have 16 bytes each.
        let sb = core::ptr::read(s);
        let sg = core::ptr::read(s.add(1));
        let sr = core::ptr::read(s.add(2));
        let sa = core::ptr::read(s.add(3));

        let db = core::ptr::read(d);
        let dg = core::ptr::read(d.add(1));
        let dr = core::ptr::read(d.add(2));
        let da = core::ptr::read(d.add(3));

        // Scalar sRGB-to-linear table lookups (256-entry table).
        src_r_lin[i] = srgb_to_linear[sr as usize];
        src_g_lin[i] = srgb_to_linear[sg as usize];
        src_b_lin[i] = srgb_to_linear[sb as usize];
        src_a[i] = sa as u16;

        dst_r_lin[i] = srgb_to_linear[dr as usize];
        dst_g_lin[i] = srgb_to_linear[dg as usize];
        dst_b_lin[i] = srgb_to_linear[db as usize];
        dst_a[i] = da as u16;
    }

    // Load into NEON 64-bit vectors (4×u16 = uint16x4_t).
    // SAFETY: vld1_u16 loads 4 consecutive u16 values from a valid array.
    let v_src_r = vld1_u16(src_r_lin.as_ptr());
    let v_src_g = vld1_u16(src_g_lin.as_ptr());
    let v_src_b = vld1_u16(src_b_lin.as_ptr());
    let v_src_a = vld1_u16(src_a.as_ptr());

    let v_dst_r = vld1_u16(dst_r_lin.as_ptr());
    let v_dst_g = vld1_u16(dst_g_lin.as_ptr());
    let v_dst_b = vld1_u16(dst_b_lin.as_ptr());
    let v_dst_a = vld1_u16(dst_a.as_ptr());

    let v_255 = vdup_n_u16(255);

    // inv_sa = 255 - src_a
    // SAFETY: vsub_u16 subtracts corresponding u16 lanes.
    let v_inv_sa = vsub_u16(v_255, v_src_a);

    // da_eff = div255(dst_a * inv_sa)
    // vmull_u16: uint16x4_t × uint16x4_t → uint32x4_t (widening multiply)
    let v_da_inv = vmull_u16(v_dst_a, v_inv_sa);
    let v_da_eff_32 = neon_div255_u32x4(v_da_inv);
    // Narrow back to u16 (values guaranteed ≤ 255).
    let v_da_eff = vmovn_u32(v_da_eff_32);

    // out_a = src_a + da_eff
    let v_out_a = vadd_u16(v_src_a, v_da_eff);

    // Blended channels in linear space:
    // num_c = src_c_lin * src_a + dst_c_lin * da_eff
    // vmull_u16 widens to uint32x4_t, vaddq_u32 adds 128-bit vectors.
    let v_num_r = vaddq_u32(vmull_u16(v_src_r, v_src_a), vmull_u16(v_dst_r, v_da_eff));
    let v_num_g = vaddq_u32(vmull_u16(v_src_g, v_src_a), vmull_u16(v_dst_g, v_da_eff));
    let v_num_b = vaddq_u32(vmull_u16(v_src_b, v_src_a), vmull_u16(v_dst_b, v_da_eff));

    // Extract lanes for scalar division and linear-to-sRGB lookup.
    let mut num_r = [0u32; 4];
    let mut num_g = [0u32; 4];
    let mut num_b = [0u32; 4];
    let mut out_a_arr = [0u16; 4];

    // SAFETY: vst1q_u32 stores 4×u32 from a uint32x4_t to a valid array.
    // vst1_u16 stores 4×u16 from a uint16x4_t to a valid array.
    vst1q_u32(num_r.as_mut_ptr(), v_num_r);
    vst1q_u32(num_g.as_mut_ptr(), v_num_g);
    vst1q_u32(num_b.as_mut_ptr(), v_num_b);
    vst1_u16(out_a_arr.as_mut_ptr(), v_out_a);

    // Scalar division and linear-to-sRGB table lookup for each pixel.
    for i in 0..4 {
        let oa = out_a_arr[i] as u32;
        if oa == 0 {
            // Fully transparent result.
            let d = dst_ptr.add(i * 4);
            core::ptr::write(d, 0);
            core::ptr::write(d.add(1), 0);
            core::ptr::write(d.add(2), 0);
            core::ptr::write(d.add(3), 0);
            continue;
        }

        let r_lin = num_r[i] / oa;
        let g_lin = num_g[i] / oa;
        let b_lin = num_b[i] / oa;

        // Linear-to-sRGB table lookup (table indexed by linear >> 4).
        let out_r = linear_to_srgb[linear_to_idx_inline(r_lin)];
        let out_g = linear_to_srgb[linear_to_idx_inline(g_lin)];
        let out_b = linear_to_srgb[linear_to_idx_inline(b_lin)];
        let out_a_u8 = if oa > 255 { 255u8 } else { oa as u8 };

        // Write BGRA pixel.
        // SAFETY: dst_ptr has at least 16 writable bytes.
        let d = dst_ptr.add(i * 4);
        core::ptr::write(d, out_b);
        core::ptr::write(d.add(1), out_g);
        core::ptr::write(d.add(2), out_r);
        core::ptr::write(d.add(3), out_a_u8);
    }
}

/// Blend 4 destination pixels against a constant pre-converted linear source
/// color using sRGB-correct alpha blending with NEON vector arithmetic.
///
/// The source color's linear values and alpha are provided as constants
/// (computed once outside the loop). Only the destination pixels vary.
///
/// # Safety
///
/// - `dst_ptr` must point to at least 16 readable and writable bytes (4 BGRA pixels).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn neon_blend_const_4px(
    dst_ptr: *mut u8,
    src_r_lin: u16,
    src_g_lin: u16,
    src_b_lin: u16,
    sa: u16,
    inv_sa: u16,
    srgb_to_linear: &[u16; 256],
    linear_to_srgb: &[u8; 4096],
) {
    // Read 4 destination BGRA pixels and do scalar sRGB lookups.
    let mut dst_r_lin = [0u16; 4];
    let mut dst_g_lin = [0u16; 4];
    let mut dst_b_lin = [0u16; 4];
    let mut dst_a = [0u16; 4];

    for i in 0..4 {
        let d = dst_ptr.add(i * 4);

        // SAFETY: dst_ptr guaranteed to have 16 bytes.
        let db = core::ptr::read(d);
        let dg = core::ptr::read(d.add(1));
        let dr = core::ptr::read(d.add(2));
        let da = core::ptr::read(d.add(3));

        dst_r_lin[i] = srgb_to_linear[dr as usize];
        dst_g_lin[i] = srgb_to_linear[dg as usize];
        dst_b_lin[i] = srgb_to_linear[db as usize];
        dst_a[i] = da as u16;
    }

    // Load into NEON vectors (uint16x4_t).
    // SAFETY: vld1_u16 loads 4 u16 values from a properly-sized array.
    let v_dst_r = vld1_u16(dst_r_lin.as_ptr());
    let v_dst_g = vld1_u16(dst_g_lin.as_ptr());
    let v_dst_b = vld1_u16(dst_b_lin.as_ptr());
    let v_dst_a = vld1_u16(dst_a.as_ptr());

    let v_inv_sa = vdup_n_u16(inv_sa);
    let v_src_a = vdup_n_u16(sa);

    // da_eff = div255(dst_a * inv_sa)
    let v_da_inv = vmull_u16(v_dst_a, v_inv_sa);
    let v_da_eff_32 = neon_div255_u32x4(v_da_inv);
    let v_da_eff = vmovn_u32(v_da_eff_32);

    // out_a = src_a + da_eff
    let v_out_a = vadd_u16(v_src_a, v_da_eff);

    // Blended channels: num_c = src_c_lin * src_a + dst_c_lin * da_eff
    // Source values are constant, broadcast to all lanes.
    let v_src_r_lin = vdup_n_u16(src_r_lin);
    let v_src_g_lin = vdup_n_u16(src_g_lin);
    let v_src_b_lin = vdup_n_u16(src_b_lin);

    let v_num_r = vaddq_u32(
        vmull_u16(v_src_r_lin, v_src_a),
        vmull_u16(v_dst_r, v_da_eff),
    );
    let v_num_g = vaddq_u32(
        vmull_u16(v_src_g_lin, v_src_a),
        vmull_u16(v_dst_g, v_da_eff),
    );
    let v_num_b = vaddq_u32(
        vmull_u16(v_src_b_lin, v_src_a),
        vmull_u16(v_dst_b, v_da_eff),
    );

    // Extract for scalar division and linear-to-sRGB lookup.
    let mut num_r = [0u32; 4];
    let mut num_g = [0u32; 4];
    let mut num_b = [0u32; 4];
    let mut out_a_arr = [0u16; 4];

    // SAFETY: vst1q_u32 / vst1_u16 store vector lanes to valid arrays.
    vst1q_u32(num_r.as_mut_ptr(), v_num_r);
    vst1q_u32(num_g.as_mut_ptr(), v_num_g);
    vst1q_u32(num_b.as_mut_ptr(), v_num_b);
    vst1_u16(out_a_arr.as_mut_ptr(), v_out_a);

    for i in 0..4 {
        let oa = out_a_arr[i] as u32;
        if oa == 0 {
            let d = dst_ptr.add(i * 4);
            core::ptr::write(d, 0);
            core::ptr::write(d.add(1), 0);
            core::ptr::write(d.add(2), 0);
            core::ptr::write(d.add(3), 0);
            continue;
        }

        let r_lin = num_r[i] / oa;
        let g_lin = num_g[i] / oa;
        let b_lin = num_b[i] / oa;

        let out_r = linear_to_srgb[linear_to_idx_inline(r_lin)];
        let out_g = linear_to_srgb[linear_to_idx_inline(g_lin)];
        let out_b = linear_to_srgb[linear_to_idx_inline(b_lin)];
        let out_a_u8 = if oa > 255 { 255u8 } else { oa as u8 };

        // SAFETY: dst_ptr has at least 16 writable bytes.
        let d = dst_ptr.add(i * 4);
        core::ptr::write(d, out_b);
        core::ptr::write(d.add(1), out_g);
        core::ptr::write(d.add(2), out_r);
        core::ptr::write(d.add(3), out_a_u8);
    }
}

/// NEON div255 approximation for a uint32x4_t vector.
///
/// Computes `(x + 1 + (x >> 8)) >> 8` for each lane, which is exact for
/// inputs in the range 0..=65025.
///
/// # Safety
///
/// Pure register operation — no memory safety requirements beyond the
/// validity of the input vector.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn neon_div255_u32x4(x: uint32x4_t) -> uint32x4_t {
    // SAFETY: all operations are pure NEON register arithmetic.
    let one = vdupq_n_u32(1);
    let x_plus_1 = vaddq_u32(x, one);
    let x_shr_8 = vshrq_n_u32::<8>(x);
    vshrq_n_u32::<8>(vaddq_u32(x_plus_1, x_shr_8))
}

/// Convert a linear light value (0–65535 u32) to a LINEAR_TO_SRGB table index.
/// Identical to `linear_to_idx` in lib.rs but inlined for the NEON module.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn linear_to_idx_inline(v: u32) -> usize {
    let idx = v >> 4;
    if idx > 4095 {
        4095
    } else {
        idx as usize
    }
}

// ---------------------------------------------------------------------------
// NEON-accelerated Gaussian blur convolution
// ---------------------------------------------------------------------------

/// Horizontal blur pass using NEON SIMD.
///
/// Processes 4 output pixels at a time along each row. For each group of 4
/// output pixels, accumulates the weighted sum across the kernel in 4 parallel
/// u64 accumulators per channel (using NEON widening multiply-accumulate).
///
/// Falls back to scalar for the tail pixels (< 4 remaining in the row).
///
/// # Safety
///
/// Called internally by `blur_horizontal`. All pointer arithmetic is bounds-
/// checked via the width/height/stride parameters.
///
/// NOTE: Despite the `_neon` suffix, this implementation uses scalar u64
/// arithmetic, not NEON SIMD intrinsics. The vertical blur counterpart
/// does use actual NEON. Consider renaming or porting to NEON (review 7.15).
#[cfg(target_arch = "aarch64")]
pub fn blur_horizontal_neon(
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
    let chunks = width / 4;
    let tail_start = chunks * 4;

    for y in 0..height {
        let src_row = (y * src_stride) as usize;
        let dst_row = (y * dst_stride) as usize;

        // NEON path: process 4 output pixels at a time.
        for chunk in 0..chunks {
            let base_x = chunk * 4;
            let mut sums_b = [0u64; 4];
            let mut sums_g = [0u64; 4];
            let mut sums_r = [0u64; 4];
            let mut sums_a = [0u64; 4];

            for k in -r..=r {
                let w = kernel[(k + r) as usize] as u64;

                for px in 0..4u32 {
                    let sx = clamp_i32(base_x as i32 + px as i32 + k, width);
                    let src_off = src_row + (sx * bpp) as usize;

                    sums_b[px as usize] += src[src_off] as u64 * w;
                    sums_g[px as usize] += src[src_off + 1] as u64 * w;
                    sums_r[px as usize] += src[src_off + 2] as u64 * w;
                    sums_a[px as usize] += src[src_off + 3] as u64 * w;
                }
            }

            // Write 4 output pixels.
            for px in 0..4u32 {
                let dst_off = dst_row + ((base_x + px) * bpp) as usize;
                dst[dst_off] = ((sums_b[px as usize] + 32768) >> 16) as u8;
                dst[dst_off + 1] = ((sums_g[px as usize] + 32768) >> 16) as u8;
                dst[dst_off + 2] = ((sums_r[px as usize] + 32768) >> 16) as u8;
                dst[dst_off + 3] = ((sums_a[px as usize] + 32768) >> 16) as u8;
            }
        }

        // Scalar tail.
        for x in tail_start..width {
            let mut sum_b: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_r: u64 = 0;
            let mut sum_a: u64 = 0;

            for k in -r..=r {
                let sx = clamp_i32(x as i32 + k, width);
                let src_off = src_row + (sx * bpp) as usize;
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

/// Vertical blur pass using NEON SIMD.
///
/// Processes 4 output pixels at a time along each row (same x, different kernel y).
/// For each group of 4 horizontally adjacent pixels, accumulates vertically.
#[cfg(target_arch = "aarch64")]
pub fn blur_vertical_neon(
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
    let chunks = width / 4;
    let tail_start = chunks * 4;

    for y in 0..height {
        let dst_row = (y * dst_stride) as usize;

        // NEON path: process 4 adjacent pixels at a time.
        for chunk in 0..chunks {
            let base_x = chunk * 4;

            // SAFETY: we accumulate into stack arrays, using NEON intrinsics
            // for the inner loop multiply-accumulate. Source reads are clamped
            // to valid rows via clamp_i32.
            unsafe {
                // Accumulators: 4 channels × 4 pixels = 16 u32 accumulators.
                // Using uint32x4_t vectors: one per channel.
                let mut acc_b = vdupq_n_u32(0);
                let mut acc_g = vdupq_n_u32(0);
                let mut acc_r = vdupq_n_u32(0);
                let mut acc_a = vdupq_n_u32(0);

                for k in -r..=r {
                    let sy = clamp_i32(y as i32 + k, height);
                    let src_off = (sy * src_stride + base_x * bpp) as usize;
                    let w = kernel[(k + r) as usize];
                    let v_w = vdupq_n_u32(w);

                    // Load 4 BGRA pixels (16 bytes).
                    // SAFETY: src_off points to 4 valid pixels within the
                    // clamped source row. base_x + 3 < width because
                    // base_x = chunk * 4 and chunk < chunks = width / 4.
                    let pixels = vld1q_u8(src.as_ptr().add(src_off));

                    // De-interleave BGRA: extract each channel into a u32x4.
                    // B = bytes 0,4,8,12; G = 1,5,9,13; R = 2,6,10,14; A = 3,7,11,15
                    let b0 = vgetq_lane_u8::<0>(pixels) as u32;
                    let b1 = vgetq_lane_u8::<4>(pixels) as u32;
                    let b2 = vgetq_lane_u8::<8>(pixels) as u32;
                    let b3 = vgetq_lane_u8::<12>(pixels) as u32;

                    let g0 = vgetq_lane_u8::<1>(pixels) as u32;
                    let g1 = vgetq_lane_u8::<5>(pixels) as u32;
                    let g2 = vgetq_lane_u8::<9>(pixels) as u32;
                    let g3 = vgetq_lane_u8::<13>(pixels) as u32;

                    let r0 = vgetq_lane_u8::<2>(pixels) as u32;
                    let r1 = vgetq_lane_u8::<6>(pixels) as u32;
                    let r2 = vgetq_lane_u8::<10>(pixels) as u32;
                    let r3 = vgetq_lane_u8::<14>(pixels) as u32;

                    let a0 = vgetq_lane_u8::<3>(pixels) as u32;
                    let a1 = vgetq_lane_u8::<7>(pixels) as u32;
                    let a2 = vgetq_lane_u8::<11>(pixels) as u32;
                    let a3 = vgetq_lane_u8::<15>(pixels) as u32;

                    // Build channel vectors and multiply-accumulate.
                    let ch_b = [b0, b1, b2, b3];
                    let ch_g = [g0, g1, g2, g3];
                    let ch_r = [r0, r1, r2, r3];
                    let ch_a = [a0, a1, a2, a3];

                    let v_b = vld1q_u32(ch_b.as_ptr());
                    let v_g = vld1q_u32(ch_g.as_ptr());
                    let v_r = vld1q_u32(ch_r.as_ptr());
                    let v_a = vld1q_u32(ch_a.as_ptr());

                    // acc += channel * weight (NEON MLA).
                    acc_b = vmlaq_u32(acc_b, v_b, v_w);
                    acc_g = vmlaq_u32(acc_g, v_g, v_w);
                    acc_r = vmlaq_u32(acc_r, v_r, v_w);
                    acc_a = vmlaq_u32(acc_a, v_a, v_w);
                }

                // Extract results and write 4 output pixels.
                // Divide by 65536 (>> 16) with rounding (+ 32768).
                let half = vdupq_n_u32(32768);
                let res_b = vshrq_n_u32::<16>(vaddq_u32(acc_b, half));
                let res_g = vshrq_n_u32::<16>(vaddq_u32(acc_g, half));
                let res_r = vshrq_n_u32::<16>(vaddq_u32(acc_r, half));
                let res_a = vshrq_n_u32::<16>(vaddq_u32(acc_a, half));

                let mut out_b = [0u32; 4];
                let mut out_g = [0u32; 4];
                let mut out_r = [0u32; 4];
                let mut out_a = [0u32; 4];
                vst1q_u32(out_b.as_mut_ptr(), res_b);
                vst1q_u32(out_g.as_mut_ptr(), res_g);
                vst1q_u32(out_r.as_mut_ptr(), res_r);
                vst1q_u32(out_a.as_mut_ptr(), res_a);

                for px in 0..4usize {
                    let dst_off = dst_row + ((base_x + px as u32) * bpp) as usize;
                    dst[dst_off] = out_b[px] as u8;
                    dst[dst_off + 1] = out_g[px] as u8;
                    dst[dst_off + 2] = out_r[px] as u8;
                    dst[dst_off + 3] = out_a[px] as u8;
                }
            }
        }

        // Scalar tail for remaining pixels.
        for x in tail_start..width {
            let mut sum_b: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_r: u64 = 0;
            let mut sum_a: u64 = 0;

            for k in -r..=r {
                let sy = clamp_i32(y as i32 + k, height);
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

/// Clamp an i32 coordinate to [0, max-1].
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn clamp_i32(val: i32, max: u32) -> u32 {
    if val < 0 {
        0
    } else if val >= max as i32 {
        max - 1
    } else {
        val as u32
    }
}
