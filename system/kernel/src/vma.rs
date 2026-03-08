//! Virtual Memory Areas — tracks logical regions of a process's address space.
//!
//! Each VMA describes a contiguous range of virtual addresses with uniform
//! permissions and backing. Used by the demand paging fault handler to
//! determine what to map on a page fault.

use super::paging::PAGE_SIZE;
use alloc::vec::Vec;

/// What backs a VMA's pages.
#[derive(Clone)]
pub enum Backing {
    /// Anonymous zero-filled pages (stack, BSS).
    Anonymous,
    /// ELF segment data: first `data_len` bytes come from `data`, rest zeroed.
    Elf { data: &'static [u8], data_len: u64 },
}

/// Describes a contiguous region of virtual address space.
#[derive(Clone)]
pub struct Vma {
    pub start: u64,
    pub end: u64, // exclusive
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub backing: Backing,
}
/// Sorted list of VMAs. No overlaps allowed.
pub struct VmaList {
    vmas: Vec<Vma>,
}

impl VmaList {
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
    pub const fn new() -> Self {
        Self { vmas: Vec::new() }
    }
    /// Compute the offset of `va` within the VMA's backing data.
    pub fn page_offset(va: u64) -> u64 {
        va & !(PAGE_SIZE - 1)
    }
}
