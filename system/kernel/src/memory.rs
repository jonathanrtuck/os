//! Kernel page table refinement and address translation.
//!
//! Refines the coarse 2MB-block TTBR1 tables from boot.S with 4KB L3
//! pages for per-section W^X enforcement.

use super::paging::{
    align_up_u64, AF, AP_RO, ATTRIDX0, DESC_PAGE, DESC_TABLE, DESC_VALID, PAGE_SIZE, PXN, SH_INNER,
    UXN,
};
use core::cell::UnsafeCell;

pub const KERNEL_VA_OFFSET: usize = 0xFFFF_0000_0000_0000;
pub const HEAP_SIZE: usize = 16 * 1024 * 1024;

const BLOCK_2MB: u64 = 2 * 1024 * 1024;

/// Wrapper for page tables in statics. Written once during init, read-only after.
struct SyncPageTable(UnsafeCell<PageTable>);

impl SyncPageTable {
    const fn new() -> Self {
        Self(UnsafeCell::new(PageTable::new()))
    }

    fn get(&self) -> *mut PageTable {
        self.0.get()
    }
}

// SAFETY: Page tables are written once during init (before timer/IRQs) and
// read-only after. No concurrent access is possible.
unsafe impl Sync for SyncPageTable {}

/// L3 page table for the kernel's 2MB block (4KB pages, W^X).
static TT1_L3_KERN: SyncPageTable = SyncPageTable::new();
/// Empty L0 table for kernel threads' TTBR0 (no user mappings).
static EMPTY_L0: SyncPageTable = SyncPageTable::new();

#[inline(always)]
pub fn phys_to_virt(pa: usize) -> usize {
    pa.wrapping_add(KERNEL_VA_OFFSET)
}
#[inline(always)]
pub fn virt_to_phys(va: usize) -> usize {
    va.wrapping_sub(KERNEL_VA_OFFSET)
}

/// Physical address of the empty L0 table (for kernel threads' TTBR0).
pub fn empty_ttbr0() -> u64 {
    virt_to_phys(EMPTY_L0.get() as usize) as u64
}

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;

    // boot.S TTBR1 L2_1 table (need to patch one entry for L3).
    static boot_tt1_l2_1: u8;
}

#[repr(align(4096))]
struct PageTable {
    entries: [u64; 512],
}

impl PageTable {
    const fn new() -> Self {
        Self { entries: [0; 512] }
    }
}

/// Refine TTBR1 with 4KB pages for the kernel's 2MB block.
///
/// boot.S created coarse 2MB-block tables. This replaces the kernel's
/// 2MB block with an L3 table providing per-section W^X permissions.
/// Called from kernel_main (running at upper VA, TTBR1 is live).
pub fn init() {
    let l3_kern = unsafe { &mut *TT1_L3_KERN.get() };
    let text_start = unsafe { &__text_start as *const u8 as u64 };
    let text_end = align_up_u64(unsafe { &__text_end as *const u8 as u64 }, PAGE_SIZE);
    let rodata_start = unsafe { &__rodata_start as *const u8 as u64 };
    let rodata_end = align_up_u64(unsafe { &__rodata_end as *const u8 as u64 }, PAGE_SIZE);
    let data_start = unsafe { &__data_start as *const u8 as u64 };
    // All these are kernel VA. Compute PA of the kernel's 2MB-aligned block.
    let text_start_pa = virt_to_phys(text_start as usize) as u64;
    let kernel_block_pa = text_start_pa & !(BLOCK_2MB - 1);
    let normal = ATTRIDX0 | AF | SH_INNER;

    for i in 0..512u64 {
        let pa = kernel_block_pa + i * PAGE_SIZE;
        let va = phys_to_virt(pa as usize) as u64;
        let attrs = if va >= text_start && va < text_end {
            normal | AP_RO | UXN // .text: RX (kernel only)
        } else if va >= rodata_start && va < rodata_end {
            normal | AP_RO | PXN | UXN // .rodata: RO
        } else if va >= data_start {
            normal | PXN | UXN // .data/.bss/stack/heap: RW
        } else {
            continue; // unmapped
        };

        l3_kern.entries[i as usize] = (pa & 0x0000_FFFF_FFFF_F000) | DESC_PAGE | attrs;
    }

    // Patch TTBR1 L2_1 to point at L3 instead of the 2MB block.
    let l3_kern_pa = virt_to_phys(TT1_L3_KERN.get() as usize) as u64;
    let kernel_l2_idx = ((kernel_block_pa >> 21) & 0x1FF) as usize;
    let l2_1 = unsafe { &boot_tt1_l2_1 as *const u8 as *mut u64 };

    unsafe {
        l2_1.add(kernel_l2_idx)
            .write_volatile(l3_kern_pa | DESC_VALID | DESC_TABLE);
    }

    // TLB invalidate + barrier so new mappings take effect.
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack)
        );
    }
}
