//! Host-side tests for user ASLR (address space layout randomization).
//!
//! Tests that per-process address space bases are randomized within their
//! allowed regions, that different PRNG seeds produce different layouts,
//! and that every process gets the same usable VA regardless of ASLR roll.

#[path = "../../random.rs"]
mod random;

/// ASLR layout computation — extracted from address_space for testability.
/// These functions compute randomized base addresses for each VA region.
#[path = "../../aslr.rs"]
mod aslr;

use aslr::*;
use random::*;

fn test_prng(seed: u8) -> Prng {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[seed; 32], 256);
    pool.try_seal().unwrap()
}

// Page size (16 KiB) — matches system_config.
const PAGE_SIZE: u64 = 16384;

// =========================================================================
// Region spec model — source of truth
// =========================================================================

#[test]
fn region_specs_are_consistent() {
    // Every region spec must have usable > 0 and entropy_bits > 0.
    for spec in &REGION_SPECS {
        assert!(spec.usable > 0, "{}: usable must be > 0", spec.name);
        assert!(
            spec.entropy_bits > 0,
            "{}: entropy_bits must be > 0",
            spec.name
        );
        // Usable must be page-aligned.
        assert_eq!(
            spec.usable % PAGE_SIZE,
            0,
            "{}: usable must be page-aligned",
            spec.name
        );
    }
}

#[test]
fn slide_window_derived_from_entropy_bits() {
    // slide_window = (1 << entropy_bits) * PAGE_SIZE
    for spec in &REGION_SPECS {
        let expected_slide = (1u64 << spec.entropy_bits) * PAGE_SIZE;
        assert_eq!(
            spec.slide_window(),
            expected_slide,
            "{}: slide_window mismatch",
            spec.name
        );
    }
}

#[test]
fn outer_size_is_usable_plus_slide() {
    for spec in &REGION_SPECS {
        assert_eq!(
            spec.outer_size(),
            spec.usable + spec.slide_window(),
            "{}: outer_size mismatch",
            spec.name
        );
    }
}

// =========================================================================
// AslrLayout has per-process endpoints (_end = base + usable)
// =========================================================================

#[test]
fn layout_end_equals_base_plus_usable() {
    // For every seed, every region's _end must equal base + usable_size.
    let heap_usable = HEAP_SPEC.usable;
    let dma_usable = DMA_SPEC.usable;
    let device_usable = DEVICE_SPEC.usable;

    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        assert_eq!(
            layout.heap_end,
            layout.heap_base + heap_usable,
            "seed {}: heap_end mismatch",
            seed
        );
        assert_eq!(
            layout.dma_end,
            layout.dma_base + dma_usable,
            "seed {}: dma_end mismatch",
            seed
        );
        assert_eq!(
            layout.device_end,
            layout.device_base + device_usable,
            "seed {}: device_end mismatch",
            seed
        );
    }
}

#[test]
fn deterministic_layout_end_fields() {
    let layout = AslrLayout::deterministic();

    assert_eq!(layout.heap_end, layout.heap_base + HEAP_SPEC.usable);
    assert_eq!(layout.dma_end, layout.dma_base + DMA_SPEC.usable);
    assert_eq!(layout.device_end, layout.device_base + DEVICE_SPEC.usable);
}

// =========================================================================
// Constant usable size — the ASLR fix
// =========================================================================

#[test]
fn heap_va_size_is_constant_across_seeds() {
    // The heap usable size must be the same for every process, regardless
    // of ASLR seed. This is the core property that was broken before.
    let expected = HEAP_SPEC.usable;

    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);
        let actual = layout.heap_end - layout.heap_base;

        assert_eq!(
            actual, expected,
            "seed {}: heap usable {:#x} != expected {:#x}",
            seed, actual, expected,
        );
    }
}

#[test]
fn dma_va_size_is_constant_across_seeds() {
    let expected = DMA_SPEC.usable;

    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);
        let actual = layout.dma_end - layout.dma_base;

        assert_eq!(
            actual, expected,
            "seed {}: DMA usable {:#x} != expected {:#x}",
            seed, actual, expected,
        );
    }
}

#[test]
fn device_va_size_is_constant_across_seeds() {
    let expected = DEVICE_SPEC.usable;

    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);
        let actual = layout.device_end - layout.device_base;

        assert_eq!(
            actual, expected,
            "seed {}: device usable {:#x} != expected {:#x}",
            seed, actual, expected,
        );
    }
}

// =========================================================================
// Per-process bases within outer region bounds
// =========================================================================

