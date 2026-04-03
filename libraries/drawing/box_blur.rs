//! Three-pass box blur — converges to Gaussian by Central Limit Theorem.
//!
//! Used by the scene tree walk (running-sum passes on pixel buffers).
//! The algorithm is shared across rendering paths.
//!
//! Three iterations of horizontal + vertical box blur with optimally chosen
//! widths produce a distribution whose shape converges to a Gaussian. This
//! is the same approach macOS/iOS use for backdrop blur.

// No alloc — drawing library is allocation-free.

/// Compute optimal box half-widths for 3 box blur iterations
/// that converge to a Gaussian with standard deviation `sigma`.
///
/// Returns 3 half-widths. Each pass uses a box of width `2*half + 1`.
/// The W3C standard formula ensures the sum of per-pass variances ≈ σ².
///
/// # Algorithm
///
/// Three box blur passes with widths w₁, w₂, w₃ produce a distribution
/// whose variance is the sum of individual variances. A box of width w
/// has variance (w²−1)/12. Setting Σ(wᵢ²−1)/12 = σ² and solving gives
/// the ideal width, then we round to odd integers and split the remainder
/// across passes.
pub fn box_blur_widths(sigma: f32) -> [u32; 3] {
    if sigma < 0.5 {
        return [1; 3];
    }

    // Work in 8.8 fixed-point to avoid f32 intrinsics (no_std).
    // sigma_fp = sigma * 256.
    let sigma_fp = (sigma * 256.0) as u64;

    // w_ideal = sqrt(12σ²/3 + 1) = sqrt(4σ² + 1)
    // In 16.16 fixed-point: 4*sigma_fp² + 65536 (since 1.0 = 65536 in 16.16).
    // isqrt_fp returns floor(sqrt(input)), which for 16.16 input gives an 8.8 result.
    let radicand = 4 * sigma_fp * sigma_fp + 65536;
    let w_ideal_fp = crate::isqrt_fp(radicand); // 8.8 fixed-point

    // Floor to integer.
    let w_ideal_int = (w_ideal_fp >> 8) as i32;

    // Largest odd integer ≤ w_ideal.
    let mut wl = w_ideal_int;
    if wl % 2 == 0 {
        wl -= 1;
    }
    if wl < 1 {
        wl = 1;
    }
    let wu = wl + 2;

    // m = round((12σ² - 3*wl² - 12*wl - 9) / (-4*wl - 4))
    // Compute in 16.16 fixed-point to keep precision.
    // 12*σ² in 16.16 = 12 * sigma_fp² (already in 16.16 since sigma_fp is 8.8).
    let twelve_sigma_sq = 12 * sigma_fp * sigma_fp; // 16.16
    let wl_i64 = wl as i64;
    // Convert wl terms to 16.16: multiply by 65536.
    let num = twelve_sigma_sq as i64 - (3 * wl_i64 * wl_i64 + 12 * wl_i64 + 9) * 65536;
    let den = (-4 * wl_i64 - 4) * 65536;

    let m = if den == 0 {
        0usize
    } else {
        // Rounded integer division: (2*num + den) / (2*den).
        let m_raw = (2 * num + den) / (2 * den);
        if m_raw < 0 {
            0
        } else if m_raw > 3 {
            3
        } else {
            m_raw as usize
        }
    };

    let mut halves = [0u32; 3];
    for (i, half) in halves.iter_mut().enumerate() {
        let w = if i < m { wl } else { wu };
        *half = if w > 0 { (w / 2) as u32 } else { 0 };
    }
    halves
}

/// Total padding needed around the capture region so that three box blur
/// passes have real content to sample at the edges (no CLAMP_TO_EDGE
/// artifacts). Equal to the sum of the three half-widths.
///
/// The caller should extract `(x - pad, y - pad, w + 2*pad, h + 2*pad)`
/// from the source, blur the padded buffer, then use only the center
/// `(w × h)` portion.
pub fn box_blur_pad(sigma: f32) -> u32 {
    let h = box_blur_widths(sigma);
    h[0] + h[1] + h[2]
}

// ── CPU three-pass box blur ──────────────────────────────────────────────

use crate::{ReadSurface, Surface};

/// Apply three-pass box blur to a surface, converging to a Gaussian with
/// σ = `sigma`. Uses O(1)-per-pixel running sums for each pass.
///
/// `tmp` must be at least `2 * src.stride * src.height` bytes (scratch for
/// two intermediate surfaces during the 3-iteration ping-pong).
///
/// Edge handling: clamp to surface bounds (equivalent to CLAMP_TO_EDGE).
///
/// Uses rounded integer division (`(sum + diameter/2) / diameter`) to
/// prevent systematic darkening from truncation bias over 3 passes.
pub fn box_blur_3pass(src: &ReadSurface, dst: &mut Surface, tmp: &mut [u8], sigma: f32) {
    let w = src.width;
    let h = src.height;
    if w == 0 || h == 0 {
        return;
    }

    let halves = box_blur_widths(sigma);
    let stride = src.stride;
    let buf_size = (stride * h) as usize;

    // Need two scratch buffers for ping-pong between passes.
    if tmp.len() < buf_size * 2 {
        return;
    }
    let (tmp_a, tmp_b) = tmp.split_at_mut(buf_size);

    // Pass 1: src → tmp_a (H) → tmp_b (V)
    box_blur_h(src.data, tmp_a, w, h, stride, halves[0]);
    box_blur_v(tmp_a, tmp_b, w, h, stride, halves[0]);

    // Pass 2: tmp_b → tmp_a (H) → tmp_b (V)
    box_blur_h(tmp_b, tmp_a, w, h, stride, halves[1]);
    box_blur_v(tmp_a, tmp_b, w, h, stride, halves[1]);

    // Pass 3: tmp_b → tmp_a (H) → dst (V)
    box_blur_h(tmp_b, tmp_a, w, h, stride, halves[2]);
    box_blur_v(tmp_a, dst.data, w, h, stride, halves[2]);
}

