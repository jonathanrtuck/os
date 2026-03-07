//! Per-process address space (TTBR0 page tables + ASID).
//!
//! Each user thread owns an `AddressSpace` with its own L0 table and ASID.
//! `map_page()` walks/creates the 4-level page table (L0→L3) using frames
//! from the page frame allocator.

use super::asid::Asid;
use super::memory;
use super::page_alloc;

const AF: u64 = 1 << 10;
const AP_EL0: u64 = 1 << 6;
const AP_RO: u64 = 1 << 7;
const ATTRIDX0: u64 = 0 << 2; // normal memory
const DESC_PAGE: u64 = 0b11;
const DESC_TABLE: u64 = 1 << 1;
const DESC_VALID: u64 = 1 << 0;
const NG: u64 = 1 << 11; // ASID-tagged (required for EL0-accessible pages)
const PXN: u64 = 1 << 53;
const SH_INNER: u64 = 0b11 << 8;
const UXN: u64 = 1 << 54;

pub struct AddressSpace {
    l0_pa: usize,
    asid: Asid,
}
pub struct PageAttrs(u64);

impl AddressSpace {
    /// Create a new empty address space with its own L0 table and ASID.
    pub fn new(asid: Asid) -> Self {
        let l0_pa = page_alloc::alloc_frame().expect("out of frames for L0 table");

        Self { l0_pa, asid }
    }
    /// Map a single 4 KiB page at `va` backed by physical frame `pa`.
    pub fn map_page(&mut self, va: u64, pa: u64, attrs: &PageAttrs) {
        let l0_va = memory::phys_to_virt(self.l0_pa) as *mut u64;

        let l1_va = walk_or_create(l0_va, Self::l0_idx(va));
        let l2_va = walk_or_create(l1_va, Self::l1_idx(va));
        let l3_va = walk_or_create(l2_va, Self::l2_idx(va));
        let l3_idx = Self::l3_idx(va);

        unsafe {
            let entry = l3_va.add(l3_idx);
            *entry = (pa & 0x0000_FFFF_FFFF_F000) | DESC_PAGE | attrs.0;
        }
    }
    /// TTBR0 value: physical address of L0 table | (ASID << 48).
    pub fn ttbr0_value(&self) -> u64 {
        self.l0_pa as u64 | ((self.asid.0 as u64) << 48)
    }
    pub fn asid(&self) -> u8 {
        self.asid.0
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
}
impl PageAttrs {
    /// User code: readable + executable, not writable.
    pub fn user_rx() -> Self {
        Self(ATTRIDX0 | AF | SH_INNER | AP_EL0 | AP_RO | NG | PXN)
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
            let next_pa = (val & 0x0000_FFFF_FFFF_F000) as usize;

            return memory::phys_to_virt(next_pa) as *mut u64;
        }

        // Allocate a new zeroed page for the next-level table.
        let next_pa = page_alloc::alloc_frame().expect("out of frames for page table");

        *entry = (next_pa as u64) | DESC_VALID | DESC_TABLE;

        memory::phys_to_virt(next_pa) as *mut u64
    }
}
