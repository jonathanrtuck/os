# Backdrop Blur Rewrite — Three-Pass Box Blur

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the broken 9-tap Gaussian hack in virgil-render with a mathematically correct three-pass box blur, and fix the architectural ordering so the node's background is drawn on top of the blur (not blurred into it). Both renderers (CpuBackend and virgil-render) produce correct frosted glass with no edge artifacts.

**Architecture:** The blur algorithm (three-pass separable box blur converging to Gaussian) lives in `libraries/drawing/`. Each renderer implements the algorithm with its own primitives: CpuBackend uses CPU running-sum passes; virgil-render uses TGSI loop-based fragment shaders with 6-pass ping-pong. The architectural fix (skip node bg → blur backdrop → draw bg on top) is applied to virgil-render's scene walk and render loop. Both renderers capture padded regions to eliminate edge clamping artifacts.

**Design notes:**

- Three-pass box blur is what macOS/iOS use. Three iterations of H+V box blur provably converge to a Gaussian (Central Limit Theorem). Box widths computed from σ via the W3C standard formula.
- CPU path: O(1) per pixel per pass via running sums — faster than the current O(diameter) Gaussian convolution for any radius. For σ=4 on a 300×200 region: box blur ≈ 360K ops vs Gaussian ≈ 1.08M multiply-accumulates.
- GPU path: O(w) texture samples per fragment per pass via a TGSI loop shader. For typical radii (8–20), w ≤ 17 samples — well within fragment shader limits.
- Both paths capture a padded region (pad = sum of 3 half-widths) so the blur has real scene content at the edges. Without padding, 3 passes of CLAMP_TO_EDGE compound — the outermost pixels are over-sampled, causing visible edge banding. The existing shadow blur already uses this pattern (`render_shadow_blurred` pads by `blur_radius`).
- The CpuBackend already has correct architectural ordering (blur before bg). Only virgil-render needs the ordering fix.
- The V-pass uses tiled column processing (TILE=8) for cache friendliness. The original single-column approach causes a cache miss per pixel per row; tiling keeps the working set in L1.
- `backdrop_blur_radius: u8` maps to σ = radius/2 (CSS convention).
- Shadow blur (`render_shadow_blurred`) migrates to the same `box_blur_3pass`, unifying both blur paths.

**Spec:** `design/v0.3-spec.md` section 2.2 (Backdrop Blur).

**Tech Stack:** Rust `no_std`, TGSI shaders, bare-metal aarch64. Host-side tests in `system/test/`.

---

## File Map

### New files

| File                            | Purpose                                                                   |
| ------------------------------- | ------------------------------------------------------------------------- |
| `libraries/drawing/box_blur.rs` | Three-pass box blur: width computation, CPU running-sum H/V passes, pad   |
| `test/tests/box_blur.rs`        | Tests for box blur math, CPU passes, convergence, symmetry, and reference |

### Modified files

| File                                           | Change                                                                                              |
| ---------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `libraries/drawing/lib.rs`                     | Add `mod box_blur`, re-export public API                                                            |
| `libraries/render/scene_render/walk.rs`        | `apply_backdrop_blur()` → padded capture + `box_blur_3pass()`; `render_shadow_blurred()` → same     |
| `services/drivers/virgil-render/scene_walk.rs` | Expand `BlurRequest` with bg color/corner_radius; skip bg quad for backdrop-blur nodes              |
| `services/drivers/virgil-render/shaders.rs`    | Replace `BLUR_H_FS`/`BLUR_V_FS` with loop-based box blur shaders (CONST[0]+CONST[1] in binding 0)  |
| `services/drivers/virgil-render/main.rs`       | 6-pass ping-pong blur pipeline with padded capture; post-blur bg quad draw                          |

---

## Task 1: Box Blur Width Computation (Shared Math)

**Objective:** Given a target Gaussian σ, compute the optimal box widths for 3 passes so their convolution converges to that Gaussian. Also provide a helper to compute the total padding needed for edge-artifact-free blurring.

**Files:**

- Create: `system/libraries/drawing/box_blur.rs`
- Create: `system/test/tests/box_blur.rs`
- Modify: `system/libraries/drawing/lib.rs`

**Design:** Uses the W3C standard formula. For N passes with target σ:

- `w_ideal = sqrt(12σ²/N + 1)`
- `w_lower = largest odd integer ≤ w_ideal`
- `w_upper = w_lower + 2`
- `m = round((12σ² - N·w_lower² - 4N·w_lower - 3N) / (-4·w_lower - 4))`
- First `m` passes use `w_lower`, remaining use `w_upper`

Returns half-widths (the box extends ±half from center, total width = 2·half+1).

- [ ] **Step 1: Write failing tests for width computation**

In `test/tests/box_blur.rs`:

```rust
use drawing::box_blur::{box_blur_widths, box_blur_pad};

#[test]
fn box_blur_widths_sigma_4_returns_three_odd_widths() {
    // σ=4.0 (blur_radius=8, CSS convention σ=radius/2)
    let halves = box_blur_widths(4.0);
    assert_eq!(halves.len(), 3); // [u32; 3]
    // Each width = 2*half+1 must be odd (guaranteed by algorithm)
    for &h in &halves {
        assert!(h > 0, "half-width must be positive");
    }
    // Total variance = sum of per-pass variances should ≈ σ²
    let total_var: f32 = halves
        .iter()
        .map(|&h| {
            let w = 2 * h + 1;
            (w * w - 1) as f32 / 12.0
        })
        .sum();
    let expected_var = 4.0f32 * 4.0;
    assert!((total_var - expected_var).abs() < 2.0,
        "total variance {total_var} should be near σ²={expected_var}");
}

#[test]
fn box_blur_widths_sigma_1_returns_small_widths() {
    let halves = box_blur_widths(1.0);
    for &h in &halves {
        assert!(h >= 1);
        assert!(h <= 2); // σ=1 → small boxes
    }
}

#[test]
fn box_blur_widths_sigma_10_returns_large_widths() {
    let halves = box_blur_widths(10.0);
    for &h in &halves {
        assert!(h >= 5);
        assert!(h <= 10);
    }
}

#[test]
fn box_blur_widths_tiny_sigma_returns_minimum() {
    // σ < 0.5 → all half-widths = 1 (3×3 box, minimum blur)
    let halves = box_blur_widths(0.1);
    assert_eq!(halves, [1, 1, 1]);
}

#[test]
fn box_blur_pad_equals_sum_of_halves() {
    let halves = box_blur_widths(4.0);
    assert_eq!(box_blur_pad(4.0), halves[0] + halves[1] + halves[2]);
}
```

