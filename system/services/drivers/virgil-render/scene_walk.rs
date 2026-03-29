//! Scene graph tree walk for GPU rendering.
//!
//! Walks the scene graph depth-first and accumulates four types of geometry:
//! - **QuadBatch** (colored): solid rectangles from node backgrounds
//! - **TexturedBatch** (textured): glyph quads from Content::Glyphs nodes
//! - **ImageBatch**: image texture draw requests
//! - **PathBatch**: stencil-then-cover path rendering
//! - **BlurRequest** (post-processing): backdrop blur regions collected for
//!   two-pass GPU Gaussian blur after all normal rendering is complete
//!
//! Each batch is uploaded to its own VBO region and drawn with the appropriate
//! shader pipeline. Blur requests are post-processed after all normal rendering.

use alloc::vec::Vec;

use scene::{Content, Node, NodeFlags, NodeId, ShapedGlyph, NULL};

use crate::atlas::{self, GlyphAtlas};

#[path = "batch_image.rs"]
pub mod batch_image;
#[path = "batch_path.rs"]
pub mod batch_path;
#[path = "batch_quad.rs"]
pub mod batch_quad;
#[path = "batch_text.rs"]
pub mod batch_text;

pub use batch_image::{ImageBatch, ImageQuad, DWORDS_PER_IMAGE_QUAD, MAX_IMAGES};
pub use batch_path::PathBatch;
pub use batch_quad::{QuadBatch, VERTEX_STRIDE};
pub use batch_text::{TexturedBatch, TEXTURED_VERTEX_STRIDE};

/// Maximum backdrop blur requests per frame.
/// In typical use there are 0–2 blurred panels per frame.
pub const MAX_BLUR_REQUESTS: usize = 8;

/// A request to blur the framebuffer region behind a node.
///
/// Collected during the scene walk and executed after all normal rendering
/// using a three-pass box blur (converges to Gaussian via CLT).
/// Coordinates are in physical pixels (post-scale).
#[derive(Clone, Copy)]
pub struct BlurRequest {
    /// Left edge in physical pixels.
    pub x: f32,
    /// Top edge in physical pixels.
    pub y: f32,
    /// Width in physical pixels.
    pub w: f32,
    /// Height in physical pixels.
    pub h: f32,
    /// Blur radius in scene points (drives kernel selection).
    pub radius: u8,
    /// Background color to draw ON TOP of the blur result.
    /// If fully transparent, no post-blur background is drawn.
    pub bg: scene::Color,
    /// Corner radius for the post-blur background quad (in physical pixels).
    pub bg_corner_radius: f32,
}

/// Total textured VBO size: image quads (MAX_IMAGES x 192 bytes) + glyph quads.
/// Image vertices occupy offset 0; glyphs start after all image data.
pub const TOTAL_TEXTURED_VBO_BYTES: usize =
    batch_text::MAX_TEXTURED_VERTEX_BYTES + MAX_IMAGES * DWORDS_PER_IMAGE_QUAD * 4;

/// Total color VBO size: background quads (non-clipped + clipped) + path fan
/// triangles + path cover quads + clip path fan triangles. All five regions
/// are packed sequentially in the same VBO.
pub const TOTAL_COLOR_VBO_BYTES: usize = batch_quad::MAX_VERTEX_BYTES
    + batch_path::MAX_PATH_FAN_DWORDS * 4
    + batch_path::MAX_PATH_COVER_DWORDS * 4
    + batch_path::MAX_CLIP_FAN_DWORDS * 4;

// -- Clip rectangle (f32, NDC-space) --------------------------------------
// render/scene_render.rs has an independent i32 variant for physical pixel
// clipping -- intentionally separate coordinate systems.

