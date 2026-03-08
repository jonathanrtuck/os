//! Split virtqueue implementation (virtio 1.0+).
//!
//! A virtqueue consists of three memory regions:
//! - **Descriptor table**: array of buffer descriptors (16 bytes each)
//! - **Available ring**: driver → device buffer notification
//! - **Used ring**: device → driver completion notification
//!
//! Memory is allocated as physically contiguous pages from the buddy
//! allocator so the device can DMA to/from them.

use super::super::memory;
use super::super::page_alloc;
use super::super::paging::PAGE_SIZE;

/// Default queue size. At 128 entries, all three regions fit in one 4 KiB page:
/// descriptors (2048) + avail ring (262) + used ring (1030) = 3340 bytes.
pub const DEFAULT_QUEUE_SIZE: u32 = 128;
/// Buffer continues in the next descriptor.
pub const DESC_F_NEXT: u16 = 1;
/// Device writes to this buffer (vs. reads).
pub const DESC_F_WRITE: u16 = 2;

/// A single virtqueue descriptor.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}
/// Element in the used ring.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}
/// Manages a single virtqueue's descriptor table, available ring, and used ring.
pub struct Virtqueue {
    size: u32,
    desc_va: usize,
    avail_va: usize,
    used_va: usize,
    desc_pa: u64,
    avail_pa: u64,
    used_pa: u64,
    /// Head of the free descriptor chain.
    free_head: u16,
    num_free: u32,
    /// Last seen used ring index (for polling).
    last_used_idx: u16,
    /// Physical address of the backing allocation (for cleanup).
    backing_pa: usize,
    backing_order: usize,
}

impl Virtqueue {
    /// Allocate and initialize a virtqueue with `size` descriptors.
    pub fn new(size: u32) -> Option<Self> {
        let desc_bytes = size as usize * core::mem::size_of::<Descriptor>();
        let avail_bytes = 6 + size as usize * 2; // flags + idx + ring[N] + used_event
        let avail_offset = desc_bytes;
        let used_offset = (avail_offset + avail_bytes + 3) & !3; // 4-byte align
        let used_bytes = 6 + size as usize * core::mem::size_of::<UsedElem>();
        let total = used_offset + used_bytes;
        let page_size = PAGE_SIZE as usize;
        let pages_needed = (total + page_size - 1) / page_size;
        let order = pages_needed.next_power_of_two().trailing_zeros() as usize;
        let pa = page_alloc::alloc_frames(order)?;
        let va = memory::phys_to_virt(pa);
        // Initialize free descriptor chain: 0 → 1 → 2 → ... → (size-1).
        let desc_ptr = va as *mut Descriptor;

        for i in 0..size {
            // SAFETY: desc_ptr was just allocated and zeroed by alloc_frames.
            unsafe {
                let d = &mut *desc_ptr.add(i as usize);

                d.next = if i + 1 < size { (i + 1) as u16 } else { 0xFFFF };
            }
        }

        Some(Self {
            size,
            desc_va: va,
            avail_va: va + avail_offset,
            used_va: va + used_offset,
            desc_pa: pa as u64,
            avail_pa: pa as u64 + avail_offset as u64,
            used_pa: pa as u64 + used_offset as u64,
            free_head: 0,
            num_free: size,
            last_used_idx: 0,
            backing_pa: pa,
            backing_order: order,
        })
    }