- [ ] **Step 2: Run tests — verify they fail**

Run: `cd system/test && cargo test --release box_blur_widths -- --test-threads=1`

- [ ] **Step 3: Implement `box_blur_widths` and `box_blur_pad`**

In `libraries/drawing/box_blur.rs`:

```rust
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

    let n = 3.0f32;
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();

    // Largest odd integer ≤ w_ideal.
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    wl = wl.max(1);
    let wu = wl + 2;

    // How many passes use the smaller width vs the larger.
    let wl_f = wl as f32;
    let m_ideal = (12.0 * sigma * sigma
        - n * wl_f * wl_f
        - 4.0 * n * wl_f
        - 3.0 * n)
        / (-4.0 * wl_f - 4.0);
    let m = m_ideal.round().max(0.0).min(n) as usize;

    let mut halves = [0u32; 3];
    for i in 0..3 {
        let w = if i < m { wl } else { wu };
        halves[i] = (w / 2).max(0) as u32;
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
```

- [ ] **Step 4: Add module to drawing lib**

In `libraries/drawing/lib.rs`, add after `mod blur;`:

```rust
pub mod box_blur;
```

And in the re-exports section, add:

```rust
pub use box_blur::{box_blur_widths, box_blur_pad};
```

- [ ] **Step 5: Run tests — verify they pass**

Run: `cd system/test && cargo test --release box_blur -- --test-threads=1`

- [ ] **Step 6: Commit**

```text
feat: box blur width computation — W3C standard formula for 3-pass Gaussian

Adds box_blur_widths(sigma) → [u32; 3] and box_blur_pad(sigma) → u32
to the drawing library. Given a target Gaussian σ, computes optimal
odd-integer box widths so three box blur passes converge to that
Gaussian (CLT). box_blur_pad returns the total padding needed for
artifact-free edge handling. Shared math used by both CpuBackend
and virgil-render.
```

---

## Task 2: CPU Three-Pass Box Blur

**Objective:** Implement the three-pass box blur for pixel buffers using O(1)-per-pixel running sums. Cache-friendly V-pass via column tiling.

**Depends on:** Task 1 (box_blur_widths).

**Files:**

- Modify: `system/libraries/drawing/box_blur.rs`
- Modify: `system/test/tests/box_blur.rs`

**Design:**

- H-pass: for each row, maintain a 4-channel running sum (B, G, R, A processed together). Slide right: subtract leftmost pixel, add rightmost. Output = `(sum + diameter/2) / diameter` (rounding, not truncation — truncation causes systematic darkening that compounds over 3 passes).
- V-pass: **tiled** — process columns in tiles of `TILE_COLS=8`. For each tile, maintain 8 × 4-channel running sums. Slide down: subtract top-leaving row, add bottom-entering row. This accesses two contiguous 32-byte regions (8 pixels) per row instead of striding across the entire buffer per pixel. Cache miss rate drops from ~100% to ~0% within each tile.
- Edge handling: clamp indices to surface bounds (same as CLAMP_TO_EDGE).
- Three iterations of H+V using the widths from `box_blur_widths()`.
- Input and output are separate buffers; a scratch buffer is used for intermediate passes.
- Rounding: `(sum + half_diameter) / diameter` prevents systematic darkening. After 3 passes, truncation-only would darken the image by up to 3 levels.

**Why tiled V-pass matters:** The original single-column approach (`for x in 0..w { for y in 0..h { src[y*stride + x*4] } }`) causes a cache miss on every pixel because each `y` step jumps by `stride` bytes (typically 1200–4096 bytes). On aarch64 with 64-byte cache lines, this means every pixel load evicts useful data. With TILE=8, each `y` step loads a contiguous 32-byte span from two rows — both fit in one cache line.

- [ ] **Step 1: Write failing tests for CPU box blur**

In `test/tests/box_blur.rs`, add:

