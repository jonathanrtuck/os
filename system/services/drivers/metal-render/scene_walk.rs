//! Scene graph tree walk and vertex emission helpers.

use alloc::vec::Vec;

use protocol::metal;
use scene::{Content, Node, NodeFlags, NodeId, NULL};

use crate::{
    atlas::{GlyphAtlas, ATLAS_HEIGHT, ATLAS_WIDTH},
    dma::DmaBuf,
    path::{draw_path_stencil_cover, parse_path_to_points, PathPointsBuf},
    round_font_size,
    virtio_helpers::send_setup,
    DSS_CLIP_TEST, DSS_NONE, DSS_STENCIL_WRITE, IMG_TEX_DIM, MAX_INLINE_BYTES, PIPE_GLYPH,
    PIPE_ROUNDED_RECT, PIPE_SHADOW, PIPE_SOLID, PIPE_SOLID_NO_MSAA, PIPE_STENCIL_WRITE,
    PIPE_TEXTURED, SAMPLER_LINEAR, SAMPLER_NEAREST, TEX_ATLAS, TEX_IMAGE, VERTEX_BYTES,
};

// ── Clip rectangle ──────────────────────────────────────────────────────

/// Clip rectangle in points (pre-scale).
#[derive(Clone, Copy)]
pub(crate) struct ClipRect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

impl ClipRect {
    pub(crate) fn intersect(&self, other: &ClipRect) -> ClipRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = (self.x + self.w).min(other.x + other.w);
        let b = (self.y + self.h).min(other.y + other.h);
        ClipRect {
            x,
            y,
            w: (r - x).max(0.0),
            h: (b - y).max(0.0),
        }
    }

    fn to_pixel_scissor(&self, scale: f32) -> (u16, u16, u16, u16) {
        let w_px = self.w * scale;
        let h_px = self.h * scale;
        (
            (self.x * scale) as u16,
            (self.y * scale) as u16,
            // Manual ceil: if fractional part > 0, round up.
            (w_px as u16) + if w_px > w_px as u16 as f32 { 1 } else { 0 },
            (h_px as u16) + if h_px > h_px as u16 as f32 { 1 } else { 0 },
        )
    }
}

// ── Image atlas ─────────────────────────────────────────────────────────

/// Per-frame image atlas packer. Each Content::InlineImage uploads to the next
/// available sub-rectangle within the shared 1024x1024 TEX_IMAGE texture.
/// Draws use matching UV coordinates, so no image overwrites another even
/// though uploads are synchronous and draws are deferred.
///
/// Simple row-based packing: images fill left-to-right in the current row.
/// When an image doesn't fit horizontally, advance to a new row (height =
/// tallest image in the previous row). Reset at frame start.
pub(crate) struct ImageAtlas {
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
}

impl ImageAtlas {
    pub(crate) fn new() -> Self {
        Self {
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
        }
    }

    /// Reserve a sub-rectangle for an image. Returns (x, y) atlas offset
    /// in pixels, or None if the image doesn't fit.
    pub(crate) fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > IMG_TEX_DIM || h > IMG_TEX_DIM {
            return None;
        }
        // Doesn't fit in current row — start a new one.
        if self.cursor_x + w > IMG_TEX_DIM {
            self.cursor_y += self.row_height;
            self.cursor_x = 0;
            self.row_height = 0;
        }
        // Doesn't fit vertically — atlas is full.
        if self.cursor_y + h > IMG_TEX_DIM {
            return None;
        }
        let x = self.cursor_x;
        let y = self.cursor_y;
        self.cursor_x += w;
        if h > self.row_height {
            self.row_height = h;
        }
        Some((x, y))
    }
}

// ── Blur request ────────────────────────────────────────────────────────

/// Collected backdrop blur request.
pub(crate) struct BlurReq {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    pub(crate) radius: u8,
    pub(crate) bg: scene::Color,
}

pub(crate) const MAX_BLURS: usize = 4;

// ── RenderContext ───────────────────────────────────────────────────────

pub(crate) struct RenderContext<'a> {
    pub(crate) cmdbuf: &'a mut metal::CommandBuffer,
    pub(crate) solid_verts: &'a mut Vec<u8>,
    pub(crate) glyph_verts: &'a mut Vec<u8>,
    pub(crate) atlas: &'a GlyphAtlas,
    pub(crate) style_registry: &'a [protocol::content::StyleRegistryEntry],
    pub(crate) scale_factor: f32,
    pub(crate) blurs: &'a mut Vec<BlurReq>,
    pub(crate) device: &'a virtio::Device,
    pub(crate) setup_vq: &'a mut virtio::Virtqueue,
    pub(crate) irq_handle: sys::InterruptHandle,
    pub(crate) setup_dma: &'a DmaBuf,
    pub(crate) path_buf: &'a mut PathPointsBuf,
    pub(crate) image_atlas: &'a mut ImageAtlas,
    pub(crate) content_region: &'a [u8],
    pub(crate) vw: f32,
    pub(crate) vh: f32,
    pub(crate) scale: f32,
}

// ── Scene walk ──────────────────────────────────────────────────────────

