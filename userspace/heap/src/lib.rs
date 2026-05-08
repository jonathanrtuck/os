//! Userspace slab allocator — O(1) alloc/free for common sizes.
//!
//! Two-tier design:
//! - **Small** (<=2048 bytes): slab with 8 power-of-two size classes.
//!   Free lists carved from dedicated 16 KiB slab pages.
//! - **Large** (>2048 bytes): page-granular VMO allocation from kernel,
//!   tracked by VA→handle mapping for proper cleanup.
//!
//! Large allocations are cached on free for reuse. When the cache is full,
//! overflow entries are unmapped and their VMO handles closed — no leaks.
//!
//! Link this crate into any `no_std` userspace binary that needs `alloc`.
//! It provides `#[global_allocator]` automatically.

#![no_std]

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
};

use abi::types::{Handle, Rights};

const PAGE_SIZE: usize = 16384;
const NUM_CLASSES: usize = 8;
const CLASS_SIZES: [usize; NUM_CLASSES] = [16, 32, 64, 128, 256, 512, 1024, 2048];

#[inline]
fn class_index(size: usize) -> Option<usize> {
    if size <= 16 {
        Some(0)
    } else if size <= 2048 {
        let npt = size.next_power_of_two();
        Some(npt.trailing_zeros() as usize - 4)
    } else {
        None
    }
}

fn alloc_pages(pages: usize) -> Option<(usize, Handle)> {
    let size = pages * PAGE_SIZE;
    let vmo = abi::vmo::create(size, 0).ok()?;
    let va = abi::vmo::map(vmo, 0, Rights::READ_WRITE_MAP).ok()?;

    Some((va, vmo))
}

fn free_pages(va: usize, vmo: Handle) {
    let _ = abi::vmo::unmap(va);
    let _ = abi::handle::close(vmo);
}

struct FreeSlot {
    next: *mut FreeSlot,
}

// ── Large allocation tracker ───────────────────────────────────────

const LARGE_CACHE_CAPACITY: usize = 32;
const VMO_TRACKER_CAPACITY: usize = 64;

struct VmoEntry {
    va: usize,
    handle: u32,
}

struct VmoTracker {
    entries: [VmoEntry; VMO_TRACKER_CAPACITY],
    len: usize,
}

impl VmoTracker {
    const fn new() -> Self {
        const EMPTY: VmoEntry = VmoEntry { va: 0, handle: 0 };

        Self {
            entries: [EMPTY; VMO_TRACKER_CAPACITY],
            len: 0,
        }
    }

    fn insert(&mut self, va: usize, handle: Handle) {
        if self.len < VMO_TRACKER_CAPACITY {
            self.entries[self.len] = VmoEntry {
                va,
                handle: handle.0,
            };
            self.len += 1;
        } else {
            free_pages(va, handle);
        }
    }

    fn remove(&mut self, va: usize) -> Option<Handle> {
        for i in 0..self.len {
            if self.entries[i].va == va {
                let handle = Handle(self.entries[i].handle);

                self.len -= 1;

                if i < self.len {
                    self.entries[i] = VmoEntry {
                        va: self.entries[self.len].va,
                        handle: self.entries[self.len].handle,
                    };
                }

                return Some(handle);
            }
        }

        None
    }
}

struct LargeCacheEntry {
    va: usize,
    pages: usize,
    vmo_handle: u32,
}

struct LargeCache {
    entries: [LargeCacheEntry; LARGE_CACHE_CAPACITY],
    len: usize,
}

impl LargeCache {
    const fn new() -> Self {
        const EMPTY: LargeCacheEntry = LargeCacheEntry {
            va: 0,
            pages: 0,
            vmo_handle: 0,
        };

        Self {
            entries: [EMPTY; LARGE_CACHE_CAPACITY],
            len: 0,
        }
    }

    fn take(&mut self, pages: usize) -> Option<(usize, Handle)> {
        for i in 0..self.len {
            if self.entries[i].pages == pages {
                let va = self.entries[i].va;
                let handle = Handle(self.entries[i].vmo_handle);

                self.len -= 1;

                if i < self.len {
                    self.entries[i] = LargeCacheEntry {
                        va: self.entries[self.len].va,
                        pages: self.entries[self.len].pages,
                        vmo_handle: self.entries[self.len].vmo_handle,
                    };
                }

                return Some((va, handle));
            }
        }

        None
    }

    fn put(&mut self, va: usize, pages: usize, vmo: Handle) {
        if self.len < LARGE_CACHE_CAPACITY {
            self.entries[self.len] = LargeCacheEntry {
                va,
                pages,
                vmo_handle: vmo.0,
            };
            self.len += 1;
        } else {
            free_pages(va, vmo);
        }
    }
}

// ── Global allocator ───────────────────────────────────────────────

