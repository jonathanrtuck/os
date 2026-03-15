// SVG path data parser and rasterizer.
//
// Parses the `d` attribute of SVG `<path>` elements into a sequence of path
// commands, then rasterizes filled paths using the same scanline/coverage
// approach as the TrueType rasterizer (non-zero winding rule, antialiased).
//
// Supports: M/m (moveto), L/l (lineto), C/c (cubic Bezier curveto), Z/z
// (closepath). Both absolute (uppercase) and relative (lowercase) variants.
//
// All math is integer/fixed-point (20.12 format). No floating point, no
// allocations, no_std. Intermediate arithmetic widened to i64 where needed
// to prevent overflow; final results truncated to i32. Safe for SVG
// coordinates up to ~524,000 at 1× scale (see svg_coord_to_fp docs).

/// Vertical oversampling factor for anti-aliasing.
const OVERSAMPLE_Y: i32 = 8;

/// Maximum path commands after parsing.
const SVG_MAX_COMMANDS: usize = 512;
/// Maximum line segments after flattening cubic Beziers.
const SVG_MAX_SEGMENTS: usize = 4096;
/// Maximum active edges during scanline sweep.
const SVG_MAX_ACTIVE: usize = 128;
/// Fixed-point 20.12 format — same as the TrueType rasterizer.
const SVG_FP_SHIFT: i32 = 12;

pub const SVG_FP_ONE: i32 = 1 << SVG_FP_SHIFT;

/// A parsed SVG path command.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SvgCommand {
    /// Move to (x, y) — absolute.
    MoveTo { x: i32, y: i32 },
    /// Line to (x, y) — absolute.
    LineTo { x: i32, y: i32 },
    /// Cubic Bezier to (x1, y1, x2, y2, x, y) — absolute.
    /// (x1,y1) and (x2,y2) are control points, (x,y) is end point.
    CubicTo {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        x: i32,
        y: i32,
    },
    /// Close the current subpath.
    Close,
}
/// Errors that can occur during SVG path parsing.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SvgError {
    /// The path data string is empty.
    EmptyData,
    /// An unrecognized command letter was encountered.
    InvalidCommand(u8),
    /// A command requires more coordinates than were available.
    MissingCoordinates,
    /// A number in the path data could not be parsed.
    InvalidNumber,
    /// The path has too many commands (exceeds SVG_MAX_COMMANDS).
    TooManyCommands,
    /// The path has too many segments after flattening (exceeds SVG_MAX_SEGMENTS).
    TooManySegments,
}

/// A parsed SVG path — a sequence of commands.
pub struct SvgPath {
    pub commands: [SvgCommand; SVG_MAX_COMMANDS],
    pub num_commands: usize,
}

impl SvgPath {
    pub const fn new() -> Self {
        SvgPath {
            commands: [SvgCommand::Close; SVG_MAX_COMMANDS],
            num_commands: 0,
        }
    }
}

/// Scratch space for SVG path rasterization.
pub struct SvgRasterScratch {
    segments: [SvgSegment; SVG_MAX_SEGMENTS],
    num_segments: usize,
}

