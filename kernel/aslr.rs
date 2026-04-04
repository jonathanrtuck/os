//! Address Space Layout Randomization.
//!
//! Computes per-process randomized base addresses for each VA region.
//! Pure computation — no page table manipulation, no arch dependencies.
//! The `AddressSpace` constructor uses an `AslrLayout` to set bump
//! allocator starting points and per-process ceilings.
//!
//! # Design: Region Spec as Source of Truth
//!
//! Each VA region is defined by a `RegionSpec(usable, entropy_bits)`:
//!
//! - `usable`: the fixed amount of VA every process gets, regardless of ASLR.
//! - `entropy_bits`: bits of randomization (2^N page-aligned positions).
//!
//! Everything else is derived:
//!
//! ```text
//! slide_window = (1 << entropy_bits) * PAGE_SIZE
//! outer_size   = usable + slide_window
//! region_end   = region_start + outer_size
//! per_process_base = region_start + random(0..2^entropy_bits) * PAGE_SIZE
//! per_process_end  = per_process_base + usable
//! ```
//!
//! `per_process_end` is what `map_heap` checks — not a global constant.
//!
//! # Region map (64 GiB user VA, T0SZ=28)
//!
//! ```text
//! 0x0040_0000  ─── Code (fixed — ELF entry, not randomized in Phase 1)
//! 0x0100_0000  ─── Heap outer  [spec: 224 MiB usable, 10-bit entropy]
//! 0x1000_0000  ─── DMA outer   [spec: 240 MiB usable, 10-bit entropy]
//! 0x2000_0000  ─── Device outer [spec: 256 MiB usable, 14-bit entropy]
//! 0x4000_0000  ─── Channel SHM  (fixed, Phase 1)
//! 0x7000_0000  ─── Stack region  [spec: 64 KiB usable, 13-bit entropy]
//! 0xC000_0000  ─── Shared mem    (fixed, Phase 1)
//! ```

use super::random;

const PAGE_SIZE: u64 = 16384;

// Channel SHM, stack, shared: fixed positions (userspace hardcodes these).
pub const CHANNEL_SHM_REGION_START: u64 = 0x0000_0000_4000_0000;
pub const CHANNEL_SHM_REGION_END: u64 = 0x0000_0000_7000_0000;
pub const STACK_REGION_START: u64 = 0x0000_0000_7000_0000;
pub const STACK_REGION_END: u64 = 0x0000_0000_8000_0000;
pub const SHARED_REGION_START: u64 = 0x0000_0000_C000_0000;
#[allow(dead_code)] // Test-only: used by kernel/host/tests/kernel_aslr.rs
pub const SHARED_REGION_END: u64 = 0x0000_0001_0000_0000;
// =========================================================================
// Outer region bounds — derived from specs
// =========================================================================
//
// Region starts are placed contiguously (heap, DMA, device) starting
// from 0x0100_0000. Ends are computed from starts + outer_size.
// Channel SHM, stack, and shared memory keep their current fixed
// positions (userspace dependency — Phase 3 bootstrap page removes this).
//
// The outer bounds are used by paging.rs for page table coverage and
// by syscalls for fast-reject validation. Per-process endpoints (from
// AslrLayout) are the actual ceilings checked by map_* functions.
pub const DEVICE_REGION_END: u64 = DEVICE_REGION_START + DEVICE_SPEC.outer_size();
pub const DEVICE_REGION_START: u64 = DMA_REGION_END;
pub const DMA_REGION_END: u64 = DMA_REGION_START + DMA_SPEC.outer_size();
pub const DMA_REGION_START: u64 = HEAP_REGION_END;
pub const HEAP_REGION_END: u64 = HEAP_REGION_START + HEAP_SPEC.outer_size();
pub const HEAP_REGION_START: u64 = 0x0000_0000_0100_0000;
/// All region specs for iteration in tests.
#[allow(dead_code)] // Test-only: used by kernel/host/tests/kernel_aslr.rs
pub const REGION_SPECS: [RegionSpec; 4] = [HEAP_SPEC, DMA_SPEC, DEVICE_SPEC, STACK_SPEC];