#[derive(Clone, Copy)]
struct ClipRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl ClipRect {
    fn intersect(self, other: ClipRect) -> ClipRect {
        let x0 = if self.x > other.x { self.x } else { other.x };
        let y0 = if self.y > other.y { self.y } else { other.y };
        let x1_a = self.x + self.w;
        let x1_b = other.x + other.w;
        let y1_a = self.y + self.h;
        let y1_b = other.y + other.h;
        let x1 = if x1_a < x1_b { x1_a } else { x1_b };
        let y1 = if y1_a < y1_b { y1_a } else { y1_b };
        let w = if x1 > x0 { x1 - x0 } else { 0.0 };
        let h = if y1 > y0 { y1 - y0 } else { 0.0 };
        ClipRect { x: x0, y: y0, w, h }
    }

    fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

// -- Scene walk -----------------------------------------------------------

/// Walk the scene graph, accumulating colored quads and textured glyph quads.
///
/// `glyphs_data` is the scene graph's data buffer (for resolving DataRef).
/// `atlas` provides glyph->texture-coordinate lookups.
/// `ascent` is the font ascent in points (for baseline positioning).
/// `blur_requests` is cleared and filled with any nodes that have
/// `backdrop_blur_radius > 0`; the render loop executes them post-frame.
#[allow(clippy::too_many_arguments)]
pub fn walk_scene(
    nodes: &[Node],
    root: NodeId,
    scale: f32,
    viewport_w: u32,
    viewport_h: u32,
    batch: &mut QuadBatch,
    text_batch: &mut TexturedBatch,
    image_batch: &mut ImageBatch,
    path_batch: &mut PathBatch,
    glyphs_data: &[u8],
    atlas: &GlyphAtlas,
    ascent: u32,
    blur_requests: &mut Vec<BlurRequest>,
) {
    batch.clear();
    text_batch.clear();
    image_batch.clear();
    path_batch.clear();
    blur_requests.clear();

    if root == NULL || nodes.is_empty() {
        return;
    }

    let clip = ClipRect {
        x: 0.0,
        y: 0.0,
        w: viewport_w as f32,
        h: viewport_h as f32,
    };

    let vw = viewport_w as f32;
    let vh = viewport_h as f32;
    walk_node(
        nodes,
        root,
        0.0,
        0.0,
        scale,
        clip,
        vw,
        vh,
        batch,
        text_batch,
        image_batch,
        path_batch,
        glyphs_data,
        atlas,
        ascent,
        blur_requests,
        false, // root is not inside a clip region
    );
}

/// Recursive depth-first walk of the scene tree.
///
/// `inside_clip` is true when this node is a descendant of a node that
/// has a `clip_path`. Content emitted while `inside_clip` is true goes
/// into the "clipped" batch sections, which the render loop draws with
/// stencil test enabled.
#[allow(clippy::too_many_arguments)]
fn walk_node(
    nodes: &[Node],
    id: NodeId,
    parent_x: f32,
    parent_y: f32,
    scale: f32,
    clip: ClipRect,
    vw: f32,
    vh: f32,
    batch: &mut QuadBatch,
    text_batch: &mut TexturedBatch,
    image_batch: &mut ImageBatch,
    path_batch: &mut PathBatch,
    glyphs_data: &[u8],
    atlas: &GlyphAtlas,
    ascent: u32,
    blur_requests: &mut Vec<BlurRequest>,
    inside_clip: bool,
) {
    let idx = id as usize;
    if idx >= nodes.len() {
        return;
    }

    let node = &nodes[idx];

    if !node.flags.contains(NodeFlags::VISIBLE) {
        return;
    }

    let abs_x = parent_x + scene::mpt_to_f32(node.x) * scale;
    let abs_y = parent_y + scene::mpt_to_f32(node.y) * scale;
    let w = scene::umpt_to_f32(node.width) * scale;
    let h = scene::umpt_to_f32(node.height) * scale;

    // Collect backdrop blur request before drawing the node itself.
    // The render loop executes blur passes after all normal rendering,
    // blurring the fully-rendered scene at this region. The node's own
    // background is drawn ON TOP of the blur result (not baked in).
    let has_backdrop_blur =
        node.backdrop_blur_radius > 0 && blur_requests.len() < MAX_BLUR_REQUESTS;

    if has_backdrop_blur {
        let region = ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        }
        .intersect(clip);
        if !region.is_empty() {
            blur_requests.push(BlurRequest {
                x: region.x,
                y: region.y,
                w: region.w,
                h: region.h,
                radius: node.backdrop_blur_radius,
                bg: node.background,
                bg_corner_radius: node.corner_radius as f32 * scale,
            });
        }
    }

