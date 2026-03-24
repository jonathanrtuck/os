//! Per-content-type rendering: image, glyph, and path content dispatch,
//! plus `mask_rounded_rect` for corner-radius clipping.
//!
//! Dependencies: `drawing`, `scene`, `fonts`, and the sibling `coords` and
//! `path_raster` modules.

use drawing::{isqrt_fp, Surface};
use fonts::cache::GlyphCache;
use scene::{Content, Node, ShapedGlyph};

use super::{
    coords::round_f32,
    path_raster::{render_path, render_path_data, scene_to_draw_color},
    RenderCtx, SceneGraph,
};
use crate::LruRasterizer;

/// Render a node's content (path, glyphs, or image) into the target surface.
///
/// `draw_x`, `draw_y` are the node's origin in the target surface.
/// `nw`, `nh` are the node's physical pixel dimensions.
/// `lru` is the optional LRU rasterizer for non-ASCII glyphs.
pub(super) fn render_content(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    node: &Node,
    draw_x: i32,
    draw_y: i32,
    nw: i32,
    nh: i32,
    lru: Option<&mut LruRasterizer>,
) {
    let scale = ctx.scale;
    match node.content {
        Content::None => {}
        Content::Path {
            color,
            fill_rule,
            stroke_width,
            contours,
        } => {
            if stroke_width > 0 {
                // Expand stroked path to filled geometry, then rasterize.
                let data =
                    if (contours.offset as usize + contours.length as usize) <= graph.data.len() {
                        &graph.data[contours.offset as usize..][..contours.length as usize]
                    } else {
                        return;
                    };
                // Decode 8.8 fixed-point stroke width to f32 points.
                let sw_pt = stroke_width as f32 / 256.0;
                let expanded = scene::stroke::expand_stroke(data, sw_pt);
                if !expanded.is_empty() {
                    render_path_data(
                        fb,
                        &expanded,
                        scale,
                        color,
                        scene::FillRule::Winding,
                        draw_x,
                        draw_y,
                        nw,
                        nh,
                    );
                }
            } else {
                render_path(
                    fb, graph, scale, contours, color, fill_rule, draw_x, draw_y, nw, nh,
                );
            }
        }
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            font_size,
            axis_hash,
        } => {
            // Select glyph cache by font identity (scene::FONT_MONO = 0,
            // scene::FONT_SANS = 1). Non-mono fonts use the prop_cache.
            let cache = if axis_hash != scene::FONT_MONO {
                ctx.prop_cache
            } else {
                ctx.mono_cache
            };
            render_glyphs(
                fb,
                graph,
                cache,
                scale,
                color,
                glyphs,
                glyph_count,
                draw_x,
                draw_y,
                font_size,
                axis_hash,
                ctx.font_size_px,
                lru,
            );
        }
        Content::InlineImage {
            data,
            src_width,
            src_height,
        } => {
            render_image(
                fb, graph, data, src_width, src_height, draw_x, draw_y, nw, nh,
            );
        }
        Content::Image { .. } => {
            // Content Region image: resolved when render services wire up Content Region.
        }
    }
}

