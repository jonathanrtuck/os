//! SVG path parser and rasterizer tests.
//!
//! The SVG decoder lives in the render library. Tests import from the render
//! crate via the Cargo dependency.

use render::svg::*;

// ---------------------------------------------------------------------------
// Parser: absolute commands
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_empty_string_returns_error() {
    let result = svg_parse_path(b"");
    assert_eq!(result.err(), Some(SvgError::EmptyData));
}

#[test]
fn svg_parse_whitespace_only_returns_error() {
    let result = svg_parse_path(b"   \t\n ");
    assert_eq!(result.err(), Some(SvgError::EmptyData));
}

#[test]
fn svg_parse_invalid_command_returns_error() {
    let result = svg_parse_path(b"X 10 20");
    assert_eq!(result.err(), Some(SvgError::InvalidCommand(b'X')));
}

#[test]
fn svg_parse_missing_coordinates_returns_error() {
    let result = svg_parse_path(b"M 10");
    assert_eq!(result.err(), Some(SvgError::MissingCoordinates));
}

#[test]
fn svg_parse_missing_cubic_coords_returns_error() {
    let result = svg_parse_path(b"M 0 0 C 1 2 3 4 5");
    assert_eq!(result.err(), Some(SvgError::MissingCoordinates));
}

#[test]
fn svg_parse_moveto_absolute() {
    let path = svg_parse_path(b"M 10 20").unwrap();
    assert_eq!(path.num_commands, 1);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_moveto_lineto_absolute() {
    let path = svg_parse_path(b"M 0 0 L 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_cubic_absolute() {
    let path = svg_parse_path(b"M 0 0 C 1 2 3 4 5 6").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(
        path.commands[1],
        SvgCommand::CubicTo {
            x1: 1,
            y1: 2,
            x2: 3,
            y2: 4,
            x: 5,
            y: 6
        }
    );
}

#[test]
fn svg_parse_close_path() {
    let path = svg_parse_path(b"M 0 0 L 10 0 L 10 10 Z").unwrap();
    assert_eq!(path.num_commands, 4);
    assert_eq!(path.commands[3], SvgCommand::Close);
}

#[test]
fn svg_parse_close_lowercase() {
    let path = svg_parse_path(b"M 0 0 L 10 0 z").unwrap();
    assert_eq!(path.num_commands, 3);
    assert_eq!(path.commands[2], SvgCommand::Close);
}

// ---------------------------------------------------------------------------
// Parser: relative commands
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_moveto_relative() {
    let path = svg_parse_path(b"m 10 20").unwrap();
    assert_eq!(path.num_commands, 1);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_lineto_relative_resolves_against_current() {
    let path = svg_parse_path(b"M 5 5 l 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 15, y: 25 });
}

#[test]
fn svg_parse_cubic_relative() {
    let path = svg_parse_path(b"M 10 10 c 1 2 3 4 5 6").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(
        path.commands[1],
        SvgCommand::CubicTo {
            x1: 11,
            y1: 12,
            x2: 13,
            y2: 14,
            x: 15,
            y: 16
        }
    );
}

#[test]
fn svg_parse_relative_moveto_chain() {
    let path = svg_parse_path(b"m 10 10 m 5 5").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 10 });
    assert_eq!(path.commands[1], SvgCommand::MoveTo { x: 15, y: 15 });
}

// ---------------------------------------------------------------------------
// Parser: coordinate formats
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_comma_separated_coords() {
    let path = svg_parse_path(b"M 10,20 L 30,40").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 30, y: 40 });
}

#[test]
fn svg_parse_negative_coords() {
    let path = svg_parse_path(b"M -10 -20").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: -10, y: -20 });
}

#[test]
fn svg_parse_no_space_between_command_and_number() {
    let path = svg_parse_path(b"M10 20").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_multiple_spaces_between_coords() {
    let path = svg_parse_path(b"M  10   20").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_implicit_lineto_after_moveto() {
    let path = svg_parse_path(b"M 0 0 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_implicit_lineto_after_relative_moveto() {
    let path = svg_parse_path(b"m 0 0 10 20").unwrap();
    assert_eq!(path.num_commands, 2);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_decimal_coords_integer_part_only() {
    let path = svg_parse_path(b"M 10.5 20.9").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 10, y: 20 });
}

#[test]
fn svg_parse_leading_decimal_treated_as_zero() {
    let path = svg_parse_path(b"M .5 .9").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
}

#[test]
fn svg_parse_leading_decimal_with_integer_part() {
    let path = svg_parse_path(b"M 3.7 .2").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 3, y: 0 });
}

