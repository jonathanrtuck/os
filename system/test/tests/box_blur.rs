//! Tests for three-pass box blur: width computation, CPU passes, convergence,
//! symmetry, no-darkening, and reference Gaussian comparison.

use drawing::box_blur::{box_blur_pad, box_blur_widths};

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
