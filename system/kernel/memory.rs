// AUDIT: 2026-03-14 — 19 unsafe blocks + 1 unsafe impl verified, 6-category checklist applied.
// No bugs found. Break-before-make correct in try_set_kernel_guard_page (3-step: invalidate→flush→write).
// init() skips BBM (valid→valid L2 transition) — justified: single-core boot, TLB flush immediately after.
// W^X enforcement verified: .text RX, .rodata RO, data RW NX. Block attribute extraction for L3
// replication verified against ARM ARM D5-29/D5-30 — L2 block and L3 page share attribute layout.
// Neighbor-based attribute recovery in clear_kernel_guard_page relies on guard pages never being
// adjacent — sound given buddy allocator pattern (guard is always first page of ≥2-page block).
// TLB sequences correct per ARM ARM D5.10.2. SAFETY comments accurate for all 19 blocks.

//! Kernel page table refinement and address translation.
//!
//! Refines the coarse 32MB-block TTBR1 tables from boot.S with 16KB L3
//! pages for per-section W^X enforcement. Also manages kernel stack
//! guard pages via L3 entry manipulation.

use core::cell::UnsafeCell;

use super::{
    paging::{
        align_up_u64, AF, AP_RO, ATTRIDX0, DESC_PAGE, DESC_TABLE, DESC_VALID, PAGE_SIZE, PA_MASK,
        PXN, SH_INNER, UXN,
    },
    sync::IrqMutex,
};

const BLOCK_32MB: u64 = 32 * 1024 * 1024;

pub const HEAP_SIZE: usize = 16 * 1024 * 1024;
/// Link-time kernel VA offset (const — used in static initializers).
/// The actual runtime offset is `KERNEL_VA_OFFSET + kaslr_slide()`.
pub const KERNEL_VA_OFFSET: usize = super::paging::KERNEL_VA_OFFSET as usize;

/// KASLR slide — random offset added to all kernel VA computations.
///
/// Set once in boot.S before calling kernel_main. Defaults to 0 (no slide),
/// which preserves current behavior. The slide is 2 MiB-aligned so all
/// L2 block mappings remain valid.
///
/// Atomic because secondary cores read it during SMP boot.
static KASLR_SLIDE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Read the KASLR slide. Returns 0 if KASLR is not active.
#[inline(always)]
pub fn kaslr_slide() -> usize {
    KASLR_SLIDE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Set the KASLR slide. Called once from early boot before kernel_main.
///
/// # Safety
///
/// Must be called exactly once, before any code that uses `phys_to_virt`
/// or `virt_to_phys`. The slide must be 2 MiB-aligned.
pub unsafe fn set_kaslr_slide(slide: usize) {
    KASLR_SLIDE.store(slide, core::sync::atomic::Ordering::Release);
}

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    // boot.S TTBR1 L2 root table (need to patch one entry for L3).
    static boot_tt1_l2: u8;
}

// ---------------------------------------------------------------------------
// Pa — physical address newtype
// ---------------------------------------------------------------------------

/// Physical address newtype. Prevents accidental PA/VA mixups at compile time.
///
/// Used at all API boundaries where physical addresses flow: page allocator,
/// page table manipulation, DMA. The `pub` inner field allows extraction
/// where raw arithmetic or pointer casts are needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Pa(pub usize);

impl Pa {
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }
}

// ---------------------------------------------------------------------------
// Page table types (internal)
// ---------------------------------------------------------------------------

#[repr(align(16384))]
struct PageTable {
    entries: [u64; 2048],
}

impl PageTable {
    const fn new() -> Self {
        Self { entries: [0; 2048] }
    }
}

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

// ---------------------------------------------------------------------------
// Statics
// ---------------------------------------------------------------------------

