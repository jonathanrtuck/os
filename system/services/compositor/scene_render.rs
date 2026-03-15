//! Scene graph renderer: walks a tree of `scene::Node` and draws to a Surface.
//!
//! Text rendering uses shaped glyph arrays from the scene graph. Each TextRun
//! stores an array of `ShapedGlyph` in the data buffer. The compositor reads
//! glyph IDs and advances, rasterizes via the glyph cache (pre-populated for
//! monospace ASCII, on-demand via LRU for other glyphs), and composites via
//! `draw_coverage`.

extern crate alloc;

use alloc::vec;

use drawing::{Color, PixelFormat, Surface};
use fonts::cache::GlyphCache;
use scene::{Content, Node, NodeFlags, NodeId, ShapedGlyph, TextRun, NULL};

/// Axis-aligned clip rectangle in absolute (framebuffer) coordinates.
#[derive(Clone, Copy)]
struct ClipRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Rendering context passed through the recursive tree walk.
pub struct RenderCtx<'a> {
    pub mono_cache: &'a GlyphCache,
    pub prop_cache: &'a GlyphCache,
    /// Pre-rasterized icon coverage map (3-channel RGB, or empty).
    pub icon_coverage: &'a [u8],
    pub icon_w: u32,
    pub icon_h: u32,
    pub icon_color: Color,
    /// Node ID where the icon should be drawn (before its text).
    pub icon_node: NodeId,
    /// Fractional display scale factor (1.0, 1.25, 1.5, 2.0, etc.).
    /// Scene graph is in logical coordinates; multiply by this to get
    /// physical pixel positions and sizes. Borders snap to whole physical
    /// pixels (round to nearest).
    pub scale: f32,
}
/// Immutable scene graph data referenced during rendering.
pub struct SceneGraph<'a> {
    pub nodes: &'a [Node],
    pub data: &'a [u8],
}

