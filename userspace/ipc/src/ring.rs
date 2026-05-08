//! SPSC ring buffer — lock-free single-producer single-consumer queue
//! in shared memory for discrete event streams.
//!
//! Layout in the shared VMO:
//! ```text
//! [RingHeader: 16 bytes][slot 0: N bytes][slot 1: N bytes]...[slot K-1: N bytes]
//! ```
//!
//! Indices are free-running u32 values that wrap naturally. Slot count
//! must be a power of two so masking works without division.

use core::sync::atomic::{AtomicU32, Ordering, fence};

#[repr(C)]
pub struct RingHeader {
    write: AtomicU32,
    read: AtomicU32,
    slot_size: u32,
    slot_count: u32,
}

pub const HEADER_SIZE: usize = core::mem::size_of::<RingHeader>();

impl RingHeader {
    pub fn init(header: &mut Self, slot_size: u32, slot_count: u32) {
        assert!(slot_count.is_power_of_two());
        assert!(slot_size > 0);

        header.write = AtomicU32::new(0);
        header.read = AtomicU32::new(0);
        header.slot_size = slot_size;
        header.slot_count = slot_count;
    }
}

pub fn required_size(slot_size: u32, slot_count: u32) -> usize {
    HEADER_SIZE + slot_size as usize * slot_count as usize
}

pub struct Producer {
    base: *mut u8,
    slot_size: u32,
    slot_count: u32,
    mask: u32,
    cached_read: u32,
}

impl Producer {
    /// # Safety
    /// `base` must point to a properly initialized `RingHeader` followed by
    /// `slot_count * slot_size` bytes of writable memory. At most one
    /// `Producer` may exist per ring — constructing a second is instant UB.
    /// The pointer must remain valid for the producer's lifetime.
    pub unsafe fn new(base: *mut u8) -> Self {
        // SAFETY: caller guarantees base points to a valid RingHeader.
        let h = unsafe { &*(base as *const RingHeader) };

        Self {
            base,
            slot_size: h.slot_size,
            slot_count: h.slot_count,
            mask: h.slot_count - 1,
            cached_read: 0,
        }
    }

    fn write_ptr(&self) -> &AtomicU32 {
        // SAFETY: base points to a valid RingHeader per constructor contract.
        unsafe { &(*self.base.cast::<RingHeader>()).write }
    }

    fn read_ptr(&self) -> &AtomicU32 {
        // SAFETY: base points to a valid RingHeader per constructor contract.
        unsafe { &(*self.base.cast::<RingHeader>()).read }
    }

    pub fn try_push(&mut self, data: &[u8]) -> bool {
        let write = self.write_ptr().load(Ordering::Relaxed);

        if write.wrapping_sub(self.cached_read) >= self.slot_count {
            self.cached_read = self.read_ptr().load(Ordering::Acquire);

            if write.wrapping_sub(self.cached_read) >= self.slot_count {
                return false;
            }
        }

        let offset = HEADER_SIZE + (write & self.mask) as usize * self.slot_size as usize;
        let len = data.len().min(self.slot_size as usize);

        // SAFETY: offset is within the VMO bounds (masked index).
        // We are the sole writer; the consumer only reads at read_idx.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.base.add(offset), len);

            if len < self.slot_size as usize {
                core::ptr::write_bytes(
                    self.base.add(offset + len),
                    0,
                    self.slot_size as usize - len,
                );
            }
        }

        self.write_ptr()
            .store(write.wrapping_add(1), Ordering::Release);

        true
    }

    pub fn available(&self) -> u32 {
        let write = self.write_ptr().load(Ordering::Relaxed);
        let read = self.read_ptr().load(Ordering::Acquire);

        self.slot_count - write.wrapping_sub(read)
    }
}

pub struct Consumer {
    base: *const u8,
    slot_size: u32,
    mask: u32,
    cached_write: u32,
}

impl Consumer {
    /// # Safety
    /// `base` must point to a properly initialized `RingHeader` followed by
    /// `slot_count * slot_size` bytes of readable memory. At most one
    /// `Consumer` may exist per ring — constructing a second is instant UB.
    /// The pointer must remain valid for the consumer's lifetime.
    pub unsafe fn new(base: *const u8) -> Self {
        // SAFETY: caller guarantees base points to a valid RingHeader.
        let h = unsafe { &*(base as *const RingHeader) };

        Self {
            base,
            slot_size: h.slot_size,
            mask: h.slot_count - 1,
            cached_write: 0,
        }
    }

    fn write_ptr(&self) -> &AtomicU32 {
        // SAFETY: base points to a valid RingHeader per constructor contract.
        unsafe { &(*self.base.cast::<RingHeader>()).write }
    }

    fn read_ptr(&self) -> &AtomicU32 {
        // SAFETY: base points to a valid RingHeader per constructor contract.
        unsafe { &(*self.base.cast::<RingHeader>()).read }
    }

    pub fn try_pop(&mut self, out: &mut [u8]) -> bool {
        let read = self.read_ptr().load(Ordering::Relaxed);

        if read == self.cached_write {
            self.cached_write = self.write_ptr().load(Ordering::Acquire);

            if read == self.cached_write {
                return false;
            }
        } else {
            // Cached value says data is available, but we still need a
            // load barrier before reading the slot — the Acquire on
            // write_ptr happened in a previous call and does not order
            // this call's slot reads on AArch64.
            fence(Ordering::Acquire);
        }

        let offset = HEADER_SIZE + (read & self.mask) as usize * self.slot_size as usize;
        let len = out.len().min(self.slot_size as usize);

        // SAFETY: offset is within the VMO bounds. The producer only writes
        // at write_idx which is strictly ahead of read_idx when we get here.
        unsafe {
            core::ptr::copy_nonoverlapping(self.base.add(offset), out.as_mut_ptr(), len);
        }

        self.read_ptr()
            .store(read.wrapping_add(1), Ordering::Release);

        true
    }

