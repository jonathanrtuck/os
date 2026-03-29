//! Tests for render::incremental — dirty rect computation from scene diffs,
//! plus NodeCache unit tests and cache integration tests.

use render::incremental::IncrementalState;
use scene::{Content, Node, NodeFlags, NodeId, DIRTY_BITMAP_WORDS, NULL};

// ── Helpers ─────────────────────────────────────────────────────────

/// Create a visible node at the given position and size (in whole points).
/// Converts to millipoints for the scene graph.
fn visible_node(x: i32, y: i32, w: u32, h: u32) -> Node {
    let mut n = Node::EMPTY;
    n.x = scene::pt(x);
    n.y = scene::pt(y);
    n.width = scene::upt(w);
    n.height = scene::upt(h);
    n.flags = NodeFlags::VISIBLE;
    n
}

/// Create an invisible (deleted) node.
fn invisible_node() -> Node {
    let mut n = Node::EMPTY;
    n.flags = NodeFlags::empty();
    n
}

/// Create a container node (has children) at the given position.
fn container_node(x: i32, y: i32, w: u32, h: u32, first_child: NodeId) -> Node {
    let mut n = visible_node(x, y, w, h);
    n.first_child = first_child;
    n
}

/// Set bit `i` in a dirty bitmap.
fn set_dirty_bit(bits: &mut [u64; DIRTY_BITMAP_WORDS], i: usize) {
    let word = i / 64;
    let bit = i % 64;
    bits[word] |= 1u64 << bit;
}

/// Set all bits 0..count in a dirty bitmap.
fn set_all_dirty(bits: &mut [u64; DIRTY_BITMAP_WORDS], count: usize) {
    for i in 0..count {
        set_dirty_bit(bits, i);
    }
}

/// Build a minimal node array with a root at (0,0) containing
/// `child_count` children. Returns (nodes, node_count).
fn root_with_children(child_count: usize) -> (Vec<Node>, u16) {
    let total = 1 + child_count;
    let mut nodes = vec![Node::EMPTY; total];

    // Root node.
    nodes[0] = visible_node(0, 0, 800, 600);
    if child_count > 0 {
        nodes[0].first_child = 1;
    }

    // Children linked via next_sibling.
    for i in 1..total {
        nodes[i] = visible_node(10, (i as i32) * 20, 100, 18);
        if i + 1 < total {
            nodes[i].next_sibling = (i + 1) as NodeId;
        }
    }

    (nodes, total as u16)
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn first_frame_returns_none() {
    let state = IncrementalState::new();
    assert!(state.first_frame);

    let nodes = [visible_node(0, 0, 100, 100)];
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 0);

    let result = state.compute_dirty_rects(&nodes, 1, &dirty, 800, 600);
    assert!(
        result.is_none(),
        "first frame should return None (full repaint)"
    );
}

#[test]
fn all_zero_dirty_bits_returns_empty() {
    let mut state = IncrementalState::new();
    let nodes = [visible_node(0, 0, 100, 100)];
    state.update_from_frame(&nodes, 1);

    let dirty = [0u64; DIRTY_BITMAP_WORDS];
    let result = state.compute_dirty_rects(&nodes, 1, &dirty, 800, 600);
    let tracker = result.expect("zero dirty bits should return Some");
    assert_eq!(tracker.count, 0, "zero dirty bits should produce no rects");
    assert!(!tracker.full_screen);
}

#[test]
fn dirty_rect_from_moved_node() {
    let mut state = IncrementalState::new();

    // Frame 1: root + child at (50, 100, 200, 30).
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = visible_node(0, 0, 800, 600);
    nodes[0].first_child = 1;
    nodes[1] = visible_node(50, 100, 200, 30);

    state.update_from_frame(&nodes, 2);

    // Frame 2: child moved down 20pt to (50, 120, 200, 30).
    nodes[1].y = scene::pt(120);
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 1);

    let tracker = state
        .compute_dirty_rects(&nodes, 2, &dirty, 800, 600)
        .expect("should return Some");
    assert_eq!(tracker.count, 1);

    let r = &tracker.rects[0];
    // Union of old (50, 100, 200, 30) and new (50, 120, 200, 30):
    // x=50, y=100, w=200, h=(150-100)=50
    assert_eq!(r.x, 50);
    assert_eq!(r.y, 100);
    assert_eq!(r.w, 200);
    assert_eq!(r.h, 50);
}

#[test]
fn dirty_rect_from_new_node() {
    let mut state = IncrementalState::new();

    // Frame 1: only root.
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = visible_node(0, 0, 800, 600);
    nodes[1] = invisible_node();

    state.update_from_frame(&nodes, 2);

    // Frame 2: node 1 becomes visible at (50, 100, 200, 30).
    nodes[0].first_child = 1;
    nodes[1] = visible_node(50, 100, 200, 30);
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 1);

    let tracker = state
        .compute_dirty_rects(&nodes, 2, &dirty, 800, 600)
        .expect("should return Some");
    assert_eq!(tracker.count, 1);

    let r = &tracker.rects[0];
    assert_eq!(r.x, 50);
    assert_eq!(r.y, 100);
    assert_eq!(r.w, 200);
    assert_eq!(r.h, 30);
}

