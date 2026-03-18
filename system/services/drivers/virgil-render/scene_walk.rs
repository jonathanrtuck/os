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

/// Accumulated quads from a scene walk.
pub struct QuadBatch {
    quads: [Quad; MAX_QUADS],
    count: usize,
    /// Number of vertices (for compatibility — 6 per quad).
    pub vertex_count: u32,
}

impl QuadBatch {
    pub const fn new() -> Self {
        Self {
            quads: [Quad {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }; MAX_QUADS],
            count: 0,
            vertex_count: 0,
        }
    }

    /// Reset for a new frame.
    pub fn clear(&mut self) {
        self.count = 0;
        self.vertex_count = 0;
    }

    /// Get the accumulated quads.
    pub fn quads(&self) -> &[Quad] {
        &self.quads[..self.count]
    }

    /// Add a colored quad.
    fn push_quad(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.count >= MAX_QUADS {
            return;
        }
        self.quads[self.count] = Quad {
            x,
            y,
            w,
            h,
            r,
            g,
            b,
            a,
        };
        self.count += 1;
        self.vertex_count += 6;
    }

    /// Placeholder for future VBO use.
    pub fn as_dwords(&self) -> &[u32] {
        &[]
    }

    /// Placeholder for future VBO use.
    pub fn size_bytes(&self) -> u32 {
        0
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
