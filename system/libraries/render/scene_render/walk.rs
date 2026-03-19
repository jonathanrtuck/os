//! Tree walk orchestrator: recursive scene graph traversal, shadow rendering,
//! background/border drawing, child recursion, and the public render API.
//!
//! Dependencies: `alloc`, `drawing`, `scene`, `protocol`, and the sibling
//! `coords`, `content`, and `path_raster` modules.

use alloc::vec;

use drawing::{Color, PixelFormat, Surface};
use scene::{Node, NodeId, NULL};

use super::{
    coords::{round_f32, scale_coord, scale_size, snap_border},
    path_raster::scene_to_draw_color,
    RenderCtx, SceneGraph,
};
use crate::surface_pool::SurfacePool;

/// Axis-aligned clip rectangle in absolute (framebuffer) coordinates.
/// Uses i32 for physical pixel math. virgil-render has an independent f32
/// variant for NDC-space clipping -- intentionally separate coordinate systems.
#[derive(Clone, Copy)]
pub(super) struct ClipRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl ClipRect {
    pub fn intersect(self, other: ClipRect) -> Option<ClipRect> {
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
/// `abs_x`, `abs_y` are the absolute **physical** pixel position of this
/// node's origin in the framebuffer. `clip` is in physical pixels.
/// Scene graph coordinates are logical; scaled by `ctx.scale` (f32).
///
/// `world_transform` is the accumulated transform from all ancestors.
/// Each node's world transform = parent_world x node_local.
///
/// `pool` provides offscreen buffers for group opacity rendering. When a
/// node has `opacity < 255`, its subtree is rendered into an offscreen
/// buffer (from the pool) and then composited at the specified opacity.
fn render_node(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    node_id: NodeId,
    abs_x: i32,
    abs_y: i32,
    clip: ClipRect,
    pool: Option<&mut SurfacePool>,
) {
    render_node_transformed(
        fb,
        graph,
        ctx,
        node_id,
        abs_x,
        abs_y,
        clip,
        pool,
        scene::AffineTransform::identity(),
    );
}

/// Inner render function that carries the accumulated world transform.
fn render_node_transformed(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    node_id: NodeId,
    abs_x: i32,
    abs_y: i32,
    clip: ClipRect,
    pool: Option<&mut SurfacePool>,
    parent_world: scene::AffineTransform,
) {
    if node_id == NULL || node_id as usize >= graph.nodes.len() {
        return;
    }

    let node = &graph.nodes[node_id as usize];

    if !node.visible() {
        return;
    }

    // opacity=0: subtree produces no visible output -- skip entirely.
    if node.opacity == 0 {
        return;
    }

    let s = ctx.scale;

    // Compose the world transform: parent x local.
    let local_xform = node.transform;
    let world_xform = parent_world.compose(local_xform);

    // If the world transform is a pure translation (no rotation, scale, or
    // skew), apply it as a simple pixel offset. Otherwise, compute the AABB
    // of the transformed node and render into an offscreen buffer.
    let is_simple_translation = world_xform.a == 1.0
        && world_xform.b == 0.0
        && world_xform.c == 0.0
        && world_xform.d == 1.0;
    if is_simple_translation {
        // Pure translation: shift the node's position by the transform's tx, ty.
        let tx_px = round_f32(world_xform.tx * s);
        let ty_px = round_f32(world_xform.ty * s);
        let nx = abs_x + scale_coord(node.x as i32, s) + tx_px;
        let ny = abs_y + scale_coord(node.y as i32, s) + ty_px;
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

        // Compute shadow geometry for damage/overflow.
        let has_shadow = node.has_shadow();

        // Group opacity path.
        if node.opacity < 255 {
            if nw <= 0 || nh <= 0 {
                return;
            }
            let (sh_left, sh_top, sh_right, sh_bottom) = if has_shadow {
                shadow_overflow(node, s)
            } else {
                (0i32, 0i32, 0i32, 0i32)
            };
            let total_w = (sh_left + nw + sh_right).max(nw) as u32;
            let total_h = (sh_top + nh + sh_bottom).max(nh) as u32;
            let ostride = total_w * 4;
            let mut offscreen_buf = vec![0u8; (ostride * total_h) as usize];
            {
                let mut off_fb = Surface {
                    data: &mut offscreen_buf,
                    width: total_w,
                    height: total_h,
                    stride: ostride,
                    format: PixelFormat::Bgra8888,
                };
                if has_shadow {
                    render_shadow(&mut off_fb, node, sh_left, sh_top, nw, nh, s);
                }
                render_node_content_translated(
                    &mut off_fb,
                    graph,
                    ctx,
                    node,
                    node_id,
                    sh_left,
                    sh_top,
                    ClipRect {
                        x: 0,
                        y: 0,
                        w: total_w as i32,
                        h: total_h as i32,
                    },
                    None,
                    world_xform,
                );
            }
            let blit_x = (nx - sh_left).max(0) as u32;
            let blit_y = (ny - sh_top).max(0) as u32;
            fb.blit_blend_with_opacity(
                &offscreen_buf,
                total_w,
                total_h,
                ostride,
                blit_x,
                blit_y,
                node.opacity,
            );
            return;
        }

        // opacity=255: render directly.
        if has_shadow {
            render_shadow(fb, node, nx, ny, nw, nh, s);
        }
        render_node_content_translated(
            fb,
            graph,
            ctx,
            node,
            node_id,
            nx,
            ny,
            visible,
            pool,
            world_xform,
        );
    } else {
        // Non-trivial transform (rotation, scale, skew):
        //
        // Strategy: render the node's content (background, text, images,
        // children) axis-aligned into a temporary offscreen buffer sized
        // to the node's untransformed physical dimensions. Then blit the
        // offscreen buffer to the framebuffer using bilinear interpolation
        // with the world transform applied. This avoids re-rasterizing
        // glyphs at rotated angles -- the glyph cache works as-is.
        //
        // For paths: transform coordinates before rasterization (matrix x vertex).

        let base_nx = abs_x + scale_coord(node.x as i32, s);
        let base_ny = abs_y + scale_coord(node.y as i32, s);
        let nw = scale_size(node.x as i32, node.width as i32, s);
        let nh = scale_size(node.y as i32, node.height as i32, s);
        if nw <= 0 || nh <= 0 {
            return;
        }

        // Compute AABB of the transformed node bounds for culling.
        let (aabb_x, aabb_y, aabb_w, aabb_h) =
            world_xform.transform_aabb(0.0, 0.0, nw as f32, nh as f32);
        let aabb_xi = round_f32(aabb_x) + base_nx;
        let aabb_yi = round_f32(aabb_y) + base_ny;
        let aabb_wi = round_f32(aabb_w).max(0);
        let aabb_hi = round_f32(aabb_h).max(0);

        if aabb_wi == 0 || aabb_hi == 0 {
            return; // Degenerate transform (e.g., scale(0,0)).
        }

        // Also account for shadow overflow in the AABB.
        let has_shadow = node.has_shadow();
        let (sh_left, sh_top, sh_right, sh_bottom) = if has_shadow {
            shadow_overflow(node, s)
        } else {
            (0i32, 0i32, 0i32, 0i32)
        };

        // Expanded AABB includes shadow.
        let exp_aabb_xi = aabb_xi - sh_left;
        let exp_aabb_yi = aabb_yi - sh_top;
        let exp_aabb_wi = aabb_wi + sh_left + sh_right;
        let exp_aabb_hi = aabb_hi + sh_top + sh_bottom;

        let aabb_rect = ClipRect {
            x: exp_aabb_xi,
            y: exp_aabb_yi,
            w: exp_aabb_wi,
            h: exp_aabb_hi,
        };

        // Cull if the AABB doesn't intersect the clip rect.
        let clipped_aabb = match clip.intersect(aabb_rect) {
            Some(c) => c,
            None => return,
        };

        // Render the node's content axis-aligned to a temporary buffer.
        let _render_w = nw as u32;
        let _render_h = nh as u32;

        // Account for shadow in the offscreen buffer size.
        let total_w = (sh_left + nw + sh_right).max(nw) as u32;
        let total_h = (sh_top + nh + sh_bottom).max(nh) as u32;
        let render_stride = total_w * 4;
        let render_size = (render_stride * total_h) as usize;

        // Cap allocation to prevent OOM.
        if render_size > 4 * 1024 * 1024 {
            return;
        }

        let mut render_buf = vec![0u8; render_size];
        {
            let mut render_fb = Surface {
                data: &mut render_buf,
                width: total_w,
                height: total_h,
                stride: render_stride,
                format: PixelFormat::Bgra8888,
            };

            // Render shadow into the offscreen buffer.
            if has_shadow {
                render_shadow(&mut render_fb, node, sh_left, sh_top, nw, nh, s);
            }

            // Render node content at offset (sh_left, sh_top) within the buffer.
            let content_clip = ClipRect {
                x: 0,
                y: 0,
                w: total_w as i32,
                h: total_h as i32,
            };
            render_node_content_translated(
                &mut render_fb,
                graph,
                ctx,
                node,
                node_id,
                sh_left,
                sh_top,
                content_clip,
                None,
                scene::AffineTransform::identity(), // children rendered axis-aligned
            );
        }

        // Compute the inverse of the world transform for bilinear resampling.
        // The inverse maps destination pixels back to source (offscreen buffer)
        // coordinates.
        let inv = match world_xform.inverse() {
            Some(inv) => inv,
            None => return, // Singular transform -- nothing to render.
        };

        // The offscreen buffer's coordinate system:
        // - (sh_left, sh_top) in the buffer corresponds to node origin (0, 0)
        //   in the node's local physical space.
        // - The world transform maps node-local (0, 0) to (aabb_x, aabb_y)
        //   relative to (base_nx, base_ny).
        //
        // For the bilinear blit, we iterate over destination pixels in the AABB
        // region. For each dest pixel (dx, dy) relative to (exp_aabb_xi, exp_aabb_yi):
        //   1. Map to node-local space using the inverse transform
        //   2. Add (sh_left, sh_top) to get offscreen buffer coordinates
        //
        // Combine these offsets into the inverse transform's translation.
        let _inv_tx_adj = inv.tx + sh_left as f32;
        let _inv_ty_adj = inv.ty + sh_top as f32;

        // Compute the adjusted inverse translation that maps from
        // expanded-AABB-local pixel coords (col, row) to buffer coords.
        let adj_aabb_x = aabb_x - sh_left as f32;
        let adj_aabb_y = aabb_y - sh_top as f32;

        // The clipped AABB may be smaller than the expanded AABB.
        // Adjust the inverse translation to account for the clip offset.
        let clip_dx = (clipped_aabb.x - exp_aabb_xi) as f32;
        let clip_dy = (clipped_aabb.y - exp_aabb_yi) as f32;
        let adj_inv_tx = inv.a * (adj_aabb_x + clip_dx)
            + inv.c * (adj_aabb_y + clip_dy)
            + inv.tx
            + sh_left as f32;
        let adj_inv_ty = inv.b * (adj_aabb_x + clip_dx)
            + inv.d * (adj_aabb_y + clip_dy)
            + inv.ty
            + sh_top as f32;

        let eff_opacity = node.opacity;

        fb.blit_transformed_bilinear(
            &render_buf,
            total_w,
            total_h,
            render_stride,
            clipped_aabb.x,
            clipped_aabb.y,
            clipped_aabb.w as u32,
            clipped_aabb.h as u32,
            inv.a,
            inv.b,
            inv.c,
            inv.d,
            adj_inv_tx,
            adj_inv_ty,
            eff_opacity,
        );
    }
}

/// Compute the shadow overflow on each side of the node bounds in physical pixels.
///
/// Returns `(left, top, right, bottom)` -- the number of physical pixels the
/// shadow extends beyond the node's bounds on each side. Used for both damage
/// tracking and offscreen buffer sizing.
fn shadow_overflow(node: &Node, scale: f32) -> (i32, i32, i32, i32) {
    let blur = round_f32(node.shadow_blur_radius as f32 * scale).max(0);
    let spread = round_f32(node.shadow_spread as f32 * scale);
    let off_x = round_f32(node.shadow_offset_x as f32 * scale);
    let off_y = round_f32(node.shadow_offset_y as f32 * scale);

    // Shadow extends by spread + blur on each side, shifted by offset.
    let extent = spread + blur;
    let left = (extent - off_x).max(0);
    let top = (extent - off_y).max(0);
    let right = (extent + off_x).max(0);
    let bottom = (extent + off_y).max(0);

    (left, top, right, bottom)
}

/// Render a box shadow behind a node.
///
/// When rendering directly to the framebuffer (opacity=255), `draw_x`/`draw_y`
/// are the node's absolute physical position. When rendering to an offscreen
/// buffer for group opacity, they are the node's offset within the buffer.
///
/// The shadow is a rounded rect (matching node's corner_radius) filled with
/// shadow_color, optionally Gaussian-blurred, offset by shadow_offset, and
/// expanded by shadow_spread.
fn render_shadow(
    fb: &mut Surface,
    node: &Node,
    draw_x: i32,
    draw_y: i32,
    nw: i32,
    nh: i32,
    scale: f32,
) {
    let blur_radius = round_f32(node.shadow_blur_radius as f32 * scale).max(0) as u32;
    let spread = round_f32(node.shadow_spread as f32 * scale);
    let off_x = round_f32(node.shadow_offset_x as f32 * scale);
    let off_y = round_f32(node.shadow_offset_y as f32 * scale);

    // Shadow rect: node bounds expanded by spread, shifted by offset.
    let sw = (nw as i32 + 2 * spread).max(0) as u32;
    let sh = (nh as i32 + 2 * spread).max(0) as u32;
    if sw == 0 || sh == 0 {
        return;
    }

    let sx = (draw_x + off_x - spread).max(0) as u32;
    let sy = (draw_y + off_y - spread).max(0) as u32;

    let shadow_color = scene_to_draw_color(node.shadow_color);

    // Physical corner radius for the shadow shape.
    let phys_radius = if node.corner_radius > 0 {
        let r = round_f32(node.corner_radius as f32 * scale);
        let max_r = (sw.min(sh) / 2) as i32;
        let sr = r + spread; // Spread expands the radius too.
        if sr < 0 {
            0u32
        } else {
            (sr as u32).min(max_r as u32)
        }
    } else {
        0u32
    };

    if blur_radius == 0 {
        // Hard shadow: just fill a rectangle (or rounded rect) at the offset.
        if phys_radius > 0 {
            fb.fill_rounded_rect_blend(sx, sy, sw, sh, phys_radius, shadow_color);
        } else if shadow_color.a == 255 {
            fb.fill_rect(sx, sy, sw, sh, shadow_color);
        } else {
            fb.fill_rect_blend(sx, sy, sw, sh, shadow_color);
        }
    } else {
        // Blurred shadow: render the shadow shape to a temporary surface,
        // apply Gaussian blur, then composite onto the destination.
        let pad = blur_radius;
        let buf_w = sw + 2 * pad;
        let buf_h = sh + 2 * pad;
        let buf_stride = buf_w * 4;
        let buf_size = (buf_stride * buf_h) as usize;

        // Cap allocation to avoid OOM (4 MiB per buffer x 3 = 12 MiB).
        if buf_size > 4 * 1024 * 1024 {
            // Fallback to hard shadow for very large blur.
            if phys_radius > 0 {
                fb.fill_rounded_rect_blend(sx, sy, sw, sh, phys_radius, shadow_color);
            } else {
                fb.fill_rect_blend(sx, sy, sw, sh, shadow_color);
            }
            return;
        }

        // Source buffer: fill the shadow shape.
        let mut src_buf = vec![0u8; buf_size];
        {
            let mut src_fb = Surface {
                data: &mut src_buf,
                width: buf_w,
                height: buf_h,
                stride: buf_stride,
                format: PixelFormat::Bgra8888,
            };
            // Draw the shadow shape centered in the padded buffer.
            if phys_radius > 0 {
                src_fb.fill_rounded_rect_blend(pad, pad, sw, sh, phys_radius, shadow_color);
            } else if shadow_color.a == 255 {
                src_fb.fill_rect(pad, pad, sw, sh, shadow_color);
            } else {
                src_fb.fill_rect_blend(pad, pad, sw, sh, shadow_color);
            }
        }

        // Apply Gaussian blur.
        let mut tmp_buf = vec![0u8; buf_size];
        let mut dst_buf = vec![0u8; buf_size];

        // Use sigma proportional to radius (CSS convention: sigma ~ radius/2).
        let sigma_fp = if blur_radius > 0 {
            // 8.8 fixed-point: sigma = radius / 2, fp = sigma * 256.
            ((blur_radius as u32) * 256 / 2).max(128)
        } else {
            256
        };

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

        drawing::blur_surface(
            &src_read,
            &mut dst_surface,
            &mut tmp_buf,
            blur_radius,
            sigma_fp,
        );

        // Composite the blurred shadow onto the destination.
        // The shadow buffer's (pad, pad) corresponds to (sx, sy) in the dest.
        let blit_x = sx.saturating_sub(pad);
        let blit_y = sy.saturating_sub(pad);

        fb.blit_blend(&dst_buf, buf_w, buf_h, buf_stride, blit_x, blit_y);
    }
}

/// Render a node's background, content, and children into a target surface.
///
/// This is the inner rendering logic used by both the direct path (opacity=255)
/// and the offscreen opacity path (opacity<255).
///
/// `draw_x`, `draw_y` are where to draw the node's background/content in the
/// target surface. For direct rendering this equals the node's absolute FB
/// position. For offscreen opacity rendering this is (0, 0).
/// `visible` is the clipped visible rectangle in the target surface's coords.
/// `world_xform` is the accumulated world transform for this node's subtree.
fn render_node_content_translated(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    node: &Node,
    node_id: NodeId,
    draw_x: i32,
    draw_y: i32,
    visible: ClipRect,
    pool: Option<&mut SurfacePool>,
    world_xform: scene::AffineTransform,
) {
    let s = ctx.scale;
    let nw = scale_size(node.x as i32, node.width as i32, s);
    let nh = scale_size(node.y as i32, node.height as i32, s);
    let node_rect = ClipRect {
        x: draw_x,
        y: draw_y,
        w: nw,
        h: nh,
    };

    // Scale corner radius from logical to physical pixels.
    let phys_radius = if node.corner_radius > 0 {
        let r = round_f32(node.corner_radius as f32 * s);
        if r < 0 {
            0u32
        } else {
            r as u32
        }
    } else {
        0u32
    };

    // Draw background.
    if node.background.a > 0 {
        let bg = scene_to_draw_color(node.background);

        if phys_radius > 0 {
            fb.fill_rounded_rect_blend(
                draw_x as u32,
                draw_y as u32,
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
            let inner_r = phys_radius.saturating_sub(bw);

            fb.fill_rounded_rect_blend(
                draw_x as u32,
                draw_y as u32,
                nw as u32,
                nh as u32,
                phys_radius,
                bc,
            );

            let inner_x = (draw_x as u32).saturating_add(bw);
            let inner_y = (draw_y as u32).saturating_add(bw);
            let inner_w = (nw as u32).saturating_sub(2 * bw);
            let inner_h = (nh as u32).saturating_sub(2 * bw);

            if inner_w > 0 && inner_h > 0 {
                let inner_color = if node.background.a > 0 {
                    scene_to_draw_color(node.background)
                } else {
                    Color::TRANSPARENT
                };

                if inner_color.a > 0 {
                    fb.fill_rounded_rect_blend(
                        inner_x,
                        inner_y,
                        inner_w,
                        inner_h,
                        inner_r,
                        inner_color,
                    );
                }
            }
        } else {
            fb.fill_rect_blend(draw_x as u32, draw_y as u32, nw as u32, bw, bc);

            let bot_y = (draw_y + nh) as u32 - bw;
            fb.fill_rect_blend(draw_x as u32, bot_y, nw as u32, bw, bc);

            fb.fill_rect_blend(
                draw_x as u32,
                draw_y as u32 + bw,
                bw,
                (nh as u32).saturating_sub(2 * bw),
                bc,
            );

            let right_x = (draw_x + nw) as u32 - bw;
            fb.fill_rect_blend(
                right_x,
                draw_y as u32 + bw,
                bw,
                (nh as u32).saturating_sub(2 * bw),
                bc,
            );
        }
    }

    // Draw content.
    super::content::render_content(fb, graph, ctx.mono_cache, s, node, draw_x, draw_y, nw, nh);

    // Recurse into children.
    let use_rounded_clip =
        node.clips_children() && phys_radius > 0 && nw > 0 && nh > 0 && node.first_child != NULL;

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
                    render_node_transformed(
                        &mut off_fb,
                        graph,
                        ctx,
                        child,
                        child_origin_x_off,
                        child_origin_y_off,
                        off_clip,
                        None,
                        scene::AffineTransform::identity(),
                    );
                }

                child = child_node.next_sibling;
            }
        }