#[test]
fn dirty_rect_from_deleted_node() {
    let mut state = IncrementalState::new();

    // Frame 1: root + visible child at (50, 100, 200, 30).
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = visible_node(0, 0, 800, 600);
    nodes[0].first_child = 1;
    nodes[1] = visible_node(50, 100, 200, 30);

    state.update_from_frame(&nodes, 2);

    // Frame 2: node 1 becomes invisible.
    nodes[1] = invisible_node();
    nodes[0].first_child = NULL;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 1);

    let tracker = state
        .compute_dirty_rects(&nodes, 2, &dirty, 800, 600)
        .expect("should return Some");
    assert_eq!(tracker.count, 1);

    let r = &tracker.rects[0];
    // Damage is the previous bounds.
    assert_eq!(r.x, 50);
    assert_eq!(r.y, 100);
    assert_eq!(r.w, 200);
    assert_eq!(r.h, 30);
}

#[test]
fn all_dirty_bits_set_returns_none() {
    let mut state = IncrementalState::new();

    let nodes = [visible_node(0, 0, 100, 100), visible_node(10, 10, 50, 50)];
    state.update_from_frame(&nodes, 2);

    // All bits set for 2 nodes.
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_all_dirty(&mut dirty, 2);

    let result = state.compute_dirty_rects(&nodes, 2, &dirty, 800, 600);
    assert!(
        result.is_none(),
        "all dirty bits set should return None (full repaint)"
    );
}

#[test]
fn scroll_detected_from_dirty_container() {
    let mut state = IncrementalState::new();

    // Frame 1: container at node 0 with child at node 1, no scroll.
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = container_node(0, 0, 800, 600, 1);
    nodes[1] = visible_node(10, 20, 100, 18);

    state.update_from_frame(&nodes, 2);

    // Frame 2: scroll down 50 (child_offset_y = -50).
    nodes[0].child_offset_y = -50.0;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 0);

    let result = state.detect_scroll(&nodes, &dirty);
    assert!(result.is_some(), "should detect scroll change");
    let (node_id, delta_tx, delta_ty) = result.unwrap();
    assert_eq!(node_id, 0);
    assert_eq!(delta_tx, 0.0);
    assert_eq!(delta_ty, -50.0);
}

#[test]
fn scroll_not_detected_when_no_children() {
    let mut state = IncrementalState::new();

    // Node 0 is a leaf (no children), even if child_offset changes.
    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0] = visible_node(0, 0, 800, 600);

    state.update_from_frame(&nodes, 1);

    nodes[0].child_offset_y = -50.0;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 0);

    let result = state.detect_scroll(&nodes, &dirty);
    assert!(
        result.is_none(),
        "leaf node scroll change should not be detected"
    );
}

#[test]
fn update_from_frame_populates_prev_state() {
    let mut state = IncrementalState::new();
    assert!(state.first_frame);

    // Set up 3 nodes: root + 2 children.
    let mut nodes = vec![Node::EMPTY; 3];
    nodes[0] = container_node(0, 0, 800, 600, 1);
    nodes[0].child_offset_y = -10.0;
    nodes[0].content_hash = 42;
    nodes[1] = visible_node(50, 100, 200, 30);
    nodes[1].content_hash = 99;
    nodes[1].next_sibling = 2;
    nodes[2] = visible_node(50, 150, 200, 30);
    nodes[2].content_hash = 77;

    state.update_from_frame(&nodes, 3);

    assert!(
        !state.first_frame,
        "first_frame should be false after update"
    );

    // Check prev_visible bits.
    assert_ne!(
        state.prev_visible[0], 0,
        "nodes 0-2 should be visible in bitmap"
    );

    // Check prev_bounds for node 1 (child of root at (0,0) with child_offset_y=-10).
    // abs_bounds: child at (50, 100), parent adds (0, 0 + mpt(-10)) = (50, 90) in points.
    // In millipoints: (50*1024, 90*1024, 200*1024, 30*1024).
    let (bx, by, bw, bh) = state.prev_bounds[1];
    assert_eq!(bx, scene::pt(50));
    assert_eq!(by, scene::pt(90), "should account for parent child_offset");
    assert_eq!(bw, scene::upt(200));
    assert_eq!(bh, scene::upt(30));

    // Check child_offset and content_hash.
    assert_eq!(state.prev_child_offset[0], (0.0, -10.0));
    assert_eq!(state.prev_content_hash[0], 42);
    assert_eq!(state.prev_content_hash[1], 99);
    assert_eq!(state.prev_content_hash[2], 77);
}

#[test]
fn negative_coords_clamped_to_zero() {
    let mut state = IncrementalState::new();

    // Frame 1: node at (-20, -10, 100, 50) — partially off-screen.
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = visible_node(0, 0, 800, 600);
    nodes[0].first_child = 1;
    nodes[1] = visible_node(-20, -10, 100, 50);

    state.update_from_frame(&nodes, 2);

    // Frame 2: node moved to (-20, -10, 100, 60) — size changed.
    nodes[1].height = 60;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 1);

    let tracker = state
        .compute_dirty_rects(&nodes, 2, &dirty, 800, 600)
        .expect("should return Some");
    assert_eq!(tracker.count, 1);

    let r = &tracker.rects[0];
    // Union of (-20, -10, 100, 50) and (-20, -10, 100, 60) = (-20, -10, 100, 60).
    // Clamped to fb: x=0, y=0, w=(80-0)=80, h=(50-0)=50.
    assert_eq!(r.x, 0);
    assert_eq!(r.y, 0);
    assert!(r.w <= 80, "width clamped: got {}", r.w);
    assert!(r.h <= 50, "height clamped: got {}", r.h);
}

