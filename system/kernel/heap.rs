// AUDIT: 2026-03-14 — 2 unsafe blocks, 3 unsafe fn, 2 unsafe impl (GlobalAlloc, Sync) verified.
// 6-category checklist applied. No bugs found. Dealloc routing invariant (address-based, not
// size-based) verified sound. All SAFETY comments verified accurate.
//
// OOM handling:
//   - alloc() returns null on exhaustion (correct per GlobalAlloc contract).
//   - dealloc() panics on out-of-region pointer (appropriate — bogus free is always a kernel bug).
//   - Rust's default handle_alloc_error panics on null from alloc(). This means any Box::new(),
//     Vec::push(), etc. in the kernel will panic on OOM. This is expected: the kernel heap (16 MiB)
//     stores only kernel objects (threads, page tables, handles, scheduling state). User data lives
//     in demand-paged user address spaces, not the kernel heap. A kernel-heap OOM indicates the
//     kernel has exhausted its fixed-size internal pool — a fatal condition with no safe recovery
//     path since kernel data structures are already at capacity. All user-controlled allocation
//     paths (process_create, thread_create, channel_create, etc.) may trigger kernel-heap OOM;
//     none return errors to userspace on OOM — they panic. Fixing this would require try_alloc
//     (fallible allocation) throughout the kernel, which is a significant refactor deferred to
//     a future hardening pass.

//! Linked-list heap allocator with coalescing.
//!
//! Maintains a free list sorted by address. On alloc, walks the list for a
//! first-fit block, splitting if needed. On dealloc, reinserts the block and
//! merges with adjacent neighbors to prevent fragmentation.
//!
//! Mutual exclusion: all alloc/dealloc operations are protected by ALLOC_LOCK
//! (IrqMutex), which masks IRQs and spins on multi-core.
//!
//! ## Slab routing
//!
//! Small allocations (≤2048 bytes) are routed to the slab allocator for O(1)
//! alloc/free. The slab gets backing pages from the buddy allocator. If the
//! buddy allocator isn't initialized yet (early boot), slab grow fails and
//! allocations fall through to the linked-list. Dealloc uses address-based
//! routing: pointers within the linked-list heap region always go back to
//! the linked-list, preventing cross-allocator contamination.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
};

use super::{paging, slab, sync::IrqMutex};

const MIN_BLOCK: usize = core::mem::size_of::<FreeBlock>();

/// Protects the allocator's free list from concurrent access.
/// Separate from the allocator struct because GlobalAlloc takes `&self`.
static ALLOC_LOCK: IrqMutex<()> = IrqMutex::new(());

#[cfg_attr(not(test), global_allocator)]
pub static ALLOCATOR: LinkedListAllocator = LinkedListAllocator::new();

/// Each free block stores its total size (including this header) and a pointer
/// to the next free block. Minimum allocation granularity = 16 bytes on aarch64.
pub struct FreeBlock {
    pub size: usize,
    pub next: *mut FreeBlock,
}
pub struct LinkedListAllocator {
    pub head: UnsafeCell<*mut FreeBlock>,
    pub region_start: UnsafeCell<usize>,
    pub region_end: UnsafeCell<usize>,
}

impl LinkedListAllocator {
    const fn new() -> Self {
        Self {
            head: UnsafeCell::new(core::ptr::null_mut()),
            region_start: UnsafeCell::new(0),
            region_end: UnsafeCell::new(0),
        }
    }