/// A line segment in fixed-point pixel coordinates.
#[derive(Clone, Copy, Default)]
struct SvgSegment {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

/// An active edge during scanline sweep.
#[derive(Clone, Copy, Default)]
struct SvgActiveEdge {
    x: i32,
    direction: i32,
}

impl SvgRasterScratch {
    pub const fn zeroed() -> Self {
        SvgRasterScratch {
            segments: [SvgSegment {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            }; SVG_MAX_SEGMENTS],
            num_segments: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Parser: SVG path data string → SvgPath
// ---------------------------------------------------------------------------

fn is_digit_or_sign(b: u8) -> bool {
    (b >= b'0' && b <= b'9') || b == b'-' || b == b'+' || b == b'.'
}
fn is_svg_command(b: u8) -> bool {
    matches!(b, b'M' | b'm' | b'L' | b'l' | b'C' | b'c' | b'Z' | b'z')
}
fn is_svg_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}
/// Parse an integer from the path data string, advancing `pos`.
/// Handles optional leading sign (+ or -) and leading decimal point
/// (e.g., ".5" parses as 0, since we use integer coordinates).
fn parse_svg_number(data: &[u8], pos: &mut usize) -> Result<i32, SvgError> {
    skip_whitespace_and_commas(data, pos);

    if *pos >= data.len() {
        return Err(SvgError::MissingCoordinates);
    }

    let mut negative = false;
    let b = data[*pos];

    if b == b'-' {
        negative = true;
        *pos += 1;
    } else if b == b'+' {
        *pos += 1;
    }

    // Handle leading decimal point (e.g., ".5" parses as 0).
    let has_integer_part = *pos < data.len() && data[*pos] >= b'0' && data[*pos] <= b'9';
    let has_decimal_start = *pos < data.len() && data[*pos] == b'.';

    if !has_integer_part && !has_decimal_start {
        return Err(SvgError::InvalidNumber);
    }

    let mut value: i32 = 0;

    while *pos < data.len() && data[*pos] >= b'0' && data[*pos] <= b'9' {
        let digit = (data[*pos] - b'0') as i32;

        // Check for overflow.
        value = value.checked_mul(10).ok_or(SvgError::InvalidNumber)?;
        value = value.checked_add(digit).ok_or(SvgError::InvalidNumber)?;
        *pos += 1;
    }

    // Skip decimal point and fractional part (we use integer coordinates).
    if *pos < data.len() && data[*pos] == b'.' {
        *pos += 1;

        while *pos < data.len() && data[*pos] >= b'0' && data[*pos] <= b'9' {
            *pos += 1;
        }
    }

    if negative {
        Ok(-value)
    } else {
        Ok(value)
    }
}
fn push_command(path: &mut SvgPath, cmd: SvgCommand) -> Result<(), SvgError> {
    if path.num_commands >= SVG_MAX_COMMANDS {
        return Err(SvgError::TooManyCommands);
    }

    path.commands[path.num_commands] = cmd;
    path.num_commands += 1;

    Ok(())
}

fn skip_whitespace_and_commas(data: &[u8], pos: &mut usize) {
    while *pos < data.len() {
        let b = data[*pos];

        if is_svg_whitespace(b) || b == b',' {
            *pos += 1;
        } else {
            break;
        }
    }
}

/// Parse an SVG path data string (the `d` attribute) into an `SvgPath`.
///
/// Coordinates are parsed as integers (sub-pixel precision via fixed-point
/// is handled at rasterization time). Supports comma-separated and space-
/// separated coordinate pairs.
///
/// Returns `Err` for empty data, invalid commands, or missing coordinates.
///
/// Note: `SvgPath` is ~16 KiB. On stack-constrained targets, prefer
/// `svg_parse_path_into()` with a heap-allocated `SvgPath`.
pub fn svg_parse_path(data: &[u8]) -> Result<SvgPath, SvgError> {
    let mut path = SvgPath::new();

    svg_parse_path_into(data, &mut path)?;

    Ok(path)
}
/// Parse an SVG path data string into a caller-provided `SvgPath`.
///
/// Same as `svg_parse_path()` but avoids allocating `SvgPath` on the
/// return stack — useful on targets with small stacks (e.g., 16 KiB
/// bare-metal userspace) where the caller can heap-allocate the path.
pub fn svg_parse_path_into(data: &[u8], path: &mut SvgPath) -> Result<(), SvgError> {
    if data.is_empty() {
        return Err(SvgError::EmptyData);
    }

    // Check if there's any non-whitespace content.
    let mut has_content = false;

    for &b in data {
        if !is_svg_whitespace(b) {
            has_content = true;

            break;
        }
    }

    if !has_content {
        return Err(SvgError::EmptyData);
    }

    path.num_commands = 0;
    let mut pos = 0usize;
    let mut current_x: i32 = 0;
    let mut current_y: i32 = 0;
    let mut subpath_start_x: i32 = 0;
    let mut subpath_start_y: i32 = 0;
    let mut last_command: u8 = 0;

    while pos < data.len() {
        skip_whitespace_and_commas(data, &mut pos);

        if pos >= data.len() {
            break;
        }

        let b = data[pos];
        // Determine command letter.
        let cmd = if is_svg_command(b) {
            pos += 1;
            last_command = b;
            b
        } else if is_digit_or_sign(b) && last_command != 0 {
            // Implicit repetition of the last command.
            // M becomes L for implicit repeats, m becomes l.
            match last_command {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            }
        } else {
            return Err(SvgError::InvalidCommand(b));
        };

        // Parse based on command.
        match cmd {
            b'M' => {
                let x = parse_svg_number(data, &mut pos)?;
                let y = parse_svg_number(data, &mut pos)?;

                current_x = x;
                current_y = y;
                subpath_start_x = x;
                subpath_start_y = y;

                push_command(path, SvgCommand::MoveTo { x, y })?;
            }
            b'm' => {
                let dx = parse_svg_number(data, &mut pos)?;
                let dy = parse_svg_number(data, &mut pos)?;

                current_x += dx;
                current_y += dy;
                subpath_start_x = current_x;
                subpath_start_y = current_y;

                push_command(
                    path,
                    SvgCommand::MoveTo {
                        x: current_x,
                        y: current_y,
                    },
                )?;
            }
            b'L' => {
                let x = parse_svg_number(data, &mut pos)?;
                let y = parse_svg_number(data, &mut pos)?;

                current_x = x;
                current_y = y;

                push_command(path, SvgCommand::LineTo { x, y })?;
            }
            b'l' => {
                let dx = parse_svg_number(data, &mut pos)?;
                let dy = parse_svg_number(data, &mut pos)?;

                current_x += dx;
                current_y += dy;

                push_command(
                    path,
                    SvgCommand::LineTo {
                        x: current_x,
                        y: current_y,
                    },
                )?;
            }
            b'C' => {
                let x1 = parse_svg_number(data, &mut pos)?;
                let y1 = parse_svg_number(data, &mut pos)?;
                let x2 = parse_svg_number(data, &mut pos)?;
                let y2 = parse_svg_number(data, &mut pos)?;
                let x = parse_svg_number(data, &mut pos)?;
                let y = parse_svg_number(data, &mut pos)?;

                current_x = x;
                current_y = y;

                push_command(
                    path,
                    SvgCommand::CubicTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x,
                        y,
                    },
                )?;
            }
            b'c' => {
                let dx1 = parse_svg_number(data, &mut pos)?;
                let dy1 = parse_svg_number(data, &mut pos)?;
                let dx2 = parse_svg_number(data, &mut pos)?;
                let dy2 = parse_svg_number(data, &mut pos)?;
                let dx = parse_svg_number(data, &mut pos)?;
                let dy = parse_svg_number(data, &mut pos)?;
                let x1 = current_x + dx1;
                let y1 = current_y + dy1;
                let x2 = current_x + dx2;
                let y2 = current_y + dy2;
                let x = current_x + dx;
                let y = current_y + dy;

                current_x = x;
                current_y = y;

                push_command(
                    path,
                    SvgCommand::CubicTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x,
                        y,
                    },
                )?;
            }
            b'Z' | b'z' => {
                current_x = subpath_start_x;
                current_y = subpath_start_y;

                push_command(path, SvgCommand::Close)?;
            }
            _ => {
                return Err(SvgError::InvalidCommand(cmd));
            }
        }
    }