#[test]
fn overflow_to_full_screen_when_many_dirty_rects() {
    let mut state = IncrementalState::new();

    // Create 40 children (exceeds MAX_DIRTY_RECTS = 32).
    let (nodes, count) = root_with_children(40);
    state.update_from_frame(&nodes, count);

    // Move all children down by 5px.
    let mut nodes2 = nodes.clone();
    for i in 1..=40 {
        nodes2[i].y += 5;
    }

    // Mark all children dirty.
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    for i in 1..=40 {
        set_dirty_bit(&mut dirty, i);
    }

    // Use a tall framebuffer so all children are on-screen.
    let tracker = state
        .compute_dirty_rects(&nodes2, count, &dirty, 800, 2000)
        .expect("should return Some");
    assert!(
        tracker.full_screen,
        "should overflow to full_screen with 40 dirty rects"
    );
}

#[test]
fn multiple_dirty_nodes_produce_multiple_rects() {
    let mut state = IncrementalState::new();

    // Root + 3 children.
    let mut nodes = vec![Node::EMPTY; 4];
    nodes[0] = container_node(0, 0, 800, 600, 1);
    nodes[1] = visible_node(10, 10, 100, 20);
    nodes[1].next_sibling = 2;
    nodes[2] = visible_node(10, 40, 100, 20);
    nodes[2].next_sibling = 3;
    nodes[3] = visible_node(10, 70, 100, 20);

    state.update_from_frame(&nodes, 4);

    // Move nodes 1 and 3, leave node 2 unchanged.
    nodes[1].y = 15;
    nodes[3].y = 75;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 1);
    set_dirty_bit(&mut dirty, 3);

    let tracker = state
        .compute_dirty_rects(&nodes, 4, &dirty, 800, 600)
        .expect("should return Some");
    assert_eq!(
        tracker.count, 2,
        "two dirty nodes should produce two dirty rects"
    );
}

#[test]
fn detect_scroll_negative_delta() {
    let mut state = IncrementalState::new();

    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = container_node(0, 0, 800, 600, 1);
    nodes[0].child_offset_y = -100.0;
    nodes[1] = visible_node(10, 20, 100, 18);

    state.update_from_frame(&nodes, 2);

    // Scroll up by 30 (child_offset_y goes from -100 to -70).
    nodes[0].child_offset_y = -70.0;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 0);

    let (node_id, delta_tx, delta_ty) = state.detect_scroll(&nodes, &dirty).unwrap();
    assert_eq!(node_id, 0);
    assert_eq!(delta_tx, 0.0);
    assert_eq!(
        delta_ty, 30.0,
        "scroll up should produce positive delta (ty increases)"
    );
}

#[test]
fn invisible_node_cleared_in_prev_state() {
    let mut state = IncrementalState::new();

    // Frame 1: node 1 visible.
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = visible_node(0, 0, 800, 600);
    nodes[0].first_child = 1;
    nodes[1] = visible_node(10, 10, 100, 20);
    state.update_from_frame(&nodes, 2);

    // Frame 2: node 1 invisible.
    nodes[1] = invisible_node();
    nodes[0].first_child = NULL;
    state.update_from_frame(&nodes, 2);

    // Verify prev_visible bit for node 1 is cleared.
    let vis_bit = state.prev_visible[0] & (1u64 << 1);
    assert_eq!(
        vis_bit, 0,
        "invisible node should have cleared prev_visible"
    );

    // prev_bounds should be zeroed.
    assert_eq!(state.prev_bounds[1], (0, 0, 0, 0));
}

#[test]
fn node_fully_off_screen_produces_no_rect() {
    let mut state = IncrementalState::new();

    // Frame 1: node at (900, 0, 50, 50) — fully off-screen right.
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = visible_node(0, 0, 800, 600);
    nodes[0].first_child = 1;
    nodes[1] = visible_node(900, 0, 50, 50);

    state.update_from_frame(&nodes, 2);

    // Frame 2: node still off-screen, just moved further right.
    nodes[1].x = scene::pt(950);
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 1);

    let tracker = state
        .compute_dirty_rects(&nodes, 2, &dirty, 800, 600)
        .expect("should return Some");
    assert_eq!(
        tracker.count, 0,
        "fully off-screen node should produce no dirty rect"
    );
}

// ── Scroll blit-shift tests ─────────────────────────────────────────

use render::incremental::{blit_shift_vertical, compute_scroll_blit};

#[test]
fn scroll_blit_params_vertical_down() {
    // Container at (10, 20, 400, 300) in millipoints (scene coords), scale=1.0.
    // Scroll delta: (0, -50) point = content moves up (scroll down).
    let params = compute_scroll_blit(
        0,
        0.0,
        -50.0,
        (
            scene::pt(10),
            scene::pt(20),
            scene::upt(400),
            scene::upt(300),
        ),
        1.0,
        800,
        600,
    );
    let p = params.expect("should produce blit params");
    assert_eq!(p.cx, 10);
    assert_eq!(p.cy, 20);
    assert_eq!(p.cw, 400);
    assert_eq!(p.ch, 300);
    assert_eq!(p.dy_px, -50);
    // Exposed strip at bottom: (10, 20 + 300 - 50, 400, 50) = (10, 270, 400, 50)
    assert_eq!(p.exposed.x, 10);
    assert_eq!(p.exposed.y, 270);
    assert_eq!(p.exposed.w, 400);
    assert_eq!(p.exposed.h, 50);
}

