//! Heap VA allocator with free list and coalescing.
//!
//! Manages virtual address ranges for the heap region. Physical pages
//! are demand-paged on first touch — this allocator only tracks VA.
//!
//! Design: bump allocator with a sorted free list for reclamation.
//! `alloc()` searches the free list (best-fit) first, then falls back
//! to bumping `next_va`. `free()` inserts the range into the free list
//! and coalesces with any adjacent entries.
//!
//! Pure computation — no page table manipulation, no arch dependencies.
//! Testable independently from `AddressSpace`.

/// A free VA range: `page_count` pages starting at `va`.
struct FreeRange {
    va: u64,
    page_count: u64,
}

const PAGE_SIZE: u64 = 16384;

/// Heap VA allocator: bump + sorted free list with coalescing.
pub struct HeapVaAllocator {
    /// Next available VA for bump allocation.
    next_va: u64,
    /// Per-process ceiling (heap_base + usable). Bump cannot exceed this.
    va_end: u64,
    /// Sorted free list of reclaimed VA ranges. Sorted by VA ascending.
    /// Adjacent ranges are coalesced on insertion.
    free_list: Vec<FreeRange>,
}

// Vec is not available in #![no_std] without alloc. The kernel has `alloc`
// via its global allocator, so this is fine for kernel use. For the host
// test build, `std` provides `Vec`.
#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(test)]
use std::vec::Vec;

impl HeapVaAllocator {
    /// Create a new allocator for the range `[base, end)`.
    pub fn new(base: u64, end: u64) -> Self {
        Self {
            next_va: base,
            va_end: end,
            free_list: Vec::new(),
        }
    }

    /// Allocate `page_count` pages of contiguous VA.
    ///
    /// Strategy: best-fit search of the free list first, then bump.
    /// Returns the base VA, or `None` if no contiguous range is available.
    pub fn alloc(&mut self, page_count: u64) -> Option<u64> {
        if page_count == 0 {
            return None;
        }

        // Search free list for best fit (smallest range >= page_count).
        if let Some(va) = self.alloc_from_free_list(page_count) {
            return Some(va);
        }

        // Fall back to bump allocator.
        let size = page_count * PAGE_SIZE;
        let va = self.next_va;

        if va + size > self.va_end {
            return None;
        }

        self.next_va = va + size;

        Some(va)
    }

    /// Return `page_count` pages starting at `va` to the free list.
    ///
    /// Inserts the range into the sorted free list and coalesces with
    /// any adjacent entries (both left and right neighbors).
    pub fn free(&mut self, va: u64, page_count: u64) {
        let range_end = va + page_count * PAGE_SIZE;

        // Find the insertion point (sorted by VA).
        let pos = self
            .free_list
            .iter()
            .position(|r| r.va >= va)
            .unwrap_or(self.free_list.len());

        // Check for coalescing with the right neighbor.
        let coalesce_right = pos < self.free_list.len()
            && range_end == self.free_list[pos].va;

        // Check for coalescing with the left neighbor.
        let coalesce_left = pos > 0 && {
            let left = &self.free_list[pos - 1];
            left.va + left.page_count * PAGE_SIZE == va
        };

        match (coalesce_left, coalesce_right) {
            (true, true) => {
                // Merge left + new + right into left.
                let right_pages = self.free_list[pos].page_count;
                self.free_list.remove(pos);
                self.free_list[pos - 1].page_count += page_count + right_pages;
            }
            (true, false) => {
                // Extend left neighbor to cover the new range.
                self.free_list[pos - 1].page_count += page_count;
            }
            (false, true) => {
                // Extend right neighbor backward to cover the new range.
                self.free_list[pos].va = va;
                self.free_list[pos].page_count += page_count;
            }
            (false, false) => {
                // No neighbors to coalesce — insert new entry.
                self.free_list.insert(pos, FreeRange { va, page_count });
            }
        }
    }

    /// Reset the allocator, discarding the free list.
    pub fn clear(&mut self) {
        self.free_list.clear();
    }

    /// Best-fit search: find the smallest free range >= `page_count`.
    ///
    /// If found, removes (or shrinks) the range and returns the VA.
    fn alloc_from_free_list(&mut self, page_count: u64) -> Option<u64> {
        // Find the best fit: smallest range that can satisfy the request.
        let mut best_idx = None;
        let mut best_excess = u64::MAX;

        for (i, range) in self.free_list.iter().enumerate() {
            if range.page_count >= page_count {
                let excess = range.page_count - page_count;

                if excess < best_excess {
                    best_excess = excess;
                    best_idx = Some(i);

                    if excess == 0 {
                        break; // Exact fit — can't do better.
                    }
                }
            }
        }

        let idx = best_idx?;
        let va = self.free_list[idx].va;

        if self.free_list[idx].page_count == page_count {
            // Exact fit — remove the entry.
            self.free_list.remove(idx);
        } else {
            // Split: take from the front, keep the remainder.
            self.free_list[idx].va += page_count * PAGE_SIZE;
            self.free_list[idx].page_count -= page_count;
        }

        Some(va)
    }
}
