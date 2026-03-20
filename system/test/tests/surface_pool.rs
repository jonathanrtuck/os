//! Tests for the compositor's SurfacePool — offscreen buffer allocation,
//! caching, reuse, clearing, and memory budget enforcement.

use render::surface_pool::{SurfacePool, DEFAULT_BUDGET};

// ── Basic allocation tests ──────────────────────────────────────────

/// Pool allocates a buffer on first request with correct dimensions.
#[test]
fn acquire_allocates_on_first_request() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    let result = pool.acquire(100, 80);
    assert!(result.is_some(), "first acquire should succeed");

    let (handle, data) = result.unwrap();
    // 100 × 80 × 4 = 32000 bytes
    assert_eq!(data.len(), 100 * 80 * 4, "buffer size should match w*h*4");
    assert_eq!(pool.alloc_count(), 1, "should have allocated once");
    assert_eq!(pool.total_bytes(), 100 * 80 * 4);

    pool.release(handle);
}

/// VAL-COMP-004: Offscreen buffer dimensions match node bounds × scale factor.
/// Node 120×80 at scale=2 allocates 240×160 offscreen buffer.
#[test]
fn buffer_dimensions_match_node_bounds_times_scale() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Simulating: logical node 120×80 at scale factor 2.0
    let phys_w = (120.0f32 * 2.0) as u32; // 240
    let phys_h = (80.0f32 * 2.0) as u32; // 160

    let result = pool.acquire(phys_w, phys_h);
    assert!(result.is_some());

    let (_handle, data) = result.unwrap();
    assert_eq!(data.len(), 240 * 160 * 4, "VAL-COMP-004: buffer = 240×160×4");
}

/// VAL-COMP-005: Offscreen buffer cleared before subtree rendering.
/// All pixels transparent (0,0,0,0) before each use.
#[test]
fn buffer_cleared_to_transparent_on_acquire() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // First acquire — fresh allocation is zeroed.
    let (handle, data) = pool.acquire(10, 10).unwrap();
    assert!(data.iter().all(|&b| b == 0), "fresh buffer should be all zeros");

    // Write non-zero data to simulate rendering.
    for byte in data.iter_mut() {
        *byte = 0xAB;
    }

    // Release and re-acquire — buffer should be cleared again.
    pool.release(handle);

    let (handle2, data2) = pool.acquire(10, 10).unwrap();
    assert!(
        data2.iter().all(|&b| b == 0),
        "VAL-COMP-005: reused buffer must be cleared to transparent (all zeros)"
    );
    pool.release(handle2);
}

/// VAL-COMP-006: Pool-based offscreen buffer reuse.
/// Second frame reuses first frame's buffer — no new heap allocation.
#[test]
fn second_frame_reuses_first_frames_buffer() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Frame 1: acquire and release.
    let (h1, _) = pool.acquire(200, 150).unwrap();
    pool.release(h1);
    assert_eq!(pool.alloc_count(), 1, "frame 1: one allocation");

    pool.end_frame();

    // Frame 2: acquire same size — should reuse, NOT allocate.
    let (h2, _) = pool.acquire(200, 150).unwrap();
    pool.release(h2);
    assert_eq!(
        pool.alloc_count(),
        1,
        "VAL-COMP-006: frame 2 should reuse frame 1's buffer (alloc count unchanged)"
    );
}

/// VAL-COMP-007: Pool handles multiple simultaneous buffers.
/// Two sibling opacity nodes: both allocated, both reused next frame.
#[test]
fn two_simultaneous_buffers_allocated_and_reused() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Frame 1: acquire two buffers simultaneously.
    let (h1, _) = pool.acquire(100, 80).unwrap();
    let (h2, _) = pool.acquire(100, 80).unwrap();
    assert_eq!(pool.alloc_count(), 2, "frame 1: two allocations");

    pool.release(h1);
    pool.release(h2);
    pool.end_frame();

    // Frame 2: acquire two same-size buffers — should reuse both.
    let (h3, _) = pool.acquire(100, 80).unwrap();
    let (h4, _) = pool.acquire(100, 80).unwrap();
    assert_eq!(
        pool.alloc_count(),
        2,
        "VAL-COMP-007: frame 2 should reuse both buffers (alloc count still 2)"
    );

    pool.release(h3);
    pool.release(h4);
}

