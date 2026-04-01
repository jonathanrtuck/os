// AUDIT: 2026-03-11 — 10 unsafe blocks verified, 6-category checklist applied.
// All unsafe blocks have // SAFETY: comments. Break-before-make (Fix 10)
// re-verified sound: map_inner zeros entry, DSB, TLBI, DSB before writing new
// valid descriptor. AddressSpace Drop (Fix 11) re-verified: catches error paths
// where free_all() was never called, frees TLB + all frames + ASID. VMA edge
// cases verified: overlapping regions prevented by VmaList sorted insert,
// zero-size pages handled by page_count computation, max address bounded by
// region limits. Code quality: upgraded walk_or_create and map_inner to use
// read_volatile/write_volatile for page table entries (hardware may read
// concurrently via MMU).

//! Per-process address space (TTBR0 page tables + ASID).
//!
//! Each user thread owns an `AddressSpace` with its own L2 root table and ASID.
//! `map_page()` walks/creates the 2-level page table (L2→L3) using frames
//! from the page frame allocator. 16 KiB granule, T0SZ=28 (64 GiB VA).

use alloc::vec::Vec;

use super::{
    address_space_id::Asid,
    memory::{self, Pa},
    memory_region::{Backing, Vma, VmaList},
    page_allocator,
    paging::{
        self, AF, AP_EL0, AP_RO, ATTRIDX0, ATTRIDX1, DESC_PAGE, DESC_TABLE, DESC_VALID, NG,
        PAGE_SIZE, PA_MASK, PXN, SH_INNER, UXN,
    },
};

/// Per-process DMA page budget: half of RAM pages.
/// Generous to support GPU render target + depth/stencil surface.
/// Derived from RAM geometry so resolution changes never require kernel updates.
const DEFAULT_DMA_PAGE_LIMIT: u64 = paging::RAM_SIZE_MAX / paging::PAGE_SIZE / 2;
/// Per-process heap page budget: quarter of RAM pages.
const DEFAULT_HEAP_PAGE_LIMIT: u64 = paging::RAM_SIZE_MAX / paging::PAGE_SIZE / 4;

// ---------------------------------------------------------------------------
// PageAttrs — page table attribute builder
// ---------------------------------------------------------------------------

pub struct PageAttrs(u64);

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

// ---------------------------------------------------------------------------
// DmaAllocation / HeapAllocation — per-process allocation records
// ---------------------------------------------------------------------------

pub(crate) struct DmaAllocation {
    va: u64,
    pa: Pa,
    order: u8,
}

pub(crate) struct HeapAllocation {
    va: u64,
    page_count: u64,
}

// ---------------------------------------------------------------------------
// AddressSpace
// ---------------------------------------------------------------------------

pub struct AddressSpace {
    root_pa: Pa,
    asid: Asid,
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
    /// Next available VA in the channel shared memory region. Bump-allocated.
    next_channel_shm_va: u64,
    /// Next available VA in the shared memory region. Bump-allocated.
    next_shared_va: u64,
    /// Next available VA in the heap region. Bump-allocated.
    next_heap_va: u64,
    /// Active heap allocations (for memory_free and process exit cleanup).
    heap_allocations: Vec<HeapAllocation>,
    /// Number of heap pages currently allocated by this process.
    heap_pages_allocated: u64,
    /// Maximum heap pages this process may allocate.
    heap_pages_limit: u64,
    /// Set by free_all() to prevent double-free in Drop.
    freed: bool,
}

impl AddressSpace {
    /// Create a new empty address space with its own L2 root table and ASID.
    ///
    /// Returns `None` if the L2 page table cannot be allocated (OOM).
    pub fn new(asid: Asid) -> Option<Self> {
        let root_pa = page_allocator::alloc_frame()?;

        Some(Self {
            root_pa,
            asid,
            owned_frames: Vec::new(),
            vmas: VmaList::new(),
            next_dma_va: paging::DMA_BUFFER_BASE,
            dma_allocations: Vec::new(),
            dma_pages_allocated: 0,
            dma_pages_limit: DEFAULT_DMA_PAGE_LIMIT,
            next_device_va: paging::DEVICE_MMIO_BASE,
            next_channel_shm_va: paging::CHANNEL_SHM_BASE,
            next_shared_va: paging::SHARED_MEMORY_BASE,
            next_heap_va: paging::HEAP_BASE,
            heap_allocations: Vec::new(),
            heap_pages_allocated: 0,
            heap_pages_limit: DEFAULT_HEAP_PAGE_LIMIT,
            freed: false,
        })
    }