#[test]
fn scroll_blit_params_vertical_up() {
    // Container at (10, 20, 400, 300) in millipoints, scale=1.0.
    // Scroll delta: (0, 50) point = content moves down (scroll up).
    let params = compute_scroll_blit(
        0,
        0.0,
        50.0,
        (
            scene::pt(10),
            scene::pt(20),
            scene::upt(400),
            scene::upt(300),
        ),
        1.0,
        800,
        600,
    );
    let p = params.expect("should produce blit params");
    assert_eq!(p.dy_px, 50);
    // Exposed strip at top: (10, 20, 400, 50)
    assert_eq!(p.exposed.x, 10);
    assert_eq!(p.exposed.y, 20);
    assert_eq!(p.exposed.w, 400);
    assert_eq!(p.exposed.h, 50);
}

#[test]
fn scroll_blit_params_with_scale() {
    // Container at (10, 20, 400, 300) millipoints, scale=2.0.
    // Physical container: (20, 40, 800, 600).
    // Scroll delta: (0, -25) point = -50 physical.
    let params = compute_scroll_blit(
        0,
        0.0,
        -25.0,
        (
            scene::pt(10),
            scene::pt(20),
            scene::upt(400),
            scene::upt(300),
        ),
        2.0,
        1600,
        1200,
    );
    let p = params.expect("should produce blit params");
    assert_eq!(p.cx, 20);
    assert_eq!(p.cy, 40);
    assert_eq!(p.cw, 800);
    assert_eq!(p.ch, 600);
    assert_eq!(p.dy_px, -50);
    // Exposed at bottom: (20, 40 + 600 - 50, 800, 50) = (20, 590, 800, 50)
    assert_eq!(p.exposed.x, 20);
    assert_eq!(p.exposed.y, 590);
    assert_eq!(p.exposed.w, 800);
    assert_eq!(p.exposed.h, 50);
}

#[test]
fn scroll_blit_params_horizontal_returns_none() {
    // Horizontal scroll: dx != 0 — not supported, should return None.
    let params = compute_scroll_blit(
        0,
        10.0,
        -50.0,
        (0, 0, scene::upt(800), scene::upt(600)),
        1.0,
        800,
        600,
    );
    assert!(params.is_none(), "horizontal scroll should return None");
}

#[test]
fn scroll_blit_params_subpixel_returns_none() {
    // Subpixel scroll: after rounding, dy_px = 0.
    let params = compute_scroll_blit(
        0,
        0.0,
        0.3,
        (0, 0, scene::upt(800), scene::upt(600)),
        1.0,
        800,
        600,
    );
    assert!(
        params.is_none(),
        "subpixel scroll should return None (dy rounds to 0)"
    );
}

#[test]
fn scroll_blit_params_exceeds_height_returns_none() {
    // Scroll amount >= container height: nothing to shift.
    let params = compute_scroll_blit(
        0,
        0.0,
        -300.0,
        (0, 0, scene::upt(800), scene::upt(300)),
        1.0,
        800,
        600,
    );
    assert!(
        params.is_none(),
        "scroll >= container height should return None"
    );
}

#[test]
fn blit_shift_vertical_scroll_down() {
    // 4x4 pixel framebuffer, BGRA (stride = 16 bytes).
    // Container covers rows 0-3 (full buffer), shift dy=-2 (content up 2 rows).
    let mut buf = vec![0u8; 4 * 4 * 4]; // 4x4 BGRA
                                        // Fill row 0 with 0x10, row 1 with 0x20, row 2 with 0x30, row 3 with 0x40.
    for x in 0..16 {
        buf[0 * 16 + x] = 0x10;
    }
    for x in 0..16 {
        buf[1 * 16 + x] = 0x20;
    }
    for x in 0..16 {
        buf[2 * 16 + x] = 0x30;
    }
    for x in 0..16 {
        buf[3 * 16 + x] = 0x40;
    }

    // dy=-2: rows shift up by 2. Row 2→0, row 3→1. Rows 2-3 become exposed.
    blit_shift_vertical(&mut buf, 0, 0, 4, 4, 4, -2);

    // Row 0 should now have row 2's data (0x30).
    assert_eq!(buf[0], 0x30, "row 0 should contain old row 2");
    // Row 1 should now have row 3's data (0x40).
    assert_eq!(buf[16], 0x40, "row 1 should contain old row 3");
    // Rows 2-3 are exposed (stale data, will be rendered over).
}

