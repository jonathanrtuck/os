//! Address Space Layout Randomization.
//!
//! Computes per-process randomized base addresses for each VA region.
//! Pure computation — no page table manipulation, no arch dependencies.
//! The `AddressSpace` constructor uses an `AslrLayout` to set bump
//! allocator starting points.
//!
//! # Region map (64 GiB user VA, T0SZ=28)
//!
//! ```text
//! 0x0040_0000 ─── Code (fixed — ELF entry, not randomized here)
//! 0x0100_0000 ─── Heap region  [0x0100_0000, 0x1000_0000)
//! 0x1000_0000 ─── DMA region   [0x1000_0000, 0x2000_0000)
//! 0x2000_0000 ─── Device MMIO  [0x2000_0000, 0x4000_0000)
//! 0x4000_0000 ─── Channel SHM  [0x4000_0000, 0x8000_0000)
//! 0x8000_0000 ─── Stack region [0x7000_0000, 0x8000_0000)  ← grows down
//! 0xC000_0000 ─── Shared mem   [0xC000_0000, 0x1_0000_0000)
//! ```
//!
//! Each region's base is randomized within its bounds at page granularity
//! (16 KiB alignment). The randomization window is the space between the
//! region's start and its end minus a minimum usable size.

use super::random;

const PAGE_SIZE: u64 = 16384;

// Region bounds — these define where each region CAN be placed.
// The actual base is randomized within [START, END - min_size).

/// Heap: [0x0100_0000, 0x1000_0000). ~240 MiB available.
pub const HEAP_REGION_START: u64 = 0x0000_0000_0100_0000;
pub const HEAP_REGION_END: u64 = 0x0000_0000_1000_0000;

/// DMA: [0x1000_0000, 0x2000_0000). 256 MiB available.
pub const DMA_REGION_START: u64 = 0x0000_0000_1000_0000;
pub const DMA_REGION_END: u64 = 0x0000_0000_2000_0000;

/// Device MMIO: [0x2000_0000, 0x4000_0000). 512 MiB available.
pub const DEVICE_REGION_START: u64 = 0x0000_0000_2000_0000;
pub const DEVICE_REGION_END: u64 = 0x0000_0000_4000_0000;

/// Channel SHM: [0x4000_0000, 0x7000_0000). 768 MiB available.
/// Ends before the stack region to prevent overlap.
pub const CHANNEL_SHM_REGION_START: u64 = 0x0000_0000_4000_0000;
pub const CHANNEL_SHM_REGION_END: u64 = 0x0000_0000_7000_0000;

/// Stack grows down. The top can be anywhere in [0x7000_0000, 0x8000_0000].
/// We reserve at least 64 KiB above the bottom for stack growth.
pub const STACK_REGION_START: u64 = 0x0000_0000_7000_0000;
pub const STACK_REGION_END: u64 = 0x0000_0000_8000_0000;

/// Shared memory: [0xC000_0000, 0x1_0000_0000). 1 GiB available.
pub const SHARED_REGION_START: u64 = 0x0000_0000_C000_0000;
pub const SHARED_REGION_END: u64 = 0x0000_0001_0000_0000;

/// Minimum usable size per region (pages). The randomized base must leave
/// at least this much space before the region end.
const MIN_HEAP_PAGES: u64 = 256; // 4 MiB
const MIN_DMA_PAGES: u64 = 64; // 1 MiB
const MIN_DEVICE_PAGES: u64 = 64; // 1 MiB
const MIN_CHANNEL_SHM_PAGES: u64 = 128; // 2 MiB
const MIN_SHARED_PAGES: u64 = 128; // 2 MiB
const MIN_STACK_PAGES: u64 = 4; // 64 KiB (matches USER_STACK_PAGES)

/// ASLR layout for a process — randomized base addresses for each region.
#[derive(Debug, Clone, Copy)]
pub struct AslrLayout {
    pub heap_base: u64,
    pub dma_base: u64,
    pub device_base: u64,
    pub channel_shm_base: u64,
    pub shared_base: u64,
    /// Top of stack (grows downward). SP is initialized to this value.
    pub stack_top: u64,
}

impl AslrLayout {
    /// Compute a randomized layout using the given PRNG.
    ///
    /// Randomizes heap, DMA, device MMIO, and stack bases. Channel SHM and
    /// shared memory stay fixed because userspace currently addresses them
    /// directly via `protocol::channel_shm_va()`. Full ASLR of those regions
    /// requires a bootstrap protocol to pass the layout to userspace.
    pub fn randomize(prng: &mut random::Prng) -> Self {
        Self {
            heap_base: randomize_base(prng, HEAP_REGION_START, HEAP_REGION_END, MIN_HEAP_PAGES),
            dma_base: randomize_base(prng, DMA_REGION_START, DMA_REGION_END, MIN_DMA_PAGES),
            device_base: randomize_base(
                prng,
                DEVICE_REGION_START,
                DEVICE_REGION_END,
                MIN_DEVICE_PAGES,
            ),
            // Fixed: userspace addresses these directly via protocol::channel_shm_va().
            channel_shm_base: CHANNEL_SHM_REGION_START,
            shared_base: SHARED_REGION_START,
            stack_top: randomize_stack_top(prng),
        }
    }

    /// Deterministic layout — used when no PRNG is available.
    ///
    /// Falls back to the original fixed base addresses. ASLR is defense-in-depth;
    /// the capability model is the primary security boundary.
    pub fn deterministic() -> Self {
        Self {
            heap_base: HEAP_REGION_START,
            dma_base: DMA_REGION_START,
            device_base: DEVICE_REGION_START,
            channel_shm_base: CHANNEL_SHM_REGION_START,
            shared_base: SHARED_REGION_START,
            stack_top: STACK_REGION_END,
        }
    }
}

/// Randomize a region base within [start, end - min_pages * PAGE_SIZE).
///
/// The result is always page-aligned. The randomization window is divided
/// into page-sized slots, and a uniform random slot is selected.
fn randomize_base(prng: &mut random::Prng, start: u64, end: u64, min_pages: u64) -> u64 {
    let min_size = min_pages * PAGE_SIZE;
    let max_base = end.saturating_sub(min_size);

    if max_base <= start {
        return start; // Region too small to randomize.
    }

    let range = max_base - start;
    let slots = range / PAGE_SIZE;

    if slots == 0 {
        return start;
    }

    let slot = prng.next_u64() % slots;
    start + slot * PAGE_SIZE
}

/// Randomize the stack top within the stack region.
///
/// Stack grows downward, so a higher top means more stack space.
/// We randomize within [STACK_REGION_START + min_stack, STACK_REGION_END].
fn randomize_stack_top(prng: &mut random::Prng) -> u64 {
    let min_stack = MIN_STACK_PAGES * PAGE_SIZE;
    let bottom = STACK_REGION_START + min_stack;

    if bottom >= STACK_REGION_END {
        return STACK_REGION_END;
    }

    let range = STACK_REGION_END - bottom;
    let slots = range / PAGE_SIZE;

    if slots == 0 {
        return STACK_REGION_END;
    }

    let slot = prng.next_u64() % slots;
    // Align to page boundary and ensure it's within bounds.
    let top = bottom + slot * PAGE_SIZE;
    // Stack top must be page-aligned.
    top
}
