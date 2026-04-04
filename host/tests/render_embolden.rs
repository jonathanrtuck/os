//! Tests for outline emboldening (stem darkening).
//!
//! Validates the macOS-style outline dilation algorithm:
//! - Dilation formula produces correct font-unit amounts at various sizes
//! - Emboldening shifts outline points outward
//! - Degenerate outlines handled gracefully
//! - Bounding box updated correctly after emboldening

use fonts::rasterize::{compute_dilation, embolden_outline, GlyphOutline, GlyphPoint};

/// Helper: create an outline from a slice of (x, y) points forming a single contour.
fn make_outline(points: &[(i32, i32)]) -> GlyphOutline {
    let mut outline = GlyphOutline::zeroed();
    for (i, &(x, y)) in points.iter().enumerate() {
        outline.points[i] = GlyphPoint {
            x,
            y,
            on_curve: true,
        };
    }
    outline.num_points = points.len() as u16;
    outline.num_contours = 1;
    outline.contour_ends[0] = (points.len() - 1) as u16;

    let mut x_min = points[0].0;
    let mut y_min = points[0].1;
    let mut x_max = points[0].0;
    let mut y_max = points[0].1;
    for &(x, y) in &points[1..] {
        if x < x_min {
            x_min = x;
        }
        if x > x_max {
            x_max = x;
        }
        if y < y_min {
            y_min = y;
        }
        if y > y_max {
            y_max = y;
        }
    }
    outline.x_min = x_min as i16;
    outline.y_min = y_min as i16;
    outline.x_max = x_max as i16;
    outline.y_max = y_max as i16;
    outline
}

// ===========================================================================
// Dilation formula tests
// ===========================================================================

#[test]
fn dilation_at_12pt_2x_1000upem() {
    // 12pt at 2x Retina → 24 device pixels.
    // With 1.3x boost: coeff_x = 0.015125 * 1.3 = 0.019663
    // dilation_x_device = min(0.39, 0.019663 * 24) = min(0.39, 0.4719) = 0.39 (capped)
    // dilation_x_glyph = 0.39 / 2 = 0.195
    // dilation_x_fu = 0.195 * 1000 / 12 = 16.25
    let (x, y) = compute_dilation(12, 1000, 2);
    let x_fu = x as f32 / 65536.0;
    let y_fu = y as f32 / 65536.0;
    assert!(
        (x_fu - 16.25).abs() < 1.0,
        "x_fu expected ~16.25, got {}",
        x_fu
    );
    // coeff_y = 0.0121 * 1.3 = 0.01573
    // dilation_y_device = min(0.39, 0.01573 * 24) = min(0.39, 0.3775) = 0.3775
    // dilation_y_glyph = 0.3775 / 2 = 0.18875
    // dilation_y_fu = 0.18875 * 1000 / 12 = 15.73
    assert!(
        (y_fu - 15.73).abs() < 1.0,
        "y_fu expected ~15.73, got {}",
        y_fu
    );
}

#[test]
fn dilation_at_14pt_2x_1000upem() {
    // 14pt at 2x → 28 device px. With 1.3x boost, cap at 0.39.
    // dilation_x_device = min(0.39, 0.019663 * 28) = min(0.39, 0.5506) = 0.39 (capped)
    // dilation_x_glyph = 0.195, dilation_x_fu = 0.195 * 1000/14 = 13.93
    let (x, _y) = compute_dilation(14, 1000, 2);
    let x_fu = x as f32 / 65536.0;
    assert!(
        (x_fu - 13.93).abs() < 1.0,
        "x_fu expected ~13.93, got {}",
        x_fu
    );
}

#[test]
fn dilation_at_36pt_2x_1000upem_capped() {
    // 36pt at 2x → 72 device px. Both axes capped at 0.39.
    // dilation_x_glyph = 0.195, dilation_x_fu = 0.195 * 1000/36 = 5.417
    let (x, y) = compute_dilation(36, 1000, 2);
    let x_fu = x as f32 / 65536.0;
    let y_fu = y as f32 / 65536.0;
    assert!(
        (x_fu - 5.417).abs() < 1.0,
        "x_fu expected ~5.417, got {}",
        x_fu
    );
    assert!(
        (y_fu - 5.417).abs() < 1.0,
        "y_fu expected ~5.417, got {}",
        y_fu
    );
}

#[test]
fn dilation_at_2000upem() {
    // 2000 upem font at 14pt, 2x. Capped at 0.39.
    // dilation_x_glyph = 0.195, dilation_x_fu = 0.195 * 2000/14 = 27.86
    let (x, _y) = compute_dilation(14, 2000, 2);
    let x_fu = x as f32 / 65536.0;
    assert!(
        (x_fu - 27.86).abs() < 1.0,
        "x_fu expected ~27.86, got {}",
        x_fu
    );
}

#[test]
fn dilation_zero_size_returns_zero() {
    let (x, y) = compute_dilation(0, 1000, 2);
    assert_eq!(x, 0);
    assert_eq!(y, 0);
}

#[test]
fn dilation_x_greater_than_y() {
    // macOS uses different coefficients: X=0.015125 > Y=0.0121
    // So X dilation should always be >= Y dilation.
    for size in [8, 10, 12, 14, 16, 20, 24, 36] {
        let (x, y) = compute_dilation(size, 1000, 2);
        assert!(
            x >= y,
            "at {}px: x dilation ({}) should be >= y ({})",
            size,
            x,
            y
        );
    }
}

// ===========================================================================
// Outline emboldening tests
// ===========================================================================

#[test]
fn embolden_zero_strength_no_change() {
    let original = [(0, 0), (0, 200), (100, 200), (100, 0)];
    let mut outline = make_outline(&original);
    embolden_outline(&mut outline, 0, 0);
    for (i, &(x, y)) in original.iter().enumerate() {
        assert_eq!(outline.points[i].x, x);
        assert_eq!(outline.points[i].y, y);
    }
}