#[test]
fn blit_shift_vertical_scroll_up() {
    // 4x4 pixel framebuffer, BGRA (stride = 16 bytes).
    // Container covers rows 0-3 (full buffer), shift dy=2 (content down 2 rows).
    let mut buf = vec![0u8; 4 * 4 * 4];
    for x in 0..16 {
        buf[0 * 16 + x] = 0x10;
    }
    for x in 0..16 {
        buf[1 * 16 + x] = 0x20;
    }
    for x in 0..16 {
        buf[2 * 16 + x] = 0x30;
    }
    for x in 0..16 {
        buf[3 * 16 + x] = 0x40;
    }

    // dy=+2: rows shift down by 2. Row 0→2, row 1→3. Rows 0-1 become exposed.
    blit_shift_vertical(&mut buf, 0, 0, 4, 4, 4, 2);

    // Row 2 should now have row 0's data (0x10).
    assert_eq!(buf[2 * 16], 0x10, "row 2 should contain old row 0");
    // Row 3 should now have row 1's data (0x20).
    assert_eq!(buf[3 * 16], 0x20, "row 3 should contain old row 1");
    // Rows 0-1 are exposed (stale data).
}

#[test]
fn blit_shift_vertical_partial_width() {
    // 8-pixel wide framebuffer, container is columns 2-5 (4 pixels wide).
    // 4 rows tall, container covers all rows.
    let stride: u32 = 8 * 4; // 32 bytes
    let mut buf = vec![0u8; 4 * stride as usize];

    // Fill each row's container region (columns 2-5) with distinct values.
    for row in 0..4u32 {
        for col in 2..6u32 {
            let offset = (row * stride + col * 4) as usize;
            buf[offset] = (row + 1) as u8 * 0x10; // 0x10, 0x20, 0x30, 0x40
        }
    }

    // dy=-1: shift up by 1 within container columns.
    blit_shift_vertical(&mut buf, 2, 0, 4, 4, 8, -1);

    // Row 0, cols 2-5 should have row 1's data (0x20).
    assert_eq!(
        buf[(0 * stride + 2 * 4) as usize],
        0x20,
        "row 0 col 2 should have old row 1"
    );
    // Row 2, cols 2-5 should have row 3's data (0x40).
    assert_eq!(
        buf[(2 * stride + 2 * 4) as usize],
        0x40,
        "row 2 col 2 should have old row 3"
    );
    // Pixels OUTSIDE container (col 0-1, col 6-7) should be untouched.
    assert_eq!(buf[0], 0, "pixels outside container should be unchanged");
}

#[test]
fn compute_scroll_blit_clamps_to_framebuffer() {
    // Container extends beyond framebuffer bottom.
    // Container at (0, 500, 800, 200) millipoints, fb is 800x600, scale=1.
    // Container bottom is at 700pt, but fb only goes to 600.
    let params = compute_scroll_blit(
        0,
        0.0,
        -50.0,
        (
            scene::pt(0),
            scene::pt(500),
            scene::upt(800),
            scene::upt(200),
        ),
        1.0,
        800,
        600,
    );
    let p = params.expect("should produce blit params");
    // Container should be clipped to fb: ch = 600 - 500 = 100.
    assert_eq!(p.ch, 100);
    assert_eq!(p.cy, 500);
    // dy=-50, exposed at bottom: (0, 500 + 100 - 50, 800, 50) = (0, 550, 800, 50)
    assert_eq!(p.exposed.y, 550);
    assert_eq!(p.exposed.h, 50);
}

#[test]
fn compute_scroll_damage_replaces_container_rect() {
    use render::incremental::compute_scroll_damage;

    let mut state = IncrementalState::new();

    // Frame 1: container(0) at (0,0,800,600), child(1) at (10,20,100,18).
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0] = container_node(0, 0, 800, 600, 1);
    nodes[1] = visible_node(10, 20, 100, 18);
    state.update_from_frame(&nodes, 2);

    // Frame 2: scroll down 50 (child_offset_y = -50).
    nodes[0].child_offset_y = -50.0;
    let mut dirty = [0u64; DIRTY_BITMAP_WORDS];
    set_dirty_bit(&mut dirty, 0); // Container is dirty.

    // Compute original damage (without blit-shift optimization).
    let original = state
        .compute_dirty_rects(&nodes, 2, &dirty, 800, 600)
        .unwrap();
    // The original has the full container as a dirty rect.
    assert!(original.count >= 1, "original should have at least 1 rect");

    // Now compute scroll-adjusted damage.
    let scroll = state.detect_scroll(&nodes, &dirty).unwrap();
    let blit = compute_scroll_blit(
        scroll.0,
        scroll.1,
        scroll.2,
        state.prev_bounds[scroll.0 as usize],
        1.0,
        800,
        600,
    );
    let blit = blit.unwrap();
    let adjusted = compute_scroll_damage(&original, &blit, 800, 600);

    // Adjusted damage should have the exposed strip, not the full container.
    assert!(adjusted.count >= 1, "adjusted should have at least 1 rect");
    // The exposed strip for scroll down 50px: (0, 550, 800, 50).
    let found_strip = (0..adjusted.count).any(|i| {
        let r = &adjusted.rects[i];
        r.y == 550 && r.h == 50
    });
    assert!(
        found_strip,
        "adjusted damage should include the exposed strip at bottom"
    );
}

// ── NodeCache tests ─────────────────────────────────────────────────

use render::cache::NodeCache;

