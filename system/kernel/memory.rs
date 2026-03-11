//! Kernel page table refinement and address translation.
//!
//! Refines the coarse 2MB-block TTBR1 tables from boot.S with 4KB L3
//! pages for per-section W^X enforcement. Also manages kernel stack
//! guard pages via L3 entry manipulation.

use super::paging::{
    align_up_u64, AF, AP_RO, ATTRIDX0, DESC_PAGE, DESC_TABLE, DESC_VALID, PAGE_SIZE, PA_MASK, PXN,
    SH_INNER, UXN,
};
use super::sync::IrqMutex;
use core::cell::UnsafeCell;

const BLOCK_2MB: u64 = 2 * 1024 * 1024;

pub const HEAP_SIZE: usize = 16 * 1024 * 1024;
pub const KERNEL_VA_OFFSET: usize = 0xFFFF_0000_0000_0000; // must match link.ld KERNEL_VA_OFFSET

/// Physical address newtype. Prevents accidental PA/VA mixups at compile time.
///
/// Used at all API boundaries where physical addresses flow: page allocator,
/// page table manipulation, DMA. The `pub` inner field allows extraction
/// where raw arithmetic or pointer casts are needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Pa(pub usize);
#[repr(align(4096))]
struct PageTable {
    entries: [u64; 512],
}
/// Wrapper for page tables in statics. Written once during init, read-only after.
struct SyncPageTable(UnsafeCell<PageTable>);

/// Empty L0 table for kernel threads' TTBR0 (no user mappings).
static EMPTY_L0: SyncPageTable = SyncPageTable::new();
/// Lock for kernel TTBR1 page table modifications (break-block, guard pages).
///
/// Lock ordering: KERNEL_PT_LOCK → page allocator lock (never the reverse).
static KERNEL_PT_LOCK: IrqMutex<()> = IrqMutex::new(());
/// L3 page table for the kernel's 2MB block (4KB pages, W^X).
static TT1_L3_KERN: SyncPageTable = SyncPageTable::new();

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    // boot.S TTBR1 L2_1 table (need to patch one entry for L3).
    static boot_tt1_l2_1: u8;
}

impl Pa {
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }
}
impl PageTable {
    const fn new() -> Self {
        Self { entries: [0; 512] }
    }
}
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

/// Broadcast TLB invalidation across all cores.
fn tlb_invalidate_all() {
    // SAFETY: TLB invalidation is a safe maintenance operation. The `is`
    // suffix broadcasts to all cores in the inner-shareable domain.
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nostack)
        );
    }
}

/// Re-map a kernel guard page as normal memory (restore its L3 entry).
///
/// Called before freeing stack frames back to the buddy allocator, since
/// `free_frames` writes a FreeBlock header at the start of the block.
pub fn clear_kernel_guard_page(va: usize) {
    assert!(va >= KERNEL_VA_OFFSET, "not a kernel VA");
    assert!(va & 0xFFF == 0, "VA not page-aligned");

    let _lock = KERNEL_PT_LOCK.lock();
    let pa = virt_to_phys(va).0 as u64;
    let l2_idx = ((pa >> 21) & 0x1FF) as usize;
    // SAFETY: boot_tt1_l2_1 is a page-aligned L2 table defined in boot.S.
    // Cast to *mut u64 is valid because page table entries are 8-byte u64s.
    // KERNEL_PT_LOCK is held, ensuring exclusive access.
    let l2_table = unsafe { &boot_tt1_l2_1 as *const u8 as *mut u64 };
    // SAFETY: l2_idx is masked to 0..511, within the 512-entry L2 table.
    let l2_entry = unsafe { l2_table.add(l2_idx).read_volatile() };

    // Must already be broken into L3 (set_kernel_guard_page did that).
    assert!(
        l2_entry & 0b11 == 0b11,
        "clear_kernel_guard_page: L2 entry is not a table descriptor"
    );

    let l3_pa = l2_entry & PA_MASK;
    let l3_table = phys_to_virt(Pa(l3_pa as usize)) as *mut u64;
    let l3_idx = ((pa >> 12) & 0x1FF) as usize;
    // Read attributes from a neighbor entry to match the original block's
    // mapping. This preserves boot.S attributes regardless of what they are.
    // The neighbor is guaranteed to be a valid mapped entry: guard pages are
    // only set on stack bases, and the adjacent page is always a usable
    // stack page.
    let neighbor_idx = if l3_idx > 0 { l3_idx - 1 } else { l3_idx + 1 };
    // SAFETY: neighbor_idx is in 0..511 (l3_idx is in 0..511, and the
    // adjustment stays in range). l3_table is a valid L3 page table.
    let neighbor = unsafe { l3_table.add(neighbor_idx).read_volatile() };
    let attrs = neighbor & !PA_MASK;

    // SAFETY: Restoring a valid L3 entry for a page the buddy allocator owns.
    unsafe {
        l3_table.add(l3_idx).write_volatile((pa & PA_MASK) | attrs);
    }

    tlb_invalidate_all();
}

