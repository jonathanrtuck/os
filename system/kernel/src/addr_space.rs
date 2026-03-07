//! Per-process address space (TTBR0 page tables + ASID).
//!
//! Each user thread owns an `AddressSpace` with its own L0 table and ASID.
//! `map_page()` walks/creates the 4-level page table (L0→L3) using frames
//! from the page frame allocator.

use super::asid::Asid;
use super::memory;
use super::page_alloc;
use alloc::vec::Vec;

const AF: u64 = 1 << 10;
const AP_EL0: u64 = 1 << 6;
const AP_RO: u64 = 1 << 7;
const ATTRIDX0: u64 = 0 << 2; // normal memory
const DESC_PAGE: u64 = 0b11;
const DESC_TABLE: u64 = 1 << 1;
const DESC_VALID: u64 = 1 << 0;
const NG: u64 = 1 << 11; // ASID-tagged (required for EL0-accessible pages)
const PA_MASK: u64 = 0x0000_FFFF_FFFF_F000;
const PXN: u64 = 1 << 53;
const SH_INNER: u64 = 0b11 << 8;
const UXN: u64 = 1 << 54;

pub struct AddressSpace {
    l0_pa: usize,
    asid: Asid,
    owned_frames: Vec<usize>,
}
pub struct PageAttrs(u64);

impl AddressSpace {
    pub fn asid(&self) -> u8 {
        self.asid.0
    }
    /// Free all resources: owned user pages, page table frames, and the L0 table.
    pub fn free_all(&mut self) {
        // Free owned user pages (code, data, stack).
        for &pa in &self.owned_frames {
            page_alloc::free_frame(pa);
        }

        // Walk page table structure and free table frames (L1, L2, L3 tables).
        let l0_va = memory::phys_to_virt(self.l0_pa) as *const u64;

        for i0 in 0..512usize {
            let e0 = unsafe { *l0_va.add(i0) };

            if e0 & DESC_VALID == 0 {
                continue;
            }

            let l1_pa = (e0 & PA_MASK) as usize;
            let l1_va = memory::phys_to_virt(l1_pa) as *const u64;

            for i1 in 0..512usize {
                let e1 = unsafe { *l1_va.add(i1) };

                if e1 & DESC_VALID == 0 {
                    continue;
                }

                let l2_pa = (e1 & PA_MASK) as usize;
                let l2_va = memory::phys_to_virt(l2_pa) as *const u64;

                for i2 in 0..512usize {
                    let e2 = unsafe { *l2_va.add(i2) };

                    if e2 & DESC_VALID == 0 {
                        continue;
                    }

                    let l3_pa = (e2 & PA_MASK) as usize;

                    page_alloc::free_frame(l3_pa);
                }

                page_alloc::free_frame(l2_pa);
            }

            page_alloc::free_frame(l1_pa);
        }

        page_alloc::free_frame(self.l0_pa);
    }
    /// Invalidate all TLB entries for this address space's ASID.
    pub fn invalidate_tlb(&self) {
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
    /// Map a page and take ownership of the frame (freed on cleanup).
    pub fn map_page(&mut self, va: u64, pa: u64, attrs: &PageAttrs) {
        self.map_inner(va, pa, attrs);
        self.owned_frames.push(pa as usize);
    }
    /// Map a shared page (caller retains ownership of the frame).
    pub fn map_shared(&mut self, va: u64, pa: u64, attrs: &PageAttrs) {
        self.map_inner(va, pa, attrs);
    }
    /// Create a new empty address space with its own L0 table and ASID.
    pub fn new(asid: Asid) -> Self {
        let l0_pa = page_alloc::alloc_frame().expect("out of frames for L0 table");

        Self {
            l0_pa,
            asid,
            owned_frames: Vec::new(),
        }
    }
    /// TTBR0 value: physical address of L0 table | (ASID << 48).
    pub fn ttbr0_value(&self) -> u64 {
        self.l0_pa as u64 | ((self.asid.0 as u64) << 48)
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

        unsafe {
            let entry = l3_va.add(l3_idx);

            *entry = (pa & PA_MASK) | DESC_PAGE | attrs.0;
        }
    }
}
impl PageAttrs {
    /// User code: readable + executable, not writable.
    pub fn user_rx() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | AP_RO | NG | PXN)
    }
    /// User read-only data: readable, not writable, not executable.
    pub fn user_ro() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | AP_RO | NG | PXN | UXN)
    }
    /// User data: readable + writable, not executable.
    pub fn user_rw() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | NG | PXN | UXN)
    }
}

/// Walk a table entry; if invalid, allocate a new table and install it.
/// Returns the VA of the next-level table.
fn walk_or_create(table_va: *mut u64, idx: usize) -> *mut u64 {
    unsafe {
        let entry = table_va.add(idx);
        let val = *entry;

        if val & DESC_VALID != 0 {
            // Existing table descriptor — extract PA, convert to VA.
            let next_pa = (val & PA_MASK) as usize;

            return memory::phys_to_virt(next_pa) as *mut u64;
        }

        // Allocate a new zeroed page for the next-level table.
        let next_pa = page_alloc::alloc_frame().expect("out of frames for page table");

        *entry = (next_pa as u64) | DESC_VALID | DESC_TABLE;

        memory::phys_to_virt(next_pa) as *mut u64
    }
}