#[test]
fn embolden_square_grows_outward() {
    let mut outline = make_outline(&[(0, 0), (0, 200), (100, 200), (100, 0)]);
    let strength = 10 << 16; // 10 font units in 16.16
    embolden_outline(&mut outline, strength, strength);

    // P0=(0,0) BL: should move left and down.
    assert!(
        outline.points[0].x < 0,
        "BL.x should be negative, got {}",
        outline.points[0].x
    );
    assert!(
        outline.points[0].y < 0,
        "BL.y should be negative, got {}",
        outline.points[0].y
    );

    // P1=(0,200) TL: should move left and up.
    assert!(
        outline.points[1].x < 0,
        "TL.x should be negative, got {}",
        outline.points[1].x
    );
    assert!(
        outline.points[1].y > 200,
        "TL.y should be > 200, got {}",
        outline.points[1].y
    );

    // P2=(100,200) TR: should move right and up.
    assert!(
        outline.points[2].x > 100,
        "TR.x should be > 100, got {}",
        outline.points[2].x
    );
    assert!(
        outline.points[2].y > 200,
        "TR.y should be > 200, got {}",
        outline.points[2].y
    );

    // P3=(100,0) BR: should move right and down.
    assert!(
        outline.points[3].x > 100,
        "BR.x should be > 100, got {}",
        outline.points[3].x
    );
    assert!(
        outline.points[3].y < 0,
        "BR.y should be negative, got {}",
        outline.points[3].y
    );
}

#[test]
fn embolden_bbox_grows() {
    // CW square (TrueType convention in y-up).
    let mut outline = make_outline(&[(10, 10), (10, 90), (90, 90), (90, 10)]);
    assert_eq!(outline.x_min, 10);
    assert_eq!(outline.x_max, 90);
    embolden_outline(&mut outline, 10 << 16, 10 << 16);
    assert!(outline.x_min < 10, "x_min should have decreased");
    assert!(outline.x_max > 90, "x_max should have increased");
    assert!(outline.y_min < 10, "y_min should have decreased");
    assert!(outline.y_max > 90, "y_max should have increased");
}

#[test]
fn embolden_degenerate_single_point_no_panic() {
    let mut outline = GlyphOutline::zeroed();
    outline.points[0] = GlyphPoint {
        x: 50,
        y: 50,
        on_curve: true,
    };
    outline.num_points = 1;
    outline.num_contours = 1;
    outline.contour_ends[0] = 0;
    embolden_outline(&mut outline, 10 << 16, 10 << 16);
}

#[test]
fn embolden_two_point_segment_no_panic() {
    let mut outline = make_outline(&[(0, 0), (100, 0)]);
    embolden_outline(&mut outline, 10 << 16, 10 << 16);
}

#[test]
fn embolden_coincident_points_no_panic() {
    let mut outline = make_outline(&[(0, 0), (0, 0), (100, 0), (50, 100)]);
    outline.contour_ends[0] = 3;
    embolden_outline(&mut outline, 10 << 16, 10 << 16);
    for i in 0..4 {
        assert!(
            outline.points[i].x.abs() < 10000,
            "point {} x overflow: {}",
            i,
            outline.points[i].x
        );
        assert!(
            outline.points[i].y.abs() < 10000,
            "point {} y overflow: {}",
            i,
            outline.points[i].y
        );
    }
}

#[test]
fn embolden_preserves_winding() {
    // After emboldening, a CW outline should remain CW (signed area same sign).
    // CW square (TrueType convention) — negative signed area in y-up.
    let mut outline = make_outline(&[(0, 0), (0, 100), (100, 100), (100, 0)]);

    // Compute initial signed area.
    let initial_area = signed_area(&outline);
    assert!(initial_area < 0, "should start CW (negative area in y-up)");

    embolden_outline(&mut outline, 10 << 16, 10 << 16);

    let final_area = signed_area(&outline);
    assert!(
        final_area < 0,
        "should remain CW after emboldening, got area={}",
        final_area
    );
    // Area magnitude should increase (outline grows outward).
    assert!(
        final_area.abs() > initial_area.abs(),
        "area magnitude should increase after emboldening"
    );
}

/// Compute the signed area of a single-contour outline (shoelace formula).
fn signed_area(outline: &GlyphOutline) -> i64 {
    let n = outline.num_points as usize;
    let mut area: i64 = 0;
    for i in 0..n {
        let j = if i + 1 >= n { 0 } else { i + 1 };
        let xi = outline.points[i].x as i64;
        let yi = outline.points[i].y as i64;
        let xj = outline.points[j].x as i64;
        let yj = outline.points[j].y as i64;
        area += xi * yj - xj * yi;
    }
    area
}

#[test]
fn embolden_symmetric_for_symmetric_shape() {
    // A centered square: emboldening should grow symmetrically.
    // CW centered square (TrueType convention).
    let mut outline = make_outline(&[(-50, -50), (-50, 50), (50, 50), (50, -50)]);
    embolden_outline(&mut outline, 10 << 16, 10 << 16);

    // Bottom-left and top-right should be roughly symmetric.
    let bl_x = outline.points[0].x;
    let tr_x = outline.points[2].x;
    assert!(
        (bl_x + tr_x).abs() <= 1,
        "should be symmetric: BL.x={}, TR.x={}",
        bl_x,
        tr_x
    );

    let bl_y = outline.points[0].y;
    let tr_y = outline.points[2].y;
    assert!(
        (bl_y + tr_y).abs() <= 1,
        "should be symmetric: BL.y={}, TR.y={}",
        bl_y,
        tr_y
    );
}
