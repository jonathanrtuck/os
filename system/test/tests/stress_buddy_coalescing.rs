//! Stress tests for buddy allocator coalescing.
//!
//! Exercises all merge paths: allocate many blocks at various orders, free in
//! patterns that trigger buddy coalescing at every order level. Verify free
//! counts return to baseline after full free.
//!
//! NOTE: page_allocator uses a global static. Since init() ADDS to existing
//! state, all buddy tests must live in ONE test function (or carefully drain
//! + reinit). We use a single test with clearly labeled sections.
//!
//! Fulfills: VAL-STRESS-003 (buddy allocator coalescing)
//!
//! Run with: cargo test --test stress_buddy_coalescing -- --test-threads=1

// ============================================================
// Stubs — reproduce just enough of the kernel environment for
// page_allocator.rs to compile on the host.
// ============================================================

mod paging {
    #[allow(dead_code)]
    pub const PAGE_SIZE: u64 = 4096;

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

#[path = "../../kernel/page_allocator.rs"]
mod page_allocator;

const PAGE_SIZE: usize = 4096;

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

    fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u64() % max as u64) as usize
    }

    fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.next_usize(i + 1);
            slice.swap(i, j);
        }
    }
}

// ============================================================
// Helper: allocate page-aligned memory from the host.
// ============================================================

fn alloc_region(pages: usize) -> (*mut u8, std::alloc::Layout) {
    let size = pages * PAGE_SIZE;
    let layout = std::alloc::Layout::from_size_align(size, PAGE_SIZE).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "host allocation failed");
    (ptr, layout)
}

// ============================================================
// Single test exercising all buddy coalescing paths.
//
// The buddy allocator uses a global static, so all tests must
// share state. We initialize once with a large region and
// exercise all paths in sequence, verifying free_count returns
// to baseline after each section.
// ============================================================