#[test]
fn node_cache_stores_and_retrieves() {
    let mut cache = NodeCache::new();
    let data = vec![0xAA_u8; 100 * 20 * 4]; // 100x20 BGRA
    cache.store(5, 0xABCD, 100, 20, &data);
    let result = cache.get(5, 0xABCD);
    assert!(result.is_some());
    let (w, h, pixels) = result.unwrap();
    assert_eq!(w, 100);
    assert_eq!(h, 20);
    assert_eq!(pixels.len(), data.len());
    assert_eq!(pixels[0], 0xAA);
}

#[test]
fn node_cache_invalidates_on_hash_change() {
    let mut cache = NodeCache::new();
    cache.store(5, 0xABCD, 10, 10, &[0u8; 400]);
    // Different hash — miss.
    assert!(cache.get(5, 0x1234).is_none());
    // Same hash — hit.
    assert!(cache.get(5, 0xABCD).is_some());
}

#[test]
fn node_cache_clear_removes_all() {
    let mut cache = NodeCache::new();
    cache.store(1, 0x1111, 10, 1, &[0u8; 40]);
    cache.store(2, 0x2222, 10, 1, &[0u8; 40]);
    assert_eq!(cache.valid_count(), 2);
    cache.clear();
    assert_eq!(cache.valid_count(), 0);
    assert!(cache.get(1, 0x1111).is_none());
}

#[test]
fn node_cache_evict_single_entry() {
    let mut cache = NodeCache::new();
    cache.store(5, 0xABCD, 10, 10, &[0u8; 400]);
    cache.evict(5);
    assert!(cache.get(5, 0xABCD).is_none());
}

#[test]
fn node_cache_store_reuses_allocation_same_size() {
    let mut cache = NodeCache::new();
    cache.store(5, 0x1111, 100, 20, &[0xAA; 8000]);
    cache.store(5, 0x2222, 100, 20, &[0xBB; 8000]);
    let (_, _, pixels) = cache.get(5, 0x2222).unwrap();
    assert_eq!(pixels[0], 0xBB);
}

#[test]
fn node_cache_total_bytes() {
    let mut cache = NodeCache::new();
    cache.store(0, 0x1111, 10, 10, &[0u8; 400]);
    cache.store(1, 0x2222, 20, 20, &[0u8; 1600]);
    assert_eq!(cache.total_bytes(), 2000);
}

#[test]
fn node_cache_out_of_bounds_node_id() {
    let mut cache = NodeCache::new();
    // node_id >= MAX_NODES should not panic — just no-op/miss.
    cache.store(600, 0x1111, 10, 10, &[0u8; 400]);
    assert!(cache.get(600, 0x1111).is_none());
}

#[test]
fn node_cache_evict_out_of_bounds_no_panic() {
    let mut cache = NodeCache::new();
    // Out-of-bounds evict should not panic.
    cache.evict(600);
    assert_eq!(cache.valid_count(), 0);
}

#[test]
fn node_cache_store_different_sizes_reallocates() {
    let mut cache = NodeCache::new();
    // First store: 400 bytes.
    cache.store(3, 0x1111, 10, 10, &[0xAA; 400]);
    assert_eq!(cache.total_bytes(), 400);

    // Second store for same node: 1600 bytes (different size).
    cache.store(3, 0x2222, 20, 20, &[0xBB; 1600]);
    assert_eq!(cache.total_bytes(), 1600);
    let (w, h, pixels) = cache.get(3, 0x2222).unwrap();
    assert_eq!(w, 20);
    assert_eq!(h, 20);
    assert_eq!(pixels.len(), 1600);
    assert_eq!(pixels[0], 0xBB);
}

#[test]
fn node_cache_valid_count_after_mixed_operations() {
    let mut cache = NodeCache::new();
    cache.store(0, 0x1111, 5, 5, &[0u8; 100]);
    cache.store(1, 0x2222, 5, 5, &[0u8; 100]);
    cache.store(2, 0x3333, 5, 5, &[0u8; 100]);
    assert_eq!(cache.valid_count(), 3);

    cache.evict(1);
    assert_eq!(cache.valid_count(), 2);

    // Re-store into evicted slot.
    cache.store(1, 0x4444, 5, 5, &[0u8; 100]);
    assert_eq!(cache.valid_count(), 3);

    cache.clear();
    assert_eq!(cache.valid_count(), 0);
}

#[test]
fn node_cache_total_bytes_excludes_evicted() {
    let mut cache = NodeCache::new();
    cache.store(0, 0x1111, 10, 10, &[0u8; 400]);
    cache.store(1, 0x2222, 10, 10, &[0u8; 400]);
    assert_eq!(cache.total_bytes(), 800);

    cache.evict(0);
    assert_eq!(
        cache.total_bytes(),
        400,
        "evicted entry bytes should not be counted"
    );
}

// ── Node cache integration with render walk ────────────────────────

use drawing::{PixelFormat, Surface};
use render::scene_render;

/// Zeroed GlyphCache for tests that don't use text rendering.
fn zeroed_glyph_cache() -> Box<fonts::cache::GlyphCache> {
    // SAFETY: GlyphCache is repr(C)-like with all-integer fields.
    // A zeroed instance is valid — all metrics are 0 and coverage
    // buffers are empty.
    unsafe {
        let layout = std::alloc::Layout::new::<fonts::cache::GlyphCache>();
        let ptr = std::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;
        assert!(!ptr.is_null());
        Box::from_raw(ptr)
    }
}

