//! Stress tests for allocator fragmentation resilience.
//!
//! Exercises the 3-tier allocation system (slab, linked-list, buddy) under
//! fragmentation pressure: many different sizes, random free order (seeded
//! RNG), slab exhaustion/growth, and mixed allocator operations.
//!
//! Fulfills: VAL-STRESS-001, VAL-STRESS-002, VAL-STRESS-004
//!
//! NOTE: The heap (linked-list) and slab+buddy allocators use global statics.
//! Tests that include the same kernel module share that state. To avoid
//! cross-test interference, we split into:
//!   - Linked-list-only tests (heap.rs included, slab stubbed out) — multiple
//!     test functions OK since we re-init the heap for each.
//!   - Slab+buddy tests (slab.rs + page_allocator.rs included) — ONE test
//!     function because slab state persists (slab pages never freed).
//!   - Mixed tests through GlobalAlloc — ONE test function (same reason).
//!
//! Run with: cargo test --test stress_alloc_fragmentation -- --test-threads=1

use core::alloc::{GlobalAlloc, Layout};

// ============================================================
// Stubs — reproduce just enough of the kernel environment for
// heap.rs, slab.rs, and page_allocator.rs to compile on the host.
// ============================================================

mod paging {
    #[allow(dead_code)]
    pub const PAGE_SIZE: u64 = 16384;
    pub const RAM_SIZE_MAX: u64 = 256 * 1024 * 1024;

    pub fn ram_end() -> u64 {
        // Stub: not used by buddy allocator tests (validation is #[cfg(not(test))]).
        0
    }

    pub const fn align_up(addr: usize, align: usize) -> usize {
        (addr + align - 1) & !(align - 1)
    }
}

mod serial {
    #[allow(dead_code)]
    pub fn panic_puts(_: &str) {}
    #[allow(dead_code)]
    pub fn panic_put_hex(_: u64) {}
}

mod memory {
    #[allow(dead_code)]
    pub const HEAP_SIZE: usize = 4096;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[repr(transparent)]
    pub struct Pa(pub usize);

    #[allow(dead_code)]
    impl Pa {
        pub const fn as_u64(self) -> u64 {
            self.0 as u64
        }
    }

    /// Identity mapping — on the host, VA == PA.
    pub fn phys_to_virt(pa: Pa) -> usize {
        pa.0
    }
    pub fn virt_to_phys(va: usize) -> Pa {
        Pa(va)
    }
}

mod sync {
    use core::{
        cell::UnsafeCell,
        ops::{Deref, DerefMut},
    };

    pub struct IrqMutex<T> {
        data: UnsafeCell<T>,
    }

    // SAFETY: Single-threaded test environment (--test-threads=1).
    // No Send bound — SlabCache contains raw pointers (*mut FreeNode)
    // which are !Send, but the single-threaded test environment is safe.
    unsafe impl<T> Sync for IrqMutex<T> {}

    impl<T> IrqMutex<T> {
        pub const fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(data),
            }
        }
        pub fn lock(&self) -> IrqGuard<'_, T> {
            IrqGuard {
                data: unsafe { &mut *self.data.get() },
            }
        }
    }

    pub struct IrqGuard<'a, T> {
        data: &'a mut T,
    }

    impl<T> Deref for IrqGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &T {
            self.data
        }
    }
    impl<T> DerefMut for IrqGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            self.data
        }
    }
}

// Include all three allocator modules.
#[path = "../../kernel/heap.rs"]
mod heap;
#[path = "../../kernel/page_allocator.rs"]
mod page_allocator;
#[path = "../../kernel/slab.rs"]
mod slab;

const PAGE_SIZE: usize = 16384;
const MIN_BLOCK: usize = 16; // size_of::<FreeBlock>() on 64-bit

// ============================================================
// Seeded PRNG (xorshift64) — deterministic, no external deps.
// ============================================================

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Returns a value in [0, max) — biased, but fine for test shuffling.
    fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u64() % max as u64) as usize
    }

    /// Fisher-Yates shuffle.
    fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.next_usize(i + 1);
            slice.swap(i, j);
        }
    }
}

// ============================================================
// Region management — allocate page-aligned memory from the host.
// ============================================================

fn alloc_region(pages: usize) -> (*mut u8, std::alloc::Layout) {
    let size = pages * PAGE_SIZE;
    let layout = std::alloc::Layout::from_size_align(size, PAGE_SIZE).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "host allocation failed");
    (ptr, layout)
}

