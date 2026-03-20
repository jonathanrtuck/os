//! Scene graph diffing — parent map and absolute bounds computation.

use crate::node::{Node, NodeId, MAX_NODES, NULL};

// ── Parent map ──────────────────────────────────────────────────────

/// Build a parent map from the node array. `parent[i]` is the parent
/// NodeId of node `i`, or `NULL` if it has no parent (root or unused).
/// One pass over the tree structure.
pub fn build_parent_map(nodes: &[Node], count: usize) -> [NodeId; MAX_NODES] {
    let mut parent = [NULL; MAX_NODES];
    let n = count.min(nodes.len()).min(MAX_NODES);
    for i in 0..n {
        let mut child = nodes[i].first_child;
        while child != NULL && (child as usize) < n {
            parent[child as usize] = i as NodeId;
            child = nodes[child as usize].next_sibling;
        }
    }
    parent
}

// ── Absolute bounds ─────────────────────────────────────────────────

/// Compute absolute bounding rect of a node by walking up the parent chain.
/// Returns `(x, y, width, height)` in absolute point coordinates.
///
/// Each parent's `content_transform` translation is added to the accumulator
/// because the content transform offsets a node's children. For scroll,
/// `content_transform.ty` is negative (content shifts up), so adding it
/// effectively subtracts the scroll offset. Without this, damage tracking
/// would compute incorrect dirty rects for nodes inside scrolled containers.
///
/// When a node has a non-identity transform, the returned bounding rect is
/// the axis-aligned bounding box (AABB) of the transformed node bounds.
/// This ensures damage tracking covers the full area affected by rotated,
/// scaled, or skewed nodes.
pub fn abs_bounds(
    nodes: &[Node],
    parent_map: &[NodeId; MAX_NODES],
    id: usize,
) -> (i32, i32, u32, u32) {
    let node = &nodes[id];
    let mut ax = node.x as i32;
    let mut ay = node.y as i32;
    let mut cur = parent_map[id];
    while cur != NULL && (cur as usize) < nodes.len() {
        let p = &nodes[cur as usize];
        // Add parent position and content_transform translation.
        // For scroll: ty is negative, so this effectively subtracts the offset.
        // Non-translation content_transforms (e.g. zoom/scale) are approximated
        // with translation only — under-damages for zoomed content. Full AABB
        // computation (transform_aabb) needed when zoom is implemented.
        ax += p.x as i32 + p.content_transform.tx as i32;
        ay += p.y as i32 + p.content_transform.ty as i32;
        cur = parent_map[cur as usize];
    }

    // Start with the node's point-based size.
    let mut bw = node.width as u32;
    let mut bh = node.height as u32;
    let mut bx = ax;
    let mut by = ay;

    // If the node has a non-identity transform, compute the AABB of the
    // transformed bounds. The transform shifts the node's visual footprint
    // — damage tracking must cover the full transformed area.
    if !node.transform.is_identity() {
        let (aabb_x, aabb_y, aabb_w, aabb_h) =
            node.transform
                .transform_aabb(0.0, 0.0, node.width as f32, node.height as f32);

        // The AABB origin is relative to the node's position.
        // Round conservatively: floor for origin, ceil for size.
        let aabb_xi = floor_f32(aabb_x) as i32;
        let aabb_yi = floor_f32(aabb_y) as i32;
        let aabb_wi = ceil_f32(aabb_w) as u32;
        let aabb_hi = ceil_f32(aabb_h) as u32;

        bx = ax + aabb_xi;
        by = ay + aabb_yi;
        bw = aabb_wi;
        bh = aabb_hi;
    }

    // Expand bounds by shadow overflow if the node has a shadow.
    if node.has_shadow() {
        let blur = node.shadow_blur_radius as i32;
        let spread = node.shadow_spread as i32;
        let off_x = node.shadow_offset_x as i32;
        let off_y = node.shadow_offset_y as i32;

        // Shadow extends by spread + blur on each side, shifted by offset.
        let extent = spread + blur;
        let left = (extent - off_x).max(0);
        let top = (extent - off_y).max(0);
        let right = (extent + off_x).max(0);
        let bottom = (extent + off_y).max(0);

        let new_x = bx - left;
        let new_y = by - top;
        let new_w = (bw as i32 + left + right).max(0) as u32;
        let new_h = (bh as i32 + top + bottom).max(0) as u32;

        return (new_x, new_y, new_w, new_h);
    }

    (bx, by, bw, bh)
}

// ── Math helpers ────────────────────────────────────────────────────

/// Floor for f32 in `no_std`.
fn floor_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;
    if x < f {
        f - 1.0
    } else {
        f
    }
}

/// Ceil for f32 in `no_std`.
fn ceil_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;
    if x > f {
        f + 1.0
    } else {
        f
    }
}