/// Render shaped glyphs from the data buffer onto the surface.
///
/// First tries the fixed ASCII cache (fast path). On miss, falls back
/// to the LRU cache. On LRU miss, rasterizes the glyph on demand,
/// inserts it into the LRU cache, and renders it.
fn render_glyphs(
    fb: &mut Surface,
    graph: &SceneGraph,
    cache: &GlyphCache,
    scale: f32,
    color: scene::Color,
    glyphs: scene::DataRef,
    glyph_count: u16,
    draw_x: i32,
    draw_y: i32,
    font_size: u16,
    axis_hash: u32,
    font_size_px: u16,
    mut lru: Option<&mut LruRasterizer>,
) {
    // Read ShapedGlyph array from the data buffer.
    let glyph_size = core::mem::size_of::<ShapedGlyph>();
    let shaped_glyphs: &[ShapedGlyph] = if glyphs.length > 0
        && (glyphs.offset as usize + glyphs.length as usize) <= graph.data.len()
        && glyphs.length as usize >= glyph_size
    {
        let bytes = &graph.data[glyphs.offset as usize..][..glyphs.length as usize];
        let count = (glyph_count as usize).min(bytes.len() / glyph_size);
        // SAFETY: ShapedGlyph is #[repr(C)], data buffer is aligned
        // by push_shaped_glyphs to ShapedGlyph alignment.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    } else {
        &[]
    };
    let glyph_color = scene_to_draw_color(color);

    // Accumulate pen position in f32 pixel space to preserve fractional
    // advances. Snap to integer pixels only for actual drawing. This
    // matches the GPU backends and prevents inter-glyph drift.
    let mut pen_x = draw_x as f32;

    // Use the node's font_size from Content::Glyphs if non-zero,
    // otherwise fall back to the backend's physical font size.
    let lru_font_size = if font_size > 0 {
        font_size
    } else {
        font_size_px
    };
    let lru_axis_hash = axis_hash;

    for sg in shaped_glyphs {
        let cx = round_f32(pen_x);

        // Fast path: fixed ASCII cache.
        if let Some((glyph, coverage)) = cache.get(sg.glyph_id) {
            let px = cx + glyph.bearing_x;
            let py = draw_y + (cache.ascent as i32 - glyph.bearing_y);

            fb.draw_coverage(px, py, coverage, glyph.width, glyph.height, glyph_color);
        } else if let Some(lru) = lru.as_deref_mut() {
            // LRU fallback: check cache hit first, then rasterize on demand.
            if lru
                .cache
                .get_with_axes(sg.glyph_id, lru_font_size, lru_axis_hash)
                .is_none()
            {
                // Cache miss — rasterize on demand and insert into LRU.
                let _ = lru.rasterize_and_get(sg.glyph_id, lru_font_size, lru_axis_hash);
            }
            // Now fetch from LRU (either was already there, or just inserted).
            if let Some(g) = lru
                .cache
                .get_with_axes(sg.glyph_id, lru_font_size, lru_axis_hash)
            {
                let px = cx + g.bearing_x;
                let py = draw_y + (cache.ascent as i32 - g.bearing_y);
                fb.draw_coverage(px, py, &g.coverage, g.width, g.height, glyph_color);
            }
        }

        // x_advance is 16.16 fixed-point points. Convert to f32 points,
        // then scale to pixels. Accumulate in float to avoid drift.
        pen_x += sg.x_advance as f32 / 65536.0 * scale;
    }
}

/// Render an image from the data buffer onto the surface.
fn render_image(
    fb: &mut Surface,
    graph: &SceneGraph,
    data: scene::DataRef,
    src_width: u16,
    src_height: u16,
    draw_x: i32,
    draw_y: i32,
    nw: i32,
    nh: i32,
) {
    if data.length == 0 || (data.offset as usize + data.length as usize) > graph.data.len() {
        return;
    }
    let pixels = &graph.data[data.offset as usize..][..data.length as usize];
    let src_stride = src_width as u32 * 4;

    // When source dimensions differ from the node's display size,
    // use bilinear resampling for smooth scaling instead of nearest-
    // neighbor. This produces blended gray for downscaled checker-
    // boards instead of aliased black/white.
    let phys_nw = nw.max(0) as u32;
    let phys_nh = nh.max(0) as u32;
    if phys_nw > 0 && phys_nh > 0 && (src_width as u32 != phys_nw || src_height as u32 != phys_nh) {
        let inv_a = src_width as f32 / phys_nw as f32;
        let inv_d = src_height as f32 / phys_nh as f32;
        let inv_tx = (inv_a - 1.0) * 0.5;
        let inv_ty = (inv_d - 1.0) * 0.5;

        fb.blit_transformed_bilinear(
            pixels,
            src_width as u32,
            src_height as u32,
            src_stride,
            draw_x,
            draw_y,
            phys_nw,
            phys_nh,
            inv_a,
            0.0,
            0.0,
            inv_d,
            inv_tx,
            inv_ty,
            255,
        );
    } else {
        fb.blit_blend(
            pixels,
            src_width as u32,
            src_height as u32,
            src_stride,
            draw_x as u32,
            draw_y as u32,
        );
    }
}