// --- Heap: primary process memory allocation region ---
// 224 MiB usable (3.5x the 64 MiB physical page limit), 10-bit entropy
// (1024 page-aligned positions). Outer = 240 MiB.
pub const HEAP_SPEC: RegionSpec = RegionSpec {
    name: "heap",
    usable: 224 * 1024 * 1024, // 224 MiB
    entropy_bits: 10,
};
// --- DMA: device DMA buffer region (vestigial — VMOs replaced DMA syscalls) ---
// 240 MiB usable, 10-bit entropy. Outer = 256 MiB.
pub const DMA_SPEC: RegionSpec = RegionSpec {
    name: "dma",
    usable: 240 * 1024 * 1024, // 240 MiB
    entropy_bits: 10,
};
// --- Device MMIO: virtio register mapping region ---
// 256 MiB usable, 14-bit entropy (16384 positions). Outer = 512 MiB.
// Device gets high entropy because MMIO addresses are the most valuable
// target for an attacker (control registers).
pub const DEVICE_SPEC: RegionSpec = RegionSpec {
    name: "device",
    usable: 256 * 1024 * 1024, // 256 MiB
    entropy_bits: 14,
};
// --- Stack: per-process thread stack ---
// 64 KiB usable (4 pages, matches USER_STACK_PAGES), 13-bit entropy
// (8192 positions). Outer = 128 MiB + 64 KiB.
// Stack grows downward: stack_top is randomized, stack_bottom = stack_top - usable.
pub const STACK_SPEC: RegionSpec = RegionSpec {
    name: "stack",
    usable: 4 * PAGE_SIZE, // 64 KiB (USER_STACK_PAGES)
    entropy_bits: 13,
};

/// ASLR layout for a process — randomized base and end addresses for each region.
///
/// Every field is per-process. The `_end` fields are derived (`base + usable_size`)
/// and stored for fast ceiling checks in `map_*` functions.
#[derive(Debug, Clone, Copy)]
pub struct AslrLayout {
    pub heap_base: u64,
    pub heap_end: u64,
    #[allow(dead_code)] // Test-only: DMA region randomized but VMOs replaced DMA syscalls
    pub dma_base: u64,
    #[allow(dead_code)] // Test-only: DMA region randomized but VMOs replaced DMA syscalls
    pub dma_end: u64,
    pub device_base: u64,
    pub device_end: u64,
    pub channel_shm_base: u64,
    pub shared_base: u64,
    /// Top of stack (grows downward). SP is initialized to this value.
    pub stack_top: u64,
}
/// A region specification: fixed usable size and entropy bits.
///
/// All VA region boundaries are derived from specs. Changing a spec
/// automatically adjusts outer bounds, slide windows, and per-process
/// endpoints. No hand-placed constants.
#[derive(Debug, Clone, Copy)]
pub struct RegionSpec {
    /// Human-readable name (for diagnostics and tests).
    #[allow(dead_code)] // Test-only: used by kernel/host/tests/kernel_aslr.rs
    pub name: &'static str,
    /// Fixed VA size every process gets for this region, in bytes.
    /// Must be page-aligned.
    pub usable: u64,
    /// Bits of ASLR entropy. The region has 2^entropy_bits possible
    /// page-aligned base positions.
    pub entropy_bits: u32,
}

impl RegionSpec {
    /// Outer region size: usable + slide window.
    /// This is the total VA reserved for the region envelope.
    pub const fn outer_size(&self) -> u64 {
        self.usable + self.slide_window()
    }
    /// Slide window: the VA range consumed by randomization.
    pub const fn slide_window(&self) -> u64 {
        (1u64 << self.entropy_bits) * PAGE_SIZE
    }
}

// Compile-time assertions: regions fit within available VA space.
const _: () = assert!(
    DEVICE_REGION_END <= CHANNEL_SHM_REGION_START,
    "device region overlaps channel SHM"
);
const _: () = assert!(
    CHANNEL_SHM_REGION_END <= STACK_REGION_END,
    "channel SHM overlaps stack"
);
const _: () = assert!(
    STACK_REGION_END <= SHARED_REGION_START,
    "stack overlaps shared memory"
);
// Verify derived bounds match the historical constants (catch regressions).
const _: () = assert!(
    HEAP_REGION_END == 0x0000_0000_1000_0000,
    "heap outer bound drift"
);
const _: () = assert!(
    DMA_REGION_END == 0x0000_0000_2000_0000,
    "DMA outer bound drift"
);
const _: () = assert!(
    DEVICE_REGION_END == 0x0000_0000_4000_0000,
    "device outer bound drift"
);

