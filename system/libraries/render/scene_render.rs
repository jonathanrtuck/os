//! Scene graph renderer: walks a tree of `scene::Node` and draws to a Surface.
//!
//! Text rendering uses shaped glyph arrays from the scene graph. Each TextRun
//! stores an array of `ShapedGlyph` in the data buffer. The compositor reads
//! glyph IDs and advances, rasterizes via the glyph cache (pre-populated for
//! monospace ASCII, on-demand via LRU for other glyphs), and composites via
//! `draw_coverage`.

use alloc::{boxed::Box, vec};

use drawing::{Color, PixelFormat, Surface};
use fonts::cache::GlyphCache;
use scene::{Content, Node, NodeFlags, NodeId, PathCmd, PathCmdKind, ShapedGlyph, TextRun, NULL};

use crate::surface_pool::SurfacePool;
use crate::svg;

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
pub fn round_f32(x: f32) -> i32 {
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
pub fn scale_coord(logical: i32, scale: f32) -> i32 {
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
pub fn scale_size(logical_pos: i32, logical_size: i32, scale: f32) -> i32 {
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
///
/// `world_transform` is the accumulated transform from all ancestors.
/// Each node's world transform = parent_world × node_local.
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
    render_node_transformed(fb, graph, ctx, node_id, abs_x, abs_y, clip, pool, scene::AffineTransform::identity());
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

    // opacity=0: subtree produces no visible output — skip entirely.
    if node.opacity == 0 {
        return;
    }

    let s = ctx.scale;

    // Compose the world transform: parent × local.
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
                    &mut off_fb, graph, ctx, node, node_id,
                    sh_left, sh_top,
                    ClipRect { x: 0, y: 0, w: total_w as i32, h: total_h as i32 },
                    None,
                    world_xform,
                );
            }
            let blit_x = (nx - sh_left).max(0) as u32;
            let blit_y = (ny - sh_top).max(0) as u32;
            fb.blit_blend_with_opacity(
                &offscreen_buf, total_w, total_h, ostride,
                blit_x, blit_y, node.opacity,
            );
            return;
        }

        // opacity=255: render directly.
        if has_shadow {
            render_shadow(fb, node, nx, ny, nw, nh, s);
        }
        render_node_content_translated(fb, graph, ctx, node, node_id, nx, ny, visible, pool, world_xform);
    } else {
        // Non-trivial transform (rotation, scale, skew):
        //
        // Strategy: render the node's content (background, text, images,
        // children) axis-aligned into a temporary offscreen buffer sized
        // to the node's untransformed physical dimensions. Then blit the
        // offscreen buffer to the framebuffer using bilinear interpolation
        // with the world transform applied. This avoids re-rasterizing
        // glyphs at rotated angles — the glyph cache works as-is.
        //
        // For paths: transform coordinates before rasterization (matrix × vertex).

        let base_nx = abs_x + scale_coord(node.x as i32, s);
        let base_ny = abs_y + scale_coord(node.y as i32, s);
        let nw = scale_size(node.x as i32, node.width as i32, s);
        let nh = scale_size(node.y as i32, node.height as i32, s);
        if nw <= 0 || nh <= 0 {
            return;
        }

        // Compute AABB of the transformed node bounds for culling.
        let (aabb_x, aabb_y, aabb_w, aabb_h) = world_xform.transform_aabb(0.0, 0.0, nw as f32, nh as f32);
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
        let render_w = nw as u32;
        let render_h = nh as u32;

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
                &mut render_fb, graph, ctx, node, node_id,
                sh_left, sh_top,
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
            None => return, // Singular transform — nothing to render.
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
        let inv_tx_adj = inv.tx + sh_left as f32;
        let inv_ty_adj = inv.ty + sh_top as f32;

        // The destination region in framebuffer coords is the AABB, but the
        // inverse transform expects coordinates relative to the AABB origin.
        // We need to account for (aabb_x, aabb_y) offset: the dest pixel
        // at (exp_aabb_xi, exp_aabb_yi) should map to (0 - sh_left, 0 - sh_top)
        // in node-local space, which is (-sh_left, -sh_top) + (sh_left, sh_top)
        // = (0, 0) in buffer space.
        //
        // Since aabb_x/aabb_y are the AABB min of the transform, and we want
        // dest pixel 0 to map to the top-left of the shadow-expanded area:
        // The mapping for dest pixel (col, row) relative to (exp_aabb_xi, exp_aabb_yi):
        //   src_x = inv.a * (col - sh_left) + inv.c * (row - sh_top) + inv.tx + sh_left
        //   where (col - sh_left, row - sh_top) is the offset from the node AABB origin.
        // But the AABB already includes the transform offset (aabb_x, aabb_y are from
        // the transform), so dest (0,0) of the expanded AABB corresponds to
        // (-sh_left, -sh_top) relative to the AABB origin.
        //
        // Simpler approach: for dest pixel at (col, row) in expanded AABB space:
        //   - Offset to AABB-local: (col - sh_left, row - sh_top)
        //   - These are in "post-transform" space relative to node origin
        //   - Apply inverse to get node-local: inv × (col - sh_left, row - sh_top)
        //   - Add (sh_left, sh_top) to get buffer coords

        // For the shadow-expanded area, we need pixels outside the transform
        // AABB too. Use a simpler mapping that directly maps:
        //   buffer_x = inv.a * (col - sh_left) + inv.c * (row - sh_top) + inv.tx + sh_left
        //   buffer_y = inv.b * (col - sh_left) + inv.d * (row - sh_top) + inv.ty + sh_top
        // But this only works for the node content, not the shadow (which is
        // already rendered at the correct position in the buffer).
        //
        // For shadow: shadow is pre-rendered in the buffer at its correct
        // offset. We want to blit the ENTIRE buffer with the transform.
        // But shadow should not be transformed (it's already positioned).
        //
        // Actually: render shadow to a separate buffer and blit it axis-aligned,
        // then render the node content to the offscreen buffer and blit with
        // the transform. This way shadow is not resampled.
        //
        // Simpler approach for this feature: render shadow + content to one
        // buffer, and blit the whole thing with the transform. The shadow
        // will be slightly rotated, but this is visually acceptable and
        // matches how CSS transforms work (shadow is part of the element's
        // visual, and both rotate together).

        // For the bilinear blit: destination covers the expanded AABB.
        // Each dest pixel (col, row) maps to buffer coords:
        //   - (col, row) is the position within the expanded AABB
        //   - For pixels in the shadow region, they're already at the right place
        //   - For pixels in the node content, we need the inverse transform
        //
        // Since the shadow is rendered in the buffer aligned to the node, and
        // the node content is also aligned, we apply the SAME transform to
        // the whole buffer. The shadow position in the buffer is correct
        // relative to the node content; after transform, both rotate together.

        // Effective inverse mapping from expanded-AABB-local coords to buffer:
        //   buf_x = inv.a * col + inv.c * row + inv.tx + sh_left
        //   buf_y = inv.b * col + inv.d * row + inv.ty + sh_top
        // where (col, row) are relative to the start of the expanded AABB.

        // Compute the adjusted inverse translation that maps from
        // expanded-AABB-local pixel coords (col, row) to buffer coords:
        //   AABB-local (col, row) → post-transform space: (aabb_x + col, aabb_y + row)
        //     where aabb_x, aabb_y are the AABB min in post-transform space.
        //   post-transform → pre-transform (node-local): inv × (aabb_x + col, aabb_y + row)
        //   node-local → buffer: + (sh_left, sh_top)
        //
        //   buf_x = inv.a * col + inv.c * row + (inv.a * aabb_x + inv.c * aabb_y + inv.tx + sh_left)
        //   buf_y = inv.b * col + inv.d * row + (inv.b * aabb_x + inv.d * aabb_y + inv.ty + sh_top)
        //
        // Note: aabb_x, aabb_y are from transform_aabb (before adding base_nx/base_ny).
        // The expanded AABB adds shadow margins, so for the shadow region we also
        // offset by (-sh_left, -sh_top) in AABB space:
        //   adj_aabb_x = aabb_x - sh_left_f
        //   adj_aabb_y = aabb_y - sh_top_f
        let adj_aabb_x = aabb_x - sh_left as f32;
        let adj_aabb_y = aabb_y - sh_top as f32;

        // The clipped AABB may be smaller than the expanded AABB.
        // Adjust the inverse translation to account for the clip offset:
        // col=0 in the clipped region corresponds to (clipped_aabb.x - exp_aabb_xi)
        // in the expanded AABB. We offset accordingly.
        let clip_dx = (clipped_aabb.x - exp_aabb_xi) as f32;
        let clip_dy = (clipped_aabb.y - exp_aabb_yi) as f32;
        let adj_inv_tx = inv.a * (adj_aabb_x + clip_dx) + inv.c * (adj_aabb_y + clip_dy) + inv.tx + sh_left as f32;
        let adj_inv_ty = inv.b * (adj_aabb_x + clip_dx) + inv.d * (adj_aabb_y + clip_dy) + inv.ty + sh_top as f32;

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
/// Returns `(left, top, right, bottom)` — the number of physical pixels the
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

    let sx = draw_x + off_x - spread;
    let sy = draw_y + off_y - spread;

    let shadow_color = scene_to_draw_color(node.shadow_color);

    // Physical corner radius for the shadow shape.
    let phys_radius = if node.corner_radius > 0 {
        let r = round_f32(node.corner_radius as f32 * scale);
        let max_r = (sw.min(sh) / 2) as i32;
        let sr = r + spread; // Spread expands the radius too.
        if sr < 0 { 0u32 } else { (sr as u32).min(max_r as u32) }
    } else {
        0u32
    };

    if blur_radius == 0 {
        // Hard shadow: just fill a rectangle (or rounded rect) at the offset.
        if phys_radius > 0 {
            fb.fill_rounded_rect_blend(sx as u32, sy as u32, sw, sh, phys_radius, shadow_color);
        } else if shadow_color.a == 255 {
            fb.fill_rect(sx as u32, sy as u32, sw, sh, shadow_color);
        } else {
            fb.fill_rect_blend(sx as u32, sy as u32, sw, sh, shadow_color);
        }
    } else {
        // Blurred shadow: render the shadow shape to a temporary surface,
        // apply Gaussian blur, then composite onto the destination.
        let pad = blur_radius;
        let buf_w = sw + 2 * pad;
        let buf_h = sh + 2 * pad;
        let buf_stride = buf_w * 4;
        let buf_size = (buf_stride * buf_h) as usize;

        // Cap allocation to avoid OOM (4 MiB per buffer × 3 = 12 MiB).
        if buf_size > 4 * 1024 * 1024 {
            // Fallback to hard shadow for very large blur.
            if phys_radius > 0 {
                fb.fill_rounded_rect_blend(sx as u32, sy as u32, sw, sh, phys_radius, shadow_color);
            } else {
                fb.fill_rect_blend(sx as u32, sy as u32, sw, sh, shadow_color);
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

        // Use sigma proportional to radius (CSS convention: sigma ≈ radius/2).
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

        drawing::blur_surface(&src_read, &mut dst_surface, &mut tmp_buf, blur_radius, sigma_fp);

        // Composite the blurred shadow onto the destination.
        // The shadow buffer's (pad, pad) corresponds to (sx, sy) in the dest.
        let blit_x = (sx - pad as i32).max(0) as u32;
        let blit_y = (sy - pad as i32).max(0) as u32;

        fb.blit_blend(
            &dst_buf,
            buf_w,
            buf_h,
            buf_stride,
            blit_x,
            blit_y,
        );
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
        if r < 0 { 0u32 } else { r as u32 }
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
                        inner_x, inner_y, inner_w, inner_h, inner_r, inner_color,
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

    // Draw icon if this is the icon node.
    let mut icon_advance: i32 = 0;
    if node_id == ctx.icon_node && !ctx.icon_coverage.is_empty() {
        let icon_y = draw_y + (nh - ctx.icon_h as i32) / 2;
        fb.draw_coverage(
            draw_x,
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
            let text_nx = draw_x + icon_advance;
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
                let gy0 = draw_y + scale_coord(run.y as i32, s);

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
                let src_stride = src_width as u32 * 4;

                // When source dimensions differ from the node's display size,
                // use bilinear resampling for smooth scaling instead of nearest-
                // neighbor. This produces blended gray for downscaled checker-
                // boards instead of aliased black/white.
                let phys_nw = nw.max(0) as u32;
                let phys_nh = nh.max(0) as u32;
                if phys_nw > 0 && phys_nh > 0
                    && (src_width as u32 != phys_nw || src_height as u32 != phys_nh)
                {
                    // Inverse transform: maps each dest pixel back to source
                    // coordinates. For a simple scale: inv_a = src_w/dst_w,
                    // inv_d = src_h/dst_h. Offset by 0.5 src pixel to center
                    // the sampling between source pixels.
                    let inv_a = src_width as f32 / phys_nw as f32;
                    let inv_d = src_height as f32 / phys_nh as f32;
                    // Center the sampling: offset by half a source pixel so
                    // destination pixel centers land between source pixels.
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
        }
        Content::Path {
            commands,
            fill,
            stroke,
            stroke_width,
            ..
        } => {
            render_path(
                fb,
                graph,
                ctx,
                commands,
                fill,
                stroke,
                stroke_width,
                draw_x,
                draw_y,
                nw,
                nh,
                visible,
            );
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
        mask_rounded_rect(&mut offscreen_buf, ow, oh, ostride, phys_radius);

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
/// Convert scene `PathCmd` array to SVG commands and rasterize.
///
/// Converts the scene graph's `PathCmd` representation to `SvgCommand`
/// for the existing scanline rasterizer. Handles both fill and stroke
/// (fill first, then stroke on top). Empty/degenerate paths are handled
/// gracefully: no crash, no stray pixels.
fn render_path(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    commands: scene::DataRef,
    fill: scene::Color,
    stroke: scene::Color,
    stroke_width: u8,
    nx: i32,
    ny: i32,
    nw: i32,
    nh: i32,
    clip: ClipRect,
) {
    // Read path commands from data buffer.
    let cmd_size = core::mem::size_of::<PathCmd>();
    let cmd_bytes = if commands.length > 0
        && (commands.offset as usize + commands.length as usize) <= graph.data.len()
    {
        &graph.data[commands.offset as usize..][..commands.length as usize]
    } else {
        return; // No commands — nothing to render.
    };
    let num_cmds = cmd_bytes.len() / cmd_size;
    if num_cmds == 0 {
        return;
    }
    // SAFETY: PathCmd is #[repr(C)], data buffer alignment is sufficient.
    let path_cmds: &[PathCmd] = unsafe {
        core::slice::from_raw_parts(cmd_bytes.as_ptr() as *const PathCmd, num_cmds)
    };

    let has_fill = fill.a > 0;
    let has_stroke = stroke.a > 0 && stroke_width > 0;
    if !has_fill && !has_stroke {
        return;
    }

    // Clamp rendering dimensions to the intersection of node bounds and clip.
    let node_clip = ClipRect { x: nx, y: ny, w: nw, h: nh };
    let effective_clip = match clip.intersect(node_clip) {
        Some(c) => c,
        None => return,
    };

    // We render the path into a coverage map sized to the node's physical
    // dimensions, then composite only the visible (clipped) portion.
    // For very large nodes, cap the coverage map to a reasonable size.
    let cov_w = if nw > 0 { nw as u32 } else { return };
    let cov_h = if nh > 0 { nh as u32 } else { return };
    let cov_size = (cov_w as usize).checked_mul(cov_h as usize);
    let cov_size = match cov_size {
        Some(s) if s > 0 && s <= 4 * 1024 * 1024 => s, // Cap at 4 MiB
        _ => return,
    };

    // Build an SvgPath from PathCmd array. Heap-allocate to avoid stack
    // overflow (SvgPath is ~16 KiB).
    let mut svg_path = svg_path_from_scene_cmds(path_cmds, ctx.scale);
    if svg_path.num_commands == 0 {
        return;
    }

    // Fill pass.
    if has_fill {
        let mut coverage = vec![0u8; cov_size];
        let mut scratch = alloc_svg_scratch();
        let result = svg::svg_rasterize(
            &svg_path,
            &mut scratch,
            &mut coverage,
            cov_w,
            cov_h,
            svg::SVG_FP_ONE, // 1:1 scale — already in physical pixels
            0,
            0,
        );
        if result.is_ok() {
            composite_coverage(
                fb, &coverage, cov_w, cov_h,
                nx, ny, scene_to_draw_color(fill), &effective_clip,
            );
        }
    }

    // Stroke pass.
    if has_stroke {
        // Build stroke outline by expanding each segment into a thin
        // polygon. We create a new SvgPath representing the stroke
        // outline and rasterize it as a filled shape.
        let phys_stroke = {
            let sw = round_f32(stroke_width as f32 * ctx.scale);
            if sw < 1 { 1i32 } else { sw }
        };

        let mut stroke_path = build_stroke_outline(path_cmds, ctx.scale, phys_stroke / 2);
        if stroke_path.num_commands > 0 {
            let mut coverage = vec![0u8; cov_size];
            let mut scratch = alloc_svg_scratch();
            let result = svg::svg_rasterize(
                &stroke_path,
                &mut scratch,
                &mut coverage,
                cov_w,
                cov_h,
                svg::SVG_FP_ONE,
                0,
                0,
            );
            if result.is_ok() {
                composite_coverage(
                    fb, &coverage, cov_w, cov_h,
                    nx, ny, scene_to_draw_color(stroke), &effective_clip,
                );
            }
        }
    }
}

/// Convert scene PathCmd array to an SvgPath, applying the display scale
/// factor. Path coordinates are logical (from the scene graph); they are
/// converted to physical pixel coordinates here.
fn svg_path_from_scene_cmds(cmds: &[PathCmd], scale: f32) -> Box<svg::SvgPath> {
    let mut path = Box::new(svg::SvgPath::new());

    for cmd in cmds {
        if path.num_commands >= 512 {
            break; // SVG_MAX_COMMANDS
        }
        let scaled = |v: i16| -> i32 { round_f32(v as f32 * scale) };
        match cmd.kind {
            PathCmdKind::MoveTo => {
                path.commands[path.num_commands] =
                    svg::SvgCommand::MoveTo { x: scaled(cmd.x), y: scaled(cmd.y) };
                path.num_commands += 1;
            }
            PathCmdKind::LineTo => {
                path.commands[path.num_commands] =
                    svg::SvgCommand::LineTo { x: scaled(cmd.x), y: scaled(cmd.y) };
                path.num_commands += 1;
            }
            PathCmdKind::CurveTo => {
                path.commands[path.num_commands] = svg::SvgCommand::CubicTo {
                    x1: scaled(cmd.x1),
                    y1: scaled(cmd.y1),
                    x2: scaled(cmd.x2),
                    y2: scaled(cmd.y2),
                    x: scaled(cmd.x),
                    y: scaled(cmd.y),
                };
                path.num_commands += 1;
            }
            PathCmdKind::Close => {
                path.commands[path.num_commands] = svg::SvgCommand::Close;
                path.num_commands += 1;
            }
        }
    }

    path
}

/// Heap-allocate a zeroed SvgRasterScratch.
fn alloc_svg_scratch() -> Box<svg::SvgRasterScratch> {
    // SAFETY: SvgRasterScratch is repr(C)-like with all-integer fields.
    // A zeroed instance is valid — all segment counts are 0.
    unsafe {
        let layout = alloc::alloc::Layout::new::<svg::SvgRasterScratch>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut svg::SvgRasterScratch;
        assert!(!ptr.is_null());
        Box::from_raw(ptr)
    }
}

/// Composite a coverage map onto the framebuffer using sRGB-correct blending.
///
/// `coverage` is `cov_w × cov_h` bytes (one byte per pixel, 0–255 alpha).
/// The coverage map is positioned at (`ox`, `oy`) in framebuffer coords.
/// Only pixels within `clip` are composited.
fn composite_coverage(
    fb: &mut Surface,
    coverage: &[u8],
    cov_w: u32,
    cov_h: u32,
    ox: i32,
    oy: i32,
    color: Color,
    clip: &ClipRect,
) {
    // Determine the visible rectangle (intersection of coverage map and clip).
    let cov_rect = ClipRect { x: ox, y: oy, w: cov_w as i32, h: cov_h as i32 };
    let vis = match clip.intersect(cov_rect) {
        Some(v) => v,
        None => return,
    };

    let fb_stride = fb.stride;
    let fb_w = fb.width;
    let fb_h = fb.height;

    for py in vis.y..(vis.y + vis.h) {
        if py < 0 || py >= fb_h as i32 {
            continue;
        }
        let cov_row = (py - oy) as u32;
        let fb_row_off = (py as u32 * fb_stride) as usize;

        for px in vis.x..(vis.x + vis.w) {
            if px < 0 || px >= fb_w as i32 {
                continue;
            }
            let cov_col = (px - ox) as u32;
            let cov_idx = (cov_row * cov_w + cov_col) as usize;
            if cov_idx >= coverage.len() {
                continue;
            }
            let alpha = coverage[cov_idx];
            if alpha == 0 {
                continue;
            }

            let fb_off = fb_row_off + (px as usize * 4);
            if fb_off + 4 > fb.data.len() {
                continue;
            }

            // Modulate color alpha by coverage.
            let src_a = ((color.a as u32 * alpha as u32) / 255) as u8;
            if src_a == 0 {
                continue;
            }

            // BGRA format.
            let dst_b = fb.data[fb_off];
            let dst_g = fb.data[fb_off + 1];
            let dst_r = fb.data[fb_off + 2];
            let dst_a = fb.data[fb_off + 3];

            // sRGB-correct blend using drawing library's LUTs.
            let src = Color { r: color.r, g: color.g, b: color.b, a: src_a };
            let dst = Color { r: dst_r, g: dst_g, b: dst_b, a: dst_a };
            let blended = src.blend_over(dst);

            fb.data[fb_off] = blended.b;
            fb.data[fb_off + 1] = blended.g;
            fb.data[fb_off + 2] = blended.r;
            fb.data[fb_off + 3] = blended.a;
        }
    }
}

/// Build a stroke outline from path commands by offsetting each segment
/// perpendicular to its direction to create a filled polygon representing
/// the stroke.
///
/// This is a simplified stroke expansion: each line segment becomes a
/// thin rectangle, and each curve is flattened first then stroked.
/// For the MVP, this produces acceptable results for basic shapes.
fn build_stroke_outline(cmds: &[PathCmd], scale: f32, half_width_phys: i32) -> Box<svg::SvgPath> {
    let mut path = Box::new(svg::SvgPath::new());
    let hw = half_width_phys;

    // Collect physical-coordinate points from the path.
    let scaled = |v: i16| -> i32 { round_f32(v as f32 * scale) };

    // First pass: build a list of physical-space points representing
    // the flattened path. Each subpath is a sequence of points.
    let mut points: alloc::vec::Vec<(i32, i32)> = alloc::vec::Vec::new();
    let mut subpath_start = 0usize;
    let mut cur_x = 0i32;
    let mut cur_y = 0i32;
    let mut has_moveto = false;

    for cmd in cmds {
        match cmd.kind {
            PathCmdKind::MoveTo => {
                // Stroke the previous subpath.
                if points.len() > subpath_start + 1 {
                    stroke_subpath(&mut path, &points[subpath_start..], hw, false);
                }
                cur_x = scaled(cmd.x);
                cur_y = scaled(cmd.y);
                subpath_start = points.len();
                points.push((cur_x, cur_y));
                has_moveto = true;
            }
            PathCmdKind::LineTo => {
                if !has_moveto {
                    // Implicit moveto at (0,0).
                    subpath_start = points.len();
                    points.push((0, 0));
                    has_moveto = true;
                    cur_x = 0;
                    cur_y = 0;
                }
                let nx = scaled(cmd.x);
                let ny = scaled(cmd.y);
                points.push((nx, ny));
                cur_x = nx;
                cur_y = ny;
            }
            PathCmdKind::CurveTo => {
                if !has_moveto {
                    subpath_start = points.len();
                    points.push((0, 0));
                    has_moveto = true;
                    cur_x = 0;
                    cur_y = 0;
                }
                // Flatten the cubic bezier into line segments.
                let x1 = scaled(cmd.x1);
                let y1 = scaled(cmd.y1);
                let x2 = scaled(cmd.x2);
                let y2 = scaled(cmd.y2);
                let ex = scaled(cmd.x);
                let ey = scaled(cmd.y);
                flatten_cubic_to_points(
                    &mut points,
                    cur_x, cur_y,
                    x1, y1, x2, y2,
                    ex, ey,
                    0,
                );
                cur_x = ex;
                cur_y = ey;
            }
            PathCmdKind::Close => {
                // Close: add closing line back to subpath start.
                if points.len() > subpath_start {
                    let (sx, sy) = points[subpath_start];
                    if cur_x != sx || cur_y != sy {
                        points.push((sx, sy));
                    }
                }
                // Stroke as closed subpath.
                if points.len() > subpath_start + 1 {
                    stroke_subpath(&mut path, &points[subpath_start..], hw, true);
                }
                if points.len() > subpath_start {
                    let (sx, sy) = points[subpath_start];
                    cur_x = sx;
                    cur_y = sy;
                }
                subpath_start = points.len();
            }
        }
    }

    // Stroke any remaining open subpath.
    if points.len() > subpath_start + 1 {
        stroke_subpath(&mut path, &points[subpath_start..], hw, false);
    }

    path
}

/// Flatten a cubic bezier curve into a sequence of points.
fn flatten_cubic_to_points(
    points: &mut alloc::vec::Vec<(i32, i32)>,
    x0: i32, y0: i32,
    x1: i32, y1: i32,
    x2: i32, y2: i32,
    x3: i32, y3: i32,
    depth: u32,
) {
    // Flatness test: if control points are close to chord midpoint, emit line.
    let mx = (x0 + x3) / 2;
    let my = (y0 + y3) / 2;
    let d1x = (x1 - mx) as i64;
    let d1y = (y1 - my) as i64;
    let d2x = (x2 - mx) as i64;
    let d2y = (y2 - my) as i64;
    let dist1_sq = d1x * d1x + d1y * d1y;
    let dist2_sq = d2x * d2x + d2y * d2y;
    // Threshold: 0.5 pixels squared.
    let threshold: i64 = 1;

    if depth >= 10 || (dist1_sq <= threshold && dist2_sq <= threshold) {
        points.push((x3, y3));
        return;
    }

    // De Casteljau split at t=0.5.
    let q0x = (x0 + x1) / 2;
    let q0y = (y0 + y1) / 2;
    let q1x = (x1 + x2) / 2;
    let q1y = (y1 + y2) / 2;
    let q2x = (x2 + x3) / 2;
    let q2y = (y2 + y3) / 2;
    let r0x = (q0x + q1x) / 2;
    let r0y = (q0y + q1y) / 2;
    let r1x = (q1x + q2x) / 2;
    let r1y = (q1y + q2y) / 2;
    let sx = (r0x + r1x) / 2;
    let sy = (r0y + r1y) / 2;

    flatten_cubic_to_points(points, x0, y0, q0x, q0y, r0x, r0y, sx, sy, depth + 1);
    flatten_cubic_to_points(points, sx, sy, r1x, r1y, q2x, q2y, x3, y3, depth + 1);
}

/// Convert a polyline into a stroke outline (a filled polygon following
/// the path edge at distance `hw`).
///
/// For each segment, computes the perpendicular offset, creating two
/// parallel polylines (left and right). The stroke polygon is:
/// left[0], left[1], ..., left[n-1], right[n-1], ..., right[1], right[0]
fn stroke_subpath(
    path: &mut svg::SvgPath,
    points: &[(i32, i32)],
    hw: i32,
    _closed: bool,
) {
    if points.len() < 2 || path.num_commands >= 510 {
        return;
    }

    // Build offset points for left and right sides.
    let n = points.len();
    let mut left: alloc::vec::Vec<(i32, i32)> = alloc::vec::Vec::with_capacity(n);
    let mut right: alloc::vec::Vec<(i32, i32)> = alloc::vec::Vec::with_capacity(n);

    for i in 0..n {
        // Compute normal at this point (average of adjacent segment normals).
        let (nx, ny) = if i == 0 {
            seg_normal(points[0], points[1])
        } else if i == n - 1 {
            seg_normal(points[n - 2], points[n - 1])
        } else {
            let (n1x, n1y) = seg_normal(points[i - 1], points[i]);
            let (n2x, n2y) = seg_normal(points[i], points[i + 1]);
            let avg_x = (n1x + n2x) / 2;
            let avg_y = (n1y + n2y) / 2;
            // Renormalize (approximate).
            let len_sq = avg_x as i64 * avg_x as i64 + avg_y as i64 * avg_y as i64;
            if len_sq > 0 {
                let len = isqrt_i64(len_sq);
                if len > 0 {
                    ((avg_x as i64 * 256 / len) as i32, (avg_y as i64 * 256 / len) as i32)
                } else {
                    (n1x, n1y)
                }
            } else {
                (n1x, n1y)
            }
        };

        let px = points[i].0;
        let py = points[i].1;
        left.push((px + (nx as i64 * hw as i64 / 256) as i32, py + (ny as i64 * hw as i64 / 256) as i32));
        right.push((px - (nx as i64 * hw as i64 / 256) as i32, py - (ny as i64 * hw as i64 / 256) as i32));
    }

    // Emit the stroke polygon: left side forward, right side backward.
    let capacity_needed = left.len() + right.len() + 2;
    if path.num_commands + capacity_needed > 512 {
        return;
    }

    path.commands[path.num_commands] = svg::SvgCommand::MoveTo { x: left[0].0, y: left[0].1 };
    path.num_commands += 1;
    for i in 1..left.len() {
        path.commands[path.num_commands] = svg::SvgCommand::LineTo { x: left[i].0, y: left[i].1 };
        path.num_commands += 1;
    }
    for i in (0..right.len()).rev() {
        path.commands[path.num_commands] = svg::SvgCommand::LineTo { x: right[i].0, y: right[i].1 };
        path.num_commands += 1;
    }
    path.commands[path.num_commands] = svg::SvgCommand::Close;
    path.num_commands += 1;
}

/// Compute the perpendicular normal to a segment (unit-length in 24.8 FP).
/// Returns (nx, ny) where the normal points to the "left" side of the
/// direction from p0 to p1.
fn seg_normal(p0: (i32, i32), p1: (i32, i32)) -> (i32, i32) {
    let dx = (p1.0 - p0.0) as i64;
    let dy = (p1.1 - p0.1) as i64;
    let len = isqrt_i64(dx * dx + dy * dy);
    if len == 0 {
        return (0, 256);
    }
    // Normal perpendicular to (dx, dy) is (-dy, dx), normalized.
    let nx = (-dy * 256 / len) as i32;
    let ny = (dx * 256 / len) as i32;
    (nx, ny)
}

/// Integer square root via Newton's method for i64.
fn isqrt_i64(x: i64) -> i64 {
    if x <= 0 {
        return 0;
    }
    let mut guess = x;
    loop {
        let next = (guess + x / guess) / 2;
        if next >= guess {
            return guess;
        }
        guess = next;
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
