//! Per-process address space (TTBR0 page tables + ASID).
//!
//! Each user thread owns an `AddressSpace` with its own L0 table and ASID.
//! `map_page()` walks/creates the 4-level page table (L0→L3) using frames
//! from the page frame allocator.

use super::address_space_id::Asid;
use super::memory::{self, Pa};
use super::memory_region::{Backing, VmaList};
use super::page_allocator;
use super::paging::{
    self, AF, AP_EL0, AP_RO, ATTRIDX0, ATTRIDX1, DESC_PAGE, DESC_TABLE, DESC_VALID, NG, PAGE_SIZE,
    PA_MASK, PXN, SH_INNER, UXN,
};
use alloc::vec::Vec;

/// Default per-process DMA page budget (8192 pages = 32 MiB).
const DEFAULT_DMA_PAGE_LIMIT: u64 = 8192;

pub struct AddressSpace {
    l0_pa: Pa,
    asid: Asid,
    generation: u64,
    owned_frames: Vec<Pa>,
    pub(crate) vmas: VmaList,
    /// Next available VA in the DMA buffer region. Bump-allocated.
    next_dma_va: u64,
    /// Active DMA buffer allocations (freed on process exit or dma_free).
    dma_allocations: Vec<DmaAllocation>,
    /// Number of DMA pages currently allocated by this process.
    dma_pages_allocated: u64,
    /// Maximum DMA pages this process may allocate.
    dma_pages_limit: u64,
    /// Next available VA in the device MMIO region. Bump-allocated.
    next_device_va: u64,
}
pub(crate) struct DmaAllocation {
    va: u64,
    pa: Pa,
    order: u8,
}
pub struct PageAttrs(u64);

impl AddressSpace {
    /// Create a new empty address space with its own L0 table and ASID.
    pub fn new(asid: Asid, generation: u64) -> Self {
        let l0_pa = page_allocator::alloc_frame().expect("out of frames for L0 table");

        Self {
            l0_pa,
            asid,
            generation,
            owned_frames: Vec::new(),
            vmas: VmaList::new(),
            next_dma_va: paging::DMA_BUFFER_BASE,
            dma_allocations: Vec::new(),
            dma_pages_allocated: 0,
            dma_pages_limit: DEFAULT_DMA_PAGE_LIMIT,
            next_device_va: paging::DEVICE_MMIO_BASE,
        }
    }

    fn l0_idx(va: u64) -> usize {
        ((va >> 39) & 0x1FF) as usize
    }
    fn l1_idx(va: u64) -> usize {
        ((va >> 30) & 0x1FF) as usize
    }
    fn l2_idx(va: u64) -> usize {
        ((va >> 21) & 0x1FF) as usize
    }
    fn l3_idx(va: u64) -> usize {
        ((va >> 12) & 0x1FF) as usize
    }
    fn map_inner(&mut self, va: u64, pa: u64, attrs: &PageAttrs) {
        let l0_va = memory::phys_to_virt(self.l0_pa) as *mut u64;
        let l1_va = walk_or_create(l0_va, Self::l0_idx(va));
        let l2_va = walk_or_create(l1_va, Self::l1_idx(va));
        let l3_va = walk_or_create(l2_va, Self::l2_idx(va));
        let l3_idx = Self::l3_idx(va);

        // SAFETY: l3_va points to a valid L3 page table (allocated by
        // walk_or_create). l3_idx is 0..511 (derived from VA bit extraction).
        unsafe {
            let entry = l3_va.add(l3_idx);
            let old = core::ptr::read_volatile(entry);

            if old & DESC_VALID != 0 {
                // Break-before-make (ARMv8 ARM B2.2.1): writing a valid
                // descriptor over an existing valid descriptor is
                // CONSTRAINED UNPREDICTABLE. Must invalidate first.
                core::ptr::write_volatile(entry, 0);
                core::arch::asm!(
                    "dsb ish",
                    "tlbi vale1is, {va}",
                    "dsb ish",
                    va = in(reg) (va >> 12) | ((self.asid.0 as u64) << 48),
                    options(nostack)
                );
            }

            *entry = (pa & PA_MASK) | DESC_PAGE | attrs.0;
        }
    }