        // Apply rounded-rect mask: zero out pixels outside the rounded
        // boundary. Uses the same SDF logic as fill_rounded_rect.
        super::content::mask_rounded_rect(&mut offscreen_buf, ow, oh, ostride, phys_radius);

        // Blit the masked offscreen buffer onto the main framebuffer.
        fb.blit_blend(
            &offscreen_buf,
            ow,
            oh,
            ostride,
            draw_x as u32,
            draw_y as u32,
        );
    } else {
        // Standard rectangular clip (corner_radius=0 or no clips_children).
        let child_clip = if node.clips_children() {
            match visible.intersect(node_rect) {
                Some(c) => c,
                None => return,
            }
        } else {
            visible
        };
        let child_origin_x = draw_x;
        let child_origin_y = draw_y - scale_coord(node.scroll_y, s);
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
                render_node_transformed(
                    fb,
                    graph,
                    ctx,
                    child,
                    child_origin_x,
                    child_origin_y,
                    child_clip,
                    None,
                    scene::AffineTransform::identity(),
                );
            }

            child = child_node.next_sibling;
        }
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

    render_node(fb, graph, ctx, 0, 0, 0, clip, None);
}

/// Render an entire scene graph with a SurfacePool for offscreen opacity.
pub fn render_scene_with_pool(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    pool: &mut SurfacePool,
) {
    if graph.nodes.is_empty() {
        return;
    }

    let clip = ClipRect {
        x: 0,
        y: 0,
        w: fb.width as i32,
        h: fb.height as i32,
    };

    render_node(fb, graph, ctx, 0, 0, 0, clip, Some(pool));
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

    render_node(fb, graph, ctx, 0, 0, 0, clip, None);
}

/// Render only the region within `dirty`, with SurfacePool for offscreen opacity.
pub fn render_scene_clipped_with_pool(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    dirty: &protocol::DirtyRect,
    pool: &mut SurfacePool,
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

    render_node(fb, graph, ctx, 0, 0, 0, clip, Some(pool));
}