impl ClipRect {
    fn intersect(self, other: ClipRect) -> Option<ClipRect> {
        let x0 = if self.x > other.x { self.x } else { other.x };
        let y0 = if self.y > other.y { self.y } else { other.y };
        let x1_a = self.x + self.w;
        let x1_b = other.x + other.w;
        let x1 = if x1_a < x1_b { x1_a } else { x1_b };
        let y1_a = self.y + self.h;
        let y1_b = other.y + other.h;
        let y1 = if y1_a < y1_b { y1_a } else { y1_b };

        if x1 > x0 && y1 > y0 {
            Some(ClipRect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            })
        } else {
            None
        }
    }
}

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Manual implementation for `no_std` (where `f32::round()` isn't available
/// without `core_maths`).
#[inline]
fn round_f32(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// Scale a logical coordinate to physical pixels using fractional scale.
///
/// Uses rounding to nearest pixel. This ensures that for integer scale
/// factors (1.0, 2.0), the result is identical to the old integer multiply.
/// For fractional scales, rounding minimises visual error.
#[inline]
fn scale_coord(logical: i32, scale: f32) -> i32 {
    round_f32(logical as f32 * scale)
}

/// Compute the physical pixel size for a logical extent starting at a
/// given logical position, using the gap-free rounding scheme.
///
/// Physical size = round((pos + size) * scale) - round(pos * scale)
///
/// This guarantees that two adjacent nodes at (x, w) and (x+w, w2) share
/// the same physical boundary — no gaps and no overlaps.
#[inline]
fn scale_size(logical_pos: i32, logical_size: i32, scale: f32) -> i32 {
    let phys_start = round_f32(logical_pos as f32 * scale);
    let phys_end = round_f32((logical_pos + logical_size) as f32 * scale);
    phys_end - phys_start
}

/// Snap a logical border width to a whole number of physical pixels.
/// Borders must always be at least 1 physical pixel when the logical
/// width is > 0. Uses round-to-nearest, with a floor of 1.
#[inline]
fn snap_border(logical_width: u32, scale: f32) -> u32 {
    if logical_width == 0 {
        return 0;
    }
    let phys = round_f32(logical_width as f32 * scale);
    if phys <= 0 { 1 } else { phys as u32 }
}

/// Recursively render a node and its children.
///
/// `abs_x`, `abs_y` are the absolute **physical** pixel position of this
/// node's origin in the framebuffer. `clip` is in physical pixels.
/// Scene graph coordinates are logical; scaled by `ctx.scale` (f32).
fn render_node(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    node_id: NodeId,
    abs_x: i32,
    abs_y: i32,
    clip: ClipRect,
) {
    if node_id == NULL || node_id as usize >= graph.nodes.len() {
        return;
    }

    let node = &graph.nodes[node_id as usize];

    if !node.visible() {
        return;
    }

    let s = ctx.scale;
    let nx = abs_x + scale_coord(node.x as i32, s);
    let ny = abs_y + scale_coord(node.y as i32, s);
    let nw = scale_size(node.x as i32, node.width as i32, s);
    let nh = scale_size(node.y as i32, node.height as i32, s);
    let node_rect = ClipRect {
        x: nx,
        y: ny,
        w: nw,
        h: nh,
    };
    let visible = match clip.intersect(node_rect) {
        Some(v) => v,
        None => return,
    };

    // Scale corner radius from logical to physical pixels.
    let phys_radius = if node.corner_radius > 0 {
        let r = round_f32(node.corner_radius as f32 * s);
        if r < 0 { 0u32 } else { r as u32 }
    } else {
        0u32
    };

    // Draw background.
    if node.background.a > 0 {
        let bg = scene_to_draw_color(node.background);

        if phys_radius > 0 {
            // Rounded rectangle background — draw at full node extent,
            // not clipped visible, so the arcs are correct. The surface
            // methods clip to bounds internally.
            fb.fill_rounded_rect_blend(
                nx as u32,
                ny as u32,
                nw as u32,
                nh as u32,
                phys_radius,
                bg,
            );
        } else if bg.a == 255 {
            fb.fill_rect(
                visible.x as u32,
                visible.y as u32,
                visible.w as u32,
                visible.h as u32,
                bg,
            );
        } else {
            fb.fill_rect_blend(
                visible.x as u32,
                visible.y as u32,
                visible.w as u32,
                visible.h as u32,
                bg,
            );
        }
    }

    // Draw border (pixel-snapped to whole physical pixels).
    if node.border.width > 0 && node.border.color.a > 0 {
        let bc = scene_to_draw_color(node.border.color);
        let bw = snap_border(node.border.width as u32, s);

        if phys_radius > 0 {
            // Rounded border: draw outer rounded rect in border color,
            // then inner rounded rect in background color (punch-out).
            // The inner radius is outer_radius - border_width.
            let inner_r = phys_radius.saturating_sub(bw);

            // Outer rounded rect (border color).
            fb.fill_rounded_rect_blend(
                nx as u32,
                ny as u32,
                nw as u32,
                nh as u32,
                phys_radius,
                bc,
            );

            // Inner rounded rect (background color or transparent punch-out).
            // Inset by border width on all sides.
            let inner_x = (nx as u32).saturating_add(bw);
            let inner_y = (ny as u32).saturating_add(bw);
            let inner_w = (nw as u32).saturating_sub(2 * bw);
            let inner_h = (nh as u32).saturating_sub(2 * bw);

            if inner_w > 0 && inner_h > 0 {
                let inner_color = if node.background.a > 0 {
                    scene_to_draw_color(node.background)
                } else {
                    // No background: need to punch out to framebuffer bg.
                    // For now, use transparent which will leave border visible.
                    Color::TRANSPARENT
                };

                if inner_color.a > 0 {
                    fb.fill_rounded_rect_blend(
                        inner_x, inner_y, inner_w, inner_h, inner_r, inner_color,
                    );
                }
            }
        } else {
            // Sharp corners: existing straight-line border.
            // Top
            fb.fill_rect_blend(nx as u32, ny as u32, nw as u32, bw, bc);

            // Bottom
            let bot_y = (ny + nh) as u32 - bw;
            fb.fill_rect_blend(nx as u32, bot_y, nw as u32, bw, bc);

            // Left
            fb.fill_rect_blend(
                nx as u32,
                ny as u32 + bw,
                bw,
                (nh as u32).saturating_sub(2 * bw),
                bc,
            );

            // Right
            let right_x = (nx + nw) as u32 - bw;
            fb.fill_rect_blend(
                right_x,
                ny as u32 + bw,
                bw,
                (nh as u32).saturating_sub(2 * bw),
                bc,
            );
        }
    }

    // Draw icon if this is the icon node.
    let mut icon_advance: i32 = 0;
    if node_id == ctx.icon_node && !ctx.icon_coverage.is_empty() {
        let icon_y = ny + (nh - ctx.icon_h as i32) / 2;
        fb.draw_coverage(
            nx,
            icon_y,
            ctx.icon_coverage,
            ctx.icon_w,
            ctx.icon_h,
            ctx.icon_color,
        );
        icon_advance = ctx.icon_w as i32 + scale_coord(8, s);
    }

    // Draw content.
    match node.content {
        Content::None => {}
        Content::Text {
            runs, run_count, ..
        } => {
            let run_size = core::mem::size_of::<TextRun>();
            let runs_bytes = if runs.length > 0
                && (runs.offset as usize + runs.length as usize) <= graph.data.len()
            {
                &graph.data[runs.offset as usize..][..runs.length as usize]
            } else {
                &[]
            };
            let text_runs: &[TextRun] = if runs_bytes.len() >= run_size {
                // SAFETY: TextRun is repr(C), data buffer alignment >= TextRun alignment.
                unsafe {
                    core::slice::from_raw_parts(
                        runs_bytes.as_ptr() as *const TextRun,
                        run_count as usize,
                    )
                }
            } else {
                &[]
            };
            let text_nx = nx + icon_advance;
            let max_y = (visible.y + visible.h) as u32;

            for run in text_runs {
                // Read ShapedGlyph array from the data buffer.
                let glyph_size = core::mem::size_of::<ShapedGlyph>();
                let shaped_glyphs: &[ShapedGlyph] = if run.glyphs.length > 0
                    && (run.glyphs.offset as usize + run.glyphs.length as usize) <= graph.data.len()
                    && run.glyphs.length as usize >= glyph_size
                {
                    let bytes =
                        &graph.data[run.glyphs.offset as usize..][..run.glyphs.length as usize];
                    let count = (run.glyph_count as usize).min(bytes.len() / glyph_size);
                    // SAFETY: ShapedGlyph is #[repr(C)], data buffer is aligned
                    // by push_shaped_glyphs to ShapedGlyph alignment.
                    unsafe {
                        core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count)
                    }
                } else {
                    &[]
                };
                let run_color = scene_to_draw_color(run.color);
                let cache = ctx.mono_cache;
                let uniform_advance = if run.advance > 0 {
                    Some(scale_coord(run.advance as i32, s))
                } else {
                    None
                };
                let gx0 = text_nx + scale_coord(run.x as i32, s);
                let gy0 = ny + scale_coord(run.y as i32, s);

                if gy0 >= max_y as i32 {
                    break;
                }

                let mut cx = gx0;

                for sg in shaped_glyphs {
                    // Look up glyph in the cache by its full u16 ID.
                    // For monospace text, glyph_id == byte value (set by core).
                    // Using u16 avoids truncation that would break glyphs with ID > 255.
                    if let Some((glyph, coverage)) = cache.get(sg.glyph_id) {
                        let px = cx + glyph.bearing_x;
                        let py = gy0 + (cache.ascent as i32 - glyph.bearing_y);

                        fb.draw_coverage(px, py, coverage, glyph.width, glyph.height, run_color);
                    }

                    cx += uniform_advance.unwrap_or(scale_coord(sg.x_advance as i32, s));
                }
            }
        }
        Content::Image {
            data,
            src_width,
            src_height,
        } => {
            if data.length > 0 && (data.offset as usize + data.length as usize) <= graph.data.len()
            {
                let pixels = &graph.data[data.offset as usize..][..data.length as usize];
                let stride = src_width as u32 * 4;

                fb.blit_blend(
                    pixels,
                    src_width as u32,
                    src_height as u32,
                    stride,
                    nx as u32,
                    ny as u32,
                );
            }
        }
        Content::Path { .. } => {
            // Path rendering is deferred for now. The cursor bar in the
            // current compositor uses direct fill_rect calls; we'll
            // implement proper path rasterization when needed.
        }
    }

    // Recurse into children.
    let use_rounded_clip = node.clips_children() && phys_radius > 0
        && nw > 0 && nh > 0 && node.first_child != NULL;

    if use_rounded_clip {
        // Corner-radius-aware clipping: render children into an offscreen
        // buffer, mask with the rounded rect shape, then composite back.
        let ow = nw as u32;
        let oh = nh as u32;
        let ostride = ow * 4;
        let mut offscreen_buf = vec![0u8; (ostride * oh) as usize];

        {
            let mut off_fb = Surface {
                data: &mut offscreen_buf,
                width: ow,
                height: oh,
                stride: ostride,
                format: PixelFormat::Bgra8888,
            };

            // Children are rendered relative to this node's origin. The
            // offscreen buffer's (0,0) corresponds to (nx, ny) in the
            // framebuffer. We pass abs_x=0, abs_y=scroll-adjusted 0 to
            // render_node for each child so they draw at the correct
            // relative position inside the offscreen buffer.
            let off_clip = ClipRect {
                x: 0,
                y: 0,
                w: nw,
                h: nh,
            };
            let child_origin_x_off = 0i32;
            let child_origin_y_off = 0i32 - scale_coord(node.scroll_y, s);
            let mut child = node.first_child;

            while child != NULL {
                if (child as usize) >= graph.nodes.len() {
                    break;
                }
                let child_node = &graph.nodes[child as usize];

                let cx = child_origin_x_off + scale_coord(child_node.x as i32, s);
                let cy = child_origin_y_off + scale_coord(child_node.y as i32, s);
                let cw = scale_size(child_node.x as i32, child_node.width as i32, s);
                let ch = scale_size(child_node.y as i32, child_node.height as i32, s);
                let child_rect = ClipRect {
                    x: cx,
                    y: cy,
                    w: cw,
                    h: ch,
                };

                if off_clip.intersect(child_rect).is_some() {
                    render_node(
                        &mut off_fb,
                        graph,
                        ctx,
                        child,
                        child_origin_x_off,
                        child_origin_y_off,
                        off_clip,
                    );
                }

                child = child_node.next_sibling;
            }
        }

        // Apply rounded-rect mask: zero out pixels outside the rounded
        // boundary. Uses the same SDF logic as fill_rounded_rect.
        mask_rounded_rect(&mut offscreen_buf, ow, oh, ostride, phys_radius);

        // Blit the masked offscreen buffer onto the main framebuffer.
        fb.blit_blend(
            &offscreen_buf,
            ow,
            oh,
            ostride,
            nx as u32,
            ny as u32,
        );
    } else {
        // Standard rectangular clip (corner_radius=0 or no clips_children).
        let child_clip = if node.clips_children() {
            match clip.intersect(node_rect) {
                Some(c) => c,
                None => return,
            }
        } else {
            clip
        };
        let child_origin_x = nx;
        let child_origin_y = ny - scale_coord(node.scroll_y, s);
        let mut child = node.first_child;

        while child != NULL {
            if (child as usize) >= graph.nodes.len() {
                break;
            }
            let child_node = &graph.nodes[child as usize];

            // Skip subtrees whose bounding box doesn't intersect the clip rect.
            let cx = child_origin_x + scale_coord(child_node.x as i32, s);
            let cy = child_origin_y + scale_coord(child_node.y as i32, s);
            let cw = scale_size(child_node.x as i32, child_node.width as i32, s);
            let ch = scale_size(child_node.y as i32, child_node.height as i32, s);
            let child_rect = ClipRect {
                x: cx,
                y: cy,
                w: cw,
                h: ch,
            };

            if child_clip.intersect(child_rect).is_some() {
                render_node(
                    fb,
                    graph,
                    ctx,
                    child,
                    child_origin_x,
                    child_origin_y,
                    child_clip,
                );
            }

            child = child_node.next_sibling;
        }
    }
}
/// Mask an BGRA pixel buffer to a rounded rectangle shape.
///
/// Pixels outside the rounded boundary are set to fully transparent.
/// Pixels on the arc edge get fractional alpha for anti-aliasing.
/// Uses the same fixed-point SDF approach as drawing::fill_rounded_rect.
fn mask_rounded_rect(buf: &mut [u8], w: u32, h: u32, stride: u32, radius: u32) {
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
        let x_arc_fp = isqrt_fp_mask(x_arc_sq);

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
                if x_arc_frac > 0 { left_solid - 1 } else { left_solid }
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

/// Integer square root (same algorithm as drawing library's isqrt_fp).
fn isqrt_fp_mask(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut result: u64 = 0;
    let mut bit: u64 = 1u64 << 30;
    while bit > x {
        bit >>= 2;
    }
    while bit != 0 {
        let candidate = result + bit;
        if x >= candidate * candidate {
            result = candidate;
        }
        bit >>= 1;
    }
    result
}

fn scene_to_draw_color(c: scene::Color) -> Color {
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// Render an entire scene graph to a framebuffer surface.
pub fn render_scene(fb: &mut Surface, graph: &SceneGraph, ctx: &RenderCtx) {
    if graph.nodes.is_empty() {
        return;
    }

    let clip = ClipRect {
        x: 0,
        y: 0,
        w: fb.width as i32,
        h: fb.height as i32,
    };

    render_node(fb, graph, ctx, 0, 0, 0, clip);
}

/// Render only the region within `dirty` (absolute pixel coordinates).
/// Nodes outside the dirty rect are clipped and skipped entirely.
pub fn render_scene_clipped(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    dirty: &protocol::DirtyRect,
) {
    if graph.nodes.is_empty() || dirty.w == 0 || dirty.h == 0 {
        return;
    }

    let clip = ClipRect {
        x: dirty.x as i32,
        y: dirty.y as i32,
        w: dirty.w as i32,
        h: dirty.h as i32,
    };

    render_node(fb, graph, ctx, 0, 0, 0, clip);
}
