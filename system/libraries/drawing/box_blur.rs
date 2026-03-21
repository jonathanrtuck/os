//! Three-pass box blur — converges to Gaussian by Central Limit Theorem.
//!
//! Used by both CpuBackend (running-sum passes on pixel buffers) and
//! virgil-render (TGSI loop shaders on GPU textures). The algorithm is
//! shared; the execution is leaf-node-specific.
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
    let num = twelve_sigma_sq as i64
        - (3 * wl_i64 * wl_i64 + 12 * wl_i64 + 9) * 65536;
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