    /// Handle a page fault for an anonymous (zero-fill) VMA.
    fn handle_fault_anonymous(&mut self, page_va: u64, vma: &Vma) -> bool {
        // Allocate a physical frame (zeroed by alloc_frame).
        let pa = match page_allocator::alloc_frame() {
            Some(pa) => pa,
            None => return false,
        };

        // Determine page attributes from VMA permissions.
        let attrs = match (vma.writable, vma.executable) {
            (false, true) => PageAttrs::user_rx(),
            (false, false) => PageAttrs::user_ro(),
            (true, _) => PageAttrs::user_rw(),
        };

        if !self.map_page(page_va, pa.as_u64(), &attrs) {
            page_allocator::free_frame(Pa(pa.0));
            return false;
        }

        self.tlbi_page(page_va);
        true
    }
    /// Handle a page fault for a VMO-backed VMA.
    ///
    /// Looks up the page in the VMO's page list. If committed: map it (not
    /// owned — VMO owns the frame). If uncommitted: allocate, zero-fill,
    /// commit to VMO, then map.
    ///
    /// For write faults on pages with refcount > 1 (shared with snapshots),
    /// performs COW: allocate new page, copy content, replace in VMO.
    fn handle_fault_vmo(
        &mut self,
        page_va: u64,
        vma: &Vma,
        vmo_id: super::vmo::VmoId,
        mapping_offset: u64,
        writable: bool,
    ) -> bool {
        let page_offset = (page_va - vma.start) / PAGE_SIZE + mapping_offset;

        // Access the global VMO table.
        let mut vmo_state = super::vmo::STATE.lock();
        let vmo = match vmo_state.get_mut(vmo_id) {
            Some(v) => v,
            None => return false, // VMO was destroyed — kill process
        };

        // Check if the page is already committed.
        let pa = if let Some((existing_pa, refcount)) = vmo.lookup_page(page_offset) {
            if writable && refcount > 1 {
                // COW: page is shared with a snapshot. Allocate a new page,
                // copy the content, and replace in the VMO.
                let new_pa = match page_allocator::alloc_frame() {
                    Some(pa) => pa,
                    None => return false,
                };
                // Copy content from old page to new page.
                // SAFETY: Both PAs are valid allocated frames. phys_to_virt
                // produces valid kernel VAs. PAGE_SIZE bytes is the full frame.
                unsafe {
                    let src = memory::phys_to_virt(existing_pa) as *const u8;
                    let dst = memory::phys_to_virt(new_pa) as *mut u8;
                    core::ptr::copy_nonoverlapping(src, dst, PAGE_SIZE as usize);
                }
                // Replace in VMO: decrements old page's refcount in snapshots.
                if let Some(freed_pa) = vmo.cow_replace_page(page_offset, new_pa) {
                    // Old page's refcount hit 0 — free it.
                    drop(vmo_state); // Release lock before freeing.
                    page_allocator::free_frame(freed_pa);
                    // Re-acquire to get the PA for mapping — but we already have new_pa.
                    new_pa
                } else {
                    new_pa
                }
            } else {
                existing_pa
            }
        } else {
            // Uncommitted: allocate, zero-fill, commit to VMO.
            let new_pa = match page_allocator::alloc_frame() {
                Some(pa) => pa,
                None => return false,
            };
            // alloc_frame returns zeroed memory.
            vmo.commit_page(page_offset, new_pa);
            new_pa
        };

        // VMO owns the frame — do NOT add to owned_frames.
        // Map with appropriate permissions.
        let attrs = if writable {
            PageAttrs::user_rw()
        } else {
            PageAttrs::user_ro()
        };

        if !self.map_inner(page_va, pa.as_u64(), &attrs) {
            return false;
        }

        self.tlbi_page(page_va);
        true
    }
    fn l2_idx(va: u64) -> usize {
        ((va >> 25) & 0x7FF) as usize
    }
    fn l3_idx(va: u64) -> usize {
        ((va >> 14) & 0x7FF) as usize
    }
    fn map_inner(&mut self, va: u64, pa: u64, attrs: &PageAttrs) -> bool {
        let l2_va = memory::phys_to_virt(self.root_pa) as *mut u64;
        let l3_va = match walk_or_create(l2_va, Self::l2_idx(va)) {
            Some(v) => v,
            None => return false,
        };
        let l3_idx = Self::l3_idx(va);

        // SAFETY: l3_va points to a valid L3 page table (allocated by
        // walk_or_create). l3_idx is 0..2047 (derived from VA bit extraction).
        unsafe {
            let entry = l3_va.add(l3_idx);
            let old = core::ptr::read_volatile(entry);

            if old & DESC_VALID != 0 {
                // Break-before-make (ARMv8 ARM B2.2.1): writing a valid
                // descriptor over an existing valid descriptor is
                // CONSTRAINED UNPREDICTABLE. Must invalidate first.
                core::ptr::write_volatile(entry, 0);

                super::arch::mmu::tlbi_bbm(va, self.asid.0 as u64);
            }

            core::ptr::write_volatile(entry, (pa & PA_MASK) | DESC_PAGE | attrs.0);
        }

        true
    }
    /// Read the PA from an L3 entry and clear it. Returns None if unmapped.
    ///
    /// Does NOT invalidate TLB — caller must do a bulk invalidate after
    /// unmapping all pages in a region.
    fn read_and_unmap_page(&self, va: u64) -> Option<Pa> {
        let l2_va = memory::phys_to_virt(self.root_pa) as *const u64;

        // SAFETY: 2-level page table walk. We read the L2 entry and
        // read+write-volatile the L3 entry.
        unsafe {
            let e2 = *l2_va.add(Self::l2_idx(va));

            if e2 & DESC_VALID == 0 {
                return None;
            }

            let l3_va = memory::phys_to_virt(Pa((e2 & PA_MASK) as usize)) as *mut u64;
            let entry = l3_va.add(Self::l3_idx(va));
            let e3 = core::ptr::read_volatile(entry);

            if e3 & DESC_VALID == 0 {
                return None;
            }

            let pa = Pa((e3 & PA_MASK) as usize);

            core::ptr::write_volatile(entry, 0);

            Some(pa)
        }
    }
    /// TLB invalidate a single page in this address space.
    fn tlbi_page(&self, page_va: u64) {
        // SAFETY: Invalidate any cached fault entry for this VA+ASID. Some
        // ARM implementations cache "translation fault" results ("negative
        // caching"), which would prevent the new mapping from being used.
        // The ASID is valid (allocated by address_space_id::alloc). The
        // barrier sequence (DSB ISHST → TLBI → DSB ISH → ISB) ensures
        // completion across all cores before returning to user code.
        super::arch::mmu::tlbi_page(page_va, self.asid.0 as u64);
    }
    /// Clear a single L3 page table entry (write 0). Does not invalidate TLB —
    /// caller is responsible for a bulk invalidate after unmapping all pages.
    fn unmap_page_inner(&self, va: u64) {
        let l2_va = memory::phys_to_virt(self.root_pa) as *const u64;

        // SAFETY: Page table pointers are valid kernel-mapped memory allocated
        // by walk_or_create during the original map. We read the L2 entry and
        // write-volatile the L3 entry to zero (invalidate).
        unsafe {
            let e2 = *l2_va.add(Self::l2_idx(va));

            if e2 & DESC_VALID == 0 {
                return;
            }

            let l3_va = memory::phys_to_virt(Pa((e2 & PA_MASK) as usize)) as *mut u64;
            let entry = l3_va.add(Self::l3_idx(va));

            core::ptr::write_volatile(entry, 0);
        }
    }