#[test]
fn per_process_heap_within_outer_bounds() {
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        assert!(
            layout.heap_base >= HEAP_REGION_START,
            "seed {}: heap_base {:#x} below region start {:#x}",
            seed,
            layout.heap_base,
            HEAP_REGION_START
        );
        assert!(
            layout.heap_end <= HEAP_REGION_END,
            "seed {}: heap_end {:#x} above region end {:#x}",
            seed,
            layout.heap_end,
            HEAP_REGION_END
        );
    }
}

#[test]
fn per_process_dma_within_outer_bounds() {
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        assert!(
            layout.dma_base >= DMA_REGION_START,
            "seed {}: dma_base below region start",
            seed
        );
        assert!(
            layout.dma_end <= DMA_REGION_END,
            "seed {}: dma_end above region end",
            seed
        );
    }
}

#[test]
fn per_process_device_within_outer_bounds() {
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        assert!(
            layout.device_base >= DEVICE_REGION_START,
            "seed {}: device_base below region start",
            seed
        );
        assert!(
            layout.device_end <= DEVICE_REGION_END,
            "seed {}: device_end above region end",
            seed
        );
    }
}

// =========================================================================
// Region base randomization (original tests, updated for new layout)
// =========================================================================

#[test]
fn randomized_layout_bases_are_page_aligned() {
    let mut prng = test_prng(0xAA);
    let layout = AslrLayout::randomize(&mut prng);

    assert_eq!(layout.heap_base % PAGE_SIZE, 0);
    assert_eq!(layout.dma_base % PAGE_SIZE, 0);
    assert_eq!(layout.device_base % PAGE_SIZE, 0);
    assert_eq!(layout.channel_shm_base % PAGE_SIZE, 0);
    assert_eq!(layout.shared_base % PAGE_SIZE, 0);
    assert_eq!(layout.stack_top % PAGE_SIZE, 0);
}

#[test]
fn randomized_layout_bases_within_bounds() {
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        assert!(
            layout.heap_base >= HEAP_REGION_START && layout.heap_end <= HEAP_REGION_END,
            "seed {}: heap [{:#x}, {:#x}) outside [{:#x}, {:#x})",
            seed,
            layout.heap_base,
            layout.heap_end,
            HEAP_REGION_START,
            HEAP_REGION_END,
        );
        assert!(
            layout.dma_base >= DMA_REGION_START && layout.dma_end <= DMA_REGION_END,
            "seed {}: DMA out of bounds",
            seed,
        );
        assert!(
            layout.device_base >= DEVICE_REGION_START && layout.device_end <= DEVICE_REGION_END,
            "seed {}: device out of bounds",
            seed,
        );
        assert!(
            layout.channel_shm_base >= CHANNEL_SHM_REGION_START
                && layout.channel_shm_base < CHANNEL_SHM_REGION_END,
            "seed {}: channel_shm out of bounds",
            seed,
        );
        assert!(
            layout.shared_base >= SHARED_REGION_START && layout.shared_base < SHARED_REGION_END,
            "seed {}: shared out of bounds",
            seed,
        );
        assert!(
            layout.stack_top >= STACK_REGION_START && layout.stack_top <= STACK_REGION_END,
            "seed {}: stack_top out of bounds",
            seed,
        );
    }
}

#[test]
fn different_seeds_produce_different_layouts() {
    let mut prng_a = test_prng(0x11);
    let mut prng_b = test_prng(0x22);

    let layout_a = AslrLayout::randomize(&mut prng_a);
    let layout_b = AslrLayout::randomize(&mut prng_b);

    let differs = layout_a.heap_base != layout_b.heap_base
        || layout_a.dma_base != layout_b.dma_base
        || layout_a.device_base != layout_b.device_base
        || layout_a.channel_shm_base != layout_b.channel_shm_base
        || layout_a.shared_base != layout_b.shared_base
        || layout_a.stack_top != layout_b.stack_top;

    assert!(differs, "different seeds should produce different layouts");
}

#[test]
fn same_seed_produces_same_layout() {
    let mut prng_a = test_prng(0x33);
    let mut prng_b = test_prng(0x33);

    let layout_a = AslrLayout::randomize(&mut prng_a);
    let layout_b = AslrLayout::randomize(&mut prng_b);

    assert_eq!(layout_a.heap_base, layout_b.heap_base);
    assert_eq!(layout_a.heap_end, layout_b.heap_end);
    assert_eq!(layout_a.dma_base, layout_b.dma_base);
    assert_eq!(layout_a.dma_end, layout_b.dma_end);
    assert_eq!(layout_a.device_base, layout_b.device_base);
    assert_eq!(layout_a.device_end, layout_b.device_end);
    assert_eq!(layout_a.channel_shm_base, layout_b.channel_shm_base);
    assert_eq!(layout_a.shared_base, layout_b.shared_base);
    assert_eq!(layout_a.stack_top, layout_b.stack_top);
}