#[test]
fn svg_parse_negative_leading_decimal() {
    let path = svg_parse_path(b"M -.5 -.9").unwrap();
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
}

// ---------------------------------------------------------------------------
// Parser: complex paths
// ---------------------------------------------------------------------------

#[test]
fn svg_parse_triangle() {
    let path = svg_parse_path(b"M 0 0 L 10 0 L 5 10 Z").unwrap();
    assert_eq!(path.num_commands, 4);
    assert_eq!(path.commands[0], SvgCommand::MoveTo { x: 0, y: 0 });
    assert_eq!(path.commands[1], SvgCommand::LineTo { x: 10, y: 0 });
    assert_eq!(path.commands[2], SvgCommand::LineTo { x: 5, y: 10 });
    assert_eq!(path.commands[3], SvgCommand::Close);
}

#[test]
fn svg_parse_multiple_subpaths() {
    let path = svg_parse_path(b"M 0 0 L 10 0 Z M 20 20 L 30 20 Z").unwrap();
    assert_eq!(path.num_commands, 6);
    assert_eq!(path.commands[3], SvgCommand::MoveTo { x: 20, y: 20 });
}

// ---------------------------------------------------------------------------
// Rasterizer tests
// ---------------------------------------------------------------------------

#[test]
fn svg_rasterize_empty_path_returns_error() {
    let path = SvgPath::new();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 10 * 10];
    let result = svg_rasterize(&path, &mut scratch, &mut coverage, 10, 10, 4096, 0, 0);
    assert!(result.is_ok());
    assert!(coverage.iter().all(|&v| v == 0));
}