    // Draw background — but NOT for backdrop-blur nodes (drawn post-blur).
    let bg = node.background;
    if bg.a > 0 && !has_backdrop_blur {
        let quad_clip = ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        }
        .intersect(clip);

        if !quad_clip.is_empty() {
            let r = bg.r as f32 / 255.0;
            let g = bg.g as f32 / 255.0;
            let b = bg.b as f32 / 255.0;
            let a = bg.a as f32 / 255.0;
            if inside_clip {
                batch.push_clip_quad(
                    quad_clip.x,
                    quad_clip.y,
                    quad_clip.w,
                    quad_clip.h,
                    vw,
                    vh,
                    r,
                    g,
                    b,
                    a,
                );
            } else {
                batch.push_quad(
                    quad_clip.x,
                    quad_clip.y,
                    quad_clip.w,
                    quad_clip.h,
                    vw,
                    vh,
                    r,
                    g,
                    b,
                    a,
                );
            }
        }
    }

    // Render content.
    match node.content {
        Content::Glyphs {
            color,
            glyphs: glyph_ref,
            glyph_count,
            style_id,
            ..
        } => {
            // Map style_id to font_id (0=mono, 1=sans).
            let font_id = (style_id as u16).min(1);
            emit_glyphs(
                text_batch,
                glyphs_data,
                glyph_ref,
                glyph_count,
                font_id,
                atlas,
                abs_x,
                abs_y,
                scale,
                ascent,
                clip,
                vw,
                vh,
                color,
                inside_clip,
            );
        }
        Content::InlineImage {
            data,
            src_width,
            src_height,
        } => {
            if data.length > 0 && src_width > 0 && src_height > 0 {
                image_batch.push(ImageQuad {
                    x: abs_x,
                    y: abs_y,
                    w,
                    h,
                    data_offset: data.offset,
                    data_length: data.length,
                    src_width,
                    src_height,
                    clipped: inside_clip,
                });
            }
        }
        Content::Image { .. } => {
            // Content Region image: not yet implemented for virgil-render.
            // Images are resolved via the Content Region registry in metal-render
            // and cpu-render. Virgil path deferred.
        }
        Content::Path {
            color,
            stroke_color: _,
            fill_rule,
            stroke_width: _,
            contours,
        } => {
            if contours.length > 0 {
                batch_path::emit_path(
                    path_batch,
                    glyphs_data,
                    contours,
                    color,
                    fill_rule,
                    abs_x,
                    abs_y,
                    w,
                    h,
                    scale,
                    vw,
                    vh,
                );
            }
        }
        Content::None => {}
    }

    // Emit clip path fan into the stencil buffer region (before children).
    // When clip_path is non-empty, children will be clipped to this path shape
    // via the GPU stencil test in the render loop.
    if !node.clip_path.is_empty() {
        batch_path::emit_clip_fan(
            path_batch,
            glyphs_data,
            node.clip_path,
            abs_x,
            abs_y,
            scale,
            vw,
            vh,
        );
    }

    // Compute clip rect for children.
    let child_clip = if node.flags.contains(NodeFlags::CLIPS_CHILDREN) {
        clip.intersect(ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        })
    } else {
        clip
    };

    if child_clip.is_empty() {
        return;
    }

    // Children are inside a clip region if this node has a clip_path,
    // or if we were already inside one from an ancestor.
    let children_clipped = inside_clip || !node.clip_path.is_empty();

    let ct_tx = node.content_transform.tx * scale;
    let ct_ty = node.content_transform.ty * scale;

    let mut child = node.first_child;
    while child != NULL {
        let child_idx = child as usize;
        if child_idx >= nodes.len() {
            break;
        }
        walk_node(
            nodes,
            child,
            abs_x + ct_tx,
            abs_y + ct_ty,
            scale,
            child_clip,
            vw,
            vh,
            batch,
            text_batch,
            image_batch,
            path_batch,
            glyphs_data,
            atlas,
            ascent,
            blur_requests,
            children_clipped,
        );
        child = nodes[child_idx].next_sibling;
    }
}