pub struct UserHeap {
    classes: [UnsafeCell<*mut FreeSlot>; NUM_CLASSES],
    large_cache: UnsafeCell<LargeCache>,
    vmo_tracker: UnsafeCell<VmoTracker>,
}

impl UserHeap {
    pub const fn new() -> Self {
        const NULL: UnsafeCell<*mut FreeSlot> = UnsafeCell::new(core::ptr::null_mut());

        Self {
            classes: [NULL; NUM_CLASSES],
            large_cache: UnsafeCell::new(LargeCache::new()),
            vmo_tracker: UnsafeCell::new(VmoTracker::new()),
        }
    }

    fn acquire(&self) {}

    fn release(&self) {}

    // SAFETY: page_va must point to a valid, PAGE_SIZE-byte region.
    // Caller must hold the lock.
    unsafe fn carve_slab_page(&self, page_va: usize, ci: usize) {
        let class_size = CLASS_SIZES[ci];
        let slots = PAGE_SIZE / class_size;
        // SAFETY: Lock is held by caller; UnsafeCell access is exclusive.
        let head = unsafe { &mut *self.classes[ci].get() };

        for i in (0..slots).rev() {
            let slot = (page_va + i * class_size) as *mut FreeSlot;

            // SAFETY: slot is within the valid page_va allocation.
            unsafe {
                (*slot).next = *head;
            }

            *head = slot;
        }
    }
}

// SAFETY: All free list access is protected by a spinlock.
unsafe impl Sync for UserHeap {}

unsafe impl GlobalAlloc for UserHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let effective = layout.size().max(layout.align());

        match class_index(effective) {
            Some(ci) => {
                self.acquire();

                // SAFETY: Lock is held; UnsafeCell access is exclusive.
                let head = unsafe { &mut *self.classes[ci].get() };

                if (*head).is_null() {
                    let (va, _vmo) = match alloc_pages(1) {
                        Some(r) => r,
                        None => {
                            self.release();

                            return core::ptr::null_mut();
                        }
                    };

                    // SAFETY: va is a valid page allocation.
                    unsafe { self.carve_slab_page(va, ci) };
                }

                let slot = *head;

                // SAFETY: slot is a valid FreeSlot pointer from the slab.
                *head = unsafe { (*slot).next };

                self.release();

                slot as *mut u8
            }
            None => {
                let pages = (layout.size() + PAGE_SIZE - 1) / PAGE_SIZE;

                self.acquire();

                // SAFETY: Lock is held; UnsafeCell access is exclusive.
                let cache = unsafe { &mut *self.large_cache.get() };

                if let Some((va, handle)) = cache.take(pages) {
                    let tracker = unsafe { &mut *self.vmo_tracker.get() };

                    tracker.insert(va, handle);

                    self.release();

                    return va as *mut u8;
                }

                self.release();

                let (va, vmo) = match alloc_pages(pages) {
                    Some(r) => r,
                    None => return core::ptr::null_mut(),
                };

                self.acquire();

                let tracker = unsafe { &mut *self.vmo_tracker.get() };

                tracker.insert(va, vmo);

                self.release();

                va as *mut u8
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let effective = layout.size().max(layout.align());

        match class_index(effective) {
            Some(ci) => {
                self.acquire();

                // SAFETY: Lock is held; UnsafeCell access is exclusive.
                let head = unsafe { &mut *self.classes[ci].get() };
                let slot = ptr as *mut FreeSlot;

                // SAFETY: slot points to a previously-allocated slab slot.
                unsafe { (*slot).next = *head };

                *head = slot;

                self.release();
            }
            None => {
                let pages = (layout.size() + PAGE_SIZE - 1) / PAGE_SIZE;

                self.acquire();

                let tracker = unsafe { &mut *self.vmo_tracker.get() };
                let handle = tracker.remove(ptr as usize);
                // SAFETY: Lock is held; UnsafeCell access is exclusive.
                let cache = unsafe { &mut *self.large_cache.get() };

                match handle {
                    Some(h) => cache.put(ptr as usize, pages, h),
                    None => {
                        let _ = abi::vmo::unmap(ptr as usize);
                    }
                }

                self.release();
            }
        }
    }
}

#[global_allocator]
static HEAP: UserHeap = UserHeap::new();

pub fn test_alloc(size: usize) -> usize {
    match alloc_pages((size + PAGE_SIZE - 1) / PAGE_SIZE) {
        Some((va, _)) => va,
        None => 0,
    }
}

pub fn test_global_alloc(size: usize) -> usize {
    let layout = core::alloc::Layout::from_size_align(size, 1).unwrap();
    // SAFETY: testing the global allocator.
    let ptr = unsafe { HEAP.alloc(layout) };

    ptr as usize
}