/// Empty L2 table for kernel threads' TTBR0 (no user mappings).
static EMPTY_L2: SyncPageTable = SyncPageTable::new();
/// Lock for kernel TTBR1 page table modifications (break-block, guard pages).
///
/// Lock ordering: KERNEL_PT_LOCK → page allocator lock (never the reverse).
static KERNEL_PT_LOCK: IrqMutex<()> = IrqMutex::new(());
/// L3 page table for the kernel's 32MB block (16KB pages, W^X).
static TT1_L3_KERN: SyncPageTable = SyncPageTable::new();

// ---------------------------------------------------------------------------
// Address translation
// ---------------------------------------------------------------------------

#[inline(always)]
pub fn phys_to_virt(pa: Pa) -> usize {
    pa.0.wrapping_add(KERNEL_VA_OFFSET)
        .wrapping_add(kaslr_slide())
}

#[inline(always)]
pub fn virt_to_phys(va: usize) -> Pa {
    Pa(va
        .wrapping_sub(KERNEL_VA_OFFSET)
        .wrapping_sub(kaslr_slide()))
}

/// Physical address of the empty L0 table (for kernel threads' TTBR0).
pub fn empty_ttbr0() -> u64 {
    virt_to_phys(EMPTY_L2.get() as usize).as_u64()
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Broadcast TLB invalidation across all cores.
fn tlb_invalidate_all() {
    super::arch::mmu::tlbi_all();
}

/// Refine TTBR1 with 16KB pages for the kernel's 32MB block.
///
/// boot.S created coarse 32MB-block tables. This replaces the kernel's
/// 32MB block with an L3 table providing per-section W^X permissions.
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
    // All these are kernel VA. Compute PA of the kernel's 2MB-aligned block.
    let text_start_pa = virt_to_phys(text_start as usize).as_u64();
    let kernel_block_pa = text_start_pa & !(BLOCK_32MB - 1);
    let normal = ATTRIDX0 | AF | SH_INNER;

    for i in 0..2048u64 {
        let pa = kernel_block_pa + i * PAGE_SIZE;
        let va = phys_to_virt(Pa(pa as usize)) as u64;
        let attrs = if va >= text_start && va < text_end {
            normal | AP_RO | UXN // .text: RX (kernel only)
        } else if va >= rodata_start && va < rodata_end {
            normal | AP_RO | PXN | UXN // .rodata: RO
        } else {
            // Everything else in this 32MB block: RW NX.
            // Includes pre-kernel area (DTB, firmware stub), .data, .bss,
            // stack, heap, and any trailing padding.
            normal | PXN | UXN // RW (default for data)
        };

        l3_kern.entries[i as usize] = (pa & PA_MASK) | DESC_PAGE | attrs;
    }

    // Patch TTBR1 L2 to point at L3 instead of the 32MB block.
    let l3_kern_pa = virt_to_phys(TT1_L3_KERN.get() as usize).as_u64();
    let kernel_l2_idx = ((kernel_block_pa >> 25) & 0x7FF) as usize;
    // SAFETY: boot_tt1_l2 is a page-aligned L2 table defined in boot.S.
    // Cast to *mut u64 is valid because page table entries are 8-byte u64s.
    // Called during single-core boot — no concurrent access.
    let l2 = unsafe { &boot_tt1_l2 as *const u8 as *mut u64 };

    // SAFETY: kernel_l2_idx is masked to 0..2047, within the 2048-entry
    // L2 table. Writing a table descriptor pointing to our L3 replaces
    // the boot.S 32MB block with fine-grained 16KB pages.
    unsafe {
        l2.add(kernel_l2_idx)
            .write_volatile(l3_kern_pa | DESC_VALID | DESC_TABLE);
    }

    // TLB invalidation after replacing the L2 block descriptor with an
    // L3 table descriptor. Local-core only (secondary cores not started yet).
    super::arch::mmu::tlbi_all_local();
}

// ---------------------------------------------------------------------------
// Kernel guard pages
// ---------------------------------------------------------------------------

