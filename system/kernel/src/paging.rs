//! Page table descriptor constants and shared memory layout definitions.
//!
//! Single source of truth for ARMv8 page table bits used by both the
//! kernel's TTBR1 refinement (memory.rs) and per-process TTBR0 tables
//! (address_space.rs).

pub const PAGE_SIZE: u64 = 4096;

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
pub const PA_MASK: u64 = 0x0000_FFFF_FFFF_F000;
pub const PXN: u64 = 1 << 53; // Privileged execute-never
pub const SH_INNER: u64 = 0b11 << 8; // Inner shareable
pub const UXN: u64 = 1 << 54; // Unprivileged execute-never

// Physical memory layout (QEMU virt, 256 MiB RAM).
// boot.S has its own `.equ` copies — keep in sync (see boot.S lines 13-14).
pub const RAM_START: u64 = 0x4000_0000;
pub const RAM_SIZE: u64 = 256 * 1024 * 1024;
pub const RAM_END: u64 = RAM_START + RAM_SIZE;

// User virtual address layout.
// All user VA constants in one place for visibility.
pub const USER_CODE_BASE: u64 = 0x0000_0000_0040_0000; // 4 MiB (matches link.ld)
pub const CHANNEL_SHM_BASE: u64 = 0x0000_0000_4000_0000; // 1 GiB
pub const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000; // 2 GiB
pub const USER_STACK_PAGES: u64 = 4; // 16 KiB
pub const USER_STACK_VA: u64 = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
// Guard page: USER_STACK_VA - PAGE_SIZE is intentionally unmapped.
// Stack overflow triggers a data abort → user_fault_handler terminates the process.
pub const DEVICE_MMIO_BASE: u64 = 0x0000_0000_2000_0000; // 512 MiB
pub const DEVICE_MMIO_END: u64 = 0x0000_0000_4000_0000; // Up to channel SHM
pub const USER_VA_END: u64 = 0x0001_0000_0000_0000; // T0SZ=16

/// Align `x` up to the next multiple of `align` (must be a power of two).
pub const fn align_up_u64(x: u64, align: u64) -> u64 {
    (x + align - 1) & !(align - 1)
}

/// Align `addr` up to the next multiple of `align` (must be a power of two).
pub const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