/// Initialize the heap with a host-allocated region.
fn init_heap(region: *mut u8, size: usize) {
    unsafe {
        let block_header = region as *mut usize;
        *block_header = size; // FreeBlock::size
        *(block_header.add(1)) = 0; // FreeBlock::next = null

        heap::ALLOCATOR.head.get().write(region as *mut _);
        heap::ALLOCATOR.region_start.get().write(region as usize);
        heap::ALLOCATOR
            .region_end
            .get()
            .write(region as usize + size);
    }
}

// ============================================================
// VAL-STRESS-001: Fragmentation resilience (linked-list only)
//
// Allocate many different sizes, free in random order (seeded
// RNG), verify allocator can still service requests.
//
// NOTE: slab::try_alloc will return null since the buddy
// allocator hasn't been initialized for the slab tests yet.
// All allocations go through the linked-list path.
// ============================================================

#[test]
fn stress_alloc_fragmentation_linked_list() {
    let heap_pages = 64; // 256 KiB
    let heap_size = heap_pages * PAGE_SIZE;
    let (ptr, layout) = alloc_region(heap_pages);
    init_heap(ptr, heap_size);

    let alloc = &heap::ALLOCATOR;
    let mut rng = Rng::new(42);

    // --- Section 1: Many sizes, random free order ---

    let sizes: Vec<usize> = vec![
        16, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3000, 4000, 4096, 5000,
        6000, 8000, 8192,
    ];
    let mut allocations: Vec<(*mut u8, Layout)> = Vec::new();

    for round in 0..10 {
        for _ in 0..50 {
            let size_idx = rng.next_usize(sizes.len());
            let size = sizes[size_idx];
            let align = if size >= 16 { 16 } else { 8 };
            let l = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { alloc.alloc(l) };
            if !p.is_null() {
                assert_eq!(
                    p as usize % align,
                    0,
                    "round {}: alloc size {} misaligned",
                    round,
                    size
                );
                unsafe { core::ptr::write_bytes(p, 0xAA, size) };
                allocations.push((p, l));
            }
        }

        // Free a random 30-70% subset.
        let free_count = allocations.len() / 3 + rng.next_usize(allocations.len() / 3 + 1);
        rng.shuffle(&mut allocations);
        for _ in 0..free_count.min(allocations.len()) {
            if let Some((p, l)) = allocations.pop() {
                let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
                assert!(
                    slice.iter().all(|&b| b == 0xAA),
                    "corruption before free in round {}",
                    round
                );
                unsafe { alloc.dealloc(p, l) };
            }
        }
    }

    // Free remaining in random order.
    rng.shuffle(&mut allocations);
    for (p, l) in allocations.drain(..) {
        let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
        assert!(
            slice.iter().all(|&b| b == 0xAA),
            "corruption in remaining allocations"
        );
        unsafe { alloc.dealloc(p, l) };
    }

    // Coalescing check.
    let big_layout = Layout::from_size_align(heap_size - MIN_BLOCK * 4, MIN_BLOCK).unwrap();
    let big = unsafe { alloc.alloc(big_layout) };
    assert!(
        !big.is_null(),
        "heap should coalesce after fragmentation stress"
    );
    unsafe { alloc.dealloc(big, big_layout) };

    // --- Section 2: Different seed, same pattern ---

    let mut rng2 = Rng::new(0xDEADBEEF);
    let mut allocs2: Vec<(*mut u8, Layout)> = Vec::new();

    for _ in 0..200 {
        let size = 16 + rng2.next_usize(8000);
        let l = Layout::from_size_align(size, 16).unwrap();
        let p = unsafe { alloc.alloc(l) };
        if !p.is_null() {
            unsafe { core::ptr::write_bytes(p, 0xBB, size) };
            allocs2.push((p, l));
        }
    }

    // Free every other one.
    let mut to_free: Vec<usize> = (0..allocs2.len()).step_by(2).collect();
    rng2.shuffle(&mut to_free);
    to_free.sort_unstable_by(|a, b| b.cmp(a));
    for idx in to_free {
        let (p, l) = allocs2.swap_remove(idx);
        unsafe { alloc.dealloc(p, l) };
    }

    // Fill holes.
    let mut hole_fills = 0;
    for _ in 0..100 {
        let size = 16 + rng2.next_usize(512);
        let l = Layout::from_size_align(size, 16).unwrap();
        let p = unsafe { alloc.alloc(l) };
        if !p.is_null() {
            unsafe { core::ptr::write_bytes(p, 0xCC, size) };
            allocs2.push((p, l));
            hole_fills += 1;
        }
    }
    assert!(hole_fills > 0, "should fill fragmentation holes");

    for (p, l) in allocs2 {
        unsafe { alloc.dealloc(p, l) };
    }

    let big2 = unsafe { alloc.alloc(big_layout) };
    assert!(!big2.is_null(), "second coalescing check failed");
    unsafe { alloc.dealloc(big2, big_layout) };

    // --- Section 3: Worst-case fragmentation ---

    let small = Layout::from_size_align(16, 16).unwrap();
    let large = Layout::from_size_align(256, 16).unwrap();
    let mut smalls = Vec::new();
    let mut larges = Vec::new();

    for _ in 0..100 {
        let ps = unsafe { alloc.alloc(small) };
        if ps.is_null() {
            break;
        }
        smalls.push(ps);
        let pl = unsafe { alloc.alloc(large) };
        if pl.is_null() {
            break;
        }
        larges.push(pl);
    }
    assert!(!smalls.is_empty() && !larges.is_empty());

    // Free all smalls → many 16-byte holes between 256-byte blocks.
    for p in &smalls {
        unsafe { alloc.dealloc(*p, small) };
    }

    let medium = Layout::from_size_align(32, 16).unwrap();
    let mut mediums = Vec::new();
    for _ in 0..50 {
        let p = unsafe { alloc.alloc(medium) };
        if p.is_null() {
            break;
        }
        mediums.push(p);
    }

    for p in mediums {
        unsafe { alloc.dealloc(p, medium) };
    }
    for p in larges {
        unsafe { alloc.dealloc(p, large) };
    }

    let big3 = unsafe { alloc.alloc(big_layout) };
    assert!(!big3.is_null(), "worst-case coalescing check failed");
    unsafe { alloc.dealloc(big3, big_layout) };

    // --- Section 4: High-volume random alloc/free (5000 ops) ---

    let mut rng3 = Rng::new(0x12345678);
    let mut live: Vec<(*mut u8, Layout)> = Vec::new();

    for _ in 0..5000 {
        if rng3.next_usize(10) < 6 || live.is_empty() {
            let size = 16 + rng3.next_usize(4096);
            let l = Layout::from_size_align(size, 16).unwrap();
            let p = unsafe { alloc.alloc(l) };
            if !p.is_null() {
                unsafe { core::ptr::write_bytes(p, 0xEE, size) };
                live.push((p, l));
            }
        } else {
            let idx = rng3.next_usize(live.len());
            let (p, l) = live.swap_remove(idx);
            let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
            assert!(
                slice.iter().all(|&b| b == 0xEE),
                "corruption in high-volume random test"
            );
            unsafe { alloc.dealloc(p, l) };
        }
    }

    for (p, l) in live {
        unsafe { alloc.dealloc(p, l) };
    }

    let big4 = unsafe { alloc.alloc(big_layout) };
    assert!(!big4.is_null(), "high-volume coalescing check failed");
    unsafe { alloc.dealloc(big4, big_layout) };

    // --- Section 5: Slab-class sizes through linked-list (no buddy) ---

    let mut rng4 = Rng::new(0xFEEDFACE);
    let mut live2: Vec<(*mut u8, Layout)> = Vec::new();
    let slab_sizes = [16, 32, 64, 128, 256, 512, 1024, 2048];

    for _ in 0..2000 {
        if rng4.next_usize(10) < 7 || live2.is_empty() {
            let size = slab_sizes[rng4.next_usize(slab_sizes.len())];
            let l = Layout::from_size_align(size, 16).unwrap();
            let p = unsafe { alloc.alloc(l) };
            if !p.is_null() {
                unsafe { core::ptr::write_bytes(p, 0xFF, size) };
                live2.push((p, l));
            }
        } else {
            let idx = rng4.next_usize(live2.len());
            let (p, l) = live2.swap_remove(idx);
            unsafe { alloc.dealloc(p, l) };
        }
    }

    for (p, l) in live2 {
        unsafe { alloc.dealloc(p, l) };
    }

    let big5 = unsafe { alloc.alloc(big_layout) };
    assert!(!big5.is_null(), "slab-fallthrough coalescing check failed");
    unsafe { alloc.dealloc(big5, big_layout) };

    // Cleanup host region.
    unsafe { std::alloc::dealloc(ptr, layout) };
}