impl AslrLayout {
    /// Deterministic layout — used when no PRNG is available.
    ///
    /// Falls back to the original fixed base addresses. Each region base is
    /// the region start, giving the maximum address layout (base at bottom of
    /// slide window). ASLR is defense-in-depth; the capability model is the
    /// primary security boundary.
    pub fn deterministic() -> Self {
        Self {
            heap_base: HEAP_REGION_START,
            heap_end: HEAP_REGION_START + HEAP_SPEC.usable,
            dma_base: DMA_REGION_START,
            dma_end: DMA_REGION_START + DMA_SPEC.usable,
            device_base: DEVICE_REGION_START,
            device_end: DEVICE_REGION_START + DEVICE_SPEC.usable,
            channel_shm_base: CHANNEL_SHM_REGION_START,
            shared_base: SHARED_REGION_START,
            stack_top: STACK_REGION_END,
        }
    }
    /// Compute a randomized layout using the given PRNG.
    ///
    /// Each region base is placed at a random page-aligned offset within
    /// the slide window. The end is always `base + usable`, guaranteeing
    /// every process gets the same usable VA regardless of the random roll.
    ///
    /// Channel SHM and shared memory stay fixed because userspace currently
    /// addresses them directly via `protocol::channel_shm_va()`. Full ASLR
    /// of those regions requires Phase 3 (bootstrap page) + Phase 4.
    pub fn randomize(prng: &mut random::Prng) -> Self {
        let heap_base = randomize_from_spec(prng, HEAP_REGION_START, &HEAP_SPEC);
        let dma_base = randomize_from_spec(prng, DMA_REGION_START, &DMA_SPEC);
        let device_base = randomize_from_spec(prng, DEVICE_REGION_START, &DEVICE_SPEC);
        let stack_top = randomize_stack_top(prng);

        Self {
            heap_base,
            heap_end: heap_base + HEAP_SPEC.usable,
            dma_base,
            dma_end: dma_base + DMA_SPEC.usable,
            device_base,
            device_end: device_base + DEVICE_SPEC.usable,
            // Fixed: userspace addresses these directly via protocol::channel_shm_va().
            channel_shm_base: CHANNEL_SHM_REGION_START,
            shared_base: SHARED_REGION_START,
            stack_top,
        }
    }
}

/// Randomize a region base using its spec.
///
/// Picks a uniform random slot from [0, 2^entropy_bits), then computes:
/// `base = region_start + slot * PAGE_SIZE`.
///
/// The result is always page-aligned. The per-process end is
/// `base + spec.usable`, which is guaranteed to fall within the outer region
/// because `outer = usable + slide_window` and `slot < slide_window / PAGE_SIZE`.
fn randomize_from_spec(prng: &mut random::Prng, region_start: u64, spec: &RegionSpec) -> u64 {
    let slots = 1u64 << spec.entropy_bits;
    let slot = prng.next_u64() % slots;

    region_start + slot * PAGE_SIZE
}
/// Randomize the stack top within the stack region.
///
/// Stack grows downward, so `stack_top` is the highest address. The stack
/// occupies [stack_top - STACK_SPEC.usable, stack_top).
///
/// The possible positions are: stack_top ∈ [STACK_REGION_START + usable,
/// STACK_REGION_START + usable + slide_window), giving exactly
/// 2^entropy_bits page-aligned positions.
fn randomize_stack_top(prng: &mut random::Prng) -> u64 {
    let slots = 1u64 << STACK_SPEC.entropy_bits;
    let slot = prng.next_u64() % slots;
    // The lowest valid stack_top: region_start + usable (stack needs room below).
    let min_top = STACK_REGION_START + STACK_SPEC.usable;

    min_top + slot * PAGE_SIZE
}