pub(crate) fn walk_scene(
    nodes: &[Node],
    data_buf: &[u8],
    reader: &scene::TripleReader,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    clip: &ClipRect,
    ctx: &mut RenderContext,
) {
    if node_id == NULL || node_id as usize >= nodes.len() {
        return;
    }

    let node = &nodes[node_id as usize];

    if !node.flags.contains(NodeFlags::VISIBLE) {
        return;
    }

    let opacity = node.opacity as f32 / 255.0;
    if opacity <= 0.0 {
        return;
    }

    // Compute absolute position in points.
    // node.transform applies around the node's own origin (node.x, node.y in parent space).
    // For identity/pure-translation transforms, this collapses to a simple offset.
    let t = &node.transform;
    let node_origin_x = parent_x + scene::mpt_to_f32(node.x);
    let node_origin_y = parent_y + scene::mpt_to_f32(node.y);
    let w = scene::umpt_to_f32(node.width);
    let h = scene::umpt_to_f32(node.height);
    // abs_x/abs_y: for identity transforms, same as before. For non-trivial transforms,
    // we use the AABB of the transformed rect for clip/scissor purposes, but vertex
    // positions are computed per-corner through the transform.
    let (abs_x, abs_y) = if t.is_identity() {
        (node_origin_x, node_origin_y)
    } else if t.is_pure_translation() {
        (node_origin_x + t.tx, node_origin_y + t.ty)
    } else {
        // For rotation/scale/skew, abs_x/abs_y is the AABB top-left for clip purposes.
        let (bx, by, _, _) = t.transform_aabb(0.0, 0.0, w, h);
        (node_origin_x + bx, node_origin_y + by)
    };
    let has_nontrivial_transform = !t.is_identity() && !t.is_pure_translation();

    let vw = ctx.vw;
    let vh = ctx.vh;
    let scale = ctx.scale;

    // Collect backdrop blur request (processed after initial render pass).
    let is_blur_node = node.backdrop_blur_radius > 0;
    let has_clip_path = node.clip_path.length > 0;
    if is_blur_node && ctx.blurs.len() < MAX_BLURS {
        ctx.blurs.push(BlurReq {
            x: abs_x,
            y: abs_y,
            w,
            h,
            radius: node.backdrop_blur_radius,
            bg: node.background,
        });
    }

    // Flush any pending glyph vertices before drawing this node's solid content.
    // This ensures correct depth ordering: previous node's text is behind this
    // node's background.
    if !ctx.glyph_verts.is_empty() {
        flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
        ctx.cmdbuf.set_render_pipeline(PIPE_GLYPH);
        ctx.cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
        ctx.cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
        flush_vertices_raw(ctx.cmdbuf, ctx.glyph_verts);
        ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
    }

    // Draw shadow if present (behind everything else).
    // Uses an analytical Gaussian fragment shader: the exact Gaussian integral
    // for rectangles (separable erf), SDF+erfc approximation for rounded rects.
    let sc = node.shadow_color;
    if sc.a > 0 {
        let sx = abs_x + node.shadow_offset_x as f32 - node.shadow_spread as f32;
        let sy = abs_y + node.shadow_offset_y as f32 - node.shadow_spread as f32;
        let sw = w + node.shadow_spread as f32 * 2.0;
        let sh = h + node.shadow_spread as f32 * 2.0;

        // Gaussian sigma: blur_radius / 2 (W3C convention), in pixel space.
        let sigma_pt = node.shadow_blur_radius as f32 / 2.0;
        let sigma_px = sigma_pt * scale;

        if sigma_px > 0.0 {
            // Blurred shadow: switch to shadow pipeline, draw extended quad.
            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);

            // Shadow rect in pixel coordinates for the fragment shader.
            let params = pack_shadow_params(
                sx * scale,
                sy * scale,
                (sx + sw) * scale,
                (sy + sh) * scale,
                sc.r as f32 / 255.0,
                sc.g as f32 / 255.0,
                sc.b as f32 / 255.0,
                (sc.a as f32 / 255.0) * opacity,
                sigma_px,
                node.corner_radius as f32 * scale,
            );

            // Pad the quad by 3sigma to capture 99.7% of the Gaussian energy.
            let pad_pt = sigma_pt * 3.0;

            ctx.cmdbuf.set_render_pipeline(PIPE_SHADOW);
            ctx.cmdbuf.set_fragment_bytes(0, &params);
            emit_shadow_quad(ctx.solid_verts, sx, sy, sw, sh, pad_pt, vw, vh, scale);
            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
            ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
        } else {
            // Zero blur radius: hard shadow (flat solid quad).
            let sr = sc.r as f32 / 255.0;
            let sg = sc.g as f32 / 255.0;
            let sb = sc.b as f32 / 255.0;
            let sa = (sc.a as f32 / 255.0) * opacity;
            emit_quad(
                ctx.solid_verts,
                sx,
                sy,
                sw,
                sh,
                vw,
                vh,
                scale,
                sr,
                sg,
                sb,
                sa,
            );
            if ctx.solid_verts.len() + 6 * VERTEX_BYTES > MAX_INLINE_BYTES {
                flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
            }
        }
    }

    // Draw background if not transparent. Skip for blur nodes and clip_path
    // nodes — clip_path backgrounds are drawn after the stencil is set up.
    let bg = node.background;
    let corner_r = node.corner_radius;
    let has_border = node.border.width > 0 && node.border.color.a > 0;
    if bg.a > 0 && !is_blur_node && !has_clip_path {
        let r = bg.r as f32 / 255.0;
        let g = bg.g as f32 / 255.0;
        let b = bg.b as f32 / 255.0;
        let a = (bg.a as f32 / 255.0) * opacity;

        if has_nontrivial_transform && (corner_r > 0 || has_border) {
            // Transformed rounded rect: SDF evaluation in local space.
            // Vertex NDC positions are transformed; texCoords stay in local
            // pixel space. GPU interpolation is linear, so each fragment gets
            // the correct local-space coordinate for SDF evaluation.
            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
            let half_w_px = w * scale * 0.5;
            let half_h_px = h * scale * 0.5;
            let radius_px = corner_r as f32 * scale;
            let (bw_px, br, bg_b, bb, ba) = if has_border {
                let bc = node.border.color;
                (
                    node.border.width as f32 * scale,
                    bc.r as f32 / 255.0,
                    bc.g as f32 / 255.0,
                    bc.b as f32 / 255.0,
                    (bc.a as f32 / 255.0) * opacity,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let params =
                pack_rounded_rect_params(half_w_px, half_h_px, radius_px, bw_px, br, bg_b, bb, ba);
            ctx.cmdbuf.set_render_pipeline(PIPE_ROUNDED_RECT);
            ctx.cmdbuf.set_fragment_bytes(0, &params);
            let mut rrect_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
            emit_transformed_rounded_rect_quad(
                &mut rrect_verts,
                node_origin_x,
                node_origin_y,
                w,
                h,
                t,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            ctx.cmdbuf.set_vertex_bytes(0, &rrect_verts);
            ctx.cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
            ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
        } else if has_nontrivial_transform {
            // Transformed solid quad (no corner rounding, no border).
            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
            let mut xf_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
            emit_transformed_quad(
                &mut xf_verts,
                node_origin_x,
                node_origin_y,
                w,
                h,
                t,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            ctx.cmdbuf.set_vertex_bytes(0, &xf_verts);
            ctx.cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
        } else if corner_r > 0 || has_border {
            // SDF rounded rect: flush pending solid verts, switch pipeline,
            // set uniform params, draw, then switch back.
            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
            let half_w_px = w * scale * 0.5;
            let half_h_px = h * scale * 0.5;
            let radius_px = corner_r as f32 * scale;
            let (bw_px, br, bg_b, bb, ba) = if has_border {
                let bc = node.border.color;
                (
                    node.border.width as f32 * scale,
                    bc.r as f32 / 255.0,
                    bc.g as f32 / 255.0,
                    bc.b as f32 / 255.0,
                    (bc.a as f32 / 255.0) * opacity,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let params =
                pack_rounded_rect_params(half_w_px, half_h_px, radius_px, bw_px, br, bg_b, bb, ba);
            ctx.cmdbuf.set_render_pipeline(PIPE_ROUNDED_RECT);
            ctx.cmdbuf.set_fragment_bytes(0, &params);
            let mut rrect_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
            emit_rounded_rect_quad(
                &mut rrect_verts,
                abs_x,
                abs_y,
                w,
                h,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            ctx.cmdbuf.set_vertex_bytes(0, &rrect_verts);
            ctx.cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
            ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
        } else {
            emit_quad(
                ctx.solid_verts,
                abs_x,
                abs_y,
                w,
                h,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            // Flush if we're close to the 4KB limit.
            if ctx.solid_verts.len() + 6 * VERTEX_BYTES > MAX_INLINE_BYTES {
                flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
            }
        }
    } else if bg.a == 0 && has_border && !is_blur_node && !has_clip_path {
        // Border-only node (no fill): draw with transparent fill.
        flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
        let half_w_px = w * scale * 0.5;
        let half_h_px = h * scale * 0.5;
        let radius_px = corner_r as f32 * scale;
        let bc = node.border.color;
        let params = pack_rounded_rect_params(
            half_w_px,
            half_h_px,
            radius_px,
            node.border.width as f32 * scale,
            bc.r as f32 / 255.0,
            bc.g as f32 / 255.0,
            bc.b as f32 / 255.0,
            (bc.a as f32 / 255.0) * opacity,
        );
        ctx.cmdbuf.set_render_pipeline(PIPE_ROUNDED_RECT);
        ctx.cmdbuf.set_fragment_bytes(0, &params);
        let mut rrect_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
        if has_nontrivial_transform {
            emit_transformed_rounded_rect_quad(
                &mut rrect_verts,
                node_origin_x,
                node_origin_y,
                w,
                h,
                t,
                vw,
                vh,
                scale,
                0.0,
                0.0,
                0.0,
                0.0,
            );
        } else {
            emit_rounded_rect_quad(
                &mut rrect_verts,
                abs_x,
                abs_y,
                w,
                h,
                vw,
                vh,
                scale,
                0.0,
                0.0,
                0.0,
                0.0,
            );
        }
        ctx.cmdbuf.set_vertex_bytes(0, &rrect_verts);
        ctx.cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
        ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
    }

    // Draw content.
    match node.content {
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            font_size,
            style_id,
        } => {
            let shaped = reader.front_shaped_glyphs(glyphs, glyph_count);
            let r = color.r as f32 / 255.0;
            let g = color.g as f32 / 255.0;
            let b = color.b as f32 / 255.0;
            let a = (color.a as f32 / 255.0) * opacity;

            let atlas_w = ATLAS_WIDTH as f32;
            let atlas_h = ATLAS_HEIGHT as f32;

            // Walk glyphs with a pen cursor that accumulates x_advance.
            // x_advance and x_offset are in scaled points (NOT 26.6 fixed-point).
            let mut pen_x = abs_x;

            // Per-node baseline from style registry.
            let baseline_y =
                if let Some(entry) = ctx.style_registry.iter().find(|e| e.style_id == style_id) {
                    let upem = entry.upem as f32;
                    if upem > 0.0 {
                        abs_y + entry.ascent_fu as f32 * font_size as f32 / upem
                    } else {
                        abs_y + font_size as f32
                    }
                } else {
                    abs_y + font_size as f32
                };

            // Glyph atlas contains device-pixel-resolution bitmaps.
            // Divide bearing/width/height by scale to position in point space.
            let glyph_scale = scale;

            // 16.16 fixed-point to f32 conversion factor.
            let fp16 = 65536.0f32;

            // Per-node font_size_px for atlas lookup.
            let node_font_size_px = round_font_size(font_size, ctx.scale_factor);

            for sg in shaped {
                if let Some(entry) = ctx.atlas.lookup(sg.glyph_id, node_font_size_px, style_id) {
                    let gx =
                        pen_x + entry.bearing_x as f32 / glyph_scale + sg.x_offset as f32 / fp16;
                    let gy = baseline_y - entry.bearing_y as f32 / glyph_scale
                        + sg.y_offset as f32 / fp16;
                    let gw = entry.width as f32 / glyph_scale;
                    let gh = entry.height as f32 / glyph_scale;

                    // UV coordinates in atlas.
                    let u0 = entry.u as f32 / atlas_w;
                    let v0 = entry.v as f32 / atlas_h;
                    let u1 = (entry.u + entry.width) as f32 / atlas_w;
                    let v1 = (entry.v + entry.height) as f32 / atlas_h;

                    emit_textured_quad(
                        ctx.glyph_verts,
                        gx,
                        gy,
                        gw,
                        gh,
                        vw,
                        vh,
                        scale,
                        u0,
                        v0,
                        u1,
                        v1,
                        r,
                        g,
                        b,
                        a,
                    );

                    if ctx.glyph_verts.len() + 6 * VERTEX_BYTES > MAX_INLINE_BYTES {
                        flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
                        ctx.cmdbuf.set_render_pipeline(PIPE_GLYPH);
                        ctx.cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
                        ctx.cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
                        flush_vertices_raw(ctx.cmdbuf, ctx.glyph_verts);
                        ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
                    }
                }
                pen_x += sg.x_advance as f32 / fp16;
            }
        }
        Content::Path {
            color,
            stroke_color,
            fill_rule,
            stroke_width,
            contours,
        } => {
            if contours.length > 0 {
                // Fill pass: render filled path when fill color is non-transparent.
                if color.a > 0 {
                    draw_path_stencil_cover(
                        ctx.cmdbuf,
                        ctx.solid_verts,
                        data_buf,
                        contours,
                        color,
                        fill_rule,
                        abs_x,
                        abs_y,
                        w,
                        h,
                        vw,
                        vh,
                        scale,
                        opacity,
                        ctx.path_buf,
                    );
                }
                // Stroke pass: expand and render stroked outline on top.
                if stroke_width > 0 && stroke_color.a > 0 {
                    let offset = contours.offset as usize;
                    let end = offset + contours.length as usize;
                    if end <= data_buf.len() {
                        let src = &data_buf[offset..end];
                        let sw_pt = stroke_width as f32 / 256.0;
                        let expanded = scene::stroke::expand_stroke(src, sw_pt);
                        if !expanded.is_empty() {
                            let exp_ref = scene::DataRef {
                                offset: 0,
                                length: expanded.len() as u32,
                            };
                            draw_path_stencil_cover(
                                ctx.cmdbuf,
                                ctx.solid_verts,
                                &expanded,
                                exp_ref,
                                stroke_color,
                                scene::FillRule::Winding,
                                abs_x,
                                abs_y,
                                w,
                                h,
                                vw,
                                vh,
                                scale,
                                opacity,
                                ctx.path_buf,
                            );
                        }
                    }
                }
            }
        }
        Content::InlineImage {
            data,
            src_width,
            src_height,
        } => {
            let pixel_bytes = src_width as u32 * src_height as u32 * 4;
            let src_start = data.offset as usize;
            let src_end = src_start + pixel_bytes as usize;
            if data.length > 0 && src_width > 0 && src_height > 0 && src_end <= data_buf.len() {
                // Pack this image into the per-frame atlas. Each image
                // gets a unique sub-rectangle so deferred draw commands
                // sample the correct pixels from the shared TEX_IMAGE.
                if let Some((atlas_x, atlas_y)) = ctx
                    .image_atlas
                    .allocate(src_width as u32, src_height as u32)
                {
                    flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);

                    // Upload to the image's sub-rectangle in the atlas.
                    let mut setup_cmdbuf = metal::CommandBuffer::new();
                    setup_cmdbuf.upload_texture(
                        TEX_IMAGE,
                        atlas_x as u16,
                        atlas_y as u16,
                        src_width,
                        src_height,
                        src_width as u32 * 4,
                        &data_buf[src_start..src_end],
                    );
                    send_setup(
                        ctx.device,
                        ctx.setup_vq,
                        ctx.irq_handle,
                        ctx.setup_dma,
                        &setup_cmdbuf,
                    );

                    // UV coordinates into this image's atlas sub-rectangle.
                    let u0 = atlas_x as f32 / IMG_TEX_DIM as f32;
                    let v0 = atlas_y as f32 / IMG_TEX_DIM as f32;
                    let u1 = (atlas_x + src_width as u32) as f32 / IMG_TEX_DIM as f32;
                    let v1 = (atlas_y + src_height as u32) as f32 / IMG_TEX_DIM as f32;
                    ctx.cmdbuf.set_render_pipeline(PIPE_TEXTURED);
                    ctx.cmdbuf.set_fragment_texture(TEX_IMAGE, 0);
                    ctx.cmdbuf.set_fragment_sampler(SAMPLER_LINEAR, 0);
                    emit_textured_quad(
                        ctx.solid_verts,
                        abs_x,
                        abs_y,
                        w,
                        h,
                        vw,
                        vh,
                        scale,
                        u0,
                        v0,
                        u1,
                        v1,
                        1.0,
                        1.0,
                        1.0,
                        1.0,
                    );
                    flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
                    ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
                }
            }
        }
        Content::Image {
            content_id,
            src_width,
            src_height,
        } => {
            // Resolve content_id from the Content Region registry.
            if !ctx.content_region.is_empty()
                && ctx.content_region.len()
                    >= core::mem::size_of::<protocol::content::ContentRegionHeader>()
            {
                // SAFETY: content_region is page-aligned shared memory; header is repr(C).
                let header = unsafe {
                    &*(ctx.content_region.as_ptr() as *const protocol::content::ContentRegionHeader)
                };
                if let Some(entry) = protocol::content::find_entry(header, content_id) {
                    let start = entry.offset as usize;
                    let end = start + entry.length as usize;
                    if end <= ctx.content_region.len() && src_width > 0 && src_height > 0 {
                        let pixel_data = &ctx.content_region[start..end];
                        if let Some((atlas_x, atlas_y)) = ctx
                            .image_atlas
                            .allocate(src_width as u32, src_height as u32)
                        {
                            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);

                            let mut setup_cmdbuf = metal::CommandBuffer::new();
                            setup_cmdbuf.upload_texture(
                                TEX_IMAGE,
                                atlas_x as u16,
                                atlas_y as u16,
                                src_width,
                                src_height,
                                src_width as u32 * 4,
                                pixel_data,
                            );
                            send_setup(
                                ctx.device,
                                ctx.setup_vq,
                                ctx.irq_handle,
                                ctx.setup_dma,
                                &setup_cmdbuf,
                            );

                            let u0 = atlas_x as f32 / IMG_TEX_DIM as f32;
                            let v0 = atlas_y as f32 / IMG_TEX_DIM as f32;
                            let u1 = (atlas_x + src_width as u32) as f32 / IMG_TEX_DIM as f32;
                            let v1 = (atlas_y + src_height as u32) as f32 / IMG_TEX_DIM as f32;
                            ctx.cmdbuf.set_render_pipeline(PIPE_TEXTURED);
                            ctx.cmdbuf.set_fragment_texture(TEX_IMAGE, 0);
                            ctx.cmdbuf.set_fragment_sampler(SAMPLER_LINEAR, 0);
                            emit_textured_quad(
                                ctx.solid_verts,
                                abs_x,
                                abs_y,
                                w,
                                h,
                                vw,
                                vh,
                                scale,
                                u0,
                                v0,
                                u1,
                                v1,
                                1.0,
                                1.0,
                                1.0,
                                1.0,
                            );
                            flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
                            ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // Walk children with child_offset applied.
    let child_base_x = abs_x + node.child_offset_x;
    let child_base_y = abs_y + node.child_offset_y;

    // If this node clips children, set up clipping.
    let child_clip = if node.flags.contains(NodeFlags::CLIPS_CHILDREN) {
        let node_rect = ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        };
        let clipped = clip.intersect(&node_rect);

        // Flush pending vertices before changing clip state.
        flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
        if !ctx.glyph_verts.is_empty() {
            ctx.cmdbuf.set_render_pipeline(PIPE_GLYPH);
            ctx.cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
            ctx.cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
            flush_vertices_raw(ctx.cmdbuf, ctx.glyph_verts);
            ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
        }

        if has_clip_path {
            // Stencil-based clip: rasterize clip path to stencil buffer,
            // then all children draw with stencil test (pass where != 0).
            let cp = node.clip_path;
            let cp_off = cp.offset as usize;
            let cp_end = cp_off + cp.length as usize;

            if cp_end <= data_buf.len() {
                let cp_parsed = parse_path_to_points(&data_buf[cp_off..cp_end], ctx.path_buf);
                let n_pts = cp_parsed.n;

                if n_pts >= 3 {
                    // Build fan triangles for the clip path.
                    let mut fan_verts: Vec<u8> = Vec::with_capacity(n_pts * 3 * VERTEX_BYTES);
                    let mut cx_sum: f32 = 0.0;
                    let mut cy_sum: f32 = 0.0;
                    for i in 0..n_pts {
                        cx_sum += ctx.path_buf[i].0;
                        cy_sum += ctx.path_buf[i].1;
                    }
                    let centroid_x = cx_sum / n_pts as f32;
                    let centroid_y = cy_sum / n_pts as f32;

                    for i in 0..n_pts - 1 {
                        let (ax, ay) = ctx.path_buf[i];
                        let (bx, by) = ctx.path_buf[i + 1];
                        for &(px, py) in &[(centroid_x, centroid_y), (bx, by), (ax, ay)] {
                            let ndc_x = ((abs_x + px) * scale / vw) * 2.0 - 1.0;
                            let ndc_y = 1.0 - ((abs_y + py) * scale / vh) * 2.0;
                            fan_verts.extend_from_slice(&ndc_x.to_le_bytes());
                            fan_verts.extend_from_slice(&ndc_y.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&1.0f32.to_le_bytes()); // a=1 (non-zero)
                        }
                    }

                    // Write clip path to stencil.
                    ctx.cmdbuf.set_render_pipeline(PIPE_STENCIL_WRITE);
                    ctx.cmdbuf.set_depth_stencil_state(DSS_STENCIL_WRITE);
                    ctx.cmdbuf.set_stencil_ref(1);
                    let mut sent = 0;
                    while sent < fan_verts.len() {
                        let chunk_end = core::cmp::min(sent + MAX_INLINE_BYTES, fan_verts.len());
                        let chunk = &fan_verts[sent..chunk_end];
                        let vc = chunk.len() / VERTEX_BYTES;
                        ctx.cmdbuf.set_vertex_bytes(0, chunk);
                        ctx.cmdbuf
                            .draw_primitives(metal::PRIM_TRIANGLE, 0, vc as u32);
                        sent = chunk_end;
                    }

                    // Use stencil test for clipped children.
                    ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
                    ctx.cmdbuf.set_depth_stencil_state(DSS_CLIP_TEST);
                    ctx.cmdbuf.set_stencil_ref(0);

                    // Draw the clip node's own background inside the stencil.
                    if bg.a > 0 {
                        let r = bg.r as f32 / 255.0;
                        let g = bg.g as f32 / 255.0;
                        let b = bg.b as f32 / 255.0;
                        let a = (bg.a as f32 / 255.0) * opacity;
                        emit_quad(
                            ctx.solid_verts,
                            abs_x,
                            abs_y,
                            w,
                            h,
                            vw,
                            vh,
                            scale,
                            r,
                            g,
                            b,
                            a,
                        );
                        flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
                    }
                }
            }
        } else {
            // Rectangular scissor clip.
            let (sx, sy, sw, sh) = clipped.to_pixel_scissor(scale);
            ctx.cmdbuf.set_scissor(sx, sy, sw, sh);
        }
        clipped
    } else {
        *clip
    };

    let mut child = node.first_child;
    while child != NULL {
        walk_scene(
            nodes,
            data_buf,
            reader,
            child,
            child_base_x,
            child_base_y,
            &child_clip,
            ctx,
        );
        if child as usize >= nodes.len() {
            break;
        }
        child = nodes[child as usize].next_sibling;
    }

    // Restore clip state after children.
    if node.flags.contains(NodeFlags::CLIPS_CHILDREN) {
        flush_solid_vertices(ctx.cmdbuf, ctx.solid_verts);
        if !ctx.glyph_verts.is_empty() {
            ctx.cmdbuf.set_render_pipeline(PIPE_GLYPH);
            ctx.cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
            ctx.cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
            flush_vertices_raw(ctx.cmdbuf, ctx.glyph_verts);
            ctx.cmdbuf.set_render_pipeline(PIPE_SOLID);
        }

        if has_clip_path {
            // Clear stencil, restore normal DSA.
            ctx.cmdbuf.set_depth_stencil_state(DSS_NONE);
        } else {
            // Restore parent scissor.
            let (sx, sy, sw, sh) = clip.to_pixel_scissor(scale);
            ctx.cmdbuf.set_scissor(sx, sy, sw, sh);
        }
    }
}

// ── Vertex emission helpers ─────────────────────────────────────────────

/// Push a solid-color quad (6 vertices) into the vertex buffer.
pub(crate) fn emit_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    // Convert points to pixels, then to NDC.
    let l = (x * scale / vw) * 2.0 - 1.0;
    let r_ndc = ((x + w) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (y * scale / vh) * 2.0;
    let b_ndc = 1.0 - ((y + h) * scale / vh) * 2.0;

    // 6 vertices: two triangles.
    let verts: [[f32; 8]; 6] = [
        [l, t, 0.0, 0.0, r, g, b, a],
        [r_ndc, t, 1.0, 0.0, r, g, b, a],
        [l, b_ndc, 0.0, 1.0, r, g, b, a],
        [r_ndc, t, 1.0, 0.0, r, g, b, a],
        [r_ndc, b_ndc, 1.0, 1.0, r, g, b, a],
        [l, b_ndc, 0.0, 1.0, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a textured quad (6 vertices) with custom UV coordinates.
pub(crate) fn emit_textured_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = (x * scale / vw) * 2.0 - 1.0;
    let r_ndc = ((x + w) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (y * scale / vh) * 2.0;
    let b_ndc = 1.0 - ((y + h) * scale / vh) * 2.0;

    let verts: [[f32; 8]; 6] = [
        [l, t, u0, v0, r, g, b, a],
        [r_ndc, t, u1, v0, r, g, b, a],
        [l, b_ndc, u0, v1, r, g, b, a],
        [r_ndc, t, u1, v0, r, g, b, a],
        [r_ndc, b_ndc, u1, v1, r, g, b, a],
        [l, b_ndc, u0, v1, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a solid-color quad with an affine transform applied to each vertex.
/// The transform maps local coordinates (0,0)->(w,h) to parent space at (ox, oy).
fn emit_transformed_quad(
    buf: &mut Vec<u8>,
    ox: f32,
    oy: f32,
    w: f32,
    h: f32,
    t: &scene::AffineTransform,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    // Transform 4 corners of the local rect through the affine, then offset to parent space.
    let corners = [(0.0f32, 0.0f32), (w, 0.0), (0.0, h), (w, h)];
    let mut tc = [(0.0f32, 0.0f32); 4];
    for (i, &(lx, ly)) in corners.iter().enumerate() {
        let (tx, ty) = t.transform_point(lx, ly);
        tc[i] = (ox + tx, oy + ty);
    }
    // Convert to NDC.
    let to_ndc = |px: f32, py: f32| -> (f32, f32) {
        ((px * scale / vw) * 2.0 - 1.0, 1.0 - (py * scale / vh) * 2.0)
    };
    let (x0, y0) = to_ndc(tc[0].0, tc[0].1);
    let (x1, y1) = to_ndc(tc[1].0, tc[1].1);
    let (x2, y2) = to_ndc(tc[2].0, tc[2].1);
    let (x3, y3) = to_ndc(tc[3].0, tc[3].1);

    // Two triangles: (0,1,2) and (1,3,2).
    let verts: [[f32; 8]; 6] = [
        [x0, y0, 0.0, 0.0, r, g, b, a],
        [x1, y1, 1.0, 0.0, r, g, b, a],
        [x2, y2, 0.0, 1.0, r, g, b, a],
        [x1, y1, 1.0, 0.0, r, g, b, a],
        [x3, y3, 1.0, 1.0, r, g, b, a],
        [x2, y2, 0.0, 1.0, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a rounded-rect quad. texCoord carries local pixel coordinates
/// relative to the rect center (the SDF evaluates per-pixel in local space).
fn emit_rounded_rect_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = (x * scale / vw) * 2.0 - 1.0;
    let r_ndc = ((x + w) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (y * scale / vh) * 2.0;
    let b_ndc = 1.0 - ((y + h) * scale / vh) * 2.0;

    // Local pixel coords: center of rect = (0,0), corners at (+/-half_w_px, +/-half_h_px).
    let half_w_px = w * scale * 0.5;
    let half_h_px = h * scale * 0.5;

    let verts: [[f32; 8]; 6] = [
        [l, t, -half_w_px, -half_h_px, r, g, b, a],
        [r_ndc, t, half_w_px, -half_h_px, r, g, b, a],
        [l, b_ndc, -half_w_px, half_h_px, r, g, b, a],
        [r_ndc, t, half_w_px, -half_h_px, r, g, b, a],
        [r_ndc, b_ndc, half_w_px, half_h_px, r, g, b, a],
        [l, b_ndc, -half_w_px, half_h_px, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a rounded-rect quad with an affine transform applied to vertex positions.
///
/// Like `emit_transformed_quad`, the 4 corners are transformed through the
/// affine to produce NDC positions. But unlike a solid quad, texCoords carry
/// LOCAL pixel coordinates (center-relative), identical to `emit_rounded_rect_quad`.
///
/// Because the affine is linear and GPU barycentric interpolation is linear,
/// each fragment receives the correct local-space coordinate. The SDF shader
/// evaluates in that local space — no shader changes needed.
fn emit_transformed_rounded_rect_quad(
    buf: &mut Vec<u8>,
    ox: f32,
    oy: f32,
    w: f32,
    h: f32,
    t: &scene::AffineTransform,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    // Transform 4 corners of the local rect, offset to parent space.
    let corners = [
        (0.0f32, 0.0f32), // top-left
        (w, 0.0),         // top-right
        (0.0, h),         // bottom-left
        (w, h),           // bottom-right
    ];
    let mut ndc = [(0.0f32, 0.0f32); 4];
    for (i, &(lx, ly)) in corners.iter().enumerate() {
        let (tx, ty) = t.transform_point(lx, ly);
        let px = ox + tx;
        let py = oy + ty;
        ndc[i] = ((px * scale / vw) * 2.0 - 1.0, 1.0 - (py * scale / vh) * 2.0);
    }

    // Local pixel coords: center of rect = (0,0), same as axis-aligned version.
    let half_w_px = w * scale * 0.5;
    let half_h_px = h * scale * 0.5;

    // Two triangles: (0,1,2) and (1,3,2).
    // NDC positions are transformed; texCoords are in local (pre-transform) space.
    let verts: [[f32; 8]; 6] = [
        [ndc[0].0, ndc[0].1, -half_w_px, -half_h_px, r, g, b, a],
        [ndc[1].0, ndc[1].1, half_w_px, -half_h_px, r, g, b, a],
        [ndc[2].0, ndc[2].1, -half_w_px, half_h_px, r, g, b, a],
        [ndc[1].0, ndc[1].1, half_w_px, -half_h_px, r, g, b, a],
        [ndc[3].0, ndc[3].1, half_w_px, half_h_px, r, g, b, a],
        [ndc[2].0, ndc[2].1, -half_w_px, half_h_px, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Pack RoundedRectParams for the fragment_rounded_rect shader.
/// Layout: { half_w, half_h, radius, border_w, border_r, border_g, border_b, border_a } (8 x f32 = 32 bytes).
fn pack_rounded_rect_params(
    half_w: f32,
    half_h: f32,
    radius: f32,
    border_w: f32,
    border_r: f32,
    border_g: f32,
    border_b: f32,
    border_a: f32,
) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..4].copy_from_slice(&half_w.to_le_bytes());
    buf[4..8].copy_from_slice(&half_h.to_le_bytes());
    buf[8..12].copy_from_slice(&radius.to_le_bytes());
    buf[12..16].copy_from_slice(&border_w.to_le_bytes());
    buf[16..20].copy_from_slice(&border_r.to_le_bytes());
    buf[20..24].copy_from_slice(&border_g.to_le_bytes());
    buf[24..28].copy_from_slice(&border_b.to_le_bytes());
    buf[28..32].copy_from_slice(&border_a.to_le_bytes());
    buf
}

/// Pack ShadowParams for the fragment_shadow shader.
/// Layout: { rect_min_x, rect_min_y, rect_max_x, rect_max_y,
///           color_r, color_g, color_b, color_a,
///           sigma, corner_radius, _pad0, _pad1 } (12 x f32 = 48 bytes).
fn pack_shadow_params(
    rect_min_x: f32,
    rect_min_y: f32,
    rect_max_x: f32,
    rect_max_y: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
    sigma: f32,
    corner_radius: f32,
) -> [u8; 48] {
    let mut buf = [0u8; 48];
    buf[0..4].copy_from_slice(&rect_min_x.to_le_bytes());
    buf[4..8].copy_from_slice(&rect_min_y.to_le_bytes());
    buf[8..12].copy_from_slice(&rect_max_x.to_le_bytes());
    buf[12..16].copy_from_slice(&rect_max_y.to_le_bytes());
    buf[16..20].copy_from_slice(&color_r.to_le_bytes());
    buf[20..24].copy_from_slice(&color_g.to_le_bytes());
    buf[24..28].copy_from_slice(&color_b.to_le_bytes());
    buf[28..32].copy_from_slice(&color_a.to_le_bytes());
    buf[32..36].copy_from_slice(&sigma.to_le_bytes());
    buf[36..40].copy_from_slice(&corner_radius.to_le_bytes());
    buf
}

/// Emit a shadow quad (6 vertices) covering the shadow rect plus blur padding.
/// texCoord carries absolute pixel-space coordinates for the fragment shader.
fn emit_shadow_quad(
    buf: &mut Vec<u8>,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    pad: f32,
    vw: f32,
    vh: f32,
    scale: f32,
) {
    // Quad extends beyond shadow rect by pad on all sides.
    let qx = sx - pad;
    let qy = sy - pad;
    let qw = sw + 2.0 * pad;
    let qh = sh + 2.0 * pad;

    // NDC for rasterization.
    let l = (qx * scale / vw) * 2.0 - 1.0;
    let r = ((qx + qw) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (qy * scale / vh) * 2.0;
    let b = 1.0 - ((qy + qh) * scale / vh) * 2.0;

    // Pixel-space coordinates for the fragment shader's Gaussian evaluation.
    let px_l = qx * scale;
    let px_r = (qx + qw) * scale;
    let px_t = qy * scale;
    let px_b = (qy + qh) * scale;

    // Color fields unused by fragment_shadow (reads from uniform buffer).
    let verts: [[f32; 8]; 6] = [
        [l, t, px_l, px_t, 0.0, 0.0, 0.0, 0.0],
        [r, t, px_r, px_t, 0.0, 0.0, 0.0, 0.0],
        [l, b, px_l, px_b, 0.0, 0.0, 0.0, 0.0],
        [r, t, px_r, px_t, 0.0, 0.0, 0.0, 0.0],
        [r, b, px_r, px_b, 0.0, 0.0, 0.0, 0.0],
        [l, b, px_l, px_b, 0.0, 0.0, 0.0, 0.0],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Flush accumulated solid-color vertices: set_vertex_bytes + draw.
pub(crate) fn flush_solid_vertices(cmdbuf: &mut metal::CommandBuffer, buf: &mut Vec<u8>) {
    if buf.is_empty() {
        return;
    }
    let vertex_count = buf.len() / VERTEX_BYTES;
    cmdbuf.set_vertex_bytes(0, buf.as_slice());
    cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vertex_count as u32);
    buf.clear();
}

/// Flush a raw vertex buffer: set_vertex_bytes + draw (any pipeline).
pub(crate) fn flush_vertices_raw(cmdbuf: &mut metal::CommandBuffer, buf: &mut Vec<u8>) {
    if buf.is_empty() {
        return;
    }
    let vertex_count = buf.len() / VERTEX_BYTES;
    cmdbuf.set_vertex_bytes(0, buf.as_slice());
    cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vertex_count as u32);
    buf.clear();
}

// ── Blur parameter packing ──────────────────────────────────────────────

/// Pack BlurParams for the blur_h/blur_v compute shaders.
/// Layout matches MSL struct: { half_width: i32, region_w: i32, region_h: i32, _pad: i32 }.
pub(crate) fn pack_blur_params(half_width: i32, region_w: i32, region_h: i32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&half_width.to_le_bytes());
    buf[4..8].copy_from_slice(&region_w.to_le_bytes());
    buf[8..12].copy_from_slice(&region_h.to_le_bytes());
    buf
}

/// Pack CopyParams for the copy_srgb_to_linear / copy_linear_to_srgb compute shaders.
/// Layout matches MSL struct: { src_x, src_y, dst_x, dst_y, width, height } (6 x i32).
pub(crate) fn pack_copy_params(
    src_x: i32,
    src_y: i32,
    dst_x: i32,
    dst_y: i32,
    width: i32,
    height: i32,
) -> [u8; 24] {
    let mut buf = [0u8; 24];
    buf[0..4].copy_from_slice(&src_x.to_le_bytes());
    buf[4..8].copy_from_slice(&src_y.to_le_bytes());
    buf[8..12].copy_from_slice(&dst_x.to_le_bytes());
    buf[12..16].copy_from_slice(&dst_y.to_le_bytes());
    buf[16..20].copy_from_slice(&width.to_le_bytes());
    buf[20..24].copy_from_slice(&height.to_le_bytes());
    buf
}

// ── Pointer coordinate scaling ──────────────────────────────────────────

/// Scale a raw pointer coordinate [0, 32767] to framebuffer pixels.
/// Same function as core's scale_pointer_coord — must produce identical results.
pub(crate) fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;
    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}
