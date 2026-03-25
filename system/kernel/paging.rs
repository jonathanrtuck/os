// AUDIT: 2026-03-14 — 0 unsafe blocks, 6-category checklist applied. Pure constants
// and helpers. All ARMv8 descriptor bits verified against spec. Address space layout
// verified: regions contiguous, no gaps or overlaps. align_up/align_up_u64 wrapping
// behavior documented. No bugs found.

//! Page table descriptor constants and shared memory layout definitions.
//!
//! Single source of truth for ARMv8 page table bits used by both the
//! kernel's TTBR1 refinement (memory.rs) and per-process TTBR0 tables
//! (address_space.rs).

// System-wide constants (SSOT: system_config.rs).
// Provides: PAGE_SIZE, PAGE_SHIFT, RAM_START, KERNEL_VA_OFFSET,
// USER_CODE_BASE, CHANNEL_SHM_BASE, USER_STACK_TOP, USER_STACK_PAGES,
// SHARED_MEMORY_BASE.
mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}
pub use system_config::*;

// Descriptor type bits.
pub const DESC_VALID: u64 = 1 << 0;
pub const DESC_TABLE: u64 = 1 << 1;
pub const DESC_PAGE: u64 = 0b11; // L3 page descriptor (VALID + TABLE)

// Attribute fields.
pub const AF: u64 = 1 << 10; // Access flag
pub const AP_EL0: u64 = 1 << 6; // EL0 accessible
pub const AP_RO: u64 = 1 << 7; // Read-only
pub const ATTRIDX0: u64 = 0 << 2; // MAIR index 0 (normal memory)
pub const ATTRIDX1: u64 = 1 << 2; // MAIR index 1 (device-nGnRE memory)
pub const NG: u64 = 1 << 11; // Non-global (ASID-tagged, for EL0 pages)
pub const PA_MASK: u64 = 0x0000_FFFF_FFFF_C000;
pub const PXN: u64 = 1 << 53; // Privileged execute-never
pub const SH_INNER: u64 = 0b11 << 8; // Inner shareable
pub const UXN: u64 = 1 << 54; // Unprivileged execute-never

/// Compile-time maximum RAM size. Used for array sizing and upper-bound
/// calculations that must be const. The actual RAM size is read from the
/// DTB at boot and may be smaller (or equal). Use `ram_end()` for the
/// runtime value.
pub const RAM_SIZE_MAX: u64 = 256 * 1024 * 1024;
pub const RAM_END_MAX: u64 = RAM_START + RAM_SIZE_MAX;

/// Actual RAM end address, set from the DTB `/memory` node during boot.
/// Defaults to `RAM_END_MAX` so the system works even if DTB parsing fails.
static ACTUAL_RAM_END: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(RAM_END_MAX);

/// Set the actual RAM end address (called once during boot from DTB data).
pub fn set_ram_end(end: u64) {
    ACTUAL_RAM_END.store(end, core::sync::atomic::Ordering::Release);
}

/// The actual RAM end address. Reads the value set by `set_ram_end()`,
/// or returns `RAM_END_MAX` if it was never called (DTB fallback).
pub fn ram_end() -> u64 {
    ACTUAL_RAM_END.load(core::sync::atomic::Ordering::Acquire)
}

pub const CHANNEL_SHM_END: u64 = USER_STACK_VA; // up to stack region
pub const USER_STACK_VA: u64 = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;

// Heap region: anonymous memory for userspace allocators.
pub const HEAP_BASE: u64 = 0x0000_0000_0100_0000; // 16 MiB
pub const HEAP_END: u64 = 0x0000_0000_1000_0000; // 256 MiB (abuts DMA region)

// Guard page: USER_STACK_VA - PAGE_SIZE is intentionally unmapped.
// Stack overflow triggers a data abort → user_fault_handler terminates the process.
pub const DMA_BUFFER_BASE: u64 = 0x0000_0000_1000_0000; // 256 MiB
pub const DMA_BUFFER_END: u64 = 0x0000_0000_2000_0000; // 512 MiB (abuts device MMIO)
pub const DEVICE_MMIO_BASE: u64 = 0x0000_0000_2000_0000; // 512 MiB
pub const DEVICE_MMIO_END: u64 = 0x0000_0000_4000_0000; // Up to channel SHM
pub const SHARED_MEMORY_END: u64 = 0x0000_0001_0000_0000; // 4 GiB
pub const USER_VA_END: u64 = 0x0000_0010_0000_0000; // T0SZ=28 (64 GiB)

// boot.S has manual .equ copies of these values. These assertions catch drift
// immediately at compile time if someone edits system_config.rs without
// updating boot.S (or vice versa).
const _: () = assert!(
    PAGE_SIZE == 16384,
    "boot.S .space 16384 and .align 14 assume 16 KiB pages"
);
const _: () = assert!(
    PAGE_SHIFT == 14,
    "boot.S .align 14 assumes PAGE_SHIFT == 14"
);
const _: () = assert!(
    RAM_START == 0x4000_0000,
    "boot.S .equ RAM_START assumes 0x40000000"
);

/// Align `addr` up to the next multiple of `align` (must be a power of two).
///
/// Uses wrapping arithmetic to avoid panicking on overflow in debug builds.
/// Callers must ensure `addr` is not so large that wrapping produces a
/// nonsensical result (all kernel callers pass valid addresses well within
/// the address space).
pub const fn align_up(addr: usize, align: usize) -> usize {
    addr.wrapping_add(align - 1) & !(align - 1)
}
/// Align `x` up to the next multiple of `align` (must be a power of two).
///
/// Uses wrapping arithmetic to avoid panicking on overflow in debug builds.
/// See [`align_up`] for details.
pub const fn align_up_u64(x: u64, align: u64) -> u64 {
    x.wrapping_add(align - 1) & !(align - 1)
}