```rust
use drawing::box_blur::{box_blur_3pass, box_blur_widths};
use drawing::{PixelFormat, ReadSurface, Surface};

fn make_surface_ro<'a>(data: &'a [u8], w: u32, h: u32) -> ReadSurface<'a> {
    ReadSurface {
        data,
        width: w,
        height: h,
        stride: w * 4,
        format: PixelFormat::Bgra8888,
    }
}

fn make_surface_rw<'a>(data: &'a mut [u8], w: u32, h: u32) -> Surface<'a> {
    Surface {
        data,
        width: w,
        height: h,
        stride: w * 4,
        format: PixelFormat::Bgra8888,
    }
}

#[test]
fn box_blur_3pass_uniform_surface_unchanged() {
    // Blurring a uniform-color surface should produce the same color
    // everywhere — no darkening, no edge artifacts.
    let w = 32u32;
    let h = 32u32;
    let mut src_buf = vec![128u8; (w * h * 4) as usize];
    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);

    box_blur_3pass(&src, &mut dst, &mut tmp_buf, 4.0);

    // ALL pixels should be 128 ± 1 (including edges — uniform input
    // means clamping reproduces the same value).
    for y in 0..h {
        for x in 0..w {
            let off = ((y * w + x) * 4) as usize;
            for c in 0..4 {
                let v = dst.data[off + c];
                assert!((v as i32 - 128).unsigned_abs() <= 1,
                    "pixel ({x},{y}) channel {c} = {v}, expected 128±1");
            }
        }
    }
}

#[test]
fn box_blur_3pass_single_white_pixel_spreads() {
    // A single white pixel in a black field should spread after blur.
    let w = 64u32;
    let h = 64u32;
    let mut src_buf = vec![0u8; (w * h * 4) as usize];
    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    // Set center pixel to white.
    let cx = w / 2;
    let cy = h / 2;
    let off = ((cy * w + cx) * 4) as usize;
    for c in 0..4 { src_buf[off + c] = 255; }

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);

    box_blur_3pass(&src, &mut dst, &mut tmp_buf, 4.0);

    // Center should still be the brightest.
    let center_off = ((cy * w + cx) * 4) as usize;
    let center_val = dst.data[center_off];
    assert!(center_val > 0, "center should be non-zero after blur");

    // A pixel 10px away should be dimmer than center.
    let far_off = ((cy * w + cx + 10) * 4) as usize;
    let far_val = dst.data[far_off];
    assert!(far_val < center_val, "distant pixel should be dimmer");

    // A pixel 20px away should be very dim or zero (σ=4, 5σ=20).
    let vfar_off = ((cy * w + cx + 20) * 4) as usize;
    let vfar_val = dst.data[vfar_off];
    assert!(vfar_val < center_val / 4, "very distant pixel should be very dim");
}

#[test]
fn box_blur_3pass_symmetric() {
    // Output should be symmetric around the source impulse.
    let w = 64u32;
    let h = 64u32;
    let mut src_buf = vec![0u8; (w * h * 4) as usize];
    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    let cx = w / 2;
    let cy = h / 2;
    let off = ((cy * w + cx) * 4) as usize;
    for c in 0..4 { src_buf[off + c] = 255; }

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);
    box_blur_3pass(&src, &mut dst, &mut tmp_buf, 4.0);

    // Horizontal symmetry: (cx+d, cy) == (cx-d, cy) ± 1.
    for d in 1..10u32 {
        let left = ((cy * w + cx - d) * 4) as usize;
        let right = ((cy * w + cx + d) * 4) as usize;
        for c in 0..4 {
            let diff = (dst.data[left + c] as i32 - dst.data[right + c] as i32).unsigned_abs();
            assert!(diff <= 1, "symmetry broken at d={d} c={c}: {} vs {}",
                dst.data[left + c], dst.data[right + c]);
        }
    }
}

#[test]
fn box_blur_3pass_no_systematic_darkening() {
    // 3 passes of truncating integer division would darken by up to 3 levels.
    // With proper rounding, a uniform 200-value surface should stay 200 ± 1.
    let w = 64u32;
    let h = 64u32;
    let mut src_buf = vec![200u8; (w * h * 4) as usize];
    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);
    box_blur_3pass(&src, &mut dst, &mut tmp_buf, 8.0);

    // Center pixel should be 200 ± 1, NOT 197 (which truncation would give).
    let center = ((32 * w + 32) * 4) as usize;
    for c in 0..4 {
        let v = dst.data[center + c];
        assert!((v as i32 - 200).unsigned_abs() <= 1,
            "channel {c} = {v}, expected 200±1 (not darkened)");
    }
}

#[test]
fn box_blur_3pass_reference_gaussian_comparison() {
    // Validate CLT convergence: compare 3-pass box blur output against
    // a reference Gaussian computed in floating-point.
    let w = 128u32;
    let h = 128u32;
    let sigma = 4.0f32;
    let cx = w / 2;
    let cy = h / 2;

    // Source: single white pixel at center.
    let mut src_buf = vec![0u8; (w * h * 4) as usize];
    let off = ((cy * w + cx) * 4) as usize;
    for c in 0..4 { src_buf[off + c] = 255; }

    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);
    box_blur_3pass(&src, &mut dst, &mut tmp_buf, sigma);

    // Reference Gaussian: g(x,y) = exp(-(x²+y²)/(2σ²)) / (2πσ²)
    // Normalized so the sum over the kernel equals 255 (the input impulse).
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut ref_sum = 0.0f64;
    let radius = (3.0 * sigma).ceil() as i32; // 3σ captures 99.7%
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let r_sq = (dx * dx + dy * dy) as f64;
            ref_sum += (-r_sq / two_sigma_sq as f64).exp();
        }
    }

    // Compare within 3σ radius (where the signal is above noise).
    let mut max_err: u32 = 0;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let r_sq = (dx * dx + dy * dy) as f64;
            let ref_val = ((-r_sq / two_sigma_sq as f64).exp() / ref_sum * 255.0) as u8;

            let px = (cx as i32 + dx) as u32;
            let py = (cy as i32 + dy) as u32;
            let actual = dst.data[((py * w + px) * 4) as usize]; // B channel

            let err = (actual as i32 - ref_val as i32).unsigned_abs();
            if err > max_err { max_err = err; }
        }
    }

    // 3-pass box blur should match a true Gaussian within ±3 levels
    // in the center region. The CLT approximation is not perfect but
    // is visually indistinguishable.
    assert!(max_err <= 3,
        "max error vs reference Gaussian = {max_err}, expected ≤ 3");
}
```

- [ ] **Step 2: Run tests — verify they fail**

Run: `cd system/test && cargo test --release box_blur_3pass -- --test-threads=1`

- [ ] **Step 3: Implement `box_blur_3pass`, `box_blur_h`, and `box_blur_v`**

In `libraries/drawing/box_blur.rs`, add below the existing functions:

```rust
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
pub fn box_blur_3pass(
    src: &ReadSurface,
    dst: &mut Surface,
    tmp: &mut [u8],
    sigma: f32,
) {
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
fn box_blur_h(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    half: u32,
) {
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
fn box_blur_v(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    half: u32,
) {
    let w = width as usize;
    let h = height as usize;
    let s = stride as usize;
    let diameter = (2 * half + 1) as usize;
    let half_diam = diameter / 2;

    // Process columns in tiles.
    let mut tile_x = 0usize;
    while tile_x < w {
        let cols = if tile_x + TILE_COLS <= w { TILE_COLS } else { w - tile_x };

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
                    dst[dst_off + c] =
                        ((sums[cx][c] + half_diam as u32) / diameter as u32) as u8;
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
```