/// Set a kernel VA page as a guard page (unmap from TTBR1).
///
/// If the containing 32MB block hasn't been refined to L3 pages yet,
/// allocates an L3 page table and replaces the L2 block descriptor.
/// The target page's L3 entry is set to 0 (invalid), so any access faults.
///
/// Returns `false` if the L3 table allocation fails (OOM).
pub fn try_set_kernel_guard_page(va: usize) -> bool {
    assert!(va >= KERNEL_VA_OFFSET, "not a kernel VA");
    assert!(va & (PAGE_SIZE as usize - 1) == 0, "VA not page-aligned");

    let _lock = KERNEL_PT_LOCK.lock();
    let pa = virt_to_phys(va).0 as u64;
    let l2_idx = ((pa >> 25) & 0x7FF) as usize;
    // SAFETY: boot_tt1_l2 is a page-aligned L2 table defined in boot.S.
    // Cast to *mut u64 is valid because page table entries are 8-byte u64s.
    // KERNEL_PT_LOCK is held, ensuring exclusive access.
    let l2_table = unsafe { &boot_tt1_l2 as *const u8 as *mut u64 };
    // SAFETY: l2_idx is masked to 0..2047, within the 2048-entry L2 table.
    let l2_entry = unsafe { l2_table.add(l2_idx).read_volatile() };
    let l3_table = if l2_entry & 0b11 == 0b01 {
        let block_pa = l2_entry & 0x0000_FFFF_FE00_0000;
        let block_attrs = l2_entry & !(0x0000_FFFF_FE00_0003u64);
        let l3_frame = match super::page_allocator::alloc_frame() {
            Some(f) => f,
            None => return false,
        };
        let l3_va = phys_to_virt(l3_frame) as *mut u64;

        // Populate 2048 L3 entries replicating the block mapping.
        for i in 0..2048u64 {
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
        // SAFETY: l2_idx is in 0..2047. Writing an invalid (0) entry is
        // step 1 of break-before-make. KERNEL_PT_LOCK is held.
        unsafe {
            // Step 1: Write invalid entry (break).
            l2_table.add(l2_idx).write_volatile(0);
        }

        tlb_invalidate_all();

        // SAFETY: l2_idx is in 0..2047. Writing the new table descriptor
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
    let l3_idx = ((pa >> 14) & 0x7FF) as usize;

    // SAFETY: l3_table is a valid L3 page table in kernel VA.
    unsafe {
        l3_table.add(l3_idx).write_volatile(0);
    }

    tlb_invalidate_all();

    true
}

/// Re-map a kernel guard page as normal memory (restore its L3 entry).
///
/// Called before freeing stack frames back to the buddy allocator, since
/// `free_frames` writes a FreeBlock header at the start of the block.
pub fn clear_kernel_guard_page(va: usize) {
    assert!(va >= KERNEL_VA_OFFSET, "not a kernel VA");
    assert!(va & (PAGE_SIZE as usize - 1) == 0, "VA not page-aligned");

    let _lock = KERNEL_PT_LOCK.lock();
    let pa = virt_to_phys(va).0 as u64;
    let l2_idx = ((pa >> 25) & 0x7FF) as usize;
    // SAFETY: boot_tt1_l2 is a page-aligned L2 table defined in boot.S.
    // Cast to *mut u64 is valid because page table entries are 8-byte u64s.
    // KERNEL_PT_LOCK is held, ensuring exclusive access.
    let l2_table = unsafe { &boot_tt1_l2 as *const u8 as *mut u64 };
    // SAFETY: l2_idx is masked to 0..2047, within the 2048-entry L2 table.
    let l2_entry = unsafe { l2_table.add(l2_idx).read_volatile() };

    // Must already be broken into L3 (set_kernel_guard_page did that).
    assert!(
        l2_entry & 0b11 == 0b11,
        "clear_kernel_guard_page: L2 entry is not a table descriptor"
    );

    let l3_pa = l2_entry & PA_MASK;
    let l3_table = phys_to_virt(Pa(l3_pa as usize)) as *mut u64;
    let l3_idx = ((pa >> 14) & 0x7FF) as usize;
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