#[test]
fn svg_rasterize_filled_square() {
    let path = svg_parse_path(b"M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 16 * 16];

    svg_rasterize(&path, &mut scratch, &mut coverage, 16, 16, 4096, 0, 0).unwrap();

    let center_idx = 5 * 16 + 5;
    assert!(
        coverage[center_idx] > 200,
        "Interior pixel (5,5) should have high coverage, got {}",
        coverage[center_idx]
    );

    let outside_idx = 12 * 16 + 12;
    assert_eq!(
        coverage[outside_idx], 0,
        "Exterior pixel (12,12) should have zero coverage"
    );
}

#[test]
fn svg_rasterize_triangle() {
    let path = svg_parse_path(b"M 0 0 L 20 0 L 0 20 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    let inside_idx = 2 * 24 + 2;
    assert!(
        coverage[inside_idx] > 100,
        "Interior pixel (2,2) should have significant coverage, got {}",
        coverage[inside_idx]
    );

    let outside_idx = 22 * 24 + 22;
    assert_eq!(coverage[outside_idx], 0, "Exterior pixel should be zero");
}

#[test]
fn svg_rasterize_with_cubic_produces_coverage() {
    let path = svg_parse_path(b"M 0 10 C 0 0 20 0 20 10 L 20 20 L 0 20 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    let center_idx = 15 * 24 + 10;
    assert!(
        coverage[center_idx] > 100,
        "Interior of curved shape should have coverage, got {}",
        coverage[center_idx]
    );
}

#[test]
fn svg_rasterize_antialiased_edges() {
    let path = svg_parse_path(b"M 0 0 L 20 0 L 10 20 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    let mut found_intermediate = false;
    for y in 0..20 {
        for x in 0..20 {
            let idx = y * 24 + x;
            let c = coverage[idx];
            if c > 0 && c < 255 {
                found_intermediate = true;
                break;
            }
        }
        if found_intermediate {
            break;
        }
    }
    assert!(
        found_intermediate,
        "Antialiased edges should produce intermediate coverage values (not just 0 or 255)"
    );
}

#[test]
fn svg_rasterize_scaled_shape() {
    let path = svg_parse_path(b"M 0 0 L 5 0 L 5 5 L 0 5 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 16 * 16];

    svg_rasterize(&path, &mut scratch, &mut coverage, 16, 16, 8192, 0, 0).unwrap();

    let inside = 5 * 16 + 5;
    assert!(
        coverage[inside] > 200,
        "Scaled interior pixel should have high coverage, got {}",
        coverage[inside]
    );

    let outside = 12 * 16 + 12;
    assert_eq!(coverage[outside], 0, "Scaled exterior should be zero");
}

#[test]
fn svg_rasterize_with_offset() {
    let path = svg_parse_path(b"M 0 0 L 5 0 L 5 5 L 0 5 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 16 * 16];

    svg_rasterize(&path, &mut scratch, &mut coverage, 16, 16, 4096, 3, 3).unwrap();

    let inside = 5 * 16 + 5;
    assert!(
        coverage[inside] > 200,
        "Offset interior pixel should have high coverage, got {}",
        coverage[inside]
    );

    let outside = 0 * 16 + 0;
    assert_eq!(coverage[outside], 0, "Origin should be zero with offset");
}

#[test]
fn svg_rasterize_winding_rule_nonzero() {
    let path =
        svg_parse_path(b"M 0 0 L 20 0 L 20 20 L 0 20 Z M 5 5 L 5 15 L 15 15 L 15 5 Z").unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 24, 4096, 0, 0).unwrap();

    let outer_idx = 2 * 24 + 2;
    assert!(
        coverage[outer_idx] > 200,
        "Outer ring should have coverage, got {}",
        coverage[outer_idx]
    );

    let inner_idx = 10 * 24 + 10;
    assert_eq!(
        coverage[inner_idx], 0,
        "Inner hole should have zero coverage (non-zero winding), got {}",
        coverage[inner_idx]
    );
}

// ===========================================================================
// SVG icon tests — document icon loading and rasterization
// ===========================================================================

const DOC_ICON_PATH: &[u8] = b"M 0 0 L 14 0 L 20 6 L 20 24 L 0 24 Z M 4 10 L 4 12 L 16 12 L 16 10 Z M 4 15 L 4 17 L 16 17 L 16 15 Z M 4 20 L 4 22 L 12 22 L 12 20 Z";

#[test]
fn svg_icon_doc_parses_successfully() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    assert!(
        path.num_commands > 15,
        "Doc icon should have many commands, got {}",
        path.num_commands
    );
}

#[test]
fn svg_icon_doc_rasterizes_at_20x24() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 28];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 28, 4096, 0, 0).unwrap();

    let body_idx = 2 * 24 + 2;
    assert!(
        coverage[body_idx] > 200,
        "Page body interior (2,2) should have high coverage, got {}",
        coverage[body_idx]
    );

    let ext_idx = 2 * 24 + 22;
    assert_eq!(coverage[ext_idx], 0, "Exterior (22,2) should be zero");
}

#[test]
fn svg_icon_doc_has_text_line_holes() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 28];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 28, 4096, 0, 0).unwrap();

    let hole1_idx = 11 * 24 + 10;
    assert!(
        coverage[hole1_idx] < 30,
        "Text line hole 1 center (10,11) should have low coverage (hole), got {}",
        coverage[hole1_idx]
    );

    let body_above = 8 * 24 + 10;
    assert!(
        coverage[body_above] > 200,
        "Body above text line (10,8) should be filled, got {}",
        coverage[body_above]
    );
}

#[test]
fn svg_icon_doc_rasterizes_scaled_for_chrome() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let icon_w: u32 = 20;
    let icon_h: u32 = 24;
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(
        &path,
        &mut scratch,
        &mut coverage,
        icon_w,
        icon_h,
        4096,
        0,
        0,
    )
    .unwrap();

    let filled_count = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        filled_count > 50,
        "Icon should have significant filled area at 20x24, got {} filled pixels",
        filled_count
    );
}

#[test]
fn svg_icon_doc_has_antialiased_diagonal() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 24 * 28];

    svg_rasterize(&path, &mut scratch, &mut coverage, 24, 28, 4096, 0, 0).unwrap();

    let mut found_intermediate = false;
    for y in 0..6 {
        for x in 14..21 {
            let idx = y as usize * 24 + x as usize;
            let c = coverage[idx];
            if c > 0 && c < 255 {
                found_intermediate = true;
                break;
            }
        }
        if found_intermediate {
            break;
        }
    }
    assert!(
        found_intermediate,
        "Diagonal edge of the doc icon should have antialiased pixels"
    );
}

// ===========================================================================
// SVG icon tests — image icon loading and rasterization
// ===========================================================================

