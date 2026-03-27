use protocol::content::{
    ContentAllocator, ContentClass, ContentEntry, ContentRegionHeader, CONTENT_ALLOC_ALIGN,
    CONTENT_HEADER_SIZE, CONTENT_ID_DYNAMIC_START, CONTENT_ID_FONT_MONO, CONTENT_ID_NONE,
    CONTENT_REGION_MAGIC, CONTENT_REGION_VERSION, MAX_CONTENT_ENTRIES, MAX_PENDING_FREE,
};

// ── Helpers ────────────────────────────────────────────────────────

/// Align up for test assertions.
fn align_up(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

/// Create a zeroed Content Region header for testing.
fn test_header() -> ContentRegionHeader {
    ContentRegionHeader {
        magic: CONTENT_REGION_MAGIC,
        version: CONTENT_REGION_VERSION,
        entry_count: 0,
        max_entries: MAX_CONTENT_ENTRIES as u32,
        data_offset: CONTENT_HEADER_SIZE as u32,
        next_alloc: CONTENT_HEADER_SIZE as u32,
        _reserved: [0; 2],
        entries: [ContentEntry::EMPTY; MAX_CONTENT_ENTRIES],
    }
}

/// Add a test entry to a header and return its index.
fn add_entry(header: &mut ContentRegionHeader, content_id: u32, offset: u32, length: u32) {
    let idx = header.entry_count as usize;
    header.entries[idx] = ContentEntry {
        content_id,
        offset,
        length,
        class: ContentClass::Pixels as u8,
        _pad: [0; 3],
        width: 0,
        height: 0,
        generation: 0,
    };
    header.entry_count += 1;
}

// ── ContentAllocator::new ──────────────────────────────────────────

#[test]
fn new_single_free_block() {
    let a = ContentAllocator::new(2048, 4 * 1024 * 1024);
    assert_eq!(a.block_count(), 1);
    assert_eq!(a.free_bytes(), 4 * 1024 * 1024 - 2048);
}

#[test]
fn new_aligns_start_up() {
    // free_start=2049 should align up to 2064 (next 16-byte boundary).
    let a = ContentAllocator::new(2049, 4096);
    let aligned = align_up(2049, CONTENT_ALLOC_ALIGN);
    assert_eq!(aligned, 2064);
    assert_eq!(a.free_bytes(), 4096 - aligned);
    assert_eq!(a.block_count(), 1);
}

#[test]
fn new_zero_size_region() {
    let a = ContentAllocator::new(4096, 4096);
    assert_eq!(a.block_count(), 0);
    assert_eq!(a.free_bytes(), 0);
}

#[test]
fn new_start_exceeds_end() {
    let a = ContentAllocator::new(8192, 4096);
    assert_eq!(a.block_count(), 0);
    assert_eq!(a.free_bytes(), 0);
}

#[test]
fn new_unaligned_start_exceeds_end_after_alignment() {
    // free_start=4090 aligns up to 4096, which equals free_end → empty.
    let a = ContentAllocator::new(4090, 4096);
    assert_eq!(a.block_count(), 0);
}

// ── allocate ───────────────────────────────────────────────────────

#[test]
fn allocate_basic() {
    let mut a = ContentAllocator::new(2048, 65536);
    let off = a.allocate(1024).unwrap();
    assert_eq!(off, 2048);
    assert_eq!(a.free_bytes(), 65536 - 2048 - 1024);
}

#[test]
fn allocate_zero_returns_none() {
    let mut a = ContentAllocator::new(2048, 65536);
    assert!(a.allocate(0).is_none());
    // No state change.
    assert_eq!(a.block_count(), 1);
}

#[test]
fn allocate_aligns_size_up() {
    let mut a = ContentAllocator::new(2048, 65536);
    let off = a.allocate(100).unwrap();
    assert_eq!(off, 2048);
    // 100 rounds up to 112 (7 × 16).
    let expected_consumed = align_up(100, CONTENT_ALLOC_ALIGN);
    assert_eq!(expected_consumed, 112);
    assert_eq!(a.free_bytes(), 65536 - 2048 - expected_consumed);
}

#[test]
fn allocate_sequential() {
    let mut a = ContentAllocator::new(2048, 65536);
    let off1 = a.allocate(256).unwrap();
    let off2 = a.allocate(512).unwrap();
    let off3 = a.allocate(128).unwrap();
    assert_eq!(off1, 2048);
    assert_eq!(off2, 2048 + 256);
    assert_eq!(off3, 2048 + 256 + 512);
    assert_eq!(a.block_count(), 1); // One remaining free block.
}

#[test]
fn allocate_exact_fit_removes_block() {
    let remaining = 65536 - 2048;
    let mut a = ContentAllocator::new(2048, 65536);
    let off = a.allocate(remaining).unwrap();
    assert_eq!(off, 2048);
    assert_eq!(a.block_count(), 0);
    assert_eq!(a.free_bytes(), 0);
}

#[test]
fn allocate_exhaustion() {
    let mut a = ContentAllocator::new(2048, 2048 + 256);
    let _ = a.allocate(256).unwrap();
    assert!(a.allocate(1).is_none());
}

#[test]
fn allocate_too_large() {
    let mut a = ContentAllocator::new(2048, 4096);
    assert!(a.allocate(4096).is_none()); // Only 2048 bytes free.
}

// ── free + reuse ───────────────────────────────────────────────────

#[test]
fn free_and_reallocate() {
    let mut a = ContentAllocator::new(2048, 65536);
    let off1 = a.allocate(1024).unwrap();
    let free_before = a.free_bytes();
    a.free(off1, 1024);
    assert_eq!(a.free_bytes(), free_before + 1024);
    // Reallocate the same space.
    let off2 = a.allocate(1024).unwrap();
    assert_eq!(off2, off1);
}

#[test]
fn free_zero_is_noop() {
    let mut a = ContentAllocator::new(2048, 65536);
    let before = a.free_bytes();
    a.free(2048, 0);
    assert_eq!(a.free_bytes(), before);
    assert_eq!(a.block_count(), 1);
}

// ── coalescing ─────────────────────────────────────────────────────

#[test]
fn coalesce_left() {
    let mut a = ContentAllocator::new(2048, 65536);
    let off1 = a.allocate(256).unwrap();
    let off2 = a.allocate(256).unwrap();
    let _off3 = a.allocate(256).unwrap();
    // Free off1, then off2 (left neighbor should coalesce).
    a.free(off1, 256);
    assert_eq!(a.block_count(), 2); // [off1..off1+256] and [off3+256..end]
    a.free(off2, 256);
    assert_eq!(a.block_count(), 2); // [off1..off2+256] merged, [off3+256..end]
                                    // First free block should span both.
}

#[test]
fn coalesce_right() {
    let mut a = ContentAllocator::new(2048, 65536);
    let off1 = a.allocate(256).unwrap();
    let off2 = a.allocate(256).unwrap();
    let _off3 = a.allocate(256).unwrap();
    // Free off2, then off1 (right neighbor should coalesce).
    a.free(off2, 256);
    assert_eq!(a.block_count(), 2);
    a.free(off1, 256);
    assert_eq!(a.block_count(), 2); // off1+off2 merged, then trailing block
}

#[test]
fn coalesce_both() {
    // Allocate the entire region (no trailing block) so we control adjacency.
    let mut a = ContentAllocator::new(0, 1024);
    let off1 = a.allocate(256).unwrap(); // 0
    let off2 = a.allocate(256).unwrap(); // 256
    let off3 = a.allocate(256).unwrap(); // 512
    let _off4 = a.allocate(256).unwrap(); // 768 — exhausts region
    assert_eq!(a.block_count(), 0);
    // Free off1 and off3 — two non-adjacent free blocks.
    a.free(off1, 256);
    a.free(off3, 256);
    assert_eq!(a.block_count(), 2); // [0..256], [512..768]
    // Free off2 — should three-way merge: off1 + off2 + off3.
    a.free(off2, 256);
    assert_eq!(a.block_count(), 1);
    assert_eq!(a.free_bytes(), 768);
}

#[test]
fn coalesce_restores_full_region() {
    let mut a = ContentAllocator::new(2048, 65536);
    let total = a.free_bytes();
    let off1 = a.allocate(1024).unwrap();
    let off2 = a.allocate(2048).unwrap();
    let off3 = a.allocate(512).unwrap();
    a.free(off2, 2048);
    a.free(off1, 1024);
    a.free(off3, 512);
    assert_eq!(a.free_bytes(), total);
    assert_eq!(a.block_count(), 1); // Fully coalesced.
}

// ── fragmentation ──────────────────────────────────────────────────

#[test]
fn fragmented_first_fit() {
    let mut a = ContentAllocator::new(0, 1024);
    // Allocate four 128-byte blocks.
    let off0 = a.allocate(128).unwrap();
    let _off1 = a.allocate(128).unwrap();
    let off2 = a.allocate(128).unwrap();
    let off3 = a.allocate(128).unwrap();
    // Free alternating blocks → fragmented free-list.
    a.free(off0, 128);
    a.free(off2, 128);
    assert_eq!(a.block_count(), 3); // [off0], [off2], [off3+128..end]
                                    // Allocate 128 — should fit in the first gap (first-fit).
    let reuse = a.allocate(128).unwrap();
    assert_eq!(reuse, off0);
    // Allocate 256 — doesn't fit in 128-byte gap at off2, takes trailing.
    let big = a.allocate(256).unwrap();
    assert_eq!(big, off3 + 128); // From the trailing free block.
}

#[test]
fn largest_free_tracks_maximum() {
    // Allocate entire region to avoid trailing-block coalescing.
    let mut a = ContentAllocator::new(0, 4096);
    assert_eq!(a.largest_free(), 4096);
    let _off1 = a.allocate(1024).unwrap(); // 0..1024
    let off2 = a.allocate(2048).unwrap(); // 1024..3072
    let _off3 = a.allocate(1024).unwrap(); // 3072..4096
    assert_eq!(a.block_count(), 0);
    // Free the middle block — it's isolated between two allocations.
    a.free(off2, 2048);
    assert_eq!(a.largest_free(), 2048);
    assert_eq!(a.block_count(), 1);
}

// ── alignment edge cases ───────────────────────────────────────────

#[test]
fn allocate_odd_sizes_stay_aligned() {
    let mut a = ContentAllocator::new(0, 65536);
    let off1 = a.allocate(1).unwrap(); // 1 → 16
    let off2 = a.allocate(17).unwrap(); // 17 → 32
    let off3 = a.allocate(33).unwrap(); // 33 → 48
    assert_eq!(off1, 0);
    assert_eq!(off2, 16);
    assert_eq!(off3, 48);
    assert_eq!(off1 % CONTENT_ALLOC_ALIGN, 0);
    assert_eq!(off2 % CONTENT_ALLOC_ALIGN, 0);
    assert_eq!(off3 % CONTENT_ALLOC_ALIGN, 0);
}

#[test]
fn free_aligns_length_to_match_allocate() {
    let mut a = ContentAllocator::new(0, 1024);
    let off = a.allocate(100).unwrap(); // Consumes 112 bytes.
    let free_after_alloc = a.free_bytes();
    a.free(off, 100); // Frees 112 bytes (aligned up).
    assert_eq!(
        a.free_bytes(),
        free_after_alloc + align_up(100, CONTENT_ALLOC_ALIGN)
    );
    assert_eq!(a.block_count(), 1); // Coalesced back.
}

// ── remove_entry ───────────────────────────────────────────────────

#[test]
fn remove_entry_basic() {
    let mut h = test_header();
    add_entry(&mut h, CONTENT_ID_FONT_MONO, 2048, 100_000);
    add_entry(&mut h, CONTENT_ID_DYNAMIC_START, 102_048, 50_000);
    assert_eq!(h.entry_count, 2);

    let result = protocol::content::remove_entry(&mut h, CONTENT_ID_DYNAMIC_START);
    assert_eq!(result, Some((102_048, 50_000)));
    assert_eq!(h.entry_count, 1);
    assert_eq!(h.entries[0].content_id, CONTENT_ID_FONT_MONO);
    assert_eq!(h.entries[1].content_id, CONTENT_ID_NONE); // Cleared.
}

#[test]
fn remove_entry_not_found() {
    let mut h = test_header();
    add_entry(&mut h, CONTENT_ID_FONT_MONO, 2048, 100_000);
    assert!(protocol::content::remove_entry(&mut h, 999).is_none());
    assert_eq!(h.entry_count, 1); // Unchanged.
}

#[test]
fn remove_entry_compacts() {
    let mut h = test_header();
    add_entry(&mut h, 1, 2048, 100);
    add_entry(&mut h, 2, 2148, 200);
    add_entry(&mut h, 3, 2348, 300);
    // Remove the middle entry.
    let result = protocol::content::remove_entry(&mut h, 2);
    assert_eq!(result, Some((2148, 200)));
    assert_eq!(h.entry_count, 2);
    assert_eq!(h.entries[0].content_id, 1);
    assert_eq!(h.entries[1].content_id, 3);
    assert_eq!(h.entries[2].content_id, CONTENT_ID_NONE);
}

#[test]
fn remove_entry_empty_header() {
    let mut h = test_header();
    assert!(protocol::content::remove_entry(&mut h, 1).is_none());
}

// ── Integration: allocator + registry ──────────────────────────────

#[test]
fn allocate_then_remove_then_reuse() {
    // Simulates the full lifecycle: alloc space → register entry → remove → free → realloc.
    let region_size: u32 = 4 * 1024 * 1024;
    let mut header = test_header();
    header.next_alloc = CONTENT_HEADER_SIZE as u32;
    let mut alloc = ContentAllocator::new(header.next_alloc, region_size);

    // Allocate space for a 100×100 BGRA image (40,000 bytes).
    let pixel_bytes: u32 = 100 * 100 * 4;
    let offset = alloc.allocate(pixel_bytes).unwrap();

    // Register it.
    let content_id = CONTENT_ID_DYNAMIC_START;
    add_entry(&mut header, content_id, offset, pixel_bytes);
    assert!(protocol::content::find_entry(&header, content_id).is_some());

    // Remove the entry and free the space.
    let (freed_offset, freed_length) =
        protocol::content::remove_entry(&mut header, content_id).unwrap();
    alloc.free(freed_offset, freed_length);

    // Reallocate — should reuse the same offset.
    let new_offset = alloc.allocate(pixel_bytes).unwrap();
    assert_eq!(new_offset, offset);
}

// ── Deferred reclamation (GC) ──────────────────────────────────────

#[test]
fn defer_free_enqueues() {
    let mut alloc = ContentAllocator::new(0, 4096);
    assert_eq!(alloc.pending_count(), 0);
    assert!(alloc.defer_free(CONTENT_ID_DYNAMIC_START, 5));
    assert_eq!(alloc.pending_count(), 1);
}

#[test]
fn defer_free_queue_full_returns_false() {
    let mut alloc = ContentAllocator::new(0, 4096);
    for i in 0..MAX_PENDING_FREE as u32 {
        assert!(alloc.defer_free(CONTENT_ID_DYNAMIC_START + i, 1));
    }
    assert_eq!(alloc.pending_count(), MAX_PENDING_FREE);
    // Queue is full.
    assert!(!alloc.defer_free(999, 1));
    assert_eq!(alloc.pending_count(), MAX_PENDING_FREE);
}

#[test]
fn sweep_does_nothing_when_reader_behind() {
    let mut header = test_header();
    let mut alloc = ContentAllocator::new(CONTENT_HEADER_SIZE as u32, 65536);
    let offset = alloc.allocate(1024).unwrap();
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START, offset, 1024);

    // Retire at generation 10.
    alloc.defer_free(CONTENT_ID_DYNAMIC_START, 10);
    assert_eq!(alloc.pending_count(), 1);

    // Reader is at generation 5 — too early, nothing reclaimed.
    let reclaimed = alloc.sweep(5, &mut header);
    assert_eq!(reclaimed, 0);
    assert_eq!(alloc.pending_count(), 1);
    // Entry still in registry.
    assert!(protocol::content::find_entry(&header, CONTENT_ID_DYNAMIC_START).is_some());
}

