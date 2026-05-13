//! Minimal path-to-coverage rasterizer for Content::Path nodes.
//!
//! Parses scene path commands (MoveTo, LineTo, CubicTo, Close),
//! flattens cubics via recursive subdivision, then fills using a
//! scanline sweep with 4× vertical oversampling for anti-aliasing.
//!
//! Output: 8bpp coverage buffer suitable for atlas upload and
//! rendering via the glyph pipeline (alpha × vertex color).

extern crate alloc;

use alloc::vec::Vec;

use scene::{
    FillRule, PATH_CLOSE, PATH_CLOSE_SIZE, PATH_CUBIC_TO, PATH_CUBIC_TO_SIZE, PATH_LINE_TO,
    PATH_LINE_TO_SIZE, PATH_MOVE_TO, PATH_MOVE_TO_SIZE,
};

const MAX_SEGMENTS: usize = 2048;
const OVERSAMPLE: i32 = 4;
const FP_SHIFT: i32 = 8;
const FP_ONE: i32 = 1 << FP_SHIFT;

struct Seg {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

fn f32_to_fp(v: f32) -> i32 {
    (v * FP_ONE as f32) as i32
}

fn read_f32(data: &[u8], off: usize) -> f32 {
    if off + 4 > data.len() {
        return 0.0;
    }

    f32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    if off + 4 > data.len() {
        return u32::MAX;
    }

    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic(
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    segs: &mut Vec<Seg>,
    cx: &mut i32,
    cy: &mut i32,
    depth: u32,
) {
    if depth > 6 || segs.len() >= MAX_SEGMENTS {
        let nx = f32_to_fp(x3);
        let ny = f32_to_fp(y3);

        segs.push(Seg {
            x0: *cx,
            y0: *cy,
            x1: nx,
            y1: ny,
        });

        *cx = nx;
        *cy = ny;

        return;
    }

    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x3) * dy - (c1y - y3) * dx).abs();
    let d2 = ((c2x - x3) * dy - (c2y - y3) * dx).abs();

    if (d1 + d2) * (d1 + d2) < 0.25 * (dx * dx + dy * dy) {
        let nx = f32_to_fp(x3);
        let ny = f32_to_fp(y3);

        segs.push(Seg {
            x0: *cx,
            y0: *cy,
            x1: nx,
            y1: ny,
        });

        *cx = nx;
        *cy = ny;

        return;
    }