const IMG_ICON_PATH: &[u8] = b"M 0 0 L 20 0 L 20 20 L 0 20 Z M 2 2 L 2 18 L 18 18 L 18 2 Z M 4 12 L 8 7 L 12 12 L 14 9 L 17 13 L 17 16 L 4 16 Z M 13 5 C 14 4 16 4 16 6 C 16 7 14 8 13 7 C 12 6 12 6 13 5 Z";

#[test]
fn svg_icon_img_parses_successfully() {
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    assert!(
        path.num_commands > 15,
        "Image icon should have many commands, got {}",
        path.num_commands
    );
}

#[test]
fn svg_icon_img_rasterizes_at_20x24() {
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 20, 24, 4096, 0, 0).unwrap();

    let filled_count = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        filled_count > 50,
        "Image icon should have significant filled area at 20x24, got {} filled pixels",
        filled_count
    );
}

#[test]
fn svg_icon_img_differs_from_doc_icon() {
    let doc_path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let img_path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut doc_scratch = SvgRasterScratch::zeroed();
    let mut img_scratch = SvgRasterScratch::zeroed();
    let mut doc_cov = [0u8; 20 * 24];
    let mut img_cov = [0u8; 20 * 24];

    svg_rasterize(
        &doc_path,
        &mut doc_scratch,
        &mut doc_cov,
        20,
        24,
        4096,
        0,
        0,
    )
    .unwrap();
    svg_rasterize(
        &img_path,
        &mut img_scratch,
        &mut img_cov,
        20,
        24,
        4096,
        0,
        0,
    )
    .unwrap();

    let diff_count = doc_cov
        .iter()
        .zip(img_cov.iter())
        .filter(|(&a, &b)| (a as i16 - b as i16).unsigned_abs() > 30)
        .count();
    assert!(
        diff_count > 40,
        "Doc and image icons should differ significantly, only {} pixels differ",
        diff_count
    );
}

#[test]
fn svg_icon_img_has_frame_border() {
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 20, 24, 4096, 0, 0).unwrap();

    let border_idx = 1 * 20 + 0;
    assert!(
        coverage[border_idx] > 100,
        "Frame border at (0,1) should be filled, got {}",
        coverage[border_idx]
    );

    let total_filled = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        total_filled > 50,
        "Icon should not be mostly blank: {} filled",
        total_filled
    );
}

#[test]
fn svg_icon_doc_rasterizes_at_20x24_chrome_size() {
    let path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 20, 24, 4096, 0, 0).unwrap();

    let filled_count = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        filled_count > 100,
        "Doc icon at 20x24 (1×) should have significant coverage, got {} filled pixels",
        filled_count
    );
}

#[test]
fn svg_icon_img_rasterizes_at_20x24_chrome_size() {
    let path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut scratch = SvgRasterScratch::zeroed();
    let mut coverage = [0u8; 20 * 24];

    svg_rasterize(&path, &mut scratch, &mut coverage, 20, 24, 4096, 0, 0).unwrap();

    let filled_count = coverage.iter().filter(|&&c| c > 0).count();
    assert!(
        filled_count > 80,
        "Image icon at 20x24 (1×) should have significant coverage, got {} filled pixels",
        filled_count
    );
}

#[test]
fn svg_icon_both_differ_at_20x24() {
    let doc_path = svg_parse_path(DOC_ICON_PATH).unwrap();
    let img_path = svg_parse_path(IMG_ICON_PATH).unwrap();
    let mut doc_scratch = SvgRasterScratch::zeroed();
    let mut img_scratch = SvgRasterScratch::zeroed();
    let mut doc_cov = [0u8; 20 * 24];
    let mut img_cov = [0u8; 20 * 24];

    svg_rasterize(
        &doc_path,
        &mut doc_scratch,
        &mut doc_cov,
        20,
        24,
        4096,
        0,
        0,
    )
    .unwrap();
    svg_rasterize(
        &img_path,
        &mut img_scratch,
        &mut img_cov,
        20,
        24,
        4096,
        0,
        0,
    )
    .unwrap();

    let diff_count = doc_cov
        .iter()
        .zip(img_cov.iter())
        .filter(|(&a, &b)| (a as i16 - b as i16).unsigned_abs() > 30)
        .count();
    assert!(
        diff_count > 40,
        "Doc and image icons should differ significantly at 20x24, only {} pixels differ",
        diff_count
    );
}