    pub fn asid(&self) -> u8 {
        self.asid.0
    }
    /// Free all resources: DMA buffers, owned user pages, page table frames, and the L0 table.
    ///
    /// # Precondition
    ///
    /// Caller must call `invalidate_tlb()` before this. Freeing frames while
    /// stale TLB entries reference them produces use-after-free.
    pub fn free_all(&mut self) {
        // Free DMA buffer allocations (physically contiguous, multi-page).
        for alloc in self.dma_allocations.drain(..) {
            page_allocator::free_frames(alloc.pa, alloc.order as usize);
        }
        self.dma_pages_allocated = 0;
        // Free owned user pages (code, data, stack).
        for &pa in &self.owned_frames {
            page_allocator::free_frame(pa);
        }

        // Walk page table structure and free table frames (L1, L2, L3 tables).
        // SAFETY: l0_pa was allocated by page_allocator::alloc_frame in new().
        // phys_to_virt produces a valid kernel VA. Each table is 4096 bytes
        // (512 entries * 8 bytes), and indices 0..512 stay within bounds.
        // Entries with DESC_VALID set contain physical addresses of tables
        // we allocated, so the derived pointers are valid.
        let l0_va = memory::phys_to_virt(self.l0_pa) as *const u64;

        for i0 in 0..512usize {
            let e0 = unsafe { *l0_va.add(i0) };

            if e0 & DESC_VALID == 0 {
                continue;
            }

            let l1_pa = Pa((e0 & PA_MASK) as usize);
            let l1_va = memory::phys_to_virt(l1_pa) as *const u64;

            for i1 in 0..512usize {
                let e1 = unsafe { *l1_va.add(i1) };

                if e1 & DESC_VALID == 0 {
                    continue;
                }

                let l2_pa = Pa((e1 & PA_MASK) as usize);
                let l2_va = memory::phys_to_virt(l2_pa) as *const u64;

                for i2 in 0..512usize {
                    let e2 = unsafe { *l2_va.add(i2) };

                    if e2 & DESC_VALID == 0 {
                        continue;
                    }

                    let l3_pa = Pa((e2 & PA_MASK) as usize);

                    page_allocator::free_frame(l3_pa);
                }

                page_allocator::free_frame(l2_pa);
            }

            page_allocator::free_frame(l1_pa);
        }

        page_allocator::free_frame(self.l0_pa);
    }
    /// Invalidate all TLB entries for this address space's ASID.
    pub fn invalidate_tlb(&self) {
        // SAFETY: TLBI aside1is invalidates all TLB entries tagged with this
        // ASID. The ASID was allocated by the address_space_id module and is valid.
        // Barriers ensure the invalidation completes before we free pages.
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi aside1is, {v}",
                "dsb ish",
                "isb",
                v = in(reg) (self.asid.0 as u64) << 48,
                options(nostack)
            );
        }
    }
    /// Get the ASID generation (for lazy revalidation on context switch).
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Handle a page fault at `va`. Returns true if the fault was resolved
    /// (page mapped), false if `va` is not covered by any VMA (kill process).
    pub fn handle_fault(&mut self, va: u64) -> bool {
        let page_va = va & !(PAGE_SIZE - 1);
        let vma = match self.vmas.lookup(page_va) {
            Some(v) => v.clone(),
            None => return false,
        };
        // Allocate a physical frame (zeroed by alloc_frame).
        let pa = match page_allocator::alloc_frame() {
            Some(pa) => pa,
            None => return false,
        };

        // Fill from backing data if needed.
        match &vma.backing {
            Backing::Anonymous => {} // Already zeroed.
            Backing::Elf { data, data_len } => {
                let seg_offset = page_va - vma.start;

                if seg_offset < *data_len {
                    let src_start = seg_offset as usize;
                    let src_end =
                        core::cmp::min((seg_offset + PAGE_SIZE) as usize, *data_len as usize);
                    let src = &data[src_start..src_end];
                    let dst = memory::phys_to_virt(pa) as *mut u8;

                    // SAFETY: `pa` was just allocated. `src` is bounded by ELF data.
                    unsafe {
                        core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                    }
                }
            }
        }

        // Determine page attributes from VMA permissions.
        let attrs = match (vma.writable, vma.executable) {
            (false, true) => PageAttrs::user_rx(),
            (false, false) => PageAttrs::user_ro(),
            (true, _) => PageAttrs::user_rw(),
        };

        self.map_page(page_va, pa.as_u64(), &attrs);

        // Invalidate any cached fault entry for this VA+ASID. Some ARM
        // implementations cache "translation fault" results ("negative
        // caching"), which would prevent the new mapping from being used.
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi vale1is, {va}",
                "dsb ish",
                "isb",
                va = in(reg) (page_va >> 12) | ((self.asid.0 as u64) << 48),
                options(nostack)
            );
        }

        true
    }
    /// Map a device MMIO region into this address space.
    ///
    /// Allocates VA from the device MMIO region (bump allocator), maps each
    /// page with device memory attributes (ATTRIDX1 = Device-nGnRE). Does not
    /// take ownership of the physical frames — device registers aren't RAM.
    ///
    /// Returns the user VA on success, or None if the device VA space is full.
    pub fn map_device_mmio(&mut self, pa: u64, size: u64) -> Option<u64> {
        let aligned_size = paging::align_up_u64(size, PAGE_SIZE);
        let va = self.next_device_va;

        if va + aligned_size > paging::DEVICE_MMIO_END {
            return None;
        }

        let attrs = PageAttrs::user_device_rw();
        let mut offset = 0;

        while offset < aligned_size {
            self.map_inner(va + offset, pa + offset, &attrs);

            offset += PAGE_SIZE;
        }

        self.next_device_va = va + aligned_size;

        Some(va)
    }
    /// Map a DMA buffer (2^order contiguous pages) into the DMA VA region.
    ///
    /// Bump-allocates VA from `DMA_BUFFER_BASE..DMA_BUFFER_END`. The physical
    /// frames are NOT added to `owned_frames` — they are tracked separately
    /// in `dma_allocations` and freed via `unmap_dma_buffer` or `free_all`.
    ///
    /// Returns the user VA on success, or None if the DMA VA space is full.
    pub fn map_dma_buffer(&mut self, pa: Pa, order: usize) -> Option<u64> {
        let num_pages = 1u64 << order;
        let size = num_pages * PAGE_SIZE;
        let va = self.next_dma_va;

        if va + size > paging::DMA_BUFFER_END {
            return None;
        }
        // Enforce per-process DMA budget.
        if self.dma_pages_allocated + num_pages > self.dma_pages_limit {
            return None;
        }

        let attrs = PageAttrs::user_rw();

        for i in 0..num_pages {
            self.map_inner(va + i * PAGE_SIZE, pa.as_u64() + i * PAGE_SIZE, &attrs);
        }

        self.next_dma_va = va + size;
        self.dma_pages_allocated += num_pages;
        self.dma_allocations.push(DmaAllocation {
            va,
            pa,
            order: order as u8,
        });

        Some(va)
    }
    /// Map a page and take ownership of the frame (freed on cleanup).
    ///
    /// Must not be called twice for the same PA — that would cause a double-free
    /// in `free_all`.
    pub fn map_page(&mut self, va: u64, pa: u64, attrs: &PageAttrs) {
        debug_assert!(
            !self.owned_frames.contains(&Pa(pa as usize)),
            "map_page: double-own of PA {:#x}",
            pa
        );

        self.map_inner(va, pa, attrs);
        self.owned_frames.push(Pa(pa as usize));
    }
    /// Map a shared page (caller retains ownership of the frame).
    pub fn map_shared(&mut self, va: u64, pa: u64, attrs: &PageAttrs) {
        self.map_inner(va, pa, attrs);
    }
    /// Re-assign this address space's ASID (for generation rollover).
    pub fn reassign_asid(&mut self, asid: Asid, generation: u64) {
        self.asid = asid;
        self.generation = generation;
    }
    /// TTBR0 value: physical address of L0 table | (ASID << 48).
    pub fn ttbr0_value(&self) -> u64 {
        self.l0_pa.as_u64() | ((self.asid.0 as u64) << 48)
    }
    /// Unmap a DMA buffer by its VA. Clears page table entries, invalidates
    /// TLB, and removes the allocation record.
    ///
    /// Returns `(pa, order)` for the caller to free via `page_allocator::free_frames`.
    /// Returns None if no DMA allocation starts at `va`.
    pub fn unmap_dma_buffer(&mut self, va: u64) -> Option<(Pa, usize)> {
        let idx = self.dma_allocations.iter().position(|a| a.va == va)?;
        let alloc = self.dma_allocations.swap_remove(idx);
        let num_pages = 1u64 << alloc.order;

        self.dma_pages_allocated -= num_pages;

        // Clear L3 page table entries for each page in the allocation.
        for i in 0..num_pages {
            self.unmap_page_inner(va + i * PAGE_SIZE);
        }

        // Bulk TLB invalidate for this ASID.
        self.invalidate_tlb();

        Some((alloc.pa, alloc.order as usize))
    }
    /// Clear a single L3 page table entry (write 0). Does not invalidate TLB —
    /// caller is responsible for a bulk invalidate after unmapping all pages.
    fn unmap_page_inner(&self, va: u64) {
        let l0_va = memory::phys_to_virt(self.l0_pa) as *const u64;

        // SAFETY: Page table pointers are valid kernel-mapped memory allocated
        // by walk_or_create during the original map. We only read L0-L2 entries
        // and write-volatile the L3 entry to zero (invalidate).
        unsafe {
            let e0 = *l0_va.add(Self::l0_idx(va));

            if e0 & DESC_VALID == 0 {
                return;
            }

            let l1_va = memory::phys_to_virt(Pa((e0 & PA_MASK) as usize)) as *const u64;
            let e1 = *l1_va.add(Self::l1_idx(va));

            if e1 & DESC_VALID == 0 {
                return;
            }

            let l2_va = memory::phys_to_virt(Pa((e1 & PA_MASK) as usize)) as *const u64;
            let e2 = *l2_va.add(Self::l2_idx(va));

            if e2 & DESC_VALID == 0 {
                return;
            }

            let l3_va = memory::phys_to_virt(Pa((e2 & PA_MASK) as usize)) as *mut u64;
            let entry = l3_va.add(Self::l3_idx(va));

            core::ptr::write_volatile(entry, 0);
        }
    }
}
impl PageAttrs {
    /// Device MMIO: Device-nGnRE (ATTRIDX1), RW, not executable.
    ///
    /// No SH_INNER — shareability for device memory is determined by MAIR,
    /// not page table attributes (ARMv8 ARM D5.5).
    pub fn user_device_rw() -> Self {
        Self(ATTRIDX1 | AF | AP_EL0 | NG | PXN | UXN)
    }
    /// User read-only data: readable, not writable, not executable.
    pub fn user_ro() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | AP_RO | NG | PXN | UXN)
    }
    /// User data: readable + writable, not executable.
    pub fn user_rw() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | NG | PXN | UXN)
    }
    /// User code: readable + executable, not writable.
    pub fn user_rx() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | AP_RO | NG | PXN)
    }
}

