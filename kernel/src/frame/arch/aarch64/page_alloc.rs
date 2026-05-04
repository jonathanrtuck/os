//! Physical page frame allocator — bitmap-based with per-page reference counts.
//!
//! One bit per 16 KiB page in a static bitmap stored in `.bss`. A parallel
//! `AtomicU16` array tracks per-page reference counts for COW support.
//! SMP-safe via word-level atomic operations on the bitmap.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use crate::config;

/// Opaque physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(pub usize);

impl PhysAddr {
    pub const fn null() -> Self {
        PhysAddr(0)
    }

    pub fn as_usize(self) -> usize {
        self.0
    }

    pub fn page_index(self) -> usize {
        self.0 / config::PAGE_SIZE
    }
}

// ---------------------------------------------------------------------------
// Bitmap storage
// ---------------------------------------------------------------------------

#[allow(clippy::declare_interior_mutable_const)]
static BITMAP: [AtomicU64; config::BITMAP_WORDS] = {
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; config::BITMAP_WORDS]
};

#[allow(clippy::declare_interior_mutable_const)]
static REFCOUNTS: [AtomicU16; config::MAX_PHYS_PAGES] = {
    const ZERO: AtomicU16 = AtomicU16::new(0);
    [ZERO; config::MAX_PHYS_PAGES]
};

/// First page index in RAM (ram_base / PAGE_SIZE).
static BASE_PAGE: AtomicUsize = AtomicUsize::new(0);
/// Total pages discovered from DTB.
static TOTAL_PAGES: AtomicUsize = AtomicUsize::new(0);
/// Pages currently free.
static FREE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Cached last-allocated word index for O(1) amortized scan.
static ALLOC_HINT: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Pure bitmap operations (host-testable)
// ---------------------------------------------------------------------------

/// Find the first clear bit in a u64, returning its position (0-63).
/// Returns None if all bits are set.
fn first_clear_bit(word: u64) -> Option<u32> {
    if word == u64::MAX {
        return None;
    }
    Some((!word).trailing_zeros())
}

/// Set bit `bit` in a bitmap word atomically. Returns true if it was
/// previously clear (i.e., we successfully claimed it).
fn atomic_set_bit(word: &AtomicU64, bit: u32) -> bool {
    let mask = 1u64 << bit;
    let prev = word.fetch_or(mask, Ordering::AcqRel);
    prev & mask == 0
}

