//! Scene graph tree walk for solid rectangle rendering.
//!
//! Walks the scene graph depth-first and accumulates colored quads
//! for rendering via scissor+clear (each quad = one scissored glClear).
//!
//! Only node backgrounds are rendered (Task 5). Glyphs, images, and
//! paths are deferred to Tasks 6-7.
//!
//! Note: this file is included via `#[path]` in the test crate —
//! it must remain dependency-free (no external `use` statements beyond `scene`).

use scene::{Node, NodeFlags, NodeId, NULL};

/// Maximum number of quads per frame.
const MAX_QUADS: usize = 256;

/// Bytes per vertex: x(f32) + y(f32) + r(f32) + g(f32) + b(f32) + a(f32) = 24.
pub const VERTEX_STRIDE: u32 = 24;

/// Maximum vertex data size in bytes (kept for VBO resource sizing).
pub const MAX_VERTEX_BYTES: usize = MAX_QUADS * 6 * VERTEX_STRIDE as usize;

/// A single colored quad (rectangle).
#[derive(Clone, Copy)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// Maximum vertex data in u32 DWORDs (6 floats per vertex, 6 vertices per quad).
const MAX_VERTEX_DWORDS: usize = MAX_QUADS * 6 * 6;

/// Accumulated quads from a scene walk, with vertex data for GPU upload.
pub struct QuadBatch {
    /// Vertex data as f32 bit representations (x, y, r, g, b, a per vertex).
    vertex_data: [u32; MAX_VERTEX_DWORDS],
    /// Current write offset in u32 DWORDs.
    vertex_len: usize,
    /// Number of vertices accumulated.
    pub vertex_count: u32,
}

impl QuadBatch {
    pub const fn new() -> Self {
        Self {
            vertex_data: [0; MAX_VERTEX_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
        }
    }

    /// Reset for a new frame.
    pub fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
    }

    /// Get the vertex data as u32 slice for DMA copy.
    pub fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    /// Push a single vertex (position float2 + color float4 = 6 floats).
    fn push_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 6 > MAX_VERTEX_DWORDS {
            return;
        }
        self.vertex_data[self.vertex_len] = x.to_bits();
        self.vertex_data[self.vertex_len + 1] = y.to_bits();
        self.vertex_data[self.vertex_len + 2] = r.to_bits();
        self.vertex_data[self.vertex_len + 3] = g.to_bits();
        self.vertex_data[self.vertex_len + 4] = b.to_bits();
        self.vertex_data[self.vertex_len + 5] = a.to_bits();
        self.vertex_len += 6;
        self.vertex_count += 1;
    }

    /// Emit a colored quad as two triangles (6 vertices).
    /// Coordinates are in pixel space (viewport transform handles NDC).
    fn push_quad(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, g: f32, b: f32, a: f32) {
        let x1 = x + w;
        let y1 = y + h;

        // Triangle 1: top-left, top-right, bottom-left
        self.push_vertex(x, y, r, g, b, a);
        self.push_vertex(x1, y, r, g, b, a);
        self.push_vertex(x, y1, r, g, b, a);

        // Triangle 2: top-right, bottom-right, bottom-left
        self.push_vertex(x1, y, r, g, b, a);
        self.push_vertex(x1, y1, r, g, b, a);
        self.push_vertex(x, y1, r, g, b, a);
    }
}

/// Clip rectangle for scissor-based clipping.
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

/// Walk the scene graph and accumulate colored quads.
pub fn walk_scene(
    nodes: &[Node],
    root: NodeId,
    scale: f32,
    viewport_w: u32,
    viewport_h: u32,
    batch: &mut QuadBatch,
) {
    batch.clear();

    if root == NULL || nodes.is_empty() {
        return;
    }

    let clip = ClipRect {
        x: 0.0,
        y: 0.0,
        w: viewport_w as f32,
        h: viewport_h as f32,
    };

    walk_node(nodes, root, 0.0, 0.0, scale, clip, batch);
}

/// Recursive depth-first walk of the scene tree.
fn walk_node(
    nodes: &[Node],
    id: NodeId,
    parent_x: f32,
    parent_y: f32,
    scale: f32,
    clip: ClipRect,
    batch: &mut QuadBatch,
) {
    let idx = id as usize;
    if idx >= nodes.len() {
        return;
    }

    let node = &nodes[idx];

    // Skip invisible nodes.
    if !node.flags.contains(NodeFlags::VISIBLE) {
        return;
    }

    // Compute absolute position in physical pixels.
    let abs_x = parent_x + (node.x as f32) * scale;
    let abs_y = parent_y + (node.y as f32) * scale;
    let w = (node.width as f32) * scale;
    let h = (node.height as f32) * scale;

    // Draw background if non-transparent.
    let bg = node.background;
    if bg.a > 0 {
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
            batch.push_quad(
                quad_clip.x,
                quad_clip.y,
                quad_clip.w,
                quad_clip.h,
                r,
                g,
                b,
                a,
            );
        }
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

    // Apply scroll offset for children.
    let scroll_y = (node.scroll_y as f32) * scale;

    // Recurse into children.
    let mut child = node.first_child;
    while child != NULL {
        let child_idx = child as usize;
        if child_idx >= nodes.len() {
            break;
        }
        walk_node(
            nodes,
            child,
            abs_x,
            abs_y - scroll_y,
            scale,
            child_clip,
            batch,
        );
        child = nodes[child_idx].next_sibling;
    }
}