    pub fn avail_pa(&self) -> u64 {
        self.avail_pa
    }
    pub fn desc_pa(&self) -> u64 {
        self.desc_pa
    }
    /// Return a descriptor chain to the free list.
    ///
    /// Validates that each descriptor index is within bounds. If a
    /// device-provided index is out of range, the chain is truncated
    /// (descriptors may leak, but the kernel is not corrupted).
    fn free_descriptor_chain(&mut self, mut idx: u16) {
        loop {
            if idx as u32 >= self.size {
                // Device provided an out-of-bounds descriptor ID. Truncate
                // the chain to prevent kernel memory corruption.
                return;
            }

            let desc = unsafe { &mut *(self.desc_va as *mut Descriptor).add(idx as usize) };
            let has_next = desc.flags & DESC_F_NEXT != 0;
            let next = desc.next;

            // Prepend to the free list.
            desc.flags = 0;
            desc.addr = 0;
            desc.len = 0;
            desc.next = self.free_head;
            self.free_head = idx;
            self.num_free += 1;

            if !has_next {
                break;
            }

            idx = next;
        }
    }
    /// Poll for a completed request. Returns `Some(UsedElem)` if the device
    /// has consumed a buffer, or `None` if no completions are pending.
    pub fn pop_used(&mut self) -> Option<UsedElem> {
        let used_idx = unsafe {
            let idx_ptr = (self.used_va + 2) as *const u16;
            core::ptr::read_volatile(idx_ptr)
        };

        if self.last_used_idx == used_idx {
            return None;
        }

        // Acquire fence AFTER reading used_idx, BEFORE reading the element.
        // Ensures we see the device's descriptor/data writes that correspond
        // to this index update. Placing it before the idx read (as before)
        // would not order the idx read relative to the element read.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        let ring_ptr = (self.used_va + 4) as *const UsedElem;
        let elem = unsafe {
            core::ptr::read_volatile(ring_ptr.add((self.last_used_idx % self.size as u16) as usize))
        };

        self.last_used_idx = self.last_used_idx.wrapping_add(1);

        if (elem.id as u32) < self.size {
            self.free_descriptor_chain(elem.id as u16);
        }
        // If elem.id is out of bounds, we skip freeing (leak descriptors
        // rather than corrupt kernel memory from a malicious device).

        Some(elem)
    }
    /// Push a single buffer onto the available ring (convenience wrapper).
    pub fn push(&mut self, buf_pa: u64, buf_len: u32, device_writable: bool) -> Option<u16> {
        self.push_chain(&[(buf_pa, buf_len, device_writable)])
    }
    /// Push a descriptor chain onto the available ring.
    ///
    /// Each element is `(physical_address, length, device_writable)`.
    /// Returns the head descriptor index, or `None` if not enough free slots.
    pub fn push_chain(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.is_empty() || (self.num_free as usize) < bufs.len() {
            return None;
        }

        let head = self.free_head;
        let mut current = head;

        for (i, &(pa, len, writable)) in bufs.iter().enumerate() {
            // SAFETY: current is a valid index into the descriptor table
            // (maintained by the free chain invariant).
            let desc = unsafe { &mut *(self.desc_va as *mut Descriptor).add(current as usize) };
            let next_free = desc.next;

            desc.addr = pa;
            desc.len = len;
            desc.flags = if writable { DESC_F_WRITE } else { 0 };

            if i + 1 < bufs.len() {
                // Chain continues — next descriptor is the next free one.
                desc.flags |= DESC_F_NEXT;
                // desc.next already points to next_free (the next free descriptor)
                current = next_free;
            } else {
                // Last in chain — terminate and update free list.
                desc.next = 0;
                self.free_head = next_free;
            }

            self.num_free -= 1;
        }

        // Add head to the available ring.
        // SAFETY: avail_va points to the available ring we allocated.
        unsafe {
            let avail_idx_ptr = (self.avail_va + 2) as *mut u16;
            let avail_idx = core::ptr::read_volatile(avail_idx_ptr);
            let ring_ptr = (self.avail_va + 4) as *mut u16;

            core::ptr::write_volatile(ring_ptr.add((avail_idx % self.size as u16) as usize), head);
            // Barrier: descriptor writes must be visible before idx update.
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            core::ptr::write_volatile(avail_idx_ptr, avail_idx.wrapping_add(1));
        }

        Some(head)
    }
    pub fn size(&self) -> u32 {
        self.size
    }
    pub fn used_pa(&self) -> u64 {
        self.used_pa
    }
    /// Spin-wait for the next used element (polling, no IRQ).
    pub fn wait_used(&mut self) -> UsedElem {
        loop {
            if let Some(elem) = self.pop_used() {
                return elem;
            }

            core::hint::spin_loop();
        }
    }
}
impl Drop for Virtqueue {
    fn drop(&mut self) {
        page_alloc::free_frames(self.backing_pa, self.backing_order);
    }
}