/// Horizontal box blur: running-sum average across each row.
///
/// For each pixel (x, y): output = average of src[x-half..=x+half, y],
/// all 4 BGRA channels processed together per pixel.
/// Out-of-bounds indices clamp to the nearest edge pixel.
/// Uses rounded division to prevent truncation bias.
fn box_blur_h(src: &[u8], dst: &mut [u8], width: u32, height: u32, stride: u32, half: u32) {
    let w = width as usize;
    let h = height as usize;
    let s = stride as usize;
    let diameter = (2 * half + 1) as usize;
    let half_diam = diameter / 2; // for rounding

    for y in 0..h {
        let row_off = y * s;

        // Initialize 4-channel running sum for first output pixel (x=0).
        let mut sum = [0u32; 4];
        for i in 0..diameter {
            let sx = clamp_idx(i as i32 - half as i32, w);
            let off = row_off + sx * 4;
            for c in 0..4 {
                sum[c] += src[off + c] as u32;
            }
        }
        for c in 0..4 {
            dst[row_off + c] = ((sum[c] + half_diam as u32) / diameter as u32) as u8;
        }

        // Slide the window right.
        for x in 1..w {
            let old_x = clamp_idx(x as i32 - half as i32 - 1, w);
            let new_x = clamp_idx(x as i32 + half as i32, w);
            let old_off = row_off + old_x * 4;
            let new_off = row_off + new_x * 4;
            let dst_off = row_off + x * 4;
            for c in 0..4 {
                sum[c] -= src[old_off + c] as u32;
                sum[c] += src[new_off + c] as u32;
                dst[dst_off + c] = ((sum[c] + half_diam as u32) / diameter as u32) as u8;
            }
        }
    }
}

/// Column tile width for V-pass cache optimization.
///
/// With TILE_COLS=8, each y iteration loads 32 contiguous bytes from two
/// rows (leaving and entering) and writes 32 bytes. All three spans fit
/// within one aarch64 cache line (64 bytes). The running-sum state for
/// the tile (8 columns × 4 channels × 4 bytes = 128 bytes) fits in 2
/// cache lines and stays hot across all y iterations.
const TILE_COLS: usize = 8;

/// Vertical box blur: tiled running-sum average down columns.
///
/// Processes columns in tiles of TILE_COLS (8) for cache friendliness.
/// Within each tile, maintains 8 independent 4-channel running sums.
/// Each y step reads two contiguous 32-byte spans (leaving row + entering
/// row at the tile's x range) instead of striding across the buffer
/// one pixel at a time.
fn box_blur_v(src: &[u8], dst: &mut [u8], width: u32, height: u32, stride: u32, half: u32) {
    let w = width as usize;
    let h = height as usize;
    let s = stride as usize;
    let diameter = (2 * half + 1) as usize;
    let half_diam = diameter / 2;

    // Process columns in tiles.
    let mut tile_x = 0usize;
    while tile_x < w {
        let cols = if tile_x + TILE_COLS <= w {
            TILE_COLS
        } else {
            w - tile_x
        };

        // Running sums: [col][channel].
        let mut sums = [[0u32; 4]; TILE_COLS];

        // Initialize sums for y=0: accumulate the initial window.
        for i in 0..diameter {
            let sy = clamp_idx(i as i32 - half as i32, h);
            let row_base = sy * s + tile_x * 4;
            for cx in 0..cols {
                let off = row_base + cx * 4;
                for c in 0..4 {
                    sums[cx][c] += src[off + c] as u32;
                }
            }
        }

        // Write y=0 output.
        for cx in 0..cols {
            let off = tile_x * 4 + cx * 4;
            for c in 0..4 {
                dst[off + c] = ((sums[cx][c] + half_diam as u32) / diameter as u32) as u8;
            }
        }

        // Slide window down for y=1..h.
        for y in 1..h {
            let old_y = clamp_idx(y as i32 - half as i32 - 1, h);
            let new_y = clamp_idx(y as i32 + half as i32, h);
            let old_base = old_y * s + tile_x * 4;
            let new_base = new_y * s + tile_x * 4;
            let dst_base = y * s + tile_x * 4;

            for cx in 0..cols {
                let old_off = old_base + cx * 4;
                let new_off = new_base + cx * 4;
                let dst_off = dst_base + cx * 4;
                for c in 0..4 {
                    sums[cx][c] -= src[old_off + c] as u32;
                    sums[cx][c] += src[new_off + c] as u32;
                    dst[dst_off + c] = ((sums[cx][c] + half_diam as u32) / diameter as u32) as u8;
                }
            }
        }

        tile_x += TILE_COLS;
    }
}

/// Clamp an index to [0, max-1]. Used for edge handling (CLAMP_TO_EDGE).
#[inline(always)]
fn clamp_idx(i: i32, max: usize) -> usize {
    if i < 0 {
        0
    } else if i >= max as i32 {
        max - 1
    } else {
        i as usize
    }
}