    pub fn pending(&self) -> u32 {
        let write = self.write_ptr().load(Ordering::Acquire);
        let read = self.read_ptr().load(Ordering::Relaxed);

        write.wrapping_sub(read)
    }

    pub fn is_empty(&self) -> bool {
        self.pending() == 0
    }

    pub fn drain(&mut self, mut f: impl FnMut(&[u8])) {
        let write = self.write_ptr().load(Ordering::Acquire);
        let mut read = self.read_ptr().load(Ordering::Relaxed);
        let ss = self.slot_size as usize;

        while read != write {
            let offset = HEADER_SIZE + (read & self.mask) as usize * ss;
            // SAFETY: same as try_pop — offset is bounded, producer is ahead.
            let slot = unsafe { core::slice::from_raw_parts(self.base.add(offset), ss) };

            f(slot);

            read = read.wrapping_add(1);
        }

        self.read_ptr().store(read, Ordering::Release);
    }
}

// SAFETY: Producer and Consumer are each designed for single-thread use.
// At most one Producer and one Consumer may exist per ring. Sending to
// another thread is safe because the underlying memory is synchronized
// via atomics on the write/read indices.
unsafe impl Send for Producer {}
unsafe impl Send for Consumer {}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{vec, vec::Vec};

    use super::*;

    fn alloc_ring(slot_size: u32, slot_count: u32) -> Vec<u8> {
        let size = required_size(slot_size, slot_count);
        let mut buf = vec![0u8; size];
        let header = unsafe { &mut *(buf.as_mut_ptr() as *mut RingHeader) };

        RingHeader::init(header, slot_size, slot_count);

        buf
    }

    #[test]
    fn push_pop_single() {
        let mut buf = alloc_ring(8, 4);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };

        assert!(producer.try_push(&[1, 2, 3, 4, 5, 6, 7, 8]));

        let mut out = [0u8; 8];

        assert!(consumer.try_pop(&mut out));
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn empty_ring_returns_false() {
        let buf = alloc_ring(4, 4);
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };
        let mut out = [0u8; 4];

        assert!(!consumer.try_pop(&mut out));
        assert!(consumer.is_empty());
    }

    #[test]
    fn full_ring_returns_false() {
        let mut buf = alloc_ring(4, 4);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };

        for i in 0u8..4 {
            assert!(producer.try_push(&[i, 0, 0, 0]));
        }

        assert!(!producer.try_push(&[99, 0, 0, 0]));
    }

    #[test]
    fn fifo_order() {
        let mut buf = alloc_ring(4, 8);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };

        for i in 0u8..5 {
            assert!(producer.try_push(&[i, 0, 0, 0]));
        }

        for i in 0u8..5 {
            let mut out = [0u8; 4];

            assert!(consumer.try_pop(&mut out));
            assert_eq!(out[0], i);
        }

        assert!(consumer.is_empty());
    }

    #[test]
    fn wrap_around() {
        let mut buf = alloc_ring(4, 4);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };

        for round in 0u8..10 {
            for slot in 0u8..4 {
                let val = round * 4 + slot;

                assert!(producer.try_push(&[val, 0, 0, 0]));
            }
            for slot in 0u8..4 {
                let mut out = [0u8; 4];

                assert!(consumer.try_pop(&mut out));
                assert_eq!(out[0], round * 4 + slot);
            }
        }
    }

    #[test]
    fn pending_count() {
        let mut buf = alloc_ring(4, 8);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let consumer = unsafe { Consumer::new(buf.as_ptr()) };

        assert_eq!(consumer.pending(), 0);

        producer.try_push(&[0; 4]);
        producer.try_push(&[0; 4]);

        assert_eq!(consumer.pending(), 2);
    }

    #[test]
    fn available_count() {
        let mut buf = alloc_ring(4, 8);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };

        assert_eq!(producer.available(), 8);

        producer.try_push(&[0; 4]);

        assert_eq!(producer.available(), 7);
    }

    #[test]
    fn drain_all() {
        let mut buf = alloc_ring(4, 8);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };

        for i in 0u8..5 {
            producer.try_push(&[i, 0, 0, 0]);
        }

        let mut collected = Vec::new();

        consumer.drain(|slot| collected.push(slot[0]));

        assert_eq!(collected, vec![0, 1, 2, 3, 4]);
        assert!(consumer.is_empty());
    }

    #[test]
    fn short_data_zero_pads() {
        let mut buf = alloc_ring(8, 4);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };

        producer.try_push(&[1, 2]);

        let mut out = [0xFF; 8];

        consumer.try_pop(&mut out);

        assert_eq!(out, [1, 2, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn required_size_calculation() {
        assert_eq!(required_size(8, 4), HEADER_SIZE + 32);
        assert_eq!(required_size(16, 64), HEADER_SIZE + 1024);
    }

    #[test]
    fn cached_path_hits_acquire_fence() {
        let mut buf = alloc_ring(4, 4);
        let mut producer = unsafe { Producer::new(buf.as_mut_ptr()) };
        let mut consumer = unsafe { Consumer::new(buf.as_ptr()) };

        producer.try_push(&[1, 0, 0, 0]);
        producer.try_push(&[2, 0, 0, 0]);

        // First pop loads write_ptr with Acquire and caches it.
        let mut out = [0u8; 4];

        assert!(consumer.try_pop(&mut out));
        assert_eq!(out[0], 1);
        // Second pop uses cached_write (no new Acquire load) — the
        // fence(Acquire) in the else branch must order the slot read.
        assert!(consumer.try_pop(&mut out));
        assert_eq!(out[0], 2);
    }
}
