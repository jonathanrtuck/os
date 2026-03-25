//! Tests for three-pass box blur: width computation, CPU passes, convergence,
//! symmetry, no-darkening, and reference Gaussian comparison.

use drawing::{
    box_blur::{box_blur_3pass, box_blur_pad, box_blur_widths},
    PixelFormat, ReadSurface, Surface,
};

// ── Width computation tests ──────────────────────────────────────────────

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
    assert!(
        (total_var - expected_var).abs() < 2.0,
        "total variance {total_var} should be near σ²={expected_var}"
    );
}

#[test]
fn box_blur_widths_sigma_1_returns_small_widths() {
    // σ=1 → W3C formula gives wl=1, wu=3. Some passes may use width=1
    // (half=0, identity pass) which is correct — the other passes blur.
    let halves = box_blur_widths(1.0);
    let max_half = halves.iter().copied().max().unwrap();
    assert!(max_half >= 1, "at least one pass must blur");
    for &h in &halves {
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

// ── Three-pass CPU blur tests ────────────────────────────────────────────

fn make_surface_ro(data: &[u8], w: u32, h: u32) -> ReadSurface<'_> {
    ReadSurface {
        data,
        width: w,
        height: h,
        stride: w * 4,
        format: PixelFormat::Bgra8888,
    }
}

fn make_surface_rw(data: &mut [u8], w: u32, h: u32) -> Surface<'_> {
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
    let src_buf = vec![128u8; (w * h * 4) as usize];
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
                assert!(
                    (v as i32 - 128).unsigned_abs() <= 1,
                    "pixel ({x},{y}) channel {c} = {v}, expected 128±1"
                );
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
    for c in 0..4 {
        src_buf[off + c] = 255;
    }

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

    // A pixel 20px away should be very dim (σ=4, 5σ=20).
    let vfar_off = ((cy * w + cx + 20) * 4) as usize;
    let vfar_val = dst.data[vfar_off];
    assert!(
        vfar_val <= center_val / 2,
        "very distant pixel ({vfar_val}) should be much dimmer than center ({center_val})"
    );
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
    for c in 0..4 {
        src_buf[off + c] = 255;
    }

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);
    box_blur_3pass(&src, &mut dst, &mut tmp_buf, 4.0);

    // Horizontal symmetry: (cx+d, cy) == (cx-d, cy) ± 1.
    for d in 1..10u32 {
        let left = ((cy * w + cx - d) * 4) as usize;
        let right = ((cy * w + cx + d) * 4) as usize;
        for c in 0..4 {
            let diff = (dst.data[left + c] as i32 - dst.data[right + c] as i32).unsigned_abs();
            assert!(
                diff <= 1,
                "symmetry broken at d={d} c={c}: {} vs {}",
                dst.data[left + c],
                dst.data[right + c]
            );
        }
    }
}

#[test]
fn box_blur_3pass_no_systematic_darkening() {
    // 3 passes of truncating integer division would darken by up to 3 levels.
    // With proper rounding, a uniform 200-value surface should stay 200 ± 1.
    let w = 64u32;
    let h = 64u32;
    let src_buf = vec![200u8; (w * h * 4) as usize];
    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);
    box_blur_3pass(&src, &mut dst, &mut tmp_buf, 8.0);

    // Center pixel should be 200 ± 1, NOT 197 (which truncation would give).
    let center = ((32 * w + 32) * 4) as usize;
    for c in 0..4 {
        let v = dst.data[center + c];
        assert!(
            (v as i32 - 200).unsigned_abs() <= 1,
            "channel {c} = {v}, expected 200±1 (not darkened)"
        );
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
    for c in 0..4 {
        src_buf[off + c] = 255;
    }

    let mut dst_buf = vec![0u8; (w * h * 4) as usize];
    let mut tmp_buf = vec![0u8; (w * h * 4) as usize * 2];

    let src = make_surface_ro(&src_buf, w, h);
    let mut dst = make_surface_rw(&mut dst_buf, w, h);
    box_blur_3pass(&src, &mut dst, &mut tmp_buf, sigma);

    // Reference Gaussian: g(x,y) = exp(-(x²+y²)/(2σ²)) / (2πσ²)
    // Normalized so the sum over the kernel equals 255 (the input impulse).
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut ref_sum = 0.0f64;
    let radius = (3.0 * sigma) as i32 + 1; // 3σ captures 99.7%
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
            if err > max_err {
                max_err = err;
            }
        }
    }

    // 3-pass box blur should match a true Gaussian within ±3 levels
    // in the center region. The CLT approximation is not perfect but
    // is visually indistinguishable.
    assert!(
        max_err <= 3,
        "max error vs reference Gaussian = {max_err}, expected ≤ 3"
    );
}