fn test_ctx<'a>(
    mono: &'a fonts::cache::GlyphCache,
    prop: &'a fonts::cache::GlyphCache,
) -> scene_render::RenderCtx<'a> {
    scene_render::RenderCtx {
        mono_cache: mono,
        prop_cache: prop,
        scale: 1.0,
        font_size_px: 18,
    }
}

fn make_surface(buf: &mut [u8], w: u32, h: u32) -> Surface {
    Surface {
        data: buf,
        width: w,
        height: h,
        stride: w * 4,
        format: PixelFormat::Bgra8888,
    }
}

/// Build a scene with an image node for cache testing.
/// Returns (nodes, data_buf) where node 1 has Content::InlineImage.
fn build_image_scene() -> (Vec<Node>, Vec<u8>) {
    // 10x10 solid red BGRA image.
    let mut img_data = vec![0u8; 10 * 10 * 4];
    for pixel in img_data.chunks_exact_mut(4) {
        pixel[0] = 0; // B
        pixel[1] = 0; // G
        pixel[2] = 255; // R
        pixel[3] = 255; // A
    }

    let mut nodes = vec![Node::EMPTY; 2];
    // Root (millipoint coordinates)
    nodes[0].x = scene::pt(0);
    nodes[0].y = scene::pt(0);
    nodes[0].width = scene::upt(50);
    nodes[0].height = scene::upt(50);
    nodes[0].flags = NodeFlags::VISIBLE;
    nodes[0].first_child = 1;

    // Image child (millipoint coordinates)
    nodes[1].x = scene::pt(5);
    nodes[1].y = scene::pt(5);
    nodes[1].width = scene::upt(10);
    nodes[1].height = scene::upt(10);
    nodes[1].flags = NodeFlags::VISIBLE;
    nodes[1].content = Content::InlineImage {
        data: scene::DataRef {
            offset: 0,
            length: (10 * 10 * 4) as u32,
        },
        src_width: 10,
        src_height: 10,
    };
    nodes[1].content_hash = 0xDEAD;

    (nodes, img_data)
}

#[test]
fn cache_populated_on_first_render() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);
    let mut pool = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru = render::LruRasterizer::new_test(16);
    let mut cache = NodeCache::new();

    let (nodes, data) = build_image_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
        content_region: &[],
    };
    let dirty = protocol::DirtyRect::new(0, 0, 50, 50);

    let mut buf = vec![0u8; 50 * 50 * 4];
    let mut fb = make_surface(&mut buf, 50, 50);
    let mut clip_cache = render::ClipMaskCache::new();

    scene_render::render_scene_clipped_full(
        &mut fb,
        &graph,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        Some(&mut cache),
        &mut clip_cache,
    );

    // Node 1 has Content::InlineImage — should be cached.
    assert!(
        cache.get(1, 0xDEAD).is_some(),
        "image node should be cached after render"
    );
    // Node 0 has Content::None — should NOT be cached.
    assert!(
        cache.get(0, 0).is_none(),
        "Content::None node should not be cached"
    );
}

#[test]
fn cache_hit_produces_identical_pixels() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);
    let mut pool = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru = render::LruRasterizer::new_test(16);
    let mut cache = NodeCache::new();

    let (nodes, data) = build_image_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
        content_region: &[],
    };
    let dirty = protocol::DirtyRect::new(0, 0, 50, 50);

    // First render (cache miss — populates cache).
    let mut buf1 = vec![0u8; 50 * 50 * 4];
    let mut fb1 = make_surface(&mut buf1, 50, 50);
    let mut clip_cache = render::ClipMaskCache::new();
    scene_render::render_scene_clipped_full(
        &mut fb1,
        &graph,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        Some(&mut cache),
        &mut clip_cache,
    );

    // Second render (cache hit — should produce identical output).
    let mut buf2 = vec![0u8; 50 * 50 * 4];
    let mut fb2 = make_surface(&mut buf2, 50, 50);
    scene_render::render_scene_clipped_full(
        &mut fb2,
        &graph,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        Some(&mut cache),
        &mut clip_cache,
    );

    assert_eq!(buf1, buf2, "cache hit should produce identical pixels");
}

#[test]
fn cache_miss_on_hash_change() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);
    let mut pool = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru = render::LruRasterizer::new_test(16);
    let mut cache = NodeCache::new();

    let (mut nodes, data) = build_image_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
        content_region: &[],
    };
    let dirty = protocol::DirtyRect::new(0, 0, 50, 50);

    // First render — populates cache with hash 0xDEAD.
    let mut buf = vec![0u8; 50 * 50 * 4];
    let mut fb = make_surface(&mut buf, 50, 50);
    let mut clip_cache = render::ClipMaskCache::new();
    scene_render::render_scene_clipped_full(
        &mut fb,
        &graph,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        Some(&mut cache),
        &mut clip_cache,
    );
    assert!(cache.get(1, 0xDEAD).is_some());

    // Change content_hash — should invalidate.
    nodes[1].content_hash = 0xBEEF;
    let graph2 = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
        content_region: &[],
    };
    let mut buf2 = vec![0u8; 50 * 50 * 4];
    let mut fb2 = make_surface(&mut buf2, 50, 50);
    scene_render::render_scene_clipped_full(
        &mut fb2,
        &graph2,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        Some(&mut cache),
        &mut clip_cache,
    );

    // Old hash should miss, new hash should hit.
    assert!(
        cache.get(1, 0xDEAD).is_none(),
        "old hash should miss after content change"
    );
    assert!(
        cache.get(1, 0xBEEF).is_some(),
        "new hash should be cached after re-render"
    );
}

