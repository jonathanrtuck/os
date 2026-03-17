//! Scene graph renderer: walks a tree of `scene::Node` and draws to a Surface.
//!
//! Text rendering uses shaped glyph arrays from the scene graph. Each Glyphs
//! node stores an array of `ShapedGlyph` in the data buffer. The renderer
//! reads glyph IDs and advances, rasterizes via the glyph cache (pre-populated
//! for monospace ASCII, on-demand via LRU for other glyphs), and composites
//! via `draw_coverage`.

use alloc::vec;

use drawing::{Color, PixelFormat, Surface};
use fonts::cache::GlyphCache;
use scene::{Content, Node, NodeId, ShapedGlyph, NULL};

use crate::surface_pool::SurfacePool;

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

    // Draw content.
    match node.content {
        Content::None => {}
        Content::Path {
            color,
            fill_rule,
            contours,
        } => {
            render_path(fb, graph, ctx, contours, color, fill_rule, draw_x, draw_y, nw, nh);
        }
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            ..
        } => {
            // Read ShapedGlyph array from the data buffer.
            let glyph_size = core::mem::size_of::<ShapedGlyph>();
            let shaped_glyphs: &[ShapedGlyph] = if glyphs.length > 0
                && (glyphs.offset as usize + glyphs.length as usize) <= graph.data.len()
                && glyphs.length as usize >= glyph_size
            {
                let bytes =
                    &graph.data[glyphs.offset as usize..][..glyphs.length as usize];
                let count = (glyph_count as usize).min(bytes.len() / glyph_size);
                // SAFETY: ShapedGlyph is #[repr(C)], data buffer is aligned
                // by push_shaped_glyphs to ShapedGlyph alignment.
                unsafe {
                    core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count)
                }
            } else {
                &[]
            };
            let glyph_color = scene_to_draw_color(color);
            let cache = ctx.mono_cache;
            let gx0 = draw_x;
            let gy0 = draw_y;

            let mut cx = gx0;

            for sg in shaped_glyphs {
                if let Some((glyph, coverage)) = cache.get(sg.glyph_id) {
                    let px = cx + glyph.bearing_x;
                    let py = gy0 + (cache.ascent as i32 - glyph.bearing_y);

                    fb.draw_coverage(px, py, coverage, glyph.width, glyph.height, glyph_color);
                }

                cx += scale_coord(sg.x_advance as i32, s);
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

// ── Path rasterization ──────────────────────────────────────────────

/// Maximum line segments after cubic Bezier flattening for a single Path node.
const PATH_MAX_SEGMENTS: usize = 4096;

/// Maximum active edges during scanline sweep.
const PATH_MAX_ACTIVE: usize = 256;

/// Fixed-point precision for path rasterization (20.12 format).
const PATH_FP_SHIFT: i32 = 12;
const PATH_FP_ONE: i32 = 1 << PATH_FP_SHIFT;

/// Vertical oversampling for anti-aliased path edges.
const PATH_OVERSAMPLE_Y: i32 = 8;

/// A line segment in physical pixel fixed-point coordinates.
#[derive(Clone, Copy)]
struct PathSegment {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

/// Read an f32 from a byte slice at the given byte offset (little-endian).
#[inline]
fn read_f32_le(data: &[u8], offset: usize) -> f32 {
    if offset + 4 > data.len() {
        return 0.0;
    }
    f32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a u32 from a byte slice at the given byte offset (little-endian).
#[inline]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() {
        return u32::MAX;
    }
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Convert a float coordinate to fixed-point.
#[inline]
fn f32_to_fp(v: f32) -> i32 {
    (v * PATH_FP_ONE as f32) as i32
}

/// Flatten a cubic Bezier curve to line segments using recursive subdivision.
/// Error threshold is ~0.25 physical pixels (in fixed-point).
fn flatten_cubic(
    x0: i32,
    y0: i32,
    c1x: i32,
    c1y: i32,
    c2x: i32,
    c2y: i32,
    x3: i32,
    y3: i32,
    segments: &mut [PathSegment],
    num_segments: &mut usize,
    depth: u32,
) {
    if *num_segments >= segments.len() {
        return;
    }
    // Flatness test: maximum deviation of control points from the chord.
    // Uses the distance from each control point to the line (x0,y0)→(x3,y3).
    let dx = x3 - x0;
    let dy = y3 - y0;
    // Compute perpendicular distances of control points from chord.
    let d1 = ((c1x - x0) as i64 * dy as i64 - (c1y - y0) as i64 * dx as i64).abs();
    let d2 = ((c2x - x0) as i64 * dy as i64 - (c2y - y0) as i64 * dx as i64).abs();
    let chord_len_sq = dx as i64 * dx as i64 + dy as i64 * dy as i64;
    // threshold: 0.25 pixels in fixed-point = PATH_FP_ONE / 4
    let threshold_fp = (PATH_FP_ONE / 4) as i128;
    let max_d = if d1 > d2 { d1 } else { d2 };
    // Flatness check: max_d^2 <= threshold^2 * chord_len  (avoid sqrt)
    // Use i128 to prevent overflow with large fixed-point coordinates.
    let flat = if chord_len_sq == 0 {
        max_d as i128 <= threshold_fp
    } else {
        depth >= 8
            || (max_d as i128 * max_d as i128)
                <= threshold_fp * threshold_fp * chord_len_sq as i128
    };

    if flat || depth >= 10 {
        if y0 != y3 {
            segments[*num_segments] = PathSegment {
                x0,
                y0,
                x1: x3,
                y1: y3,
            };
            *num_segments += 1;
        }
        return;
    }

    // De Casteljau subdivision at t=0.5.
    let m01x = (x0 + c1x) >> 1;
    let m01y = (y0 + c1y) >> 1;
    let m12x = (c1x + c2x) >> 1;
    let m12y = (c1y + c2y) >> 1;
    let m23x = (c2x + x3) >> 1;
    let m23y = (c2y + y3) >> 1;
    let m012x = (m01x + m12x) >> 1;
    let m012y = (m01y + m12y) >> 1;
    let m123x = (m12x + m23x) >> 1;
    let m123y = (m12y + m23y) >> 1;
    let mx = (m012x + m123x) >> 1;
    let my = (m012y + m123y) >> 1;

    flatten_cubic(x0, y0, m01x, m01y, m012x, m012y, mx, my, segments, num_segments, depth + 1);
    flatten_cubic(mx, my, m123x, m123y, m23x, m23y, x3, y3, segments, num_segments, depth + 1);
}

/// Rasterize path contours to a coverage buffer using scanline sweep.
///
/// `segments` are in physical pixel fixed-point coordinates.
/// Coverage buffer is 1 byte per pixel, `width × height`.
fn path_scanline_fill(
    segments: &[PathSegment],
    num_segments: usize,
    coverage: &mut [u8],
    width: u32,
    height: u32,
    fill_rule: scene::FillRule,
) {
    if num_segments == 0 || width == 0 || height == 0 {
        return;
    }

    let mut active_x = vec![0i32; PATH_MAX_ACTIVE];
    let mut active_dir = vec![0i32; PATH_MAX_ACTIVE];

    for row in 0..height {
        let y_top_fp = row as i32 * PATH_FP_ONE;
        let sub_step = PATH_FP_ONE / PATH_OVERSAMPLE_Y;

        for sub in 0..PATH_OVERSAMPLE_Y {
            let scan_y = y_top_fp + sub * sub_step + sub_step / 2;
            let mut num_active = 0usize;

            // Find active edges for this scanline.
            for si in 0..num_segments {
                let seg = &segments[si];
                let (y_top, y_bot, x_top, x_bot, dir) = if seg.y0 < seg.y1 {
                    (seg.y0, seg.y1, seg.x0, seg.x1, 1i32)
                } else {
                    (seg.y1, seg.y0, seg.x1, seg.x0, -1i32)
                };
                if y_top > scan_y || y_bot <= scan_y {
                    continue;
                }
                if num_active >= PATH_MAX_ACTIVE {
                    break;
                }
                let dy = y_bot - y_top;
                let t = scan_y - y_top;
                let x = if dy == 0 {
                    x_top
                } else {
                    x_top + ((x_bot - x_top) as i64 * t as i64 / dy as i64) as i32
                };
                active_x[num_active] = x;
                active_dir[num_active] = dir;
                num_active += 1;
            }

            // Sort active edges by x (insertion sort — small N).
            for i in 1..num_active {
                let key_x = active_x[i];
                let key_dir = active_dir[i];
                let mut j = i;
                while j > 0 && active_x[j - 1] > key_x {
                    active_x[j] = active_x[j - 1];
                    active_dir[j] = active_dir[j - 1];
                    j -= 1;
                }
                active_x[j] = key_x;
                active_dir[j] = key_dir;
            }

            // Apply fill rule.
            let contribution = (256 / PATH_OVERSAMPLE_Y) as u16;

            match fill_rule {
                scene::FillRule::Winding => {
                    // Winding rule: fill spans where winding != 0.
                    let mut winding: i32 = 0;
                    let mut edge_idx = 0;
                    while edge_idx < num_active {
                        let old_winding = winding;
                        winding += active_dir[edge_idx];
                        if old_winding == 0 && winding != 0 {
                            let x_start = active_x[edge_idx];
                            let mut ei = edge_idx + 1;
                            while ei < num_active {
                                winding += active_dir[ei];
                                if winding == 0 {
                                    let x_end = active_x[ei];
                                    path_fill_span(
                                        coverage,
                                        width,
                                        row,
                                        x_start,
                                        x_end,
                                        contribution,
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
                scene::FillRule::EvenOdd => {
                    // Even-odd rule: toggle fill at each edge crossing.
                    let mut i = 0;
                    while i + 1 < num_active {
                        let x_start = active_x[i];
                        let x_end = active_x[i + 1];
                        path_fill_span(coverage, width, row, x_start, x_end, contribution);
                        i += 2;
                    }
                }
            }
        }
    }
}

/// Fill a horizontal span in the coverage buffer with anti-aliased edges.
fn path_fill_span(
    coverage: &mut [u8],
    width: u32,
    row: u32,
    x_start_fp: i32,
    x_end_fp: i32,
    contribution: u16,
) {
    let px_start = x_start_fp >> PATH_FP_SHIFT;
    let px_end_raw = x_end_fp + PATH_FP_ONE - 1;
    let px_end = px_end_raw >> PATH_FP_SHIFT;
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
        if idx >= coverage.len() {
            break;
        }
        let cov = if px as i32 == (x_start_fp >> PATH_FP_SHIFT)
            && px as i32 == ((x_end_fp - 1) >> PATH_FP_SHIFT)
        {
            let frac = x_end_fp - x_start_fp;
            (contribution as i32 * frac / PATH_FP_ONE) as u16
        } else if px as i32 == (x_start_fp >> PATH_FP_SHIFT) {
            let right_edge = ((px + 1) as i32) << PATH_FP_SHIFT;
            let frac = right_edge - x_start_fp;
            (contribution as i32 * frac / PATH_FP_ONE) as u16
        } else if px as i32 == ((x_end_fp - 1) >> PATH_FP_SHIFT) {
            let left_edge = (px as i32) << PATH_FP_SHIFT;
            let frac = x_end_fp - left_edge;
            (contribution as i32 * frac / PATH_FP_ONE) as u16
        } else {
            contribution
        };
        let val = coverage[idx] as u16 + cov;
        coverage[idx] = if val > 255 { 255 } else { val as u8 };
    }
}

/// Render a `Content::Path` node: read contour commands, flatten cubics,
/// scanline fill with AA, composite onto framebuffer.
fn render_path(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    contours: scene::DataRef,
    color: scene::Color,
    fill_rule: scene::FillRule,
    draw_x: i32,
    draw_y: i32,
    nw: i32,
    nh: i32,
) {
    if contours.length == 0 || nw <= 0 || nh <= 0 {
        return;
    }

    let data = if (contours.offset as usize + contours.length as usize) <= graph.data.len() {
        &graph.data[contours.offset as usize..][..contours.length as usize]
    } else {
        return;
    };

    let s = ctx.scale;

    // Parse path commands and build segment list in physical pixel fixed-point.
    let mut segments = vec![PathSegment { x0: 0, y0: 0, x1: 0, y1: 0 }; PATH_MAX_SEGMENTS];
    let mut num_segments = 0usize;

    let mut cursor_x = 0i32;
    let mut cursor_y = 0i32;
    let mut contour_start_x = 0i32;
    let mut contour_start_y = 0i32;

    let mut offset = 0usize;
    while offset < data.len() {
        let tag = read_u32_le(data, offset);
        match tag {
            scene::PATH_MOVE_TO => {
                if offset + scene::PATH_MOVE_TO_SIZE > data.len() {
                    break;
                }
                let x = read_f32_le(data, offset + 4);
                let y = read_f32_le(data, offset + 8);
                cursor_x = f32_to_fp(x * s);
                cursor_y = f32_to_fp(y * s);
                contour_start_x = cursor_x;
                contour_start_y = cursor_y;
                offset += scene::PATH_MOVE_TO_SIZE;
            }
            scene::PATH_LINE_TO => {
                if offset + scene::PATH_LINE_TO_SIZE > data.len() {
                    break;
                }
                let x = read_f32_le(data, offset + 4);
                let y = read_f32_le(data, offset + 8);
                let nx = f32_to_fp(x * s);
                let ny = f32_to_fp(y * s);
                if cursor_y != ny && num_segments < PATH_MAX_SEGMENTS {
                    segments[num_segments] = PathSegment {
                        x0: cursor_x,
                        y0: cursor_y,
                        x1: nx,
                        y1: ny,
                    };
                    num_segments += 1;
                }
                cursor_x = nx;
                cursor_y = ny;
                offset += scene::PATH_LINE_TO_SIZE;
            }
            scene::PATH_CUBIC_TO => {
                if offset + scene::PATH_CUBIC_TO_SIZE > data.len() {
                    break;
                }
                let c1x = read_f32_le(data, offset + 4);
                let c1y = read_f32_le(data, offset + 8);
                let c2x = read_f32_le(data, offset + 12);
                let c2y = read_f32_le(data, offset + 16);
                let x = read_f32_le(data, offset + 20);
                let y = read_f32_le(data, offset + 24);
                flatten_cubic(
                    cursor_x,
                    cursor_y,
                    f32_to_fp(c1x * s),
                    f32_to_fp(c1y * s),
                    f32_to_fp(c2x * s),
                    f32_to_fp(c2y * s),
                    f32_to_fp(x * s),
                    f32_to_fp(y * s),
                    &mut segments,
                    &mut num_segments,
                    0,
                );
                cursor_x = f32_to_fp(x * s);
                cursor_y = f32_to_fp(y * s);
                offset += scene::PATH_CUBIC_TO_SIZE;
            }
            scene::PATH_CLOSE => {
                // Close: line back to contour start.
                if cursor_y != contour_start_y && num_segments < PATH_MAX_SEGMENTS {
                    segments[num_segments] = PathSegment {
                        x0: cursor_x,
                        y0: cursor_y,
                        x1: contour_start_x,
                        y1: contour_start_y,
                    };
                    num_segments += 1;
                }
                cursor_x = contour_start_x;
                cursor_y = contour_start_y;
                offset += scene::PATH_CLOSE_SIZE;
            }
            _ => break, // Unknown command — stop parsing.
        }
    }

    // Implicit close: if cursor is not at contour start, close it.
    if (cursor_x != contour_start_x || cursor_y != contour_start_y)
        && cursor_y != contour_start_y
        && num_segments < PATH_MAX_SEGMENTS
    {
        segments[num_segments] = PathSegment {
            x0: cursor_x,
            y0: cursor_y,
            x1: contour_start_x,
            y1: contour_start_y,
        };
        num_segments += 1;
    }

    if num_segments == 0 {
        return;
    }

    // Compute bounding box of all segments in physical pixels.
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for i in 0..num_segments {
        let seg = &segments[i];
        let sx0 = seg.x0 >> PATH_FP_SHIFT;
        let sy0 = seg.y0 >> PATH_FP_SHIFT;
        let sx1 = seg.x1 >> PATH_FP_SHIFT;
        let sy1 = seg.y1 >> PATH_FP_SHIFT;
        if sx0 < min_x { min_x = sx0; }
        if sx1 < min_x { min_x = sx1; }
        if sy0 < min_y { min_y = sy0; }
        if sy1 < min_y { min_y = sy1; }
        if sx0 > max_x { max_x = sx0; }
        if sx1 > max_x { max_x = sx1; }
        if sy0 > max_y { max_y = sy0; }
        if sy1 > max_y { max_y = sy1; }
    }
    // Add 1-pixel margin for AA.
    min_x -= 1;
    min_y -= 1;
    max_x += 2;
    max_y += 2;

    // Clamp to node bounds (physical pixels).
    if min_x < 0 { min_x = 0; }
    if min_y < 0 { min_y = 0; }

    let cov_w = (max_x - min_x) as u32;
    let cov_h = (max_y - min_y) as u32;
    if cov_w == 0 || cov_h == 0 {
        return;
    }

    // Cap allocation to prevent OOM.
    let cov_size = cov_w as usize * cov_h as usize;
    if cov_size > 4 * 1024 * 1024 {
        return;
    }

    // Translate segments so (min_x, min_y) becomes origin of coverage buffer.
    let off_x = min_x * PATH_FP_ONE;
    let off_y = min_y * PATH_FP_ONE;
    for i in 0..num_segments {
        segments[i].x0 -= off_x;
        segments[i].y0 -= off_y;
        segments[i].x1 -= off_x;
        segments[i].y1 -= off_y;
    }

    // Rasterize into coverage buffer.
    let mut coverage = vec![0u8; cov_size];
    path_scanline_fill(
        &segments,
        num_segments,
        &mut coverage,
        cov_w,
        cov_h,
        fill_rule,
    );

    // Composite coverage onto framebuffer.
    let path_color = scene_to_draw_color(color);
    let blit_x = draw_x + min_x;
    let blit_y = draw_y + min_y;
    fb.draw_coverage(blit_x, blit_y, &coverage, cov_w, cov_h, path_color);
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