    let m01x = (x0 + c1x) * 0.5;
    let m01y = (y0 + c1y) * 0.5;
    let m12x = (c1x + c2x) * 0.5;
    let m12y = (c1y + c2y) * 0.5;
    let m23x = (c2x + x3) * 0.5;
    let m23y = (c2y + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;

    flatten_cubic(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        segs,
        cx,
        cy,
        depth + 1,
    );
    flatten_cubic(
        mx,
        my,
        m123x,
        m123y,
        m23x,
        m23y,
        x3,
        y3,
        segs,
        cx,
        cy,
        depth + 1,
    );
}

pub fn flatten_to_buffer(
    path_data: &[u8],
    scale: f32,
    stroke_data: Option<&[u8]>,
    buf: &mut [u8],
) -> usize {
    let data = stroke_data.unwrap_or(path_data);
    let max_segs = buf.len() / 16;
    let mut count = 0usize;
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    let mut start_x = 0.0f32;
    let mut start_y = 0.0f32;
    let mut off = 0;

    while off < data.len() && count < max_segs.min(MAX_SEGMENTS) {
        let tag = read_u32(data, off);

        match tag {
            PATH_MOVE_TO => {
                if off + PATH_MOVE_TO_SIZE > data.len() {
                    break;
                }

                cx = read_f32(data, off + 4) * scale;
                cy = read_f32(data, off + 8) * scale;
                start_x = cx;
                start_y = cy;
                off += PATH_MOVE_TO_SIZE;
            }
            PATH_LINE_TO => {
                if off + PATH_LINE_TO_SIZE > data.len() {
                    break;
                }

                let nx = read_f32(data, off + 4) * scale;
                let ny = read_f32(data, off + 8) * scale;
                let base = count * 16;

                buf[base..base + 4].copy_from_slice(&cx.to_le_bytes());
                buf[base + 4..base + 8].copy_from_slice(&cy.to_le_bytes());
                buf[base + 8..base + 12].copy_from_slice(&nx.to_le_bytes());
                buf[base + 12..base + 16].copy_from_slice(&ny.to_le_bytes());

                cx = nx;
                cy = ny;
                count += 1;
                off += PATH_LINE_TO_SIZE;
            }
            PATH_CUBIC_TO => {
                if off + PATH_CUBIC_TO_SIZE > data.len() {
                    break;
                }

                let c1x = read_f32(data, off + 4) * scale;
                let c1y = read_f32(data, off + 8) * scale;
                let c2x = read_f32(data, off + 12) * scale;
                let c2y = read_f32(data, off + 16) * scale;
                let x3 = read_f32(data, off + 20) * scale;
                let y3 = read_f32(data, off + 24) * scale;

                flatten_cubic_f32(
                    cx, cy, c1x, c1y, c2x, c2y, x3, y3, buf, &mut count, max_segs, 0,
                );

                cx = x3;
                cy = y3;
                off += PATH_CUBIC_TO_SIZE;
            }
            PATH_CLOSE => {
                if (cx - start_x).abs() > 0.001 || (cy - start_y).abs() > 0.001 {
                    if count < max_segs {
                        let base = count * 16;
                        buf[base..base + 4].copy_from_slice(&cx.to_le_bytes());
                        buf[base + 4..base + 8].copy_from_slice(&cy.to_le_bytes());
                        buf[base + 8..base + 12].copy_from_slice(&start_x.to_le_bytes());
                        buf[base + 12..base + 16].copy_from_slice(&start_y.to_le_bytes());
                        count += 1;
                    }

                    cx = start_x;
                    cy = start_y;
                }

                off += PATH_CLOSE_SIZE;
            }
            _ => break,
        }
    }

    count
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic_f32(
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    buf: &mut [u8],
    count: &mut usize,
    max_segs: usize,
    depth: u32,
) {
    if depth > 6 || *count >= max_segs {
        let base = (*count).min(max_segs.saturating_sub(1)) * 16;

        if *count < max_segs {
            buf[base..base + 4].copy_from_slice(&x0.to_le_bytes());
            buf[base + 4..base + 8].copy_from_slice(&y0.to_le_bytes());
            buf[base + 8..base + 12].copy_from_slice(&x3.to_le_bytes());
            buf[base + 12..base + 16].copy_from_slice(&y3.to_le_bytes());
            *count += 1;
        }

        return;
    }

    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x3) * dy - (c1y - y3) * dx).abs();
    let d2 = ((c2x - x3) * dy - (c2y - y3) * dx).abs();

    if (d1 + d2) * (d1 + d2) < 0.25 * (dx * dx + dy * dy) {
        if *count < max_segs {
            let base = *count * 16;

            buf[base..base + 4].copy_from_slice(&x0.to_le_bytes());
            buf[base + 4..base + 8].copy_from_slice(&y0.to_le_bytes());
            buf[base + 8..base + 12].copy_from_slice(&x3.to_le_bytes());
            buf[base + 12..base + 16].copy_from_slice(&y3.to_le_bytes());

            *count += 1;
        }

        return;
    }

    let m01x = (x0 + c1x) * 0.5;
    let m01y = (y0 + c1y) * 0.5;
    let m12x = (c1x + c2x) * 0.5;
    let m12y = (c1y + c2y) * 0.5;
    let m23x = (c2x + x3) * 0.5;
    let m23y = (c2y + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;

    flatten_cubic_f32(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        buf,
        count,
        max_segs,
        depth + 1,
    );
    flatten_cubic_f32(
        mx,
        my,
        m123x,
        m123y,
        m23x,
        m23y,
        x3,
        y3,
        buf,
        count,
        max_segs,
        depth + 1,
    );
}

pub fn rasterize_path(
    path_data: &[u8],
    width: u32,
    height: u32,
    scale: f32,
    fill_rule: FillRule,
    stroke_data: Option<&[u8]>,
) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let data = stroke_data.unwrap_or(path_data);
    let mut segs = Vec::with_capacity(256);
    let mut cx = 0i32;
    let mut cy = 0i32;
    let mut start_x = 0i32;
    let mut start_y = 0i32;
    let mut off = 0;

