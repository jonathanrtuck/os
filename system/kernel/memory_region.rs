//! Virtual Memory Areas — tracks logical regions of a process's address space.
//!
//! Each VMA describes a contiguous range of virtual addresses with uniform
//! permissions and backing. Used by the demand paging fault handler to
//! determine what to map on a page fault.

use alloc::vec::Vec;

/// Describes a contiguous region of virtual address space.
#[derive(Clone)]
pub struct Vma {
    pub start: u64,
    pub end: u64, // exclusive
    // NOTE: Guard pages are implemented via gaps in the VMA list (no VMA covers
    // the guard address → fault → kill), not via readable=false. This field
    // exists for future use if fine-grained no-access regions are needed within
    // an otherwise mapped range (e.g., PROT_NONE pages for ASan red-zones).
    // Currently, readable is always true for all VMAs.
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub backing: Backing,
}
/// Sorted list of VMAs. No overlaps allowed.
pub struct VmaList {
    vmas: Vec<Vma>,
}

/// What backs a VMA's pages.
#[derive(Clone)]
pub enum Backing {
    /// Anonymous zero-filled pages (stack, BSS).
    Anonymous,
    /// ELF segment data: first `data_len` bytes come from `data`, rest zeroed.
    Elf { data: &'static [u8], data_len: u64 },
}

impl VmaList {
    pub const fn new() -> Self {
        Self { vmas: Vec::new() }
    }

    /// Insert a VMA, maintaining sorted order by start address.
    pub fn insert(&mut self, vma: Vma) {
        let pos = self
            .vmas
            .binary_search_by_key(&vma.start, |v| v.start)
            .unwrap_or_else(|e| e);

        self.vmas.insert(pos, vma);
    }
    /// Look up the VMA containing `va`. Returns None if `va` is in a gap
    /// (e.g., guard page).
    pub fn lookup(&self, va: u64) -> Option<&Vma> {
        // Binary search for the VMA whose range contains va.
        //
        // The comparator returns the Ordering from the VMA's perspective:
        // - Greater means "I'm too high for this va" (va < my start)
        // - Less means "I'm too low for this va" (va >= my end)
        // - Equal means "va is within my range"
        //
        // This is inverted from the typical "compare va to vma" perspective
        // because binary_search_by passes the *element* as the argument.
        let idx = self
            .vmas
            .binary_search_by(|vma| {
                if va < vma.start {
                    core::cmp::Ordering::Greater
                } else if va >= vma.end {
                    core::cmp::Ordering::Less
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .ok()?;

        Some(&self.vmas[idx])
    }
    /// Remove the VMA whose start address matches `start`.
    ///
    /// Returns the removed VMA, or None if no VMA starts at that address.
    pub fn remove(&mut self, start: u64) -> Option<Vma> {
        let idx = self.vmas.binary_search_by_key(&start, |v| v.start).ok()?;

        Some(self.vmas.remove(idx))
    }
}