#[test]
fn deterministic_layout_uses_region_starts() {
    let layout = AslrLayout::deterministic();

    assert_eq!(layout.heap_base, HEAP_REGION_START);
    assert_eq!(layout.dma_base, DMA_REGION_START);
    assert_eq!(layout.device_base, DEVICE_REGION_START);
    assert_eq!(layout.channel_shm_base, CHANNEL_SHM_REGION_START);
    assert_eq!(layout.shared_base, SHARED_REGION_START);
    assert_eq!(layout.stack_top, STACK_REGION_END);
}

#[test]
fn channel_shm_and_shared_are_fixed_even_with_randomization() {
    // Phase 1: channel SHM and shared stay fixed (userspace dependency).
    // Full ASLR requires Phase 3 (bootstrap page) + Phase 4.
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);
        assert_eq!(
            layout.channel_shm_base, CHANNEL_SHM_REGION_START,
            "seed {}: channel_shm should be fixed",
            seed
        );
        assert_eq!(
            layout.shared_base, SHARED_REGION_START,
            "seed {}: shared should be fixed",
            seed
        );
    }
}

// =========================================================================
// Entropy measurement
// =========================================================================

#[test]
fn heap_base_has_meaningful_entropy() {
    let mut seen = std::collections::HashSet::new();
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);
        seen.insert(layout.heap_base);
    }

    assert!(
        seen.len() >= 16,
        "heap_base took only {} distinct values across 256 seeds — too few",
        seen.len()
    );
}

#[test]
fn stack_top_has_meaningful_entropy() {
    let mut seen = std::collections::HashSet::new();
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);
        seen.insert(layout.stack_top);
    }

    assert!(
        seen.len() >= 16,
        "stack_top took only {} distinct values across 256 seeds — too few",
        seen.len()
    );
}

// =========================================================================
// Region non-overlap
// =========================================================================

#[test]
fn randomized_regions_do_not_overlap() {
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        // Outer regions are ordered and non-overlapping by construction.
        // Per-process regions are subsets, so they also don't overlap.
        assert!(
            layout.heap_end <= layout.dma_base || layout.heap_base >= layout.dma_end,
            "seed {}: heap and dma overlap",
            seed
        );
        assert!(
            layout.dma_end <= layout.device_base || layout.dma_base >= layout.device_end,
            "seed {}: dma and device overlap",
            seed
        );
    }
}

// =========================================================================
// Outer bounds derived from specs (compile-time)
// =========================================================================

#[test]
fn outer_region_bounds_derived_from_specs() {
    // The outer region constants in aslr.rs must exactly equal
    // region_start + outer_size for each region. This verifies the
    // constants aren't hand-placed but derived from specs.
    assert_eq!(HEAP_REGION_END, HEAP_REGION_START + HEAP_SPEC.outer_size());
    assert_eq!(DMA_REGION_END, DMA_REGION_START + DMA_SPEC.outer_size());
    assert_eq!(
        DEVICE_REGION_END,
        DEVICE_REGION_START + DEVICE_SPEC.outer_size()
    );
}

#[test]
fn regions_do_not_overlap_or_cross() {
    // Regions are placed contiguously (no guard gaps in Phase 1).
    // Guard gaps are added in Phase 6 when T0SZ shrinks.
    // Verify ordering: heap ≤ DMA ≤ device ≤ channel SHM.
    assert!(HEAP_REGION_END <= DMA_REGION_START, "heap overlaps DMA");
    assert!(DMA_REGION_END <= DEVICE_REGION_START, "DMA overlaps device");
    assert!(
        DEVICE_REGION_END <= CHANNEL_SHM_REGION_START,
        "device overlaps channel SHM"
    );
}

// =========================================================================
// Entropy bits produce correct number of slots
// =========================================================================

#[test]
fn entropy_bits_produce_correct_slot_count() {
    // For N entropy bits, there should be 2^N possible page-aligned base positions.
    // We can't exhaustively test (2^14 = 16384 positions) but we verify the
    // slide window covers the right number of slots.
    let heap_slots = HEAP_SPEC.slide_window() / PAGE_SIZE;
    assert_eq!(
        heap_slots,
        1u64 << HEAP_SPEC.entropy_bits,
        "heap: slot count doesn't match entropy bits"
    );

    let device_slots = DEVICE_SPEC.slide_window() / PAGE_SIZE;
    assert_eq!(
        device_slots,
        1u64 << DEVICE_SPEC.entropy_bits,
        "device: slot count doesn't match entropy bits"
    );
}
