//! Tests for render::incremental — dirty rect computation from scene diffs.

use render::incremental::IncrementalState;
use scene::{Node, NodeFlags, NodeId, DIRTY_BITMAP_WORDS, NULL};

// ── Helpers ─────────────────────────────────────────────────────────

/// Create a visible node at the given position and size.
fn visible_node(x: i32, y: i32, w: u16, h: u16) -> Node {
    let mut n = Node::EMPTY;
    n.x = x;
    n.y = y;
    n.width = w;
    n.height = h;
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
fn container_node(x: i32, y: i32, w: u16, h: u16, first_child: NodeId) -> Node {
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

    // Frame 2: child moved down 20px to (50, 120, 200, 30).
    nodes[1].y = 120;
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

    // Frame 2: scroll down 50 (content_transform ty = -50).
    nodes[0].content_transform = scene::AffineTransform::translate(0.0, -50.0);
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

    // Node 0 is a leaf (no children), even if content_transform changes.
    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0] = visible_node(0, 0, 800, 600);

    state.update_from_frame(&nodes, 1);

    nodes[0].content_transform = scene::AffineTransform::translate(0.0, -50.0);
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
    nodes[0].content_transform = scene::AffineTransform::translate(0.0, -10.0);
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

    // Check prev_bounds for node 1 (child of root at (0,0) with content_transform ty=-10).
    // abs_bounds: child at (50, 100), parent adds (0, 0 + (-10)) = (50, 90).
    let (bx, by, bw, bh) = state.prev_bounds[1];
    assert_eq!(bx, 50);
    assert_eq!(by, 90, "should account for parent content_transform");
    assert_eq!(bw, 200);
    assert_eq!(bh, 30);

    // Check content_transform and content_hash.
    assert_eq!(
        state.prev_content_transform[0],
        scene::AffineTransform::translate(0.0, -10.0)
    );
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
    nodes[0].content_transform = scene::AffineTransform::translate(0.0, -100.0);
    nodes[1] = visible_node(10, 20, 100, 18);

    state.update_from_frame(&nodes, 2);

    // Scroll up by 30 (ty goes from -100 to -70).
    nodes[0].content_transform = scene::AffineTransform::translate(0.0, -70.0);
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
    nodes[1].x = 950;
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