// ── Size matching tests ─────────────────────────────────────────────

/// Different buffer sizes are tracked separately — cannot reuse a 100×80
/// buffer for a 200×150 request.
#[test]
fn different_sizes_tracked_separately() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Allocate a 100×80 buffer.
    let (h1, _) = pool.acquire(100, 80).unwrap();
    pool.release(h1);
    assert_eq!(pool.alloc_count(), 1);

    // Request a different size — must allocate new buffer.
    let (h2, _) = pool.acquire(200, 150).unwrap();
    pool.release(h2);
    assert_eq!(pool.alloc_count(), 2, "different size must trigger new allocation");

    // Request original size again — reuses first buffer.
    let (h3, _) = pool.acquire(100, 80).unwrap();
    pool.release(h3);
    assert_eq!(pool.alloc_count(), 2, "original size reuses first buffer");
}

// ── Memory budget tests ─────────────────────────────────────────────

/// 3 full-screen buffers at 1024×768×4 (~3MB each, ~9MB total) don't
/// exhaust the 32 MiB heap budget.
#[test]
fn three_fullscreen_buffers_within_budget() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);
    let w = 1024u32;
    let h = 768u32;
    let buf_size = (w as usize) * (h as usize) * 4; // ~3.1 MB

    let (h1, d1) = pool.acquire(w, h).unwrap();
    assert_eq!(d1.len(), buf_size);

    let (h2, d2) = pool.acquire(w, h).unwrap();
    assert_eq!(d2.len(), buf_size);

    let (h3, d3) = pool.acquire(w, h).unwrap();
    assert_eq!(d3.len(), buf_size);

    let total = pool.total_bytes();
    assert_eq!(total, 3 * buf_size);
    assert!(
        total <= DEFAULT_BUDGET,
        "3 full-screen buffers ({} bytes) must fit within {} byte budget",
        total,
        DEFAULT_BUDGET
    );

    pool.release(h1);
    pool.release(h2);
    pool.release(h3);
}

/// Allocation that would exceed the budget returns None.
#[test]
fn allocation_exceeding_budget_returns_none() {
    // Small budget: 1 MiB
    let mut pool = SurfacePool::new(1024 * 1024);

    // First allocation: 512×512×4 = 1 MiB — should succeed.
    let (h1, _) = pool.acquire(512, 512).unwrap();

    // Second allocation of same size would exceed budget.
    let result = pool.acquire(512, 512);
    assert!(
        result.is_none(),
        "allocation exceeding budget should return None"
    );

    pool.release(h1);
}

/// Zero dimensions are rejected.
#[test]
fn zero_dimensions_rejected() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    assert!(pool.acquire(0, 100).is_none(), "zero width rejected");
    assert!(pool.acquire(100, 0).is_none(), "zero height rejected");
    assert!(pool.acquire(0, 0).is_none(), "zero w×h rejected");
    assert_eq!(pool.alloc_count(), 0, "no allocations for zero dimensions");
}

// ── Frame lifecycle tests ───────────────────────────────────────────

/// Unused entries are freed at end_frame().
#[test]
fn unused_entries_freed_at_end_frame() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Frame 1: allocate two sizes.
    let (h1, _) = pool.acquire(100, 80).unwrap();
    let (h2, _) = pool.acquire(50, 50).unwrap();
    pool.release(h1);
    pool.release(h2);
    pool.end_frame();

    assert_eq!(pool.entry_count(), 2);

    // Frame 2: only use the 100×80 size.
    let (h3, _) = pool.acquire(100, 80).unwrap();
    pool.release(h3);
    pool.end_frame();

    // The 50×50 entry was not used this frame — should be freed.
    assert_eq!(pool.entry_count(), 1, "unused 50×50 entry should be freed");
    assert_eq!(
        pool.total_bytes(),
        100 * 80 * 4,
        "only 100×80 buffer memory should remain"
    );
}