    while off < data.len() && segs.len() < MAX_SEGMENTS {
        let tag = read_u32(data, off);

        match tag {
            PATH_MOVE_TO => {
                if off + PATH_MOVE_TO_SIZE > data.len() {
                    break;
                }

                let x = read_f32(data, off + 4) * scale;
                let y = read_f32(data, off + 8) * scale;

                cx = f32_to_fp(x);
                cy = f32_to_fp(y);
                start_x = cx;
                start_y = cy;
                off += PATH_MOVE_TO_SIZE;
            }
            PATH_LINE_TO => {
                if off + PATH_LINE_TO_SIZE > data.len() {
                    break;
                }

                let x = read_f32(data, off + 4) * scale;
                let y = read_f32(data, off + 8) * scale;
                let nx = f32_to_fp(x);
                let ny = f32_to_fp(y);

                segs.push(Seg {
                    x0: cx,
                    y0: cy,
                    x1: nx,
                    y1: ny,
                });

                cx = nx;
                cy = ny;
                off += PATH_LINE_TO_SIZE;
            }
            PATH_CUBIC_TO => {
                if off + PATH_CUBIC_TO_SIZE > data.len() {
                    break;
                }

                let c1x = read_f32(data, off + 4) * scale;
                let c1y = read_f32(data, off + 8) * scale;
                let c2x = read_f32(data, off + 12) * scale;
                let c2y = read_f32(data, off + 16) * scale;
                let x = read_f32(data, off + 20) * scale;
                let y = read_f32(data, off + 24) * scale;
                let fx = cx as f32 / FP_ONE as f32;
                let fy = cy as f32 / FP_ONE as f32;

                flatten_cubic(
                    fx, fy, c1x, c1y, c2x, c2y, x, y, &mut segs, &mut cx, &mut cy, 0,
                );

                off += PATH_CUBIC_TO_SIZE;
            }
            PATH_CLOSE => {
                if (cx, cy) != (start_x, start_y) {
                    segs.push(Seg {
                        x0: cx,
                        y0: cy,
                        x1: start_x,
                        y1: start_y,
                    });

                    cx = start_x;
                    cy = start_y;
                }

                off += PATH_CLOSE_SIZE;
            }
            _ => break,
        }
    }

    if segs.is_empty() {
        return Vec::new();
    }

    let w = width as usize;
    let h = height as usize;
    let oh = h * OVERSAMPLE as usize;
    let mut accum = alloc::vec![0i16; w * oh];

    for seg in &segs {
        let (mut y0, mut x0, mut y1, mut x1) = (seg.y0, seg.x0, seg.y1, seg.x1);

        y0 = y0 * OVERSAMPLE / FP_ONE;
        y1 = y1 * OVERSAMPLE / FP_ONE;
        x0 = x0 * 256 / FP_ONE;
        x1 = x1 * 256 / FP_ONE;

        if y0 == y1 {
            continue;
        }

        let (dir, sy, ey, sx, ex) = if y0 < y1 {
            (1i16, y0, y1, x0, x1)
        } else {
            (-1i16, y1, y0, x1, x0)
        };
        let sy = sy.max(0);
        let ey = ey.min(oh as i32);

        if sy >= ey {
            continue;
        }

        let dy = ey - sy;

        for row in sy..ey {
            let t = row - sy;
            let x = sx + (ex - sx) * t / dy;
            let xi = x / 256;
            let frac = x & 255;

            if xi >= 0 && (xi as usize) < w {
                accum[row as usize * w + xi as usize] += dir * (256 - frac) as i16;
            }
            if xi + 1 >= 0 && ((xi + 1) as usize) < w {
                accum[row as usize * w + (xi + 1) as usize] += dir * frac as i16;
            }
        }
    }

    let mut coverage = alloc::vec![0u8; w * h];

    for py in 0..h {
        for sub in 0..OVERSAMPLE as usize {
            let row = py * OVERSAMPLE as usize + sub;

            if row >= oh {
                break;
            }

            let mut winding = 0i32;

            for px in 0..w {
                winding += accum[row * w + px] as i32;

                let cov = match fill_rule {
                    FillRule::Winding => (winding.abs().min(256) * 255 / 256) as u32,
                    FillRule::EvenOdd => {
                        let v = winding.abs() % 512;
                        let c = if v > 256 { 512 - v } else { v };

                        (c.min(256) * 255 / 256) as u32
                    }
                };
                let existing = coverage[py * w + px] as u32;
                let blended = existing + cov / OVERSAMPLE as u32;

                coverage[py * w + px] = blended.min(255) as u8;
            }
        }
    }

    coverage
}
