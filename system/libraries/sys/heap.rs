//! Userspace heap allocator — linked-list first-fit with coalescing.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{asm::align_up, syscalls::memory_alloc, types::HeapStats, PAGE_SIZE};

pub(crate) struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const MIN_BLOCK: usize = core::mem::size_of::<FreeBlock>();

pub struct UserHeap {
    head: UnsafeCell<*mut FreeBlock>,
    lock: AtomicBool,
    // Instrumentation counters (protected by the same spinlock as head).
    total_allocated: UnsafeCell<usize>,
    total_freed: UnsafeCell<usize>,
    pages_requested: UnsafeCell<usize>,
}

impl UserHeap {
    pub const fn new() -> Self {
        Self {
            head: UnsafeCell::new(core::ptr::null_mut()),
            lock: AtomicBool::new(false),
            total_allocated: UnsafeCell::new(0),
            total_freed: UnsafeCell::new(0),
            pages_requested: UnsafeCell::new(0),
        }
    }

    pub(crate) fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    /// Request pages from the kernel and add them to the free list.
    ///
    /// Allocates enough pages to satisfy `min_size` bytes. Returns true
    /// on success, false if the kernel refuses (out of memory / budget).
    unsafe fn grow(&self, min_size: usize) -> bool {
        let pages = (min_size + PAGE_SIZE - 1) / PAGE_SIZE;
        let va = match memory_alloc(pages as u64) {
            Ok(va) => va,
            Err(_) => return false,
        };
        let block = va as *mut FreeBlock;
        let head = &mut *self.head.get();

        (*block).size = pages * PAGE_SIZE;
        (*block).next = *head;
        *head = block;
        *self.pages_requested.get() += pages;

        true
    }

    pub(crate) fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Heap allocator — linked-list first-fit with coalescing.
//
// Grows on demand by calling `memory_alloc`. Programs use this by adding
// `extern crate alloc;` to get Vec, String, Box, etc. Programs that never
// import `alloc` pay no cost.
// ---------------------------------------------------------------------------
unsafe impl GlobalAlloc for UserHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.acquire();

        let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
        let align = layout.align().max(MIN_BLOCK);
        let head = &mut *self.head.get();
        let mut prev = head as *mut *mut FreeBlock;

        // First-fit search with alignment handling.
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

            // Unlink this block.
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

            *self.total_allocated.get() += size;
            self.release();

            return alloc_start as *mut u8;
        }

        // Free list exhausted — grow and retry once.
        if self.grow(size) {
            // Retry from the head (new block was prepended).
            prev = &mut *self.head.get() as *mut *mut FreeBlock;

            loop {
                let current = *prev;

                if current.is_null() {
                    break;
                }

                let block_addr = current as usize;
                let block_size = (*current).size;
                let alloc_start = align_up(block_addr, align);
                let front_pad = alloc_start - block_addr;

                if front_pad > 0 && front_pad < MIN_BLOCK {
                    prev = &mut (*current).next;
                    continue;
                }
                if front_pad + size > block_size {
                    prev = &mut (*current).next;
                    continue;
                }

                let back_left = block_size - front_pad - size;

                *prev = (*current).next;

                if front_pad >= MIN_BLOCK {
                    let front = block_addr as *mut FreeBlock;

                    (*front).size = front_pad;
                    (*front).next = *prev;
                    *prev = front;
                    prev = &mut (*front).next;
                }
                if back_left >= MIN_BLOCK {
                    let back = (alloc_start + size) as *mut FreeBlock;

                    (*back).size = back_left;
                    (*back).next = *prev;
                    *prev = back;
                }

                *self.total_allocated.get() += size;
                self.release();

                return alloc_start as *mut u8;
            }
        }

        self.release();

        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.acquire();

        let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
        *self.total_freed.get() += size;
        let addr = ptr as usize;
        let head = &mut *self.head.get();

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

        self.release();
    }
}

// SAFETY: All free list access is protected by a spinlock (AtomicBool CAS).
unsafe impl Sync for UserHeap {}

/// Return heap usage statistics.
///
/// Acquires the heap spinlock to read consistent counters. Safe to call
/// from any thread, but not from within an allocator callback.
pub fn heap_stats() -> HeapStats {
    crate::HEAP.acquire();
    // SAFETY: Counters are protected by the same spinlock as the free list.
    let stats = unsafe {
        HeapStats {
            total_allocated: *crate::HEAP.total_allocated.get(),
            total_freed: *crate::HEAP.total_freed.get(),
            pages_requested: *crate::HEAP.pages_requested.get(),
        }
    };
    crate::HEAP.release();
    stats
}