/// Walk a table entry; if invalid, allocate a new table and install it.
/// Returns the VA of the next-level table.
fn walk_or_create(table_va: *mut u64, idx: usize) -> *mut u64 {
    // SAFETY: table_va was either:
    //   (a) The L0 table PA converted via phys_to_virt (AddressSpace::new
    //       allocates the L0 frame), or
    //   (b) A next-level table VA returned by a prior call to this function.
    // In both cases it points to a 4096-byte (512-entry) page table in
    // kernel-mapped memory. idx is 0..511 (derived from VA bit extraction
    // via `(va >> shift) & 0x1FF`), so `table_va.add(idx)` is in bounds.
    // If the entry is valid, its PA came from alloc_frame + phys_to_virt,
    // producing a valid kernel VA. If invalid, we allocate a new zeroed
    // frame — alloc_frame returns page-aligned physical memory that
    // phys_to_virt maps into the kernel's TTBR1 window.
    unsafe {
        let entry = table_va.add(idx);
        let val = *entry;

        if val & DESC_VALID != 0 {
            // Existing table descriptor — extract PA, convert to VA.
            let next_pa = Pa((val & PA_MASK) as usize);

            return memory::phys_to_virt(next_pa) as *mut u64;
        }

        // Allocate a new zeroed page for the next-level table.
        let next_pa = page_allocator::alloc_frame().expect("out of frames for page table");

        *entry = next_pa.as_u64() | DESC_VALID | DESC_TABLE;

        memory::phys_to_virt(next_pa) as *mut u64
    }
}