#[test]
fn sweep_reclaims_when_reader_caught_up() {
    let mut header = test_header();
    let mut alloc = ContentAllocator::new(CONTENT_HEADER_SIZE as u32, 65536);
    let free_before = alloc.free_bytes();
    let offset = alloc.allocate(1024).unwrap();
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START, offset, 1024);

    alloc.defer_free(CONTENT_ID_DYNAMIC_START, 10);

    // Reader caught up to generation 10 — safe to reclaim.
    let reclaimed = alloc.sweep(10, &mut header);
    assert_eq!(reclaimed, 1);
    assert_eq!(alloc.pending_count(), 0);
    // Entry removed from registry.
    assert!(protocol::content::find_entry(&header, CONTENT_ID_DYNAMIC_START).is_none());
    assert_eq!(header.entry_count, 0);
    // Space returned to free-list.
    assert_eq!(alloc.free_bytes(), free_before);
}

#[test]
fn sweep_reclaims_when_reader_ahead() {
    let mut header = test_header();
    let mut alloc = ContentAllocator::new(CONTENT_HEADER_SIZE as u32, 65536);
    let offset = alloc.allocate(512).unwrap();
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START, offset, 512);

    alloc.defer_free(CONTENT_ID_DYNAMIC_START, 10);

    // Reader is past generation 10.
    let reclaimed = alloc.sweep(15, &mut header);
    assert_eq!(reclaimed, 1);
    assert_eq!(alloc.pending_count(), 0);
}