- [ ] **Step 4: Run tests — verify they pass**

Run: `cd system/test && cargo test --release box_blur -- --test-threads=1`

- [ ] **Step 5: Commit**

```text
feat: three-pass box blur — O(1) running-sum with tiled V-pass

Adds box_blur_3pass() using running-sum H/V passes. Three iterations
converge to a Gaussian (CLT). O(1) per pixel per pass regardless of
radius. V-pass uses TILE_COLS=8 column tiling for cache friendliness
(32-byte contiguous access per row vs stride-jumping per pixel).
Rounded division prevents systematic darkening over 3 passes.
Reference Gaussian comparison validates CLT convergence within ±3
levels.
```

---

## Task 3: CpuBackend — Padded Backdrop Blur

**Objective:** Update `apply_backdrop_blur()` to use `box_blur_3pass()` with a padded capture region so edge clamping artifacts don't compound over 3 passes.

**Depends on:** Task 2.

**Files:**

- Modify: `system/libraries/render/scene_render/walk.rs` (`apply_backdrop_blur`, lines 604–685)

**Design:** The padded capture follows the same pattern as `render_shadow_blurred` (line 532: `let pad = blur_radius`), which already pads to give the blur real content at the edges. For backdrop blur:

1. Compute `pad = box_blur_pad(sigma)` — total effective radius across 3 passes.
2. Extract `(x0 - pad, y0 - pad, region_w + 2*pad, region_h + 2*pad)` from the framebuffer, clamped to FB bounds.
3. Blur the padded buffer.
4. Write back only the center `(region_w × region_h)` portion — the padding is used for blur quality and discarded.

This eliminates the edge banding that occurs when 3 passes of CLAMP_TO_EDGE compound the same border pixel.

- [ ] **Step 1: Rewrite `apply_backdrop_blur` with padded capture**

Replace the function body (lines 604–685) with:

```rust
fn apply_backdrop_blur(
    fb: &mut Surface,
    draw_x: i32,
    draw_y: i32,
    nw: i32,
    nh: i32,
    blur_radius_pt: u8,
    scale: f32,
) {
    let blur_px = ((blur_radius_pt as f32 * scale) as u32).min(MAX_BACKDROP_BLUR_PX);
    if blur_px == 0 {
        return;
    }

    let sigma = blur_px as f32 / 2.0;
    let pad = drawing::box_blur_pad(sigma);

    // Node region in FB coordinates, clamped.
    let nx0 = (draw_x.max(0) as u32).min(fb.width);
    let ny0 = (draw_y.max(0) as u32).min(fb.height);
    let nx1 = ((draw_x + nw).max(0) as u32).min(fb.width);
    let ny1 = ((draw_y + nh).max(0) as u32).min(fb.height);
    let node_w = nx1 - nx0;
    let node_h = ny1 - ny0;
    if node_w == 0 || node_h == 0 {
        return;
    }

    // Padded capture region, clamped to FB bounds.
    let cx0 = nx0.saturating_sub(pad);
    let cy0 = ny0.saturating_sub(pad);
    let cx1 = (nx1 + pad).min(fb.width);
    let cy1 = (ny1 + pad).min(fb.height);
    let cap_w = cx1 - cx0;
    let cap_h = cy1 - cy0;

    let cap_stride = cap_w * 4;
    let buf_size = (cap_stride * cap_h) as usize;

    // Cap allocation to prevent OOM (same 4 MiB limit as shadow blur).
    if buf_size > 4 * 1024 * 1024 {
        return;
    }

    // 1. Extract padded region from the framebuffer.
    let mut src_buf = vec![0u8; buf_size];
    for row in 0..cap_h {
        let fb_offset = ((cy0 + row) * fb.stride + cx0 * 4) as usize;
        let src_offset = (row * cap_stride) as usize;
        let row_bytes = (cap_w * 4) as usize;
        if fb_offset + row_bytes <= fb.data.len() && src_offset + row_bytes <= src_buf.len() {
            src_buf[src_offset..src_offset + row_bytes]
                .copy_from_slice(&fb.data[fb_offset..fb_offset + row_bytes]);
        }
    }

    // 2. Three-pass box blur (converges to Gaussian, CLT).
    let mut tmp_buf = vec![0u8; buf_size * 2];
    let mut dst_buf = vec![0u8; buf_size];

    let src_read = drawing::ReadSurface {
        data: &src_buf,
        width: cap_w,
        height: cap_h,
        stride: cap_stride,
        format: drawing::PixelFormat::Bgra8888,
    };
    let mut dst_surface = Surface {
        data: &mut dst_buf,
        width: cap_w,
        height: cap_h,
        stride: cap_stride,
        format: PixelFormat::Bgra8888,
    };

    drawing::box_blur_3pass(&src_read, &mut dst_surface, &mut tmp_buf, sigma);

    // 3. Write back only the center (node) portion — padding is discarded.
    //    The offset into the blurred buffer where the node region starts:
    let pad_left = nx0 - cx0;
    let pad_top = ny0 - cy0;
    for row in 0..node_h {
        let fb_offset = ((ny0 + row) * fb.stride + nx0 * 4) as usize;
        let dst_offset = ((pad_top + row) * cap_stride + pad_left * 4) as usize;
        let row_bytes = (node_w * 4) as usize;
        if fb_offset + row_bytes <= fb.data.len() && dst_offset + row_bytes <= dst_buf.len() {
            fb.data[fb_offset..fb_offset + row_bytes]
                .copy_from_slice(&dst_buf[dst_offset..dst_offset + row_bytes]);
        }
    }
}
```

Also add the re-export to drawing in the file's imports:

```rust
pub use drawing::box_blur_3pass; // if not already re-exported
```

- [ ] **Step 2: Run full test suite**

Run: `cd system/test && cargo test --release -- --test-threads=1`

- [ ] **Step 3: Visual verification (CPU renderer)**

Build and capture screenshot with the CPU renderer:

```sh
cd system && cargo build --release
./test-qemu.sh --boot-only --boot-wait 6 --wait 2
```