// -- Glyph emission -------------------------------------------------------

/// Emit textured quads for a Glyphs content node.
///
/// Resolves the ShapedGlyph array from the data buffer, looks up each
/// glyph in the atlas, and pushes a textured quad per visible glyph.
#[allow(clippy::too_many_arguments)]
fn emit_glyphs(
    text_batch: &mut TexturedBatch,
    glyphs_data: &[u8],
    glyph_ref: scene::DataRef,
    glyph_count: u16,
    font_id: u16,
    atlas: &GlyphAtlas,
    node_x: f32,
    node_y: f32,
    scale: f32,
    ascent: u32,
    clip: ClipRect,
    vw: f32,
    vh: f32,
    color: scene::Color,
    inside_clip: bool,
) {
    let glyph_size = core::mem::size_of::<ShapedGlyph>();
    let offset = glyph_ref.offset as usize;
    let byte_len = glyph_count as usize * glyph_size;
    let end = offset + byte_len;

    if end > glyphs_data.len() || glyph_count == 0 {
        return;
    }

    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = color.a as f32 / 255.0;

    // Atlas texcoord scale (pixels -> 0.0-1.0).
    let atlas_w = atlas::ATLAS_WIDTH as f32;
    let atlas_h = atlas::ATLAS_HEIGHT as f32;

    // Baseline position: node_y + ascent (points, scaled).
    let baseline_y = node_y + (ascent as f32) * scale;
    let mut pen_x = node_x;

    for i in 0..glyph_count as usize {
        let glyph_offset = offset + i * glyph_size;
        // SAFETY: We verified end <= glyphs_data.len() above, and ShapedGlyph
        // is repr(C) with 8 bytes. The data buffer is 4-byte aligned.
        let sg: ShapedGlyph = unsafe {
            core::ptr::read_unaligned(glyphs_data.as_ptr().add(glyph_offset) as *const _)
        };

        // 16.16 fixed-point to f32 points.
        let fp16 = 65536.0f32;

        if let Some(entry) = atlas.lookup(sg.glyph_id, font_id) {
            let gx = pen_x + (entry.bearing_x as f32) * scale + (sg.x_offset as f32 / fp16) * scale;
            let gy =
                baseline_y - (entry.bearing_y as f32) * scale + (sg.y_offset as f32 / fp16) * scale;
            let gw = (entry.width as f32) * scale;
            let gh = (entry.height as f32) * scale;

            // Cull glyphs entirely outside the clip rect.
            if gx + gw <= clip.x
                || gy + gh <= clip.y
                || gx >= clip.x + clip.w
                || gy >= clip.y + clip.h
            {
                pen_x += (sg.x_advance as f32 / fp16) * scale;
                continue;
            }

            let u0 = entry.u as f32 / atlas_w;
            let v0 = entry.v as f32 / atlas_h;
            let u1 = (entry.u as f32 + entry.width as f32) / atlas_w;
            let v1 = (entry.v as f32 + entry.height as f32) / atlas_h;

            if inside_clip {
                text_batch
                    .push_clip_textured_quad(gx, gy, gw, gh, vw, vh, u0, v0, u1, v1, r, g, b, a);
            } else {
                text_batch.push_textured_quad(gx, gy, gw, gh, vw, vh, u0, v0, u1, v1, r, g, b, a);
            }
        }

        pen_x += (sg.x_advance as f32 / fp16) * scale;
    }
}