#[test]
#[cfg_attr(miri, ignore)] // Miri strict provenance rejects integer-to-pointer casts in phys_to_virt
fn stress_buddy_coalescing_all_merge_paths() {
    // Use 4096 pages (16 MiB) — enough for order-11 blocks.
    let buddy_pages = 4096;
    let (ptr, layout) = alloc_region(buddy_pages);
    page_allocator::init(ptr as usize, ptr as usize + buddy_pages * PAGE_SIZE);

    let initial_free = page_allocator::free_count();
    assert_eq!(initial_free, buddy_pages, "buddy should start fully free");

    // ============================================================
    // Section 1: Sequential order-0 free (ascending PA)
    //
    // Allocate all pages at order 0, free in ascending PA order.
    // Adjacent buddies should coalesce up the tree.
    // ============================================================

    {
        let mut pages: Vec<memory::Pa> = Vec::new();
        for _ in 0..buddy_pages {
            pages.push(page_allocator::alloc_frame().expect("should allocate"));
        }
        assert_eq!(page_allocator::free_count(), 0, "s1: all pages allocated");

        // Sort by PA for ascending order.
        pages.sort_by_key(|pa| pa.0);

        for pa in &pages {
            page_allocator::free_frame(*pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s1: sequential free should return to baseline"
        );

        // Verify full coalescing: order-11 (2048 pages) should work.
        let big = page_allocator::alloc_frames(11);
        assert!(big.is_some(), "s1: should yield order-11 block");
        page_allocator::free_frames(big.unwrap(), 11);
    }

    // ============================================================
    // Section 2: Reverse-order free (descending PA)
    //
    // Free pages from the highest PA downward — exercises the
    // opposite coalescing direction.
    // ============================================================

    {
        let mut pages: Vec<memory::Pa> = Vec::new();
        for _ in 0..buddy_pages {
            pages.push(page_allocator::alloc_frame().expect("should allocate"));
        }
        assert_eq!(page_allocator::free_count(), 0, "s2: all allocated");

        pages.sort_by_key(|pa| std::cmp::Reverse(pa.0));

        for pa in &pages {
            page_allocator::free_frame(*pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s2: reverse-order free should return to baseline"
        );

        let big = page_allocator::alloc_frames(11);
        assert!(
            big.is_some(),
            "s2: should yield order-11 block after reverse free"
        );
        page_allocator::free_frames(big.unwrap(), 11);
    }

    // ============================================================
    // Section 3: Random-order free (seeded RNG)
    //
    // Buddy coalescing must work regardless of free order.
    // ============================================================

    {
        let mut rng = Rng::new(0xBEEF_CAFE);
        let mut pages: Vec<memory::Pa> = Vec::new();
        for _ in 0..buddy_pages {
            pages.push(page_allocator::alloc_frame().expect("should allocate"));
        }
        assert_eq!(page_allocator::free_count(), 0, "s3: all allocated");

        rng.shuffle(&mut pages);

        for pa in &pages {
            page_allocator::free_frame(*pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s3: random-order free should fully coalesce"
        );

        let big = page_allocator::alloc_frames(11);
        assert!(
            big.is_some(),
            "s3: should yield order-11 block after random free"
        );
        page_allocator::free_frames(big.unwrap(), 11);
    }

    // ============================================================
    // Section 4: Multi-order alloc then free (orders 0-10)
    //
    // Allocate one block at each order, free all. Verify
    // coalescing handles mixed-size blocks.
    // ============================================================

    {
        let mut blocks: Vec<(memory::Pa, usize)> = Vec::new();
        let mut consumed = 0;

        for order in 0..=10 {
            let pa = page_allocator::alloc_frames(order)
                .unwrap_or_else(|| panic!("s4: should allocate order {}", order));
            consumed += 1usize << order;
            blocks.push((pa, order));
        }

        // Total: 1+2+4+8+16+32+64+128+256+512+1024 = 2047 pages.
        assert_eq!(consumed, 2047);
        assert_eq!(
            page_allocator::free_count(),
            initial_free - consumed,
            "s4: multi-order alloc consumed correct pages"
        );

        for (pa, order) in &blocks {
            page_allocator::free_frames(*pa, *order);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s4: multi-order free should return to baseline"
        );
    }

    // ============================================================
    // Section 5: Multi-order reverse-order free
    // ============================================================

    {
        let mut blocks: Vec<(memory::Pa, usize)> = Vec::new();
        for order in 0..=10 {
            let pa = page_allocator::alloc_frames(order).expect("s5: alloc");
            blocks.push((pa, order));
        }

        for (pa, order) in blocks.iter().rev() {
            page_allocator::free_frames(*pa, *order);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s5: reverse-order multi-order free should return to baseline"
        );
    }

    // ============================================================
    // Section 6: Split-and-coalesce cycle
    //
    // Allocating one order-0 page forces the allocator to split
    // a high-order block all the way down, creating buddies at
    // every level. Freeing it should coalesce back up.
    // ============================================================

    {
        let pa0 = page_allocator::alloc_frame().expect("s6: alloc one page");
        page_allocator::free_frame(pa0);

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s6: single alloc+free should coalesce back to baseline"
        );

        let big = page_allocator::alloc_frames(11);
        assert!(
            big.is_some(),
            "s6: should yield order-11 after full coalesce"
        );
        page_allocator::free_frames(big.unwrap(), 11);
    }

    // ============================================================
    // Section 7: Coalescing at each specific order level
    //
    // For each order 0-10: allocate two blocks, free both.
    // They should coalesce into one block at order+1.
    // ============================================================

    {
        for order in 0..=10 {
            let pa1 = page_allocator::alloc_frames(order)
                .unwrap_or_else(|| panic!("s7: first alloc order {}", order));
            let pa2 = page_allocator::alloc_frames(order)
                .unwrap_or_else(|| panic!("s7: second alloc order {}", order));

            let consumed = (1usize << order) * 2;
            assert_eq!(
                page_allocator::free_count(),
                initial_free - consumed,
                "s7: order {} should consume {} pages",
                order,
                consumed
            );

            page_allocator::free_frames(pa1, order);
            page_allocator::free_frames(pa2, order);

            assert_eq!(
                page_allocator::free_count(),
                initial_free,
                "s7: order {} free should return to baseline",
                order
            );

            // Verify coalescing by allocating at one order higher.
            let combined = page_allocator::alloc_frames(order + 1);
            assert!(
                combined.is_some(),
                "s7: coalesced order-{} blocks should yield order-{}",
                order,
                order + 1
            );
            page_allocator::free_frames(combined.unwrap(), order + 1);
        }
    }

    // ============================================================
    // Section 8: Alternating buddy free pattern
    //
    // Allocate 4 blocks at each order, free blocks 0 and 2
    // (one from each buddy pair), then free 1 and 3 (the other).
    // Exercises deferred coalescing.
    // ============================================================

    {
        for order in 0..=8 {
            let mut blocks: Vec<memory::Pa> = Vec::new();
            for _ in 0..4 {
                blocks.push(
                    page_allocator::alloc_frames(order)
                        .unwrap_or_else(|| panic!("s8: alloc order {}", order)),
                );
            }

            blocks.sort_by_key(|pa| pa.0);

            // Free blocks 0 and 2 first (one from each buddy pair).
            page_allocator::free_frames(blocks[0], order);
            page_allocator::free_frames(blocks[2], order);

            // Free blocks 1 and 3 — should coalesce with their buddies.
            page_allocator::free_frames(blocks[1], order);
            page_allocator::free_frames(blocks[3], order);

            assert_eq!(
                page_allocator::free_count(),
                initial_free,
                "s8: order {} alternating buddy free should return to baseline",
                order
            );
        }
    }

    // ============================================================
    // Section 9: Checkerboard pattern
    //
    // Allocate all pages, free every other one. Large allocs
    // should fail (fragmented). Then free the rest — full
    // coalescing should restore large-block capability.
    // ============================================================

    {
        let mut all_pages: Vec<memory::Pa> = Vec::new();
        for _ in 0..buddy_pages {
            all_pages.push(page_allocator::alloc_frame().expect("s9: alloc"));
        }
        assert_eq!(page_allocator::free_count(), 0, "s9: all allocated");

        all_pages.sort_by_key(|pa| pa.0);

        let mut even_pages = Vec::new();
        let mut odd_pages = Vec::new();
        for (i, pa) in all_pages.into_iter().enumerate() {
            if i % 2 == 0 {
                even_pages.push(pa);
            } else {
                odd_pages.push(pa);
            }
        }

        // Free even-indexed pages.
        for pa in &even_pages {
            page_allocator::free_frame(*pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            buddy_pages / 2,
            "s9: half the pages should be free"
        );

        // Large allocation should fail in checkerboard state.
        let big = page_allocator::alloc_frames(8);
        assert!(
            big.is_none(),
            "s9: order-8 should fail with checkerboard fragmentation"
        );

        // Re-alloc the even pages we freed (to restore the fully-allocated state)
        // and then free ALL pages for clean coalescing.
        let mut re_even = Vec::new();
        for _ in 0..even_pages.len() {
            re_even.push(page_allocator::alloc_frame().expect("s9: re-alloc"));
        }

        // Now free all pages together.
        let mut all_to_free = Vec::new();
        all_to_free.extend_from_slice(&odd_pages);
        all_to_free.extend_from_slice(&re_even);

        for pa in &all_to_free {
            page_allocator::free_frame(*pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s9: freeing all pages should return to baseline"
        );

        let big2 = page_allocator::alloc_frames(11);
        assert!(
            big2.is_some(),
            "s9: full coalescing should enable order-11 alloc"
        );
        page_allocator::free_frames(big2.unwrap(), 11);
    }

    // ============================================================
    // Section 10: Repeated full alloc/free cycles (20 cycles)
    //
    // Stress the state machine by cycling through full
    // alloc + random free 20 times.
    // ============================================================

    {
        let mut rng = Rng::new(0x1234_5678);

        for cycle in 0..20 {
            let mut pages: Vec<memory::Pa> = Vec::new();
            loop {
                match page_allocator::alloc_frame() {
                    Some(pa) => pages.push(pa),
                    None => break,
                }
            }

            assert_eq!(
                page_allocator::free_count(),
                0,
                "s10 cycle {}: all allocated",
                cycle
            );
            assert_eq!(
                pages.len(),
                buddy_pages,
                "s10 cycle {}: got all pages",
                cycle
            );

            rng.shuffle(&mut pages);
            for pa in &pages {
                page_allocator::free_frame(*pa);
            }

            assert_eq!(
                page_allocator::free_count(),
                initial_free,
                "s10 cycle {}: free count should return to baseline",
                cycle
            );

            // Verify coalescing with order-11 alloc.
            let big = page_allocator::alloc_frames(11);
            assert!(
                big.is_some(),
                "s10 cycle {}: should yield order-11 block",
                cycle
            );
            page_allocator::free_frames(big.unwrap(), 11);
        }
    }

    // ============================================================
    // Section 11: Mixed-order repeated cycles (10 cycles)
    //
    // Allocate at random orders until exhaustion, free in random
    // order, repeat.
    // ============================================================

    {
        let mut rng = Rng::new(0xFACE_B00C);

        for cycle in 0..10 {
            let mut blocks: Vec<(memory::Pa, usize)> = Vec::new();

            loop {
                let order = rng.next_usize(8); // orders 0-7
                match page_allocator::alloc_frames(order) {
                    Some(pa) => blocks.push((pa, order)),
                    None => break,
                }
            }

            assert!(
                !blocks.is_empty(),
                "s11 cycle {}: should allocate blocks",
                cycle
            );

            rng.shuffle(&mut blocks);
            for (pa, order) in &blocks {
                page_allocator::free_frames(*pa, *order);
            }

            assert_eq!(
                page_allocator::free_count(),
                initial_free,
                "s11 cycle {}: should return to baseline",
                cycle
            );
        }
    }

    // ============================================================
    // Section 12: Staircase alloc — one block at each order 0-10,
    // free in random order (10 cycles).
    // ============================================================

    {
        let mut rng = Rng::new(0xCAFE_D00D);

        for _cycle in 0..10 {
            let mut blocks: Vec<(memory::Pa, usize)> = Vec::new();
            for order in 0..=10 {
                let pa = page_allocator::alloc_frames(order)
                    .unwrap_or_else(|| panic!("s12: alloc order {}", order));
                blocks.push((pa, order));
            }

            rng.shuffle(&mut blocks);
            for (pa, order) in &blocks {
                page_allocator::free_frames(*pa, *order);
            }

            assert_eq!(
                page_allocator::free_count(),
                initial_free,
                "s12: staircase free should return to baseline"
            );
        }
    }

    // ============================================================
    // Section 13: Interleaved alloc/free at multiple orders
    //
    // 2000 operations: randomly alloc at orders 0-5 or free a
    // random block. Verify no corruption and baseline return.
    // ============================================================

    {
        let mut rng = Rng::new(0xDEAD_BEEF);
        let mut live: Vec<(memory::Pa, usize)> = Vec::new();

        for _ in 0..2000 {
            if rng.next_usize(10) < 6 || live.is_empty() {
                let order = rng.next_usize(6);
                if let Some(pa) = page_allocator::alloc_frames(order) {
                    let size = PAGE_SIZE << order;
                    let va = pa.0 as *mut u8;
                    unsafe { core::ptr::write_bytes(va, 0xAB, size) };
                    live.push((pa, order));
                }
            } else {
                let idx = rng.next_usize(live.len());
                let (pa, order) = live.swap_remove(idx);
                let size = PAGE_SIZE << order;
                let slice = unsafe { core::slice::from_raw_parts(pa.0 as *const u8, size) };
                assert!(
                    slice.iter().all(|&b| b == 0xAB),
                    "s13: corruption in order-{} block at PA 0x{:x}",
                    order,
                    pa.0
                );
                page_allocator::free_frames(pa, order);
            }
        }

        for (pa, order) in live {
            page_allocator::free_frames(pa, order);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s13: interleaved multi-order should return to baseline"
        );
    }

    // ============================================================
    // Section 14: Fragmentation pressure — allocate all at order 0,
    // free every 4th, verify fragmentation blocks large allocs,
    // then free all and verify full coalescing.
    // ============================================================

    {
        let mut pages: Vec<memory::Pa> = Vec::new();
        for _ in 0..buddy_pages {
            pages.push(page_allocator::alloc_frame().expect("s14: alloc"));
        }

        pages.sort_by_key(|pa| pa.0);

        let mut freed = Vec::new();
        let mut kept = Vec::new();
        for (i, pa) in pages.into_iter().enumerate() {
            if i % 4 == 0 {
                page_allocator::free_frame(pa);
                freed.push(pa);
            } else {
                kept.push(pa);
            }
        }

        assert_eq!(
            page_allocator::free_count(),
            buddy_pages / 4,
            "s14: quarter of pages free"
        );

        // Large alloc should fail in fragmented state.
        let big = page_allocator::alloc_frames(2);
        assert!(
            big.is_none(),
            "s14: order-2 should fail with every-4th fragmentation"
        );

        // Free all remaining.
        for pa in &kept {
            page_allocator::free_frame(*pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s14: full free should return to baseline"
        );

        let big2 = page_allocator::alloc_frames(10);
        assert!(
            big2.is_some(),
            "s14: full coalescing should enable order-10 alloc"
        );
        page_allocator::free_frames(big2.unwrap(), 10);
    }

    // ============================================================
    // Section 15: Rapid order-0 alloc/free (5000 operations)
    // with corruption detection and baseline verification.
    // ============================================================

    {
        let mut rng = Rng::new(0xABCD_EF01);
        let mut live: Vec<memory::Pa> = Vec::new();

        for op in 0..5000 {
            if rng.next_usize(10) < 6 || live.is_empty() {
                if let Some(pa) = page_allocator::alloc_frame() {
                    let va = pa.0 as *mut u8;
                    unsafe { core::ptr::write_bytes(va, 0xCC, PAGE_SIZE) };
                    live.push(pa);
                }
            } else {
                let idx = rng.next_usize(live.len());
                let pa = live.swap_remove(idx);
                let slice = unsafe { core::slice::from_raw_parts(pa.0 as *const u8, PAGE_SIZE) };
                assert!(
                    slice.iter().all(|&b| b == 0xCC),
                    "s15 op {}: corruption at PA 0x{:x}",
                    op,
                    pa.0
                );
                page_allocator::free_frame(pa);
            }
        }

        for pa in live {
            page_allocator::free_frame(pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "s15: rapid order-0 should return to baseline"
        );

        let big = page_allocator::alloc_frames(11);
        assert!(
            big.is_some(),
            "s15: should yield order-11 after rapid cycle"
        );
        page_allocator::free_frames(big.unwrap(), 11);
    }

    // ============================================================
    // Final: verify free count equals initial.
    // ============================================================

    assert_eq!(
        page_allocator::free_count(),
        initial_free,
        "final: free count should equal initial baseline"
    );

    // Cleanup host region.
    unsafe { std::alloc::dealloc(ptr, layout) };
}
