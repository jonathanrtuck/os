//! Host-side tests for user ASLR (address space layout randomization).
//!
//! Tests that per-process address space bases are randomized within their
//! allowed regions, that different PRNG seeds produce different layouts,
//! and that the bump allocators remain functional with randomized bases.

#[path = "../../kernel/random.rs"]
mod random;

/// ASLR layout computation — extracted from address_space for testability.
/// These functions compute randomized base addresses for each VA region.
#[path = "../../kernel/aslr.rs"]
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
// Region base randomization
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
    // Run with many seeds to catch boundary violations.
    for seed in 0..=255u8 {
        let mut prng = test_prng(seed);
        let layout = AslrLayout::randomize(&mut prng);

        // Each base must be within its allowed region.
        assert!(
            layout.heap_base >= HEAP_REGION_START && layout.heap_base < HEAP_REGION_END,
            "seed {}: heap_base {:#x} out of [{:#x}, {:#x})",
            seed,
            layout.heap_base,
            HEAP_REGION_START,
            HEAP_REGION_END,
        );
        assert!(
            layout.dma_base >= DMA_REGION_START && layout.dma_base < DMA_REGION_END,
            "seed {}: dma_base {:#x} out of bounds",
            seed,
            layout.dma_base,
        );
        assert!(
            layout.device_base >= DEVICE_REGION_START && layout.device_base < DEVICE_REGION_END,
            "seed {}: device_base {:#x} out of bounds",
            seed,
            layout.device_base,
        );
        assert!(
            layout.channel_shm_base >= CHANNEL_SHM_REGION_START
                && layout.channel_shm_base < CHANNEL_SHM_REGION_END,
            "seed {}: channel_shm_base {:#x} out of bounds",
            seed,
            layout.channel_shm_base,
        );
        assert!(
            layout.shared_base >= SHARED_REGION_START && layout.shared_base < SHARED_REGION_END,
            "seed {}: shared_base {:#x} out of bounds",
            seed,
            layout.shared_base,
        );
        // Stack grows downward: stack_top must be above the minimum.
        assert!(
            layout.stack_top >= STACK_REGION_START && layout.stack_top <= STACK_REGION_END,
            "seed {}: stack_top {:#x} out of bounds",
            seed,
            layout.stack_top,
        );
    }
}

#[test]
fn different_seeds_produce_different_layouts() {
    let mut prng_a = test_prng(0x11);
    let mut prng_b = test_prng(0x22);

    let layout_a = AslrLayout::randomize(&mut prng_a);
    let layout_b = AslrLayout::randomize(&mut prng_b);

    // At least one base should differ.
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
    assert_eq!(layout_a.dma_base, layout_b.dma_base);
    assert_eq!(layout_a.device_base, layout_b.device_base);
    assert_eq!(layout_a.channel_shm_base, layout_b.channel_shm_base);
    assert_eq!(layout_a.shared_base, layout_b.shared_base);
    assert_eq!(layout_a.stack_top, layout_b.stack_top);
}

#[test]
fn deterministic_layout_uses_fixed_bases() {
    let layout = AslrLayout::deterministic();

    // When no PRNG is available, bases must be the original fixed values.
    assert_eq!(layout.heap_base, HEAP_REGION_START);
    assert_eq!(layout.dma_base, DMA_REGION_START);
    assert_eq!(layout.device_base, DEVICE_REGION_START);
    assert_eq!(layout.channel_shm_base, CHANNEL_SHM_REGION_START);
    assert_eq!(layout.shared_base, SHARED_REGION_START);
    assert_eq!(layout.stack_top, STACK_REGION_END);
}

#[test]
fn channel_shm_and_shared_are_fixed_even_with_randomization() {
    // These regions stay fixed because userspace addresses them directly.
    // Full ASLR requires a bootstrap protocol to pass the layout.
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
// Entropy measurement — verify meaningful randomization
// =========================================================================

#[test]
fn heap_base_has_meaningful_entropy() {
    // Generate 256 layouts with different seeds and check that the heap
    // base takes on at least 16 distinct values (4+ bits of entropy).
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

        // Regions are ordered: heap < dma < device < channel_shm < stack < shared
        assert!(
            layout.heap_base < layout.dma_base,
            "seed {}: heap overlaps dma",
            seed
        );
        assert!(
            layout.dma_base < layout.device_base,
            "seed {}: dma overlaps device",
            seed
        );
        assert!(
            layout.device_base < layout.channel_shm_base,
            "seed {}: device overlaps channel_shm",
            seed
        );
        assert!(
            layout.channel_shm_base < layout.stack_top,
            "seed {}: channel_shm overlaps stack",
            seed
        );
    }
}