#[test]
fn sweep_partial_reclaim() {
    let mut header = test_header();
    let mut alloc = ContentAllocator::new(CONTENT_HEADER_SIZE as u32, 65536);

    // Two images at different generations.
    let off1 = alloc.allocate(1024).unwrap();
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START, off1, 1024);
    alloc.defer_free(CONTENT_ID_DYNAMIC_START, 5);

    let off2 = alloc.allocate(2048).unwrap();
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START + 1, off2, 2048);
    alloc.defer_free(CONTENT_ID_DYNAMIC_START + 1, 15);

    // Reader at gen 10: first entry is safe, second is not.
    let reclaimed = alloc.sweep(10, &mut header);
    assert_eq!(reclaimed, 1);
    assert_eq!(alloc.pending_count(), 1);
    assert!(protocol::content::find_entry(&header, CONTENT_ID_DYNAMIC_START).is_none());
    assert!(protocol::content::find_entry(&header, CONTENT_ID_DYNAMIC_START + 1).is_some());

    // Reader advances to gen 15: second entry now safe.
    let reclaimed = alloc.sweep(15, &mut header);
    assert_eq!(reclaimed, 1);
    assert_eq!(alloc.pending_count(), 0);
}

#[test]
fn sweep_coalesces_freed_space() {
    let mut header = test_header();
    let mut alloc = ContentAllocator::new(CONTENT_HEADER_SIZE as u32, 65536);
    let total_free = alloc.free_bytes();

    // Allocate three adjacent blocks.
    let off1 = alloc.allocate(256).unwrap();
    let off2 = alloc.allocate(256).unwrap();
    let off3 = alloc.allocate(256).unwrap();
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START, off1, 256);
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START + 1, off2, 256);
    add_entry(&mut header, CONTENT_ID_DYNAMIC_START + 2, off3, 256);

    // Retire all three at the same generation.
    alloc.defer_free(CONTENT_ID_DYNAMIC_START, 1);
    alloc.defer_free(CONTENT_ID_DYNAMIC_START + 1, 1);
    alloc.defer_free(CONTENT_ID_DYNAMIC_START + 2, 1);

    // Sweep — all three freed and coalesced.
    alloc.sweep(1, &mut header);
    assert_eq!(alloc.pending_count(), 0);
    assert_eq!(alloc.free_bytes(), total_free);
    // Should coalesce into a single block with the trailing free space.
    assert_eq!(alloc.block_count(), 1);
}