/// Clear bit `bit` in a bitmap word atomically.
fn atomic_clear_bit(word: &AtomicU64, bit: u32) {
    let mask = 1u64 << bit;
    word.fetch_and(!mask, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the page allocator from DTB-discovered RAM layout.
///
/// Marks all pages as free, then reserves pages occupied by the kernel
/// image, DTB, initial page tables, and the bitmap/refcount arrays themselves.
#[cfg(target_os = "none")]
pub fn init(ram_base: usize, ram_size: usize, kernel_end: usize) {
    let page_count = ram_size / config::PAGE_SIZE;
    let base_page = ram_base / config::PAGE_SIZE;

    BASE_PAGE.store(base_page, Ordering::Relaxed);
    TOTAL_PAGES.store(page_count, Ordering::Relaxed);

    // Mark all RAM pages as free (clear bits).
    // Bitmap is zero-initialized in .bss, so this is already done.

    // Reserve pages from ram_base to kernel_end (kernel image + DTB + page tables).
    let reserved_end = kernel_end.div_ceil(config::PAGE_SIZE);

    #[allow(clippy::needless_range_loop)]
    for page in base_page..reserved_end {
        let word = page / 64;
        let bit = (page % 64) as u32;

        BITMAP[word].fetch_or(1u64 << bit, Ordering::Relaxed);
        REFCOUNTS[page].store(1, Ordering::Relaxed);
    }

    let reserved = reserved_end - base_page;

    FREE_COUNT.store(page_count - reserved, Ordering::Relaxed);
    ALLOC_HINT.store(reserved_end / 64, Ordering::Relaxed);
}

/// Allocate a single physical page. Returns None if memory is exhausted.
pub fn alloc_page() -> Option<PhysAddr> {
    let base = BASE_PAGE.load(Ordering::Relaxed);
    let count = TOTAL_PAGES.load(Ordering::Relaxed);

    if count == 0 {
        return None;
    }

    let end_page = base + count;
    let first_word = base / 64;
    let last_word = end_page.div_ceil(64);
    let word_count = last_word - first_word;

    let hint = ALLOC_HINT.load(Ordering::Relaxed);
    let hint = if hint >= first_word && hint < last_word {
        hint - first_word
    } else {
        0
    };

    // Scan from hint within the RAM word range, wrapping around.
    for offset in 0..word_count {
        let word_idx = first_word + (hint + offset) % word_count;
        let word_val = BITMAP[word_idx].load(Ordering::Acquire);

        if let Some(bit) = first_clear_bit(word_val) {
            let page_idx = word_idx * 64 + bit as usize;

            if page_idx < base || page_idx >= end_page {
                continue;
            }

            if atomic_set_bit(&BITMAP[word_idx], bit) {
                REFCOUNTS[page_idx].store(1, Ordering::Relaxed);
                FREE_COUNT.fetch_sub(1, Ordering::Relaxed);
                ALLOC_HINT.store(word_idx, Ordering::Relaxed);

                return Some(PhysAddr(page_idx * config::PAGE_SIZE));
            }
        }
    }

    None
}

/// Spinlock for contiguous allocation (rare operation, prevents livelock
/// when two cores scan overlapping regions simultaneously).
static CONTIGUOUS_LOCK: AtomicBool = AtomicBool::new(false);

fn contiguous_lock() {
    while CONTIGUOUS_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

fn contiguous_unlock() {
    CONTIGUOUS_LOCK.store(false, Ordering::Release);
}

/// Allocate `count` contiguous physical pages. Returns the base address.
/// Protected by a dedicated spinlock to prevent livelock under contention.
/// Single-page alloc_page() remains lock-free.
pub fn alloc_contiguous(count: usize) -> Option<PhysAddr> {
    if count == 0 {
        return None;
    }

    contiguous_lock();
    let result = alloc_contiguous_inner(count);
    contiguous_unlock();
    result
}

fn alloc_contiguous_inner(count: usize) -> Option<PhysAddr> {
    let base = BASE_PAGE.load(Ordering::Relaxed);
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let end_page = base + total;

    // Simple linear scan for a contiguous run of free pages.
    let mut run_start = base;
    let mut run_len = 0;

    for page in base..end_page {
        let word = page / 64;
        let bit = (page % 64) as u32;
        let val = BITMAP[word].load(Ordering::Acquire);

        if val & (1u64 << bit) == 0 {
            if run_len == 0 {
                run_start = page;
            }
            run_len += 1;
            if run_len == count {
                // Claim all pages in the run.
                #[allow(clippy::needless_range_loop)]
                for p in run_start..run_start + count {
                    let w = p / 64;
                    let b = (p % 64) as u32;

                    if !atomic_set_bit(&BITMAP[w], b) {
                        // Race: someone took a page. Release what we claimed.
                        for q in run_start..p {
                            let qw = q / 64;
                            let qb = (q % 64) as u32;

                            atomic_clear_bit(&BITMAP[qw], qb);
                        }
                        // Retry from this point.
                        run_len = 0;
                        break;
                    }
                    REFCOUNTS[p].store(1, Ordering::Relaxed);
                }
                if run_len == count {
                    FREE_COUNT.fetch_sub(count, Ordering::Relaxed);
                    return Some(PhysAddr(run_start * config::PAGE_SIZE));
                }
            }
        } else {
            run_len = 0;
        }
    }

    None
}

/// Increment page reference count (for COW sharing).
pub fn addref(addr: PhysAddr) {
    let page = addr.page_index();

    REFCOUNTS[page].fetch_add(1, Ordering::AcqRel);
}

/// Decrement page reference count. Returns true if the page was freed
/// (refcount reached zero).
pub fn release(addr: PhysAddr) -> bool {
    let page = addr.page_index();
    let prev = REFCOUNTS[page].fetch_sub(1, Ordering::AcqRel);

    if prev == 1 {
        // Refcount went to zero — free the page.
        let word = page / 64;
        let bit = (page % 64) as u32;

        atomic_clear_bit(&BITMAP[word], bit);
        FREE_COUNT.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Query the current reference count of a page.
pub fn refcount(addr: PhysAddr) -> u16 {
    let page = addr.page_index();

    REFCOUNTS[page].load(Ordering::Relaxed)
}

/// Total pages in the system.
pub fn total_pages() -> usize {
    TOTAL_PAGES.load(Ordering::Relaxed)
}

/// Currently free pages.
pub fn free_pages() -> usize {
    FREE_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    // -- Pure bitmap logic --

    #[test]
    fn first_clear_bit_all_zero() {
        assert_eq!(first_clear_bit(0), Some(0));
    }

    #[test]
    fn first_clear_bit_low_set() {
        assert_eq!(first_clear_bit(0b111), Some(3));
    }

    #[test]
    fn first_clear_bit_all_set() {
        assert_eq!(first_clear_bit(u64::MAX), None);
    }

    #[test]
    fn first_clear_bit_alternating() {
        assert_eq!(first_clear_bit(0x5555_5555_5555_5555), Some(1));
    }

    // -- Atomic bit operations --

    #[test]
    fn set_bit_on_zero_word() {
        let word = AtomicU64::new(0);
        assert!(atomic_set_bit(&word, 5));
        assert_eq!(word.load(Ordering::Relaxed), 1 << 5);
    }

    #[test]
    fn set_bit_already_set() {
        let word = AtomicU64::new(1 << 5);
        assert!(!atomic_set_bit(&word, 5));
    }

    #[test]
    fn clear_bit() {
        let word = AtomicU64::new(1 << 5);
        atomic_clear_bit(&word, 5);
        assert_eq!(word.load(Ordering::Relaxed), 0);
    }

    // -- Allocator integration (using static bitmap directly) --
    //
    // These tests manipulate global statics. #[serial] prevents concurrent
    // execution. The serial_test crate is a dev-dependency.

    use serial_test::serial;

    fn setup_allocator(page_count: usize) {
        // Clear bitmap and refcounts for the range.
        for i in 0..((page_count + 63) / 64) {
            BITMAP[i].store(0, Ordering::Relaxed);
        }
        for i in 0..page_count {
            REFCOUNTS[i].store(0, Ordering::Relaxed);
        }
        BASE_PAGE.store(0, Ordering::Relaxed);
        TOTAL_PAGES.store(page_count, Ordering::Relaxed);
        FREE_COUNT.store(page_count, Ordering::Relaxed);
        ALLOC_HINT.store(0, Ordering::Relaxed);
    }

    #[test]
    #[serial]
    fn alloc_returns_page_aligned_address() {
        setup_allocator(64);
        let addr = alloc_page().unwrap();
        assert_eq!(addr.as_usize() % config::PAGE_SIZE, 0);
    }

    #[test]
    #[serial]
    fn alloc_sets_refcount_to_one() {
        setup_allocator(64);
        let addr = alloc_page().unwrap();
        assert_eq!(refcount(addr), 1);
    }

    #[test]
    #[serial]
    fn alloc_then_release_frees_page() {
        setup_allocator(64);
        let free_before = free_pages();
        let addr = alloc_page().unwrap();
        assert_eq!(free_pages(), free_before - 1);
        assert!(release(addr));
        assert_eq!(free_pages(), free_before);
    }

    #[test]
    #[serial]
    fn alloc_exhaustion_returns_none() {
        setup_allocator(2);
        let _a = alloc_page().unwrap();
        let _b = alloc_page().unwrap();
        assert!(alloc_page().is_none());
    }

    #[test]
    #[serial]
    fn addref_increments_refcount() {
        setup_allocator(64);
        let addr = alloc_page().unwrap();
        assert_eq!(refcount(addr), 1);
        addref(addr);
        assert_eq!(refcount(addr), 2);
        addref(addr);
        assert_eq!(refcount(addr), 3);
    }

    #[test]
    #[serial]
    fn release_with_refcount_gt_1_does_not_free() {
        setup_allocator(64);
        let free_before = free_pages();
        let addr = alloc_page().unwrap();
        addref(addr);
        assert!(!release(addr)); // refcount 2 -> 1, not freed
        assert_eq!(free_pages(), free_before - 1);
        assert!(release(addr)); // refcount 1 -> 0, freed
        assert_eq!(free_pages(), free_before);
    }

    #[test]
    #[serial]
    fn alloc_contiguous_basic() {
        setup_allocator(64);
        let base = alloc_contiguous(4).unwrap();
        // All 4 pages should be consecutive.
        for i in 0..4 {
            let page = base.page_index() + i;
            assert_eq!(refcount(PhysAddr(page * config::PAGE_SIZE)), 1);
        }
    }

    #[test]
    #[serial]
    fn alloc_free_realloc_no_leak() {
        setup_allocator(16);
        let initial_free = free_pages();
        let mut pages = Vec::new();
        for _ in 0..16 {
            pages.push(alloc_page().unwrap());
        }
        assert_eq!(free_pages(), 0);
        for p in &pages {
            release(*p);
        }
        assert_eq!(free_pages(), initial_free);
        // Reallocate — should succeed.
        for _ in 0..16 {
            assert!(alloc_page().is_some());
        }
    }
}