    if path.num_commands == 0 {
        return Err(SvgError::EmptyData);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Flatten: SvgPath → line segments in fixed-point pixel coordinates
// ---------------------------------------------------------------------------

/// Convert SVG coordinate to fixed-point pixel coordinate.
///
/// `scale` is a 20.12 fixed-point multiplier (SVG_FP_ONE = 1× scale).
/// `offset` is already in fixed-point.
///
/// Since `scale` carries the 12-bit fractional shift, `coord * scale` directly
/// produces a fixed-point result. Example: coord=10, scale=4096 (1×) →
/// 10 * 4096 = 40960 = 10.0 in 20.12 FP. coord=10, scale=8192 (2×) →
/// 10 * 8192 = 81920 = 20.0 in 20.12 FP.
///
/// # Overflow safety
///
/// The multiplication is performed in i64 to avoid intermediate overflow,
/// then truncated to i32. The result is valid as long as
/// `coord * scale + offset` fits in i32 (±2^31). At 1× scale (4096),
/// coordinates up to ~524,287 are safe. At 8× scale (32768), coordinates
/// up to ~65,535 are safe. The addition of `offset` can also overflow if
/// both the product and offset are near the i32 boundary. In practice,
/// SVG icons in this system use coordinates < 100 and scales ≤ 8×, so
/// overflow is not a concern. Callers rendering very large SVGs at high
/// scale factors should validate coordinate bounds first.
fn svg_coord_to_fp(coord: i32, scale: i32, offset: i32) -> i32 {
    // Widening to i64 prevents overflow during multiplication; the final
    // cast + add assume the result fits in i32 (see doc above).
    (coord as i64 * scale as i64) as i32 + offset
}
fn svg_emit_segment(
    scratch: &mut SvgRasterScratch,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
) -> Result<(), SvgError> {
    if y0 == y1 {
        return Ok(()); // Horizontal lines don't affect scanline fill.
    }
    if scratch.num_segments >= SVG_MAX_SEGMENTS {
        return Err(SvgError::TooManySegments);
    }

    scratch.segments[scratch.num_segments] = SvgSegment { x0, y0, x1, y1 };
    scratch.num_segments += 1;

    Ok(())
}
/// Recursively flatten a cubic Bezier (p0, c1, c2, p3) into line segments.
///
/// Uses De Casteljau subdivision. Stops when the control points are close
/// enough to the chord (flatness test) or at max recursion depth.
fn svg_flatten_cubic(
    scratch: &mut SvgRasterScratch,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    x3: i32,
    y3: i32,
    depth: u32,
) -> Result<(), SvgError> {
    if scratch.num_segments >= SVG_MAX_SEGMENTS {
        return Err(SvgError::TooManySegments);
    }

    // Flatness test: max distance from control points to the chord (p0→p3).
    // We use a simplified test: if both control points are within 0.5 pixel
    // of the line from p0 to p3, the curve is flat enough.
    let threshold = (SVG_FP_ONE / 2) as i64 * (SVG_FP_ONE / 2) as i64;
    // Distance from control point (x1,y1) to the midpoint of chord.
    let mx = ((x0 as i64 + x3 as i64) / 2) as i32;
    let my = ((y0 as i64 + y3 as i64) / 2) as i32;
    let d1x = x1 as i64 - mx as i64;
    let d1y = y1 as i64 - my as i64;
    let d2x = x2 as i64 - mx as i64;
    let d2y = y2 as i64 - my as i64;
    let dist1_sq = d1x * d1x + d1y * d1y;
    let dist2_sq = d2x * d2x + d2y * d2y;

    if depth >= 10 || (dist1_sq <= threshold && dist2_sq <= threshold) {
        // Flat enough — emit line segment from p0 to p3.
        svg_emit_segment(scratch, x0, y0, x3, y3)?;

        return Ok(());
    }

    // De Casteljau split at t=0.5.
    let q0x = ((x0 as i64 + x1 as i64) / 2) as i32;
    let q0y = ((y0 as i64 + y1 as i64) / 2) as i32;
    let q1x = ((x1 as i64 + x2 as i64) / 2) as i32;
    let q1y = ((y1 as i64 + y2 as i64) / 2) as i32;
    let q2x = ((x2 as i64 + x3 as i64) / 2) as i32;
    let q2y = ((y2 as i64 + y3 as i64) / 2) as i32;
    let r0x = ((q0x as i64 + q1x as i64) / 2) as i32;
    let r0y = ((q0y as i64 + q1y as i64) / 2) as i32;
    let r1x = ((q1x as i64 + q2x as i64) / 2) as i32;
    let r1y = ((q1y as i64 + q2y as i64) / 2) as i32;
    let sx = ((r0x as i64 + r1x as i64) / 2) as i32;
    let sy = ((r0y as i64 + r1y as i64) / 2) as i32;

    svg_flatten_cubic(scratch, x0, y0, q0x, q0y, r0x, r0y, sx, sy, depth + 1)?;
    svg_flatten_cubic(scratch, sx, sy, r1x, r1y, q2x, q2y, x3, y3, depth + 1)?;

    Ok(())
}
/// Convert an SvgPath to line segments in fixed-point pixel coordinates,
/// suitable for scanline rasterization.
///
/// `scale` is a 20.12 fixed-point scaling factor. For example, to render
/// path coordinates at 1:1, use `SVG_FP_ONE` (4096). To scale up 2×, use
/// `SVG_FP_ONE * 2` (8192).
///
/// `offset_x` and `offset_y` are pixel offsets (in fixed-point) applied
/// after scaling.
fn svg_flatten_path(
    path: &SvgPath,
    scratch: &mut SvgRasterScratch,
    scale: i32,
    offset_x: i32,
    offset_y: i32,
) -> Result<(), SvgError> {
    scratch.num_segments = 0;

    let mut cur_x: i32 = 0; // fixed-point
    let mut cur_y: i32 = 0;
    let mut subpath_x: i32 = 0;
    let mut subpath_y: i32 = 0;

    for i in 0..path.num_commands {
        match path.commands[i] {
            SvgCommand::MoveTo { x, y } => {
                cur_x = svg_coord_to_fp(x, scale, offset_x);
                cur_y = svg_coord_to_fp(y, scale, offset_y);
                subpath_x = cur_x;
                subpath_y = cur_y;
            }
            SvgCommand::LineTo { x, y } => {
                let nx = svg_coord_to_fp(x, scale, offset_x);
                let ny = svg_coord_to_fp(y, scale, offset_y);

                svg_emit_segment(scratch, cur_x, cur_y, nx, ny)?;

                cur_x = nx;
                cur_y = ny;
            }
            SvgCommand::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let cx1 = svg_coord_to_fp(x1, scale, offset_x);
                let cy1 = svg_coord_to_fp(y1, scale, offset_y);
                let cx2 = svg_coord_to_fp(x2, scale, offset_x);
                let cy2 = svg_coord_to_fp(y2, scale, offset_y);
                let ex = svg_coord_to_fp(x, scale, offset_x);
                let ey = svg_coord_to_fp(y, scale, offset_y);

                svg_flatten_cubic(scratch, cur_x, cur_y, cx1, cy1, cx2, cy2, ex, ey, 0)?;

                cur_x = ex;
                cur_y = ey;
            }
            SvgCommand::Close => {
                if cur_x != subpath_x || cur_y != subpath_y {
                    svg_emit_segment(scratch, cur_x, cur_y, subpath_x, subpath_y)?;
                }

                cur_x = subpath_x;
                cur_y = subpath_y;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Rasterizer: line segments → coverage map
// ---------------------------------------------------------------------------

/// Add coverage for a horizontal span within one sub-scanline.
fn svg_fill_coverage_span(
    coverage: &mut [u8],
    width: u32,
    row: u32,
    x_start_fp: i32,
    x_end_fp: i32,
    oversample: i32,
) {
    let contribution = (256 / oversample) as u16;
    let px_start = x_start_fp >> SVG_FP_SHIFT;
    let px_end = (x_end_fp + SVG_FP_ONE - 1) >> SVG_FP_SHIFT;
    let px_start = if px_start < 0 { 0 } else { px_start as u32 };
    let px_end = if px_end < 0 {
        return;
    } else if (px_end as u32) > width {
        width
    } else {
        px_end as u32
    };
    let row_start = (row * width) as usize;

    for px in px_start..px_end {
        let idx = row_start + px as usize;

        if idx < coverage.len() {
            let cov = if px as i32 == (x_start_fp >> SVG_FP_SHIFT)
                && px as i32 == ((x_end_fp - 1) >> SVG_FP_SHIFT)
            {
                let frac = x_end_fp - x_start_fp;

                (contribution as i32 * frac / SVG_FP_ONE) as u16
            } else if px as i32 == (x_start_fp >> SVG_FP_SHIFT) {
                let right_edge = ((px + 1) as i32) << SVG_FP_SHIFT;
                let frac = right_edge - x_start_fp;

                (contribution as i32 * frac / SVG_FP_ONE) as u16
            } else if px as i32 == ((x_end_fp - 1) >> SVG_FP_SHIFT) {
                let left_edge = (px as i32) << SVG_FP_SHIFT;
                let frac = x_end_fp - left_edge;

                (contribution as i32 * frac / SVG_FP_ONE) as u16
            } else {
                contribution
            };

            let val = coverage[idx] as u16 + cov;

            coverage[idx] = if val > 255 { 255 } else { val as u8 };
        }
    }
}
/// Scanline rasterizer for SVG segments — non-zero winding rule with
/// 4× vertical oversampling. Same algorithm as the TrueType rasterizer.
fn svg_rasterize_segments(
    scratch: &SvgRasterScratch,
    coverage: &mut [u8],
    width: u32,
    height: u32,
) {
    let nseg = scratch.num_segments;

    if nseg == 0 {
        return;
    }

    let mut active: [SvgActiveEdge; SVG_MAX_ACTIVE] =
        [SvgActiveEdge { x: 0, direction: 0 }; SVG_MAX_ACTIVE];
    let mut num_active: usize;

    for row in 0..height {
        let y_top_fp = row as i32 * SVG_FP_ONE;
        let sub_step = SVG_FP_ONE / OVERSAMPLE_Y;

        for sub in 0..OVERSAMPLE_Y {
            let scan_y = y_top_fp + sub * sub_step + sub_step / 2;

            // Build active edge list for this sub-scanline.
            num_active = 0;

            for si in 0..nseg {
                let seg = &scratch.segments[si];
                let (y_top, y_bot, x_top, x_bot, dir) = if seg.y0 < seg.y1 {
                    (seg.y0, seg.y1, seg.x0, seg.x1, 1i32)
                } else {
                    (seg.y1, seg.y0, seg.x1, seg.x0, -1i32)
                };

                if y_top > scan_y || y_bot <= scan_y {
                    continue;
                }
                if num_active >= SVG_MAX_ACTIVE {
                    break;
                }

                let dy = y_bot - y_top;
                let t = scan_y - y_top;
                let x = if dy == 0 {
                    x_top
                } else {
                    x_top + ((x_bot - x_top) as i64 * t as i64 / dy as i64) as i32
                };

                active[num_active] = SvgActiveEdge { x, direction: dir };
                num_active += 1;
            }

            // Sort active edges by x (insertion sort).
            for i in 1..num_active {
                let key = active[i];
                let mut j = i;

                while j > 0 && active[j - 1].x > key.x {
                    active[j] = active[j - 1];
                    j -= 1;
                }

                active[j] = key;
            }

            // Non-zero winding rule fill.
            let mut winding: i32 = 0;
            let mut edge_idx = 0;

            while edge_idx < num_active {
                let old_winding = winding;

                winding += active[edge_idx].direction;

                if old_winding == 0 && winding != 0 {
                    let x_start = active[edge_idx].x;
                    let mut ei = edge_idx + 1;

                    while ei < num_active {
                        winding += active[ei].direction;

                        if winding == 0 {
                            let x_end = active[ei].x;

                            svg_fill_coverage_span(
                                coverage,
                                width,
                                row,
                                x_start,
                                x_end,
                                OVERSAMPLE_Y,
                            );

                            edge_idx = ei + 1;

                            break;
                        }
                        ei += 1;
                    }
                    if winding != 0 {
                        break;
                    }
                } else {
                    edge_idx += 1;
                }
            }
        }
    }
}

/// Rasterize an SVG path into a coverage map (0–255 per pixel).
///
/// The coverage map has dimensions `width × height`. Each byte represents
/// the alpha coverage of that pixel. The path is scaled by `scale` (a 20.12
/// fixed-point factor) and offset by (`offset_x`, `offset_y`) in pixels.
///
/// Uses non-zero winding rule with 4× vertical oversampling for antialiasing.
pub fn svg_rasterize(
    path: &SvgPath,
    scratch: &mut SvgRasterScratch,
    coverage: &mut [u8],
    width: u32,
    height: u32,
    scale: i32,
    offset_x: i32,
    offset_y: i32,
) -> Result<(), SvgError> {
    // Clear coverage map.
    let total = (width * height) as usize;

    for i in 0..total {
        if i < coverage.len() {
            coverage[i] = 0;
        }
    }

    // Convert offsets to fixed-point.
    let fp_offset_x = offset_x * SVG_FP_ONE;
    let fp_offset_y = offset_y * SVG_FP_ONE;

    // Flatten path into line segments.
    svg_flatten_path(path, scratch, scale, fp_offset_x, fp_offset_y)?;
    // Scanline rasterization with vertical oversampling.
    svg_rasterize_segments(scratch, coverage, width, height);

    Ok(())
}