/// Entries used this frame are kept across end_frame().
#[test]
fn used_entries_survive_end_frame() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Allocate, release, end frame.
    let (h1, _) = pool.acquire(64, 64).unwrap();
    pool.release(h1);
    pool.end_frame();

    // Use same size next frame.
    let (h2, _) = pool.acquire(64, 64).unwrap();
    pool.release(h2);
    pool.end_frame();

    // Entry should still exist (reused).
    assert_eq!(pool.entry_count(), 1);
    assert_eq!(pool.alloc_count(), 1, "should have allocated only once across frames");
}

/// Multiple end_frame() calls shrink the pool when scene simplifies.
#[test]
fn pool_shrinks_when_scene_simplifies() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Frame 1: complex scene with 4 buffers.
    let mut handles = Vec::new();
    for i in 0..4 {
        let (h, _) = pool.acquire(100 + i * 10, 80).unwrap();
        handles.push(h);
    }
    for h in &handles {
        pool.release(*h);
    }
    pool.end_frame();
    assert_eq!(pool.entry_count(), 4);

    // Frame 2: simple scene with 0 buffers.
    pool.end_frame();
    assert_eq!(pool.entry_count(), 0, "all entries freed when nothing used");
    assert_eq!(pool.total_bytes(), 0, "all memory reclaimed");
}

/// Buffer that's still in_use is NOT freed by end_frame().
#[test]
fn in_use_buffer_not_freed_by_end_frame() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    let (h1, _) = pool.acquire(100, 80).unwrap();
    // Don't release h1 — it's still in use.

    pool.end_frame();

    // The in-use entry should survive.
    assert_eq!(pool.entry_count(), 1, "in-use entry should not be freed");

    pool.release(h1);
}

// ── Clearing verification ───────────────────────────────────────────

/// Verify clearing across frames: data from frame 1 does not leak into
/// frame 2's buffer.
#[test]
fn no_stale_data_across_frames() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Frame 1: fill with non-zero data.
    let (h1, data1) = pool.acquire(20, 20).unwrap();
    for (i, byte) in data1.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }
    pool.release(h1);
    pool.end_frame();

    // Frame 2: reacquire — must be all zeros.
    let (h2, data2) = pool.acquire(20, 20).unwrap();
    for (i, &byte) in data2.iter().enumerate() {
        assert_eq!(
            byte, 0,
            "stale data leaked at byte offset {}: got {}, expected 0",
            i, byte
        );
    }
    pool.release(h2);
}

/// Budget accounting: releasing and re-acquiring doesn't double-count.
#[test]
fn budget_accounting_consistent_across_reuse() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);
    let buf_size = 100 * 80 * 4;

    let (h1, _) = pool.acquire(100, 80).unwrap();
    assert_eq!(pool.total_bytes(), buf_size);

    pool.release(h1);
    assert_eq!(pool.total_bytes(), buf_size, "release doesn't change total_bytes");

    let (h2, _) = pool.acquire(100, 80).unwrap();
    assert_eq!(
        pool.total_bytes(),
        buf_size,
        "reuse doesn't increase total_bytes"
    );

    pool.release(h2);
}

/// Budget decreases when entries are freed at end_frame.
#[test]
fn budget_decreases_when_entries_freed() {
    let mut pool = SurfacePool::new(DEFAULT_BUDGET);

    // Frame 1: allocate and release two buffers.
    let (h1, _) = pool.acquire(100, 80).unwrap();
    let (h2, _) = pool.acquire(50, 50).unwrap();
    pool.release(h1);
    pool.release(h2);

    let total_before = pool.total_bytes();
    assert_eq!(total_before, 100 * 80 * 4 + 50 * 50 * 4);

    pool.end_frame(); // Both used this frame — kept for reuse.
    assert_eq!(pool.entry_count(), 2, "both entries kept after frame 1");

    // Frame 2: don't use either entry → both freed at end_frame.
    pool.end_frame();

    assert_eq!(pool.total_bytes(), 0, "total_bytes should be 0 after freeing all entries");
    assert_eq!(pool.entry_count(), 0);
}