// ============================================================
// VAL-STRESS-002: Slab exhaustion and growth
// VAL-STRESS-004: Mixed allocator operations
//
// Combined into one test because slab and buddy share global
// state that persists across test functions.
// ============================================================

#[test]
#[cfg_attr(miri, ignore)] // Miri strict provenance rejects integer-to-pointer casts in phys_to_virt
fn stress_alloc_slab_and_mixed() {
    // --- Part A: Slab exhaustion and growth (VAL-STRESS-002) ---

    // Allocate a region for the buddy allocator to back slab pages.
    let buddy_pages = 512; // 2 MiB
    let (buddy_ptr, buddy_layout) = alloc_region(buddy_pages);
    page_allocator::init(
        buddy_ptr as usize,
        buddy_ptr as usize + buddy_pages * PAGE_SIZE,
    );

    let initial_free = page_allocator::free_count();
    assert_eq!(initial_free, buddy_pages, "buddy should start fully free");

    // Size classes: 64, 128, 256, 512, 1024, 2048 bytes.
    // Objects per 4 KiB page: 64, 32, 16, 8, 4, 2.
    let size_classes: [(usize, usize); 6] = [
        (64, PAGE_SIZE / 64),
        (128, PAGE_SIZE / 128),
        (256, PAGE_SIZE / 256),
        (512, PAGE_SIZE / 512),
        (1024, PAGE_SIZE / 1024),
        (2048, PAGE_SIZE / 2048),
    ];

    let mut all_ptrs: Vec<Vec<*mut u8>> = Vec::new();
    let mut total_slab_pages = 0usize;

    for &(obj_size, objs_per_page) in &size_classes {
        let mut ptrs = Vec::new();
        let target_objects = objs_per_page * 5; // Fill 5 pages per class.

        for _ in 0..target_objects {
            let p = slab::try_alloc(obj_size, 8);
            if p.is_null() {
                break;
            }
            assert_eq!(
                p as usize % obj_size,
                0,
                "slab class {} misaligned",
                obj_size
            );
            // Write pattern — only to bytes after the FreeNode header area,
            // because slab::free() will overwrite the first 8 bytes.
            // Actually, write the full object — we check BEFORE freeing.
            unsafe { core::ptr::write_bytes(p, 0xDD, obj_size) };
            ptrs.push(p);
        }

        assert!(
            ptrs.len() >= objs_per_page,
            "should fill at least one slab page for class {}",
            obj_size
        );

        let pages = (ptrs.len() + objs_per_page - 1) / objs_per_page;
        total_slab_pages += pages;
        all_ptrs.push(ptrs);
    }

    // Verify buddy pages were consumed (slab growth).
    let free_after = page_allocator::free_count();
    assert!(
        free_after < initial_free,
        "slab should consume buddy pages: initial={}, after={}",
        initial_free,
        free_after
    );
    let consumed = initial_free - free_after;
    assert!(
        consumed >= total_slab_pages,
        "consumed ({}) >= slab pages needed ({})",
        consumed,
        total_slab_pages
    );

    // Verify patterns intact.
    for (class_idx, ptrs) in all_ptrs.iter().enumerate() {
        let obj_size = size_classes[class_idx].0;
        for &p in ptrs {
            let slice = unsafe { core::slice::from_raw_parts(p, obj_size) };
            assert!(
                slice.iter().all(|&b| b == 0xDD),
                "corruption in slab class {} at {:p}",
                obj_size,
                p
            );
        }
    }

    // Free all slab objects.
    for (class_idx, ptrs) in all_ptrs.iter().enumerate() {
        let obj_size = size_classes[class_idx].0;
        for &p in ptrs {
            let freed = unsafe { slab::try_free(p, obj_size, 8) };
            assert!(freed, "slab should accept free for class {}", obj_size);
        }
    }

    // Verify clean state: re-alloc should reuse freed objects, not consume
    // new buddy pages.
    for &(obj_size, objs_per_page) in &size_classes {
        let free_before = page_allocator::free_count();
        let mut realloc_ptrs = Vec::new();
        for _ in 0..objs_per_page {
            let p = slab::try_alloc(obj_size, 8);
            if p.is_null() {
                break;
            }
            realloc_ptrs.push(p);
        }
        let free_after_realloc = page_allocator::free_count();
        assert_eq!(
            free_before, free_after_realloc,
            "re-alloc should not consume new buddy pages (class {})",
            obj_size
        );
        assert_eq!(realloc_ptrs.len(), objs_per_page, "class {}", obj_size);

        // Uniqueness check.
        let mut sorted: Vec<usize> = realloc_ptrs.iter().map(|&p| p as usize).collect();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), objs_per_page, "pointers must be unique");

        for &p in &realloc_ptrs {
            unsafe { slab::try_free(p, obj_size, 8) };
        }
    }

    // --- Part B: Slab exhaustion boundary ---

    // Allocate all remaining buddy pages through slab until exhaustion.
    let mut exhaust_ptrs = Vec::new();
    let obj_size_exhaust = 64;
    loop {
        let p = slab::try_alloc(obj_size_exhaust, 8);
        if p.is_null() {
            break;
        }
        exhaust_ptrs.push(p);
    }

    // We can't assert an exact count because the slab already has free objects
    // from Part A. But buddy should be fully consumed.
    assert_eq!(
        page_allocator::free_count(),
        0,
        "buddy should be exhausted after slab fills all pages"
    );
    assert!(
        exhaust_ptrs.len() > 0,
        "should have allocated at least some objects"
    );

    // Free all back.
    for &p in &exhaust_ptrs {
        unsafe { slab::try_free(p, obj_size_exhaust, 8) };
    }

    // Re-alloc should not need new buddy pages (slab pages persist).
    let free_before_realloc = page_allocator::free_count();
    for _ in 0..exhaust_ptrs.len() {
        let p = slab::try_alloc(obj_size_exhaust, 8);
        assert!(!p.is_null(), "re-alloc from freed slab should succeed");
        unsafe { slab::try_free(p, obj_size_exhaust, 8) };
    }
    assert_eq!(
        page_allocator::free_count(),
        free_before_realloc,
        "re-alloc should not consume new buddy pages"
    );

    // --- Part C: Mixed allocator operations (VAL-STRESS-004) ---

    // Set up the heap (linked-list) in a separate region.
    let heap_pages = 32; // 128 KiB
    let heap_size = heap_pages * PAGE_SIZE;
    let (heap_ptr, heap_layout) = alloc_region(heap_pages);
    init_heap(heap_ptr, heap_size);

    // Free all slab objects from the exhaust test so buddy has pages
    // available for new slab growth in the mixed test. Actually, slab
    // pages are never freed back to buddy — they persist. But the slab
    // free lists have objects from the freed exhaust_ptrs, so new slab
    // allocs will reuse those.

    let alloc = &heap::ALLOCATOR;
    let mut rng = Rng::new(0xCAFEBABE);

    #[derive(Debug)]
    enum AllocSource {
        Slab,
        Heap,
        Buddy,
    }

    struct Allocation {
        ptr: *mut u8,
        size: usize,
        source: AllocSource,
        pattern: u8,
    }

    let mut live: Vec<Allocation> = Vec::new();

    for op in 0..1000 {
        let action = rng.next_usize(10);

        if action < 6 || live.is_empty() {
            let source_choice = rng.next_usize(3);
            let pattern = (op & 0xFF) as u8;

            match source_choice {
                0 => {
                    // Slab allocation.
                    let class = [64, 128, 256, 512, 1024, 2048][rng.next_usize(6)];
                    let p = slab::try_alloc(class, 8);
                    if !p.is_null() {
                        unsafe { core::ptr::write_bytes(p, pattern, class) };
                        live.push(Allocation {
                            ptr: p,
                            size: class,
                            source: AllocSource::Slab,
                            pattern,
                        });
                    }
                }
                1 => {
                    // Linked-list allocation (large sizes only).
                    let size = 3000 + rng.next_usize(5000);
                    let l = Layout::from_size_align(size, 16).unwrap();
                    let p = unsafe { alloc.alloc(l) };
                    if !p.is_null() {
                        let addr = p as usize;
                        let heap_start = heap_ptr as usize;
                        assert!(
                            addr >= heap_start && addr < heap_start + heap_size,
                            "large alloc should be in heap region"
                        );
                        unsafe { core::ptr::write_bytes(p, pattern, size) };
                        live.push(Allocation {
                            ptr: p,
                            size,
                            source: AllocSource::Heap,
                            pattern,
                        });
                    }
                }
                _ => {
                    // Buddy allocation (whole pages).
                    if let Some(pa) = page_allocator::alloc_frame() {
                        let p = pa.0 as *mut u8;
                        unsafe { core::ptr::write_bytes(p, pattern, PAGE_SIZE) };
                        live.push(Allocation {
                            ptr: p,
                            size: PAGE_SIZE,
                            source: AllocSource::Buddy,
                            pattern,
                        });
                    }
                }
            }
        } else {
            // Free a random allocation.
            let idx = rng.next_usize(live.len());
            let a = live.swap_remove(idx);

            let slice = unsafe { core::slice::from_raw_parts(a.ptr, a.size) };
            assert!(
                slice.iter().all(|&b| b == a.pattern),
                "op {}: corruption in {:?} at {:p} (size {})",
                op,
                a.source,
                a.ptr,
                a.size
            );

            match a.source {
                AllocSource::Slab => {
                    let freed = unsafe { slab::try_free(a.ptr, a.size, 8) };
                    assert!(freed, "slab should accept its own allocation");
                }
                AllocSource::Heap => {
                    let l = Layout::from_size_align(a.size, 16).unwrap();
                    unsafe { alloc.dealloc(a.ptr, l) };
                }
                AllocSource::Buddy => {
                    page_allocator::free_frame(memory::Pa(a.ptr as usize));
                }
            }
        }
    }

    // Verify all remaining.
    for a in &live {
        let slice = unsafe { core::slice::from_raw_parts(a.ptr, a.size) };
        assert!(
            slice.iter().all(|&b| b == a.pattern),
            "final check: corruption in {:?} at {:p}",
            a.source,
            a.ptr
        );
    }

    // Free all remaining.
    for a in live {
        match a.source {
            AllocSource::Slab => {
                unsafe { slab::try_free(a.ptr, a.size, 8) };
            }
            AllocSource::Heap => {
                let l = Layout::from_size_align(a.size, 16).unwrap();
                unsafe { alloc.dealloc(a.ptr, l) };
            }
            AllocSource::Buddy => {
                page_allocator::free_frame(memory::Pa(a.ptr as usize));
            }
        }
    }

    // Verify heap coalesced.
    let big_layout = Layout::from_size_align(heap_size - MIN_BLOCK * 4, MIN_BLOCK).unwrap();
    let big = unsafe { alloc.alloc(big_layout) };
    assert!(!big.is_null(), "heap should coalesce after mixed stress");
    unsafe { alloc.dealloc(big, big_layout) };

    // --- Part D: Mixed through GlobalAlloc (slab+linked-list routing) ---

    let mut rng2 = Rng::new(0xC0FFEE);
    let mut live2: Vec<(*mut u8, Layout, u8)> = Vec::new();

    for op in 0..2000 {
        if rng2.next_usize(10) < 6 || live2.is_empty() {
            let size = if rng2.next_usize(2) == 0 {
                [64, 128, 256, 512, 1024, 2048][rng2.next_usize(6)]
            } else {
                3000 + rng2.next_usize(5000)
            };
            let l = Layout::from_size_align(size, 16).unwrap();
            let pattern = (op & 0xFF) as u8;
            let p = unsafe { alloc.alloc(l) };
            if !p.is_null() {
                unsafe { core::ptr::write_bytes(p, pattern, size) };
                live2.push((p, l, pattern));
            }
        } else {
            let idx = rng2.next_usize(live2.len());
            let (p, l, pattern) = live2.swap_remove(idx);
            let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
            assert!(
                slice.iter().all(|&b| b == pattern),
                "GlobalAlloc mixed routing corruption at op {}",
                op
            );
            unsafe { alloc.dealloc(p, l) };
        }
    }

    for (p, l, pattern) in &live2 {
        let slice = unsafe { core::slice::from_raw_parts(*p, l.size()) };
        assert!(
            slice.iter().all(|&b| b == *pattern),
            "final GlobalAlloc check corruption"
        );
    }
    for (p, l, _) in live2 {
        unsafe { alloc.dealloc(p, l) };
    }

    let big2 = unsafe { alloc.alloc(big_layout) };
    assert!(!big2.is_null(), "GlobalAlloc mixed coalescing check failed");
    unsafe { alloc.dealloc(big2, big_layout) };

    // Cleanup host regions.
    unsafe {
        std::alloc::dealloc(heap_ptr, heap_layout);
        std::alloc::dealloc(buddy_ptr, buddy_layout);
    }
}
