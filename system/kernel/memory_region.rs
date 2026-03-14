// AUDIT: 2026-03-14 — 0 unsafe blocks. 6-category checklist applied. Pure logic
// audit: binary search insert/lookup/remove verified correct for empty list,
// adjacent VMAs, gaps, boundary addresses (0, u64::MAX-PAGE_SIZE), zero-length
// and inverted ranges (harmless no-match). "No overlaps" invariant is
// caller-enforced (3 call sites in address_space.rs/process.rs, all correct).
// Previously identified issues (11.34 page_offset dead code, 11.35 readable
// field) already removed. No bugs found.

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