Capture via QMP `screendump` + verify the frosted glass demo shows blurred colored rectangles with no dark edge banding.

- [ ] **Step 4: Commit**

```text
feat: CpuBackend padded three-pass box blur

apply_backdrop_blur() now captures a padded region (pad = sum of 3
half-widths) before blurring, then writes back only the center
portion. This eliminates edge banding from compounded CLAMP_TO_EDGE
over 3 passes. Same pattern as render_shadow_blurred. Uses
box_blur_3pass() — O(1) per pixel via running sums.
```

---

## Task 4: CpuBackend — Shadow Blur Migration

**Objective:** Migrate `render_shadow_blurred()` from the old single-pass Gaussian to `box_blur_3pass()`, unifying both blur paths onto the same algorithm.

**Depends on:** Task 2.

**Files:**

- Modify: `system/libraries/render/scene_render/walk.rs` (`render_shadow_blurred`, lines 522–594)

**Design:** Shadow blur already has correct padding (line 532: `let pad = blur_radius`). Replace:

1. Change `pad` computation from `blur_radius` to `box_blur_pad(sigma)` for optimal padding.
2. Replace `blur_surface(&src_read, &mut dst_surface, &mut tmp_buf, blur_radius, sigma_fp)` with `box_blur_3pass(&src_read, &mut dst_surface, &mut tmp_buf, sigma)`.
3. Increase `tmp_buf` size from `buf_size` to `buf_size * 2` (ping-pong scratch).
4. Remove unused `sigma_fp` computation.

The old `blur_surface` API (radius + sigma_fp) is replaced by the simpler `box_blur_3pass` API (sigma only).

- [ ] **Step 1: Update `render_shadow_blurred`**

In `walk.rs`, modify lines 522–594:

```rust
fn render_shadow_blurred(
    fb: &mut Surface,
    sx: u32,
    sy: u32,
    sw: u32,
    sh: u32,
    phys_radius: u32,
    shadow_color: Color,
    blur_radius: u32,
) {
    let sigma = blur_radius as f32 / 2.0;
    let pad = drawing::box_blur_pad(sigma);
    let buf_w = sw + 2 * pad;
    let buf_h = sh + 2 * pad;
    let buf_stride = buf_w * 4;
    let buf_size = (buf_stride * buf_h) as usize;

    if buf_size > 4 * 1024 * 1024 {
        fill_shadow_shape(fb, sx, sy, sw, sh, phys_radius, shadow_color);
        return;
    }

    let mut src_buf = vec![0u8; buf_size];
    {
        let mut src_fb = Surface {
            data: &mut src_buf,
            width: buf_w,
            height: buf_h,
            stride: buf_stride,
            format: PixelFormat::Bgra8888,
        };
        fill_shadow_shape(&mut src_fb, pad, pad, sw, sh, phys_radius, shadow_color);
    }

    let mut tmp_buf = vec![0u8; buf_size * 2]; // 2× for ping-pong
    let mut dst_buf = vec![0u8; buf_size];

    let src_read = drawing::ReadSurface {
        data: &src_buf,
        width: buf_w,
        height: buf_h,
        stride: buf_stride,
        format: PixelFormat::Bgra8888,
    };
    let mut dst_surface = Surface {
        data: &mut dst_buf,
        width: buf_w,
        height: buf_h,
        stride: buf_stride,
        format: PixelFormat::Bgra8888,
    };

    drawing::box_blur_3pass(&src_read, &mut dst_surface, &mut tmp_buf, sigma);

    let blit_x = sx.saturating_sub(pad);
    let blit_y = sy.saturating_sub(pad);
    fb.blit_blend(&dst_buf, buf_w, buf_h, buf_stride, blit_x, blit_y);
}
```

- [ ] **Step 2: Run full test suite**

Run: `cd system/test && cargo test --release -- --test-threads=1`

- [ ] **Step 3: Visual verification**

Verify shadows still render correctly via QEMU screenshot. The visual output should be nearly identical (the difference between single-pass Gaussian and 3-pass box blur is subtle for small radii).

- [ ] **Step 4: Commit**

```text
feat: shadow blur migrated to three-pass box blur

render_shadow_blurred() now uses box_blur_3pass() instead of
blur_surface(). Unifies both blur paths (backdrop + shadow) onto
the same O(1) running-sum algorithm. Padding uses box_blur_pad()
for optimal sizing. tmp_buf increased to 2× for ping-pong.
```

---

## Task 5: virgil-render — Expand BlurRequest + Skip Background

**Objective:** Fix the architectural ordering: the backdrop-blur node's background should NOT be part of the blurred content. It should be drawn on top of the blur result.

**Depends on:** None (can be done in parallel with Tasks 1–4).

**Files:**

- Modify: `system/services/drivers/virgil-render/scene_walk.rs`

**Design:** When `backdrop_blur_radius > 0`:

1. Collect the blur request with the node's background color and corner radius
2. Do NOT push the background quad into the normal batch
3. The render loop will draw the background after the blur pass

- [ ] **Step 1: Expand BlurRequest struct**

In `scene_walk.rs`, replace the current `BlurRequest`:

```rust
#[derive(Clone, Copy)]
pub struct BlurRequest {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: u8,
    /// Background color to draw ON TOP of the blur result.
    /// If fully transparent, no post-blur background is drawn.
    pub bg: scene::Color,
    /// Corner radius for the post-blur background quad (in physical pixels).
    pub bg_corner_radius: f32,
}
```

- [ ] **Step 2: Skip background for backdrop-blur nodes**

In `walk_node()`, modify the blur request collection (lines 209–230) and the background emission (lines 232–276). When `backdrop_blur_radius > 0`, store the bg info in BlurRequest and skip the normal batch push:

```rust
    // Collect backdrop blur request before drawing the node itself.
    let has_backdrop_blur = node.backdrop_blur_radius > 0
        && blur_requests.len() < MAX_BLUR_REQUESTS;

    if has_backdrop_blur {
        let region = ClipRect { x: abs_x, y: abs_y, w, h }.intersect(clip);
        if !region.is_empty() {
            blur_requests.push(BlurRequest {
                x: region.x,
                y: region.y,
                w: region.w,
                h: region.h,
                radius: node.backdrop_blur_radius,
                bg: node.background,
                bg_corner_radius: node.corner_radius as f32 * scale,
            });
        }
    }

    // Draw background — but NOT for backdrop-blur nodes (drawn post-blur).
    let bg = node.background;
    if bg.a > 0 && !has_backdrop_blur {
        // ... existing background quad push code ...
    }
```

- [ ] **Step 3: Build to verify compilation**

Run: `cd system && cargo build --release`

- [ ] **Step 4: Commit**

```text
feat: virgil-render skip bg for backdrop-blur nodes

BlurRequest now carries bg color and corner_radius. The scene walk
skips the normal background quad for nodes with backdrop_blur_radius > 0.
The render loop will draw the background on top of the blur result
(next commit). This fixes the architectural ordering so the frosted
glass tint isn't baked into the blur input.
```

---

## Task 6: TGSI Box Blur Shaders

**Objective:** Replace the fixed 9-tap Gaussian shaders with loop-based box blur shaders that work for any radius.

**Depends on:** None (can be done in parallel with Tasks 1–5).

**Files:**

- Modify: `system/services/drivers/virgil-render/shaders.rs`

**Design:** One H-blur shader and one V-blur shader, each using TGSI `BGNLOOP`/`ENDLOOP`/`BRK` to iterate from `-half_width` to `+half_width`. Both CONST registers live in a **single** constant buffer (binding index 0), uploaded as one 8-dword array:

- `CONST[0].x` = horizontal texel step (1.0 / BLUR_MAX_DIM)
- `CONST[0].y` = vertical texel step (1.0 / BLUR_MAX_DIM)
- `CONST[0].z` = max U texcoord (content boundary for clamping)
- `CONST[0].w` = max V texcoord (content boundary for clamping)
- `CONST[1].x` = half_width (as float, e.g., 3.0)
- `CONST[1].y` = 1.0 / (2 \* half_width + 1) (normalization factor)
- `CONST[1].z` = unused (0.0)
- `CONST[1].w` = unused (0.0)

**Critical:** In TGSI, `CONST[0]` and `CONST[1]` are the first two vec4 registers in the same constant buffer binding. A single `cmd_set_constant_buffer(PIPE_SHADER_FRAGMENT, 0, &[8 dwords])` populates both. Do NOT call `cmd_set_constant_buffer` twice with indices 0 and 1 — that would create two separate buffer bindings, which is not what the shader expects.

Each tap: offset texcoord by `i * step`, clamp to [0, max], sample, accumulate. After the loop, multiply by normalization factor.

- [ ] **Step 1: Replace BLUR_H_FS**

In `shaders.rs`, replace the horizontal blur shader:

```rust
// ── Loop-based horizontal box blur fragment shader ──────────────────────
//
// Accumulates (2*half_width + 1) texel samples along the X axis with
// uniform weight, producing a box-averaged output. Used as one pass of
// a 3-pass box blur that converges to Gaussian (CLT).
//
// The loop iterates from -half_width to +half_width (inclusive).
// Each tap's texcoord is clamped to [0, CONST[0].z] to implement
// CLAMP_TO_EDGE at the captured sub-region boundary.
//
// Constant buffer (binding 0, 8 floats = 2 vec4):
//   CONST[0] = [h_texel_step, v_texel_step, max_u, max_v]
//   CONST[1] = [half_width, 1/(2*half+1), 0, 0]
//
// Registers:
//   TEMP[0] = accumulator (RGBA)
//   TEMP[1] = texel sample
//   TEMP[2] = mutable UV (x modified per tap)
//   TEMP[3].x = loop counter (starts at -half_width)
//   TEMP[3].y = upper bound (half_width + 1, exclusive)
//   TEMP[4].x = comparison result

pub const BLUR_H_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL CONST[0]\n\
DCL CONST[1]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
DCL TEMP[2]\n\
DCL TEMP[3]\n\
DCL TEMP[4]\n\
IMM[0] FLT32 { 0.0, 1.0, 0.0, 0.0 }\n\
  0: MOV TEMP[0], IMM[0].xxxx\n\
  1: MOV TEMP[2], IN[0]\n\
  2: MOV TEMP[3].x, -CONST[1].xxxx\n\
  3: ADD TEMP[3].y, CONST[1].xxxx, IMM[0].yyyy\n\
  4: BGNLOOP\n\
  5: SGE TEMP[4].x, TEMP[3].xxxx, TEMP[3].yyyy\n\
  6: IF TEMP[4].xxxx\n\
  7: BRK\n\
  8: ENDIF\n\
  9: MAD TEMP[2].x, TEMP[3].xxxx, CONST[0].xxxx, IN[0].xxxx\n\
 10: MAX TEMP[2].x, TEMP[2].xxxx, IMM[0].xxxx\n\
 11: MIN TEMP[2].x, TEMP[2].xxxx, CONST[0].zzzz\n\
 12: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 13: ADD TEMP[0], TEMP[0], TEMP[1]\n\
 14: ADD TEMP[3].x, TEMP[3].xxxx, IMM[0].yyyy\n\
 15: ENDLOOP\n\
 16: MUL OUT[0], TEMP[0], CONST[1].yyyy\n\
 17: END\n\0";
```

- [ ] **Step 2: Replace BLUR_V_FS**

Same structure, but modifies `.y` instead of `.x`:

```rust
// ── Loop-based vertical box blur fragment shader ────────────────────────
//
// Same algorithm as BLUR_H_FS but samples along the Y axis.
// Texcoord clamping uses CONST[0].w (max V) and 0.0 (min V).
//
// Constant buffer layout: same as BLUR_H_FS (binding 0, 8 floats).

pub const BLUR_V_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL CONST[0]\n\
DCL CONST[1]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
DCL TEMP[2]\n\
DCL TEMP[3]\n\
DCL TEMP[4]\n\
IMM[0] FLT32 { 0.0, 1.0, 0.0, 0.0 }\n\
  0: MOV TEMP[0], IMM[0].xxxx\n\
  1: MOV TEMP[2], IN[0]\n\
  2: MOV TEMP[3].x, -CONST[1].xxxx\n\
  3: ADD TEMP[3].y, CONST[1].xxxx, IMM[0].yyyy\n\
  4: BGNLOOP\n\
  5: SGE TEMP[4].x, TEMP[3].xxxx, TEMP[3].yyyy\n\
  6: IF TEMP[4].xxxx\n\
  7: BRK\n\
  8: ENDIF\n\
  9: MAD TEMP[2].y, TEMP[3].xxxx, CONST[0].yyyy, IN[0].yyyy\n\
 10: MAX TEMP[2].y, TEMP[2].yyyy, IMM[0].xxxx\n\
 11: MIN TEMP[2].y, TEMP[2].yyyy, CONST[0].wwww\n\
 12: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 13: ADD TEMP[0], TEMP[0], TEMP[1]\n\
 14: ADD TEMP[3].x, TEMP[3].xxxx, IMM[0].yyyy\n\
 15: ENDLOOP\n\
 16: MUL OUT[0], TEMP[0], CONST[1].yyyy\n\
 17: END\n\0";
```

- [ ] **Step 3: Update module doc comment**

Replace the shader list in the module doc (line 9–17 of `shaders.rs`) to describe the new shaders:

```rust
//! 6. `BLUR_H_FS`: loop-based horizontal box blur (any radius)
//! 7. `BLUR_V_FS`: loop-based vertical box blur (any radius)
```

- [ ] **Step 4: Build to verify shader compilation**

Run: `cd system && cargo build --release`

- [ ] **Step 5: Commit**

```text
feat: TGSI loop-based box blur shaders — any radius

Replaces the fixed 9-tap Gaussian fragment shaders with loop-based box
blur that works for any half-width. BGNLOOP/ENDLOOP with dynamic count
from constant buffer. Texcoord clamping via MIN/MAX prevents sampling
beyond the captured sub-region.

Constant buffer layout: single binding 0 with 8 floats (2 vec4).
CONST[0] = texel steps + bounds, CONST[1] = half_width + 1/diameter.
Uploaded via one cmd_set_constant_buffer call.
```

---

## Task 7: virgil-render — Three-Pass Ping-Pong + Padded Capture + Post-Blur Background

**Objective:** Wire up the 6-pass blur pipeline (3 iterations × H+V) with padded capture and draw the node's background on top of the blur result.

**Depends on:** Tasks 5 and 6.

**Files:**

- Modify: `system/services/drivers/virgil-render/main.rs` (blur execution loop, ~lines 1196–1385)

**Design:**

Pass sequence for each BlurRequest:

1. Blit padded FB region → BLUR_CAPTURE
2. H-blur BLUR_CAPTURE → BLUR_INTERMEDIATE (pass 1, half_width[0])
3. V-blur BLUR_INTERMEDIATE → BLUR_CAPTURE (pass 1, half_width[0])
4. H-blur BLUR_CAPTURE → BLUR_INTERMEDIATE (pass 2, half_width[1])
5. V-blur BLUR_INTERMEDIATE → BLUR_CAPTURE (pass 2, half_width[1])
6. H-blur BLUR_CAPTURE → BLUR_INTERMEDIATE (pass 3, half_width[2])
7. V-blur BLUR_INTERMEDIATE → main FB at node position (pass 3, half_width[2])
8. Draw bg quad on top of blur result (if bg.a > 0)

**Padded capture:**

The capture region extends by `pad` pixels on each side (where `pad = halves[0] + halves[1] + halves[2]`), clamped to framebuffer bounds. This gives the blur real scene content at the edges.

```text
pad = sum of halves
capture_x = max(0, bx - pad)
capture_y = max(0, by - pad)
capture_w = min(width, bx + bw + pad) - capture_x
capture_h = min(height, by + bh + pad) - capture_y
```

All intermediate blur passes use the full padded dimensions: `padded_u = capture_w / BLUR_MAX_DIM`, `padded_v = capture_h / BLUR_MAX_DIM`.

The final V-blur targets the main framebuffer at the node's position. Its quad vertices map to the node bounds in NDC, but its texcoords map to the CENTER of the padded texture:

```text
pad_left = bx - capture_x
pad_top  = by - capture_y
u0 = pad_left / BLUR_MAX_DIM
v0 = pad_top  / BLUR_MAX_DIM
u1 = (pad_left + bw) / BLUR_MAX_DIM
v1 = (pad_top  + bh) / BLUR_MAX_DIM
```

The shader's texcoord clamp bounds stay at [0, padded_u/v] for all passes — this allows sampling into the padding (which is real scene content), not beyond it.

**Constant buffer:** A single 8-dword array per draw call, uploaded via one `cmd_set_constant_buffer(PIPE_SHADER_FRAGMENT, 0, &cb_data_8)`:

```text
cb_data_8 = [
    texel_step_h,  texel_step_v,  padded_u,  padded_v,   // CONST[0]
    half_width_f,  inv_diameter,   0.0,       0.0,        // CONST[1]
]
```

Where `texel_step_h = 1.0 / BLUR_MAX_DIM` (always 1 pixel per tap — box blur samples adjacent pixels, not scaled).

**Post-blur background quad:**

After the final V-blur, if `blur.bg.a > 0`:

1. Switch to color vertex/fragment shaders (HANDLE_VS, HANDLE_FS)
2. Bind normal HANDLE_VE (position + color vertex elements)
3. Write a quad (6 vertices × 6 floats = 144 bytes) to the color VBO at a scratch offset
4. Draw the quad at (bx, by, bw, bh) in NDC with `blur.bg` color

Use the tail end of the color VBO (after all normal quads) as scratch space. Alternatively, reuse the blur quad slot in the text VBO since blur rendering is complete.

**Submission discipline:** Each of the 6 shader passes (3 iterations × H+V) MUST be a separate `submit_3d` call. Do NOT batch multiple passes into one command buffer — the same texture alternates between sampler input and render target across passes. The submit acts as a fence ensuring the GPU finishes writing before the next pass reads.