/// Physical address of the empty L0 table (for kernel threads' TTBR0).
pub fn empty_ttbr0() -> u64 {
    virt_to_phys(EMPTY_L0.get() as usize).as_u64()
}
/// Refine TTBR1 with 4KB pages for the kernel's 2MB block.
///
/// boot.S created coarse 2MB-block tables. This replaces the kernel's
/// 2MB block with an L3 table providing per-section W^X permissions.
/// Called from kernel_main (running at upper VA, TTBR1 is live).
pub fn init() {
    // SAFETY: Called once during boot before any concurrent access.
    // TT1_L3_KERN is a static SyncPageTable; this is the single write
    // point referenced by the `unsafe impl Sync` invariant.
    let l3_kern = unsafe { &mut *TT1_L3_KERN.get() };
    // SAFETY: Linker-defined symbol from link.ld — taking its address
    // yields the VA of the .text section start. Valid kernel VA.
    let text_start = unsafe { &__text_start as *const u8 as u64 };
    // SAFETY: Linker-defined symbol — VA of .text section end.
    let text_end = align_up_u64(unsafe { &__text_end as *const u8 as u64 }, PAGE_SIZE);
    // SAFETY: Linker-defined symbol — VA of .rodata section start.
    let rodata_start = unsafe { &__rodata_start as *const u8 as u64 };
    // SAFETY: Linker-defined symbol — VA of .rodata section end.
    let rodata_end = align_up_u64(unsafe { &__rodata_end as *const u8 as u64 }, PAGE_SIZE);
    // SAFETY: Linker-defined symbol — VA of .data section start.
    let data_start = unsafe { &__data_start as *const u8 as u64 };
    // All these are kernel VA. Compute PA of the kernel's 2MB-aligned block.
    let text_start_pa = virt_to_phys(text_start as usize).as_u64();
    let kernel_block_pa = text_start_pa & !(BLOCK_2MB - 1);
    let normal = ATTRIDX0 | AF | SH_INNER;

    for i in 0..512u64 {
        let pa = kernel_block_pa + i * PAGE_SIZE;
        let va = phys_to_virt(Pa(pa as usize)) as u64;
        let attrs = if va >= text_start && va < text_end {
            normal | AP_RO | UXN // .text: RX (kernel only)
        } else if va >= rodata_start && va < rodata_end {
            normal | AP_RO | PXN | UXN // .rodata: RO
        } else {
            // Everything else in this 2MB block: RW NX.
            // Includes pre-kernel area (DTB, firmware stub), .data, .bss,
            // stack, heap, and any trailing padding.
            normal | PXN | UXN // RW (default for data)
        };

        l3_kern.entries[i as usize] = (pa & 0x0000_FFFF_FFFF_F000) | DESC_PAGE | attrs;
    }

    // Patch TTBR1 L2_1 to point at L3 instead of the 2MB block.
    let l3_kern_pa = virt_to_phys(TT1_L3_KERN.get() as usize).as_u64();
    let kernel_l2_idx = ((kernel_block_pa >> 21) & 0x1FF) as usize;
    // SAFETY: boot_tt1_l2_1 is a page-aligned L2 table defined in boot.S.
    // Cast to *mut u64 is valid because page table entries are 8-byte u64s.
    // Called during single-core boot — no concurrent access.
    let l2_1 = unsafe { &boot_tt1_l2_1 as *const u8 as *mut u64 };

    // SAFETY: kernel_l2_idx is masked to 0..511, within the 512-entry
    // L2 table. Writing a table descriptor pointing to our L3 replaces
    // the boot.S 2MB block with fine-grained 4KB pages.
    unsafe {
        l2_1.add(kernel_l2_idx)
            .write_volatile(l3_kern_pa | DESC_VALID | DESC_TABLE);
    }

    // SAFETY: TLB invalidation after replacing the L2 block descriptor
    // with an L3 table descriptor. Uses vmalle1 (not vmalle1is) because
    // this runs during boot before secondary cores are started. DSB
    // ensures the table write is visible before TLB invalidation; ISB
    // ensures the pipeline sees the updated TLB state.
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
#[inline(always)]
pub fn phys_to_virt(pa: Pa) -> usize {
    pa.0.wrapping_add(KERNEL_VA_OFFSET)
}
/// Set a kernel VA page as a guard page (unmap from TTBR1).
///
/// If the containing 2MB block hasn't been refined to L3 pages yet,
/// allocates an L3 page table and replaces the L2 block descriptor.
/// The target page's L3 entry is set to 0 (invalid), so any access faults.
///
/// Returns `false` if the L3 table allocation fails (OOM).
pub fn try_set_kernel_guard_page(va: usize) -> bool {
    assert!(va >= KERNEL_VA_OFFSET, "not a kernel VA");
    assert!(va & 0xFFF == 0, "VA not page-aligned");

    let _lock = KERNEL_PT_LOCK.lock();
    let pa = virt_to_phys(va).0 as u64;
    let l2_idx = ((pa >> 21) & 0x1FF) as usize;
    // SAFETY: boot_tt1_l2_1 is a page-aligned L2 table defined in boot.S.
    // Cast to *mut u64 is valid because page table entries are 8-byte u64s.
    // KERNEL_PT_LOCK is held, ensuring exclusive access.
    let l2_table = unsafe { &boot_tt1_l2_1 as *const u8 as *mut u64 };
    // SAFETY: l2_idx is masked to 0..511, within the 512-entry L2 table.
    let l2_entry = unsafe { l2_table.add(l2_idx).read_volatile() };
    let l3_table = if l2_entry & 0b11 == 0b01 {
        let block_pa = l2_entry & 0x0000_FFFF_FFE0_0000;
        let block_attrs = l2_entry & !(0x0000_FFFF_FFE0_0003u64);
        let l3_frame = match super::page_allocator::alloc_frame() {
            Some(f) => f,
            None => return false,
        };
        let l3_va = phys_to_virt(l3_frame) as *mut u64;

        // Populate 512 L3 entries replicating the block mapping.
        for i in 0..512u64 {
            let page_pa = block_pa + i * PAGE_SIZE;

            // SAFETY: l3_va points to a freshly allocated, zeroed page.
            unsafe {
                l3_va
                    .add(i as usize)
                    .write_volatile((page_pa & PA_MASK) | block_attrs | DESC_PAGE);
            }
        }

        // Break-before-make (ARMv8 ARM B2.2.1): replacing a valid block
        // descriptor with a valid table descriptor requires an intermediate
        // invalid step. Without this, other cores walking the TLB could see
        // an inconsistent descriptor (CONSTRAINED UNPREDICTABLE).
        // SAFETY: l2_idx is in 0..511. Writing an invalid (0) entry is
        // step 1 of break-before-make. KERNEL_PT_LOCK is held.
        unsafe {
            // Step 1: Write invalid entry (break).
            l2_table.add(l2_idx).write_volatile(0);
        }

        tlb_invalidate_all();

        // SAFETY: l2_idx is in 0..511. Writing the new table descriptor
        // is step 2 of break-before-make. The TLB was invalidated above
        // to ensure no stale block descriptor remains cached.
        // KERNEL_PT_LOCK is held.
        unsafe {
            l2_table
                .add(l2_idx)
                .write_volatile(l3_frame.as_u64() | DESC_VALID | DESC_TABLE);
        }

        tlb_invalidate_all();

        l3_va
    } else {
        // Already a table descriptor — extract the existing L3 table.
        let l3_pa = l2_entry & PA_MASK;

        phys_to_virt(Pa(l3_pa as usize)) as *mut u64
    };

    // Clear the target page's L3 entry (invalid → faults on access).
    let l3_idx = ((pa >> 12) & 0x1FF) as usize;

    // SAFETY: l3_table is a valid L3 page table in kernel VA.
    unsafe {
        l3_table.add(l3_idx).write_volatile(0);
    }

    tlb_invalidate_all();

    true
}

/// Panicking wrapper for boot-time guard page setup.
pub fn set_kernel_guard_page(va: usize) {
    assert!(
        try_set_kernel_guard_page(va),
        "set_kernel_guard_page: out of memory"
    );
}
#[inline(always)]
pub fn virt_to_phys(va: usize) -> Pa {
    Pa(va.wrapping_sub(KERNEL_VA_OFFSET))
}