#[test]
fn sweep_with_empty_pending_is_noop() {
    let mut header = test_header();
    let mut alloc = ContentAllocator::new(CONTENT_HEADER_SIZE as u32, 65536);
    let free_before = alloc.free_bytes();
    let reclaimed = alloc.sweep(100, &mut header);
    assert_eq!(reclaimed, 0);
    assert_eq!(alloc.free_bytes(), free_before);
}

#[test]
fn full_lifecycle_alloc_register_defer_sweep_reuse() {
    let region_size: u32 = 4 * 1024 * 1024;
    let mut header = test_header();
    header.next_alloc = CONTENT_HEADER_SIZE as u32;
    let mut alloc = ContentAllocator::new(header.next_alloc, region_size);
    let total_free = alloc.free_bytes();

    // Allocate and register an image.
    let pixel_bytes: u32 = 200 * 200 * 4;
    let offset = alloc.allocate(pixel_bytes).unwrap();
    let content_id = CONTENT_ID_DYNAMIC_START;
    add_entry(&mut header, content_id, offset, pixel_bytes);

    // Core publishes scene at gen 5 referencing this image.
    // Later, core publishes scene at gen 8 WITHOUT this image.
    alloc.defer_free(content_id, 8);
    assert_eq!(alloc.pending_count(), 1);

    // Reader finishes gen 7 — not yet safe.
    assert_eq!(alloc.sweep(7, &mut header), 0);
    assert_eq!(alloc.pending_count(), 1);

    // Reader finishes gen 8 — safe to reclaim.
    assert_eq!(alloc.sweep(8, &mut header), 1);
    assert_eq!(alloc.pending_count(), 0);
    assert_eq!(header.entry_count, 0);
    assert_eq!(alloc.free_bytes(), total_free);

    // Space can be reused.
    let new_offset = alloc.allocate(pixel_bytes).unwrap();
    assert_eq!(new_offset, offset);
}