- [ ] **Step 1: Add imports**

```rust
use drawing::box_blur_widths;
```

- [ ] **Step 2: Rewrite the blur execution loop**

Replace the current blur loop (lines 1196–1385) with the padded 6-pass ping-pong. For each blur request:

a. Compute `halves = box_blur_widths(radius as f32 / 2.0)` and `pad = halves[0] + halves[1] + halves[2]`
b. Compute padded capture region (clamped to FB bounds)
c. Blit padded FB region → BLUR_CAPTURE
d. For each of the 3 iterations `i`:
   - Compute `half = halves[i]`, `inv_diam = 1.0 / (2 * half + 1) as f32`
   - Build 8-dword constant buffer: `[1.0/DIM, 1.0/DIM, padded_u, padded_v, half as f32, inv_diam, 0.0, 0.0]`
   - H-blur: source → BLUR_INTERMEDIATE (fullscreen quad, viewport = capture_w × capture_h)
   - V-blur: BLUR_INTERMEDIATE → destination
     - Passes 0–1: destination = BLUR_CAPTURE (ping-pong)
     - Pass 2: destination = main FB (quad at node NDC, texcoords = center of padded texture)
e. If `blur.bg.a > 0`: draw background quad at node position

- [ ] **Step 3: Build**

Run: `cd system && cargo build --release`

- [ ] **Step 4: Visual verification**

Capture virgl screenshot and verify:

- The frosted glass panel shows blurred colored rectangles
- The white tint is drawn ON TOP (not blurred into the content)
- No dark edge artifacts on any side
- The blur is smooth and visually correct

```sh
# Launch virgl QEMU, wait, capture window
pkill -f "qemu-system-aarch64.*test.img" 2>/dev/null; sleep 0.5
SCRIPT_DIR="/Users/user/Sites/os/system"
"${SCRIPT_DIR}/bin/qemu/qemu-system-aarch64" \
    -machine virt,gic-version=3 -cpu cortex-a53 -smp 4 -m 256M \
    -rtc base=localtime -global virtio-mmio.force-legacy=false \
    -drive "file=${SCRIPT_DIR}/test.img,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 -device virtio-gpu-gl-device \
    -device virtio-keyboard-device -device virtio-tablet-device \
    -fsdev "local,id=fsdev0,path=${SCRIPT_DIR}/share,security_model=none" \
    -device "virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare" \
    -display cocoa,gl=es -serial file:/tmp/qemu-serial.log \
    -monitor unix:/tmp/qemu-mon.sock,server,nowait \
    -device "loader,file=${SCRIPT_DIR}/virt.dtb,addr=0x40000000,force-raw=on" \
    -kernel "${SCRIPT_DIR}/target/aarch64-unknown-none/release/kernel" &
QPID=$!; sleep 10
WID=$(/tmp/find_qemu_wid 2>/dev/null)
[ -n "$WID" ] && screencapture -x -l"$WID" /tmp/qemu-virgl.png
kill $QPID 2>/dev/null; wait $QPID 2>/dev/null || true
```

- [ ] **Step 5: Run full test suite**

Run: `cd system/test && cargo test --release -- --test-threads=1`

- [ ] **Step 6: Commit**

```text
feat: virgil-render three-pass box blur with padded capture

Replaces the broken 2-pass 9-tap Gaussian with mathematically correct
three-pass box blur (6 draw calls: 3 iterations × H+V). Ping-pongs
between BLUR_CAPTURE and BLUR_INTERMEDIATE textures. Box widths from
box_blur_widths() — same algorithm as CpuBackend.

Padded capture: extends the blit region by the total effective radius
on each side, giving the blur real scene content at edges. The final
V-blur maps its output texcoords to the center of the padded texture,
discarding the padding. Eliminates edge banding.

Architecture fix: the node's background is drawn AFTER the blur via
a separate color quad. The frosted glass tint is composited on top
of the blurred backdrop instead of being baked into the blur input.

Constant buffer: single 8-dword upload (CONST[0] + CONST[1] in
binding 0). texel_step = 1/BLUR_MAX_DIM (1 pixel per tap for box
blur, not scaled by radius like the old Gaussian).
```

---

## Build Order Summary

```text
Task 1: Box blur width computation + pad helper (shared math)
  ├── Task 2: CPU three-pass box blur (tiled V-pass, rounded division)
  │     ├── Task 3: CpuBackend backdrop blur (padded capture)
  │     └── Task 4: CpuBackend shadow blur migration
  │
Task 5: BlurRequest expansion + skip bg (virgil-render scene walk)
Task 6: TGSI box blur shaders (loop-based, single 8-dword CB)
  └── Task 7: Three-pass ping-pong + padded capture + post-blur bg (virgil-render main)
```

Tasks 1–4 (CPU path) and Tasks 5–6 (GPU prep) can be parallelized.
Task 7 depends on Tasks 5 and 6.
Task 4 can run in parallel with Task 3 (both depend only on Task 2).

---

## Acceptance Criteria

- [ ] All existing tests pass (~2,013+)
- [ ] New box blur tests pass: width computation, 3-pass convergence, symmetry, no-darkening, reference Gaussian comparison (≤ ±3 levels)
- [ ] CpuBackend: `apply_backdrop_blur` uses padded `box_blur_3pass` — visually verified via QEMU screenshot
- [ ] CpuBackend: `render_shadow_blurred` uses `box_blur_3pass` — shadows visually correct
- [ ] virgil-render: 6-pass ping-pong with padded capture and loop-based TGSI shaders — visually verified
- [ ] virgil-render: node's background drawn on top of blur (frosted glass tint visible, not baked in)
- [ ] No dark edge banding on any side (both renderers)
- [ ] No systematic darkening (rounded division verified by no-darkening test)
- [ ] Blur radius is responsive: radius=8 produces clearly visible blur (~8px spread)
- [ ] Both renderers produce visually equivalent results for the same scene
- [ ] Constant buffer uploaded as single 8-dword call (not two separate calls)