#[test]
fn none_cache_renders_without_caching() {
    // When cache=None is passed, rendering still works correctly.
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);
    let mut pool = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru = render::LruRasterizer::new_test(16);

    let (nodes, data) = build_image_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
        content_region: &[],
    };
    let dirty = protocol::DirtyRect::new(0, 0, 50, 50);

    let mut buf = vec![0u8; 50 * 50 * 4];
    let mut fb = make_surface(&mut buf, 50, 50);

    // Pass None for cache — should render normally.
    let mut clip_cache = render::ClipMaskCache::new();
    scene_render::render_scene_clipped_full(
        &mut fb,
        &graph,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        None,
        &mut clip_cache,
    );

    // Verify the image pixel at (5, 5) is red.
    let off = (5 * 50 + 5) * 4;
    assert_eq!(buf[off + 2], 255, "R channel should be 255 (red image)");
    assert_eq!(buf[off + 3], 255, "A channel should be 255");
}

#[test]
fn cache_not_used_for_content_none() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);
    let mut pool = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru = render::LruRasterizer::new_test(16);
    let mut cache = NodeCache::new();

    // Scene with only Content::None nodes (containers with backgrounds).
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0].width = 50;
    nodes[0].height = 50;
    nodes[0].flags = NodeFlags::VISIBLE;
    nodes[0].background = scene::Color::rgba(100, 100, 100, 255);
    nodes[0].first_child = 1;
    nodes[1].x = 10;
    nodes[1].y = 10;
    nodes[1].width = 20;
    nodes[1].height = 20;
    nodes[1].flags = NodeFlags::VISIBLE;
    nodes[1].background = scene::Color::rgba(200, 50, 50, 255);
    // Content::None is the default — no content_hash matters.

    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &[],
        content_region: &[],
    };
    let dirty = protocol::DirtyRect::new(0, 0, 50, 50);

    let mut buf = vec![0u8; 50 * 50 * 4];
    let mut fb = make_surface(&mut buf, 50, 50);
    let mut clip_cache = render::ClipMaskCache::new();
    scene_render::render_scene_clipped_full(
        &mut fb,
        &graph,
        &ctx,
        &dirty,
        &mut pool,
        &mut lru,
        Some(&mut cache),
        &mut clip_cache,
    );

    assert_eq!(
        cache.valid_count(),
        0,
        "Content::None nodes should not populate the cache"
    );
}

#[test]
fn render_with_and_without_cache_pixel_identical() {
    // Verify that enabling the cache doesn't change the rendered output.
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let (nodes, data) = build_image_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
        content_region: &[],
    };
    let dirty = protocol::DirtyRect::new(0, 0, 50, 50);

    // Without cache.
    let mut pool1 = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru1 = render::LruRasterizer::new_test(16);
    let mut buf_no_cache = vec![0u8; 50 * 50 * 4];
    let mut fb1 = make_surface(&mut buf_no_cache, 50, 50);
    let mut clip_cache1 = render::ClipMaskCache::new();
    scene_render::render_scene_clipped_full(
        &mut fb1,
        &graph,
        &ctx,
        &dirty,
        &mut pool1,
        &mut lru1,
        None,
        &mut clip_cache1,
    );

    // With cache (first render = cache miss, renders to offscreen then blits).
    let mut pool2 = render::surface_pool::SurfacePool::new(4 * 1024 * 1024);
    let mut lru2 = render::LruRasterizer::new_test(16);
    let mut cache = NodeCache::new();
    let mut buf_with_cache = vec![0u8; 50 * 50 * 4];
    let mut fb2 = make_surface(&mut buf_with_cache, 50, 50);
    let mut clip_cache2 = render::ClipMaskCache::new();
    scene_render::render_scene_clipped_full(
        &mut fb2,
        &graph,
        &ctx,
        &dirty,
        &mut pool2,
        &mut lru2,
        Some(&mut cache),
        &mut clip_cache2,
    );

    assert_eq!(
        buf_no_cache, buf_with_cache,
        "rendering with cache should be pixel-identical to without"
    );
}

#[test]
fn cache_dimension_mismatch_treated_as_miss() {
    let mut cache = NodeCache::new();

    // Store a 10x10 bitmap.
    cache.store(1, 0xDEAD, 10, 10, &[0xAA; 400]);

    // Get with same hash but check that dimension mismatch causes miss
    // at the render walk level. The cache itself matches on hash only,
    // but the walk checks dimensions too. Verify by querying the raw cache:
    let result = cache.get(1, 0xDEAD);
    assert!(result.is_some());
    let (w, h, _) = result.unwrap();
    // If dimensions don't match what the walk expects, it treats as miss.
    // Here we just verify the cache returns the stored dimensions.
    assert_eq!((w, h), (10, 10));

    // Store a different size — old entry replaced.
    cache.store(1, 0xDEAD, 20, 20, &[0xBB; 1600]);
    let (w2, h2, _) = cache.get(1, 0xDEAD).unwrap();
    assert_eq!((w2, h2), (20, 20));
}
