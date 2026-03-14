//! Scene graph renderer: walks a tree of `scene::Node` and draws to a Surface.

use drawing::{Color, GlyphCache, Surface};
use scene::{Content, Node, NodeFlags, NodeId, TextRun, NULL};

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

/// Recursively render a node and its children.
///
/// `abs_x`, `abs_y` are the absolute pixel position of this node's origin
/// in the framebuffer. `clip` is the current clipping rectangle.
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

    let nx = abs_x + node.x as i32;
    let ny = abs_y + node.y as i32;
    let nw = node.width as i32;
    let nh = node.height as i32;
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

    // Draw background.
    if node.background.a > 0 {
        let bg = scene_to_draw_color(node.background);

        if bg.a == 255 {
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

    // Draw border.
    if node.border.width > 0 && node.border.color.a > 0 {
        let bc = scene_to_draw_color(node.border.color);
        let bw = node.border.width as u32;

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
        icon_advance = ctx.icon_w as i32 + 8;
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
                let glyphs: &[u8] = if run.glyphs.length > 0
                    && (run.glyphs.offset as usize + run.glyphs.length as usize) <= graph.data.len()
                {
                    &graph.data[run.glyphs.offset as usize..][..run.glyphs.length as usize]
                } else {
                    &[]
                };
                let run_color = scene_to_draw_color(run.color);
                let cache = ctx.mono_cache;
                let advance = if run.advance > 0 {
                    run.advance as u32
                } else {
                    match cache.get(b' ') {
                        Some((g, _)) => g.advance,
                        None => 8,
                    }
                };
                let gx0 = text_nx + run.x as i32;
                let gy0 = ny + run.y as i32;

                if gy0 >= max_y as i32 {
                    break;
                }

                let mut cx = gx0;

                for &ch in glyphs {
                    if let Some((glyph, coverage)) = cache.get(ch) {
                        let px = cx + glyph.bearing_x;
                        let py = gy0 + (cache.ascent as i32 - glyph.bearing_y);

                        fb.draw_coverage(px, py, coverage, glyph.width, glyph.height, run_color);
                    }

                    cx += advance as i32;
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
    let child_clip = if node.clips_children() {
        match clip.intersect(node_rect) {
            Some(c) => c,
            None => return,
        }
    } else {
        clip
    };
    let child_origin_x = nx;
    let child_origin_y = ny - node.scroll_y;
    let mut child = node.first_child;

    while child != NULL {
        render_node(
            fb,
            graph,
            ctx,
            child,
            child_origin_x,
            child_origin_y,
            child_clip,
        );

        if (child as usize) < graph.nodes.len() {
            child = graph.nodes[child as usize].next_sibling;
        } else {
            break;
        }
    }
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