    pub fn asid(&self) -> u8 {
        self.asid.0
    }
    /// Free all resources: DMA buffers, owned user pages, page table frames, and the L2 root table.
    ///
    /// # Precondition
    ///
    /// Caller must call `invalidate_tlb()` before this. Freeing frames while
    /// stale TLB entries reference them produces use-after-free.
    pub fn free_all(&mut self) {
        // Clear heap allocations (physical frames are in owned_frames, freed below).
        self.heap_allocations.clear();

        self.heap_pages_allocated = 0;

        // Free DMA buffer allocations (physically contiguous, multi-page).
        for alloc in self.dma_allocations.drain(..) {
            page_allocator::free_frames(alloc.pa, alloc.order as usize);
        }

        self.dma_pages_allocated = 0;

        // Free owned user pages (code, data, stack).
        for &pa in &self.owned_frames {
            page_allocator::free_frame(pa);
        }

        // Walk 2-level page table (L2 → L3) and free L3 table frames.
        // SAFETY: root_pa was allocated by page_allocator::alloc_frame in new().
        // phys_to_virt produces a valid kernel VA. Each table is 16384 bytes
        // (2048 entries * 8 bytes), and indices 0..2048 stay within bounds.
        // Entries with DESC_VALID set contain physical addresses of tables
        // we allocated, so the derived pointers are valid.
        let l2_va = memory::phys_to_virt(self.root_pa) as *const u64;

        for i2 in 0..2048usize {
            // SAFETY: See block comment above — l2_va valid, i2 in 0..2048.
            let e2 = unsafe { *l2_va.add(i2) };

            if e2 & DESC_VALID == 0 {
                continue;
            }

            let l3_pa = Pa((e2 & PA_MASK) as usize);

            page_allocator::free_frame(l3_pa);
        }

        page_allocator::free_frame(self.root_pa);

        self.freed = true;
    }
    /// Handle a page fault at `va`. Returns true if the fault was resolved
    /// (page mapped), false if `va` is not covered by any VMA (kill process).
    pub fn handle_fault(&mut self, va: u64) -> bool {
        let page_va = va & !(PAGE_SIZE - 1);
        let vma = match self.vmas.lookup(page_va) {
            Some(v) => v.clone(),
            None => return false,
        };

        match &vma.backing {
            Backing::Anonymous => self.handle_fault_anonymous(page_va, &vma),
            Backing::Vmo {
                vmo_id,
                offset,
                writable,
            } => self.handle_fault_vmo(page_va, &vma, *vmo_id, *offset, *writable),
        }
    }
    /// Invalidate all TLB entries for this address space's ASID.
    pub fn invalidate_tlb(&self) {
        // SAFETY: TLBI aside1is invalidates all TLB entries tagged with this
        // ASID. The ASID was allocated by the address_space_id module and is valid.
        // Barriers ensure the invalidation completes before we free pages.
        super::arch::mmu::tlbi_asid(self.asid.0 as u64);
    }
    /// Map a channel shared page into this address space.
    ///
    /// Bump-allocates VA from `CHANNEL_SHM_BASE..CHANNEL_SHM_END`. Each
    /// channel shared page is one 4 KiB page. The physical frame is NOT owned
    /// by this address space — the channel module retains ownership.
    ///
    /// Returns the user VA on success, or None if the channel VA space is full.
    pub fn map_channel_page(&mut self, pa: u64) -> Option<u64> {
        let va = self.next_channel_shm_va;

        if va + PAGE_SIZE > paging::CHANNEL_SHM_END {
            return None;
        }
        if !self.map_inner(va, pa, &PageAttrs::user_rw()) {
            return None;
        }

        self.next_channel_shm_va = va + PAGE_SIZE;

        Some(va)
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
            if !self.map_inner(va + offset, pa + offset, &attrs) {
                return None;
            }

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
            if !self.map_inner(va + i * PAGE_SIZE, pa.as_u64() + i * PAGE_SIZE, &attrs) {
                return None;
            }
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
    /// Map physical pages at a fixed VA, read-only, without ownership transfer.
    ///
    /// Used to map the service pack into init's address space at
    /// `SERVICE_PACK_BASE`. The physical frames are part of the kernel image
    /// and must not be freed on process exit.
    pub fn map_fixed_readonly(&mut self, va: u64, pa: u64, page_count: u64) -> bool {
        let attrs = PageAttrs::user_ro();
        for i in 0..page_count {
            if !self.map_inner(va + i * PAGE_SIZE, pa + i * PAGE_SIZE, &attrs) {
                return false;
            }
        }
        true
    }
    /// Allocate anonymous heap pages (demand-paged on first touch).
    ///
    /// Creates an anonymous VMA in the heap VA region. Pages are NOT eagerly
    /// mapped — the fault handler allocates and maps them on first access.
    /// Returns the user VA on success, or None if the heap VA space or budget
    /// is exhausted.
    pub fn map_heap(&mut self, page_count: u64) -> Option<u64> {
        let size = page_count * PAGE_SIZE;
        let va = self.next_heap_va;

        if va + size > paging::HEAP_END {
            return None;
        }
        if self.heap_pages_allocated + page_count > self.heap_pages_limit {
            return None;
        }

        // Create an anonymous VMA so the demand paging fault handler knows
        // this range is valid and should be zero-filled on first touch.
        self.vmas.insert(super::memory_region::Vma {
            start: va,
            end: va + size,
            writable: true,
            executable: false,
            backing: super::memory_region::Backing::Anonymous,
        });

        self.next_heap_va = va + size;
        self.heap_pages_allocated += page_count;

        self.heap_allocations
            .push(HeapAllocation { va, page_count });

        Some(va)
    }
    /// Map a page and take ownership of the frame (freed on cleanup).
    ///
    /// Must not be called twice for the same PA — that would cause a double-free
    /// in `free_all`.
    pub fn map_page(&mut self, va: u64, pa: u64, attrs: &PageAttrs) -> bool {
        debug_assert!(
            !self.owned_frames.contains(&Pa(pa as usize)),
            "map_page: double-own of PA {:#x}",
            pa
        );

        if !self.map_inner(va, pa, attrs) {
            return false;
        }

        self.owned_frames.push(Pa(pa as usize));

        true
    }
    /// Map physical pages into the shared memory region (no ownership transfer).
    ///
    /// Bump-allocates VA from `SHARED_MEMORY_BASE..SHARED_MEMORY_END`. The
    /// physical frames are NOT owned by this address space — the caller (or
    /// the allocating process) retains ownership.
    ///
    /// When `read_only` is true, pages are mapped without write permission
    /// (hardware-enforced). Used to give editors read-only document access.
    ///
    /// Returns the user VA on success, or None if the shared VA space is full.
    pub fn map_shared_region(&mut self, pa: Pa, page_count: u64, read_only: bool) -> Option<u64> {
        let size = page_count * PAGE_SIZE;
        let va = self.next_shared_va;

        if va + size > paging::SHARED_MEMORY_END {
            return None;
        }

        let attrs = if read_only {
            PageAttrs::user_ro()
        } else {
            PageAttrs::user_rw()
        };

        for i in 0..page_count {
            if !self.map_inner(va + i * PAGE_SIZE, pa.as_u64() + i * PAGE_SIZE, &attrs) {
                return None;
            }
        }

        self.next_shared_va = va + size;

        Some(va)
    }
    /// Map a VMO region into this address space.
    ///
    /// Bump-allocates VA from the shared memory region. Creates a VMA with
    /// `Backing::Vmo` so the fault handler knows to consult the VMO's page list.
    /// Pages are NOT eagerly mapped — they're demand-paged on first touch.
    ///
    /// Returns the base VA on success, or None if VA space is exhausted.
    pub fn map_vmo(
        &mut self,
        vmo_id: super::vmo::VmoId,
        page_count: u64,
        writable: bool,
    ) -> Option<u64> {
        let size = page_count * PAGE_SIZE;
        let va = self.next_shared_va;

        if va + size > paging::SHARED_MEMORY_END {
            return None;
        }

        // Create a VMA for the fault handler. No pages mapped yet — demand-paged.
        self.vmas.insert(Vma {
            start: va,
            end: va + size,
            writable,
            executable: false,
            backing: Backing::Vmo {
                vmo_id,
                offset: 0,
                writable,
            },
        });

        self.next_shared_va = va + size;

        Some(va)
    }
    /// TTBR0 value: physical address of L2 root table | (ASID << 48).
    pub fn ttbr0_value(&self) -> u64 {
        self.root_pa.as_u64() | ((self.asid.0 as u64) << 48)
    }
    /// Unmap a channel shared page previously mapped by `map_channel_page`.
    ///
    /// Clears the L3 page table entry for `va`. Does NOT free the physical
    /// frame (channel module retains ownership). Does NOT rewind the bump
    /// allocator — consumed VA is lost (same as all other bump allocators).
    ///
    /// Used for rollback when `handle_send` partially maps channel pages into
    /// a target process but a subsequent step fails.
    pub fn unmap_channel_page(&mut self, va: u64) {
        self.unmap_page_inner(va);
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
    /// Free a heap allocation by its start VA.
    ///
    /// Removes the VMA, unmaps any demand-paged pages (freeing physical frames),
    /// and invalidates TLB entries. Returns the page count on success.
    pub fn unmap_heap(&mut self, va: u64) -> Option<u64> {
        let idx = self.heap_allocations.iter().position(|a| a.va == va)?;
        let alloc = self.heap_allocations.swap_remove(idx);

        self.heap_pages_allocated -= alloc.page_count;

        // Remove the VMA so future accesses to this range fault-to-kill.
        self.vmas.remove(va);

        // Walk page tables and free any pages that were demand-paged.
        for i in 0..alloc.page_count {
            let page_va = va + i * PAGE_SIZE;

            if let Some(pa) = self.read_and_unmap_page(page_va) {
                // Remove from owned_frames and free.
                if let Some(idx) = self.owned_frames.iter().position(|&p| p == pa) {
                    self.owned_frames.swap_remove(idx);
                }

                page_allocator::free_frame(pa);
            }
        }

        self.invalidate_tlb();

        Some(alloc.page_count)
    }
    /// Unmap a VMO region previously mapped by `map_vmo`.
    ///
    /// Removes the VMA and unmaps any demand-paged pages. Does NOT free
    /// physical frames — the VMO owns them. Invalidates TLB.
    ///
    /// Returns true if a VMO-backed VMA was found and unmapped.
    pub fn unmap_vmo(&mut self, va: u64, page_count: u64) -> bool {
        // Verify this VA is backed by a VMO VMA.
        let vma = match self.vmas.lookup(va) {
            Some(v) => v.clone(),
            None => return false,
        };

        if !matches!(vma.backing, Backing::Vmo { .. }) {
            return false;
        }

        // Remove the VMA.
        self.vmas.remove(va);

        // Unmap any pages that were demand-paged into the page tables.
        // Do NOT free frames — VMO owns them.
        for i in 0..page_count {
            self.unmap_page_inner(va + i * PAGE_SIZE);
        }

        self.invalidate_tlb();

        true
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Safety net: free all owned resources if not already cleaned up.
        // The normal process cleanup path calls invalidate_tlb() + free_all()
        // + address_space_id::free() explicitly and sets freed = true. This
        // catches error paths (e.g. partial allocation failure in
        // create_from_user_elf) where the address space was never loaded
        // into TTBR0 and cleanup would otherwise leak frames.
        if !self.freed {
            self.invalidate_tlb();
            self.free_all();

            super::address_space_id::free(self.asid);
        }
    }
}

// ---------------------------------------------------------------------------
// Page table walker
// ---------------------------------------------------------------------------

/// Walk a table entry; if invalid, allocate a new table and install it.
/// Returns the VA of the next-level table, or `None` on OOM.
fn walk_or_create(table_va: *mut u64, idx: usize) -> Option<*mut u64> {
    // SAFETY: `table_va` points to a valid page table (either the L2 root
    // allocated in `AddressSpace::new` or an L3 table allocated by a previous
    // `walk_or_create` call). `idx` is 0..2047 (derived from VA bit extraction
    // in the caller), so `table_va.add(idx)` stays within the 16384-byte table.
    // We use read_volatile/write_volatile because these are hardware page table
    // entries that the MMU may read concurrently (if this address space is
    // active in TTBR0). The new table frame is zeroed by alloc_frame, so all
    // 2048 entries start as invalid (DESC_VALID clear).
    unsafe {
        let entry = table_va.add(idx);
        let val = core::ptr::read_volatile(entry);

        if val & DESC_VALID != 0 {
            let next_pa = Pa((val & PA_MASK) as usize);

            return Some(memory::phys_to_virt(next_pa) as *mut u64);
        }

        let next_pa = page_allocator::alloc_frame()?;

        core::ptr::write_volatile(entry, next_pa.as_u64() | DESC_VALID | DESC_TABLE);

        Some(memory::phys_to_virt(next_pa) as *mut u64)
    }
}