    /// Check if an address falls within the linked-list heap region.
    ///
    /// Used by dealloc to route frees to the correct allocator. Pointers
    /// within this region were allocated by the linked-list; pointers outside
    /// (in slab pages from the buddy allocator) were allocated by the slab.
    ///
    /// # Safety
    ///
    /// Reads `region_start` and `region_end` through `UnsafeCell::get()`.
    /// Sound without holding ALLOC_LOCK because these fields are write-once:
    /// set by `init()` during single-threaded boot and never modified after.
    /// All subsequent reads see the initialized values.
    unsafe fn is_in_heap_region(&self, ptr: *mut u8) -> bool {
        // SAFETY: region_start and region_end are write-once fields set by
        // init() during single-threaded boot. No concurrent mutation possible.
        let addr = ptr as usize;
        let rs = *self.region_start.get();
        let re = *self.region_end.get();

        addr >= rs && addr < re
    }
}
// SAFETY: The GlobalAlloc contract requires that alloc returns a valid pointer
// or null, and dealloc receives a pointer previously returned by alloc with a
// compatible layout. Our implementation satisfies this: alloc walks a sorted
// free list for a first-fit block (returning null on exhaustion), and dealloc
// validates the pointer is within the heap region before reinserting. All
// internal state is protected by ALLOC_LOCK (IrqMutex with ticket spinlock).
unsafe impl GlobalAlloc for LinkedListAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: All pointer dereferences below are sound because:
        // 1. ALLOC_LOCK ensures mutual exclusion (no concurrent list traversal).
        // 2. The free list is initialized by init() with a single block
        //    spanning [__kernel_end, __kernel_end + HEAP_SIZE) — all within
        //    kernel-mapped memory (TTBR1 via phys_to_virt).
        // 3. FreeBlock pointers in the list come from either init() or prior
        //    dealloc() calls, both of which only insert addresses within the
        //    heap region. The sorted-by-address invariant and coalescing
        //    logic maintain list integrity.

        // Try slab allocator first for small allocations.
        let slab_ptr = slab::try_alloc(layout.size(), layout.align());

        if !slab_ptr.is_null() {
            return slab_ptr;
        }

        let _guard = ALLOC_LOCK.lock();
        let head = &mut *self.head.get();
        let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
        let align = layout.align().max(MIN_BLOCK);
        let mut prev = head as *mut *mut FreeBlock;
        let mut result: *mut u8 = core::ptr::null_mut();

        loop {
            let current = *prev;

            if current.is_null() {
                break;
            }

            let block_addr = current as usize;
            let block_size = (*current).size;
            let alloc_start = align_up(block_addr, align);
            let front_pad = alloc_start - block_addr;

            // Front padding must fit a free block header, or be zero.
            if front_pad > 0 && front_pad < MIN_BLOCK {
                prev = &mut (*current).next;

                continue;
            }
            if front_pad + size > block_size {
                prev = &mut (*current).next;

                continue;
            }

            let back_left = block_size - front_pad - size;

            // Unlink this block from the free list.
            *prev = (*current).next;

            // Return front padding as a smaller free block.
            if front_pad >= MIN_BLOCK {
                let front = block_addr as *mut FreeBlock;

                (*front).size = front_pad;
                (*front).next = *prev;
                *prev = front;
                prev = &mut (*front).next;
            }
            // Return back leftover as a free block.
            if back_left >= MIN_BLOCK {
                let back = (alloc_start + size) as *mut FreeBlock;

                (*back).size = back_left;
                (*back).next = *prev;
                *prev = back;
            }

            result = alloc_start as *mut u8;

            break;
        }

        result
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: All pointer dereferences below are sound because:
        // 1. ALLOC_LOCK ensures mutual exclusion.
        // 2. The bounds check below ensures `ptr` is within the heap region.
        // 3. The sorted insertion walk only follows `next` pointers that were
        //    placed by init() or prior dealloc() calls (valid kernel VAs).
        // 4. Coalescing only merges geometrically adjacent blocks, so the
        //    merged block's size always stays within the heap region.

        // Route to the correct allocator based on pointer origin.
        // Slab pages come from the buddy allocator (above the heap region).
        // Linked-list allocations are within [region_start, region_end).
        // During early boot (before buddy allocator init), slab grow fails
        // and all allocations go to the linked-list. We must not route those
        // frees through slab, or its free list gets contaminated with
        // linked-list addresses.
        if !self.is_in_heap_region(ptr) && slab::try_free(ptr, layout.size(), layout.align()) {
            return;
        }

        let _guard = ALLOC_LOCK.lock();
        let head = &mut *self.head.get();
        let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
        let addr = ptr as usize;
        // Validate that the freed address is within the heap region.
        // A bogus free would corrupt the free list, so catching it early
        // is worth the branch cost in a production kernel.
        let rs = *self.region_start.get();
        let re = *self.region_end.get();

        assert!(
            addr >= rs && addr < re,
            "dealloc: address outside heap region"
        );

        // Walk to the sorted insertion point.
        let mut prev_block: *mut FreeBlock = core::ptr::null_mut();
        let mut current = *head;

        while !current.is_null() && (current as usize) < addr {
            prev_block = current;
            current = (*current).next;
        }

        // Insert freed region.
        let block = addr as *mut FreeBlock;

        (*block).size = size;
        (*block).next = current;

        if prev_block.is_null() {
            *head = block;
        } else {
            (*prev_block).next = block;
        }

        // Coalesce with next neighbor.
        if !current.is_null() && addr + size == current as usize {
            (*block).size += (*current).size;
            (*block).next = (*current).next;
        }

        // Coalesce with previous neighbor.
        if !prev_block.is_null() {
            let prev_end = prev_block as usize + (*prev_block).size;

            if prev_end == addr {
                (*prev_block).size += (*block).size;
                (*prev_block).next = (*block).next;
            }
        }
    }
}
// SAFETY: All access to the free list is protected by ALLOC_LOCK (IrqMutex
// with ticket spinlock), ensuring mutual exclusion across cores and IRQs.
unsafe impl Sync for LinkedListAllocator {}

fn align_up(addr: usize, align: usize) -> usize {
    paging::align_up(addr, align)
}

/// Initialize the heap. Call after `memory::init()` (page tables must be live).
pub fn init() {
    extern "C" {
        static __kernel_end: u8;
    }

    // SAFETY: `__kernel_end` is a linker symbol marking the end of the kernel's
    // BSS section. Taking its address yields a valid pointer into kernel memory.
    // We only need the numeric address, not the value at that address.
    let start = align_up(unsafe { &__kernel_end as *const u8 as usize }, MIN_BLOCK);

    // SAFETY: Called once during single-threaded boot (before SMP or interrupts).
    // `start` points to kernel-mapped memory (TTBR1 via phys_to_virt) after
    // `__kernel_end`, sized to `HEAP_SIZE`. Writing the FreeBlock header and
    // UnsafeCell fields is sound because no other code accesses the allocator
    // until init() returns and ALLOC_LOCK guards all subsequent access.
    unsafe {
        use super::memory;
        let block = start as *mut FreeBlock;

        (*block).size = memory::HEAP_SIZE;
        (*block).next = core::ptr::null_mut();

        *ALLOCATOR.head.get() = block;
        *ALLOCATOR.region_start.get() = start;
        *ALLOCATOR.region_end.get() = start + memory::HEAP_SIZE;
    }
}