/// Mask an BGRA pixel buffer to a rounded rectangle shape.
///
/// Pixels outside the rounded boundary are set to fully transparent.
/// Pixels on the arc edge get fractional alpha for anti-aliasing.
/// Uses the same fixed-point SDF approach as drawing::fill_rounded_rect.
pub(super) fn mask_rounded_rect(buf: &mut [u8], w: u32, h: u32, stride: u32, radius: u32) {
    if w == 0 || h == 0 || radius == 0 {
        return;
    }

    let max_r = if w < h { w } else { h } / 2;
    let r = if radius < max_r { radius } else { max_r };
    if r == 0 {
        return;
    }

    // Only corner arc rows need masking (first `r` rows and last `r` rows).
    // Interior rows are fully inside the rounded rect.
    for arc_row in 0..r {
        let dy_fp: i64 = (r as i64 * 256) - (arc_row as i64 * 256) - 128;
        let dy_sq = (dy_fp * dy_fp) as u64;
        let r_sq = (r as u64 * 256) * (r as u64 * 256);
        let x_arc_sq = if r_sq > dy_sq { r_sq - dy_sq } else { 0 };
        let x_arc_fp = isqrt_fp(x_arc_sq);

        let x_arc_int = (x_arc_fp >> 8) as u32;
        let x_arc_frac = (x_arc_fp & 0xFF) as u32; // 0..255

        let left_solid = r - x_arc_int;
        let right_solid = w - r + x_arc_int;

        let rows: [u32; 2] = [arc_row, h - 1 - arc_row];
        for &py in &rows {
            if py >= h {
                continue;
            }
            let row_off = (py * stride) as usize;

            // Clear pixels to the left of the arc (outside the rounded corner).
            let clear_end = if left_solid > 0 {
                if x_arc_frac > 0 {
                    left_solid - 1
                } else {
                    left_solid
                }
            } else {
                0
            };
            for px in 0..clear_end {
                let off = row_off + (px * 4) as usize;
                if off + 4 <= buf.len() {
                    buf[off] = 0;
                    buf[off + 1] = 0;
                    buf[off + 2] = 0;
                    buf[off + 3] = 0;
                }
            }

            // Left AA pixel: scale alpha by coverage.
            if left_solid > 0 && x_arc_frac > 0 {
                let lx = left_solid - 1;
                let off = row_off + (lx * 4) as usize;
                if off + 4 <= buf.len() {
                    let orig_a = buf[off + 3] as u32;
                    let new_a = (orig_a * x_arc_frac) >> 8;
                    buf[off + 3] = if new_a > 255 { 255 } else { new_a as u8 };
                }
            }

            // Clear pixels to the right of the arc.
            let right_clear_start = if x_arc_frac > 0 {
                right_solid + 1
            } else {
                right_solid
            };
            for px in right_clear_start..w {
                let off = row_off + (px * 4) as usize;
                if off + 4 <= buf.len() {
                    buf[off] = 0;
                    buf[off + 1] = 0;
                    buf[off + 2] = 0;
                    buf[off + 3] = 0;
                }
            }

            // Right AA pixel.
            if right_solid < w && x_arc_frac > 0 {
                let rx = right_solid;
                let off = row_off + (rx * 4) as usize;
                if off + 4 <= buf.len() {
                    let orig_a = buf[off + 3] as u32;
                    let new_a = (orig_a * x_arc_frac) >> 8;
                    buf[off + 3] = if new_a > 255 { 255 } else { new_a as u8 };
                }
            }
        }
    }
}
