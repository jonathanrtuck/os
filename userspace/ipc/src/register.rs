//! State register — seqlock-based shared memory for continuous state
//! where only the latest value matters (e.g., pointer position).
//!
//! Layout in the shared VMO:
//! ```text
//! [generation: u64][value: T bytes, padded to 8-byte alignment]
//! ```
//!
//! Writers bump the generation through an odd intermediate (write in
//! progress) to an even final (write complete). Readers spin on odd
//! generations and retry if the generation changed during the read.

use core::sync::atomic::{AtomicU64, Ordering, fence};

pub const HEADER_SIZE: usize = core::mem::size_of::<AtomicU64>();

pub fn required_size(value_size: usize) -> usize {
    HEADER_SIZE + align_up(value_size, 8)
}

fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

pub fn init(base: *mut u8) {
    // SAFETY: caller guarantees base points to at least HEADER_SIZE
    // bytes of writable memory, aligned to 8 bytes.
    let counter = unsafe { &*(base as *const AtomicU64) };

    counter.store(0, Ordering::Relaxed);
}

pub struct Writer {
    base: *mut u8,
    generation: u64,
    value_size: usize,
}

impl Writer {
    /// # Safety
    /// `base` must point to an initialized register region with at least
    /// `required_size(value_size)` bytes. The writer must be the sole
    /// writer. The pointer must remain valid for the writer's lifetime.
    /// `base` must be 8-byte aligned.
    pub unsafe fn new(base: *mut u8, value_size: usize) -> Self {
        Self {
            base,
            generation: 0,
            value_size,
        }
    }

    fn gen_ptr(&self) -> &AtomicU64 {
        // SAFETY: base is valid and aligned per constructor contract.
        unsafe { &*(self.base as *const AtomicU64) }
    }

    fn value_ptr(&self) -> *mut u8 {
        // SAFETY: base + HEADER_SIZE is within the allocation.
        unsafe { self.base.add(HEADER_SIZE) }
    }

    pub fn write(&mut self, data: &[u8]) {
        let len = data.len().min(self.value_size);

        // Odd generation signals "write in progress"
        self.generation += 1;
        self.gen_ptr().store(self.generation, Ordering::Release);

        // SAFETY: value region is within the allocation and we are the
        // sole writer.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.value_ptr(), len);

            if len < self.value_size {
                core::ptr::write_bytes(self.value_ptr().add(len), 0, self.value_size - len);
            }
        }

        // Even generation signals "write complete"
        self.generation += 1;
        self.gen_ptr().store(self.generation, Ordering::Release);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

pub struct Reader {
    base: *const u8,
    last_generation: u64,
    value_size: usize,
}

impl Reader {
    /// # Safety
    /// `base` must point to an initialized register region with at least
    /// `required_size(value_size)` bytes. The reader must not write to
    /// the memory. The pointer must remain valid for the reader's lifetime.
    /// `base` must be 8-byte aligned.
    pub unsafe fn new(base: *const u8, value_size: usize) -> Self {
        Self {
            base,
            last_generation: 0,
            value_size,
        }
    }

    fn gen_ptr(&self) -> &AtomicU64 {
        // SAFETY: base is valid and aligned per constructor contract.
        unsafe { &*(self.base as *const AtomicU64) }
    }

    fn value_ptr(&self) -> *const u8 {
        // SAFETY: base + HEADER_SIZE is within the allocation.
        unsafe { self.base.add(HEADER_SIZE) }
    }

    /// Read the current value if the generation has changed since the
    /// last successful read. Returns `true` if a new value was read.
    pub fn try_read(&mut self, out: &mut [u8]) -> bool {
        let len = out.len().min(self.value_size);

        loop {
            let gen1 = self.gen_ptr().load(Ordering::Acquire);

            if gen1 == self.last_generation {
                return false;
            }

            if gen1 & 1 != 0 {
                core::hint::spin_loop();

                continue;
            }

            // Full load barrier: on AArch64, the Acquire load above only
            // orders *that* load vs later atomics — it does NOT prevent
            // the CPU from speculating the non-atomic copy_nonoverlapping
            // reads ahead of it. fence(Acquire) issues dmb ish.
            fence(Ordering::Acquire);

            // SAFETY: value region is within the allocation and the even
            // generation means no concurrent write.
            unsafe {
                core::ptr::copy_nonoverlapping(self.value_ptr(), out.as_mut_ptr(), len);
            }

            // Prevent the compiler/CPU from sinking the data reads past
            // the gen2 check.
            fence(Ordering::Acquire);

            let gen2 = self.gen_ptr().load(Ordering::Relaxed);

            if gen1 == gen2 {
                self.last_generation = gen1;

                return true;
            }

            core::hint::spin_loop();
        }
    }

    /// Read the current value unconditionally, even if the generation
    /// hasn't changed.
    pub fn read(&mut self, out: &mut [u8]) {
        let len = out.len().min(self.value_size);

        loop {
            let gen1 = self.gen_ptr().load(Ordering::Acquire);

            if gen1 & 1 != 0 {
                core::hint::spin_loop();

                continue;
            }

            fence(Ordering::Acquire);

            unsafe {
                core::ptr::copy_nonoverlapping(self.value_ptr(), out.as_mut_ptr(), len);
            }

            fence(Ordering::Acquire);

            let gen2 = self.gen_ptr().load(Ordering::Relaxed);

            if gen1 == gen2 {
                self.last_generation = gen1;

                return;
            }

            core::hint::spin_loop();
        }
    }

    pub fn has_changed(&self) -> bool {
        let current = self.gen_ptr().load(Ordering::Acquire);

        current != self.last_generation && current & 1 == 0
    }
}

unsafe impl Send for Writer {}
unsafe impl Send for Reader {}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{vec, vec::Vec};

    use super::*;

    fn alloc_register(value_size: usize) -> Vec<u8> {
        let size = required_size(value_size);
        let mut buf = vec![0u8; size + 8]; // extra for alignment
        let offset = buf.as_ptr().align_offset(8);
        let aligned = &mut buf[offset..offset + size];

        init(aligned.as_mut_ptr());

        buf
    }

    fn aligned_ptr(buf: &mut [u8]) -> *mut u8 {
        let offset = buf.as_ptr().align_offset(8);

        unsafe { buf.as_mut_ptr().add(offset) }
    }

    #[test]
    fn write_then_read() {
        let mut buf = alloc_register(8);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 8) };
        let mut reader = unsafe { Reader::new(base as *const u8, 8) };

        writer.write(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let mut out = [0u8; 8];

        assert!(reader.try_read(&mut out));
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn no_change_returns_false() {
        let mut buf = alloc_register(4);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 4) };
        let mut reader = unsafe { Reader::new(base as *const u8, 4) };

        writer.write(&[1, 2, 3, 4]);

        let mut out = [0u8; 4];

        assert!(reader.try_read(&mut out));
        assert!(!reader.try_read(&mut out));
    }

    #[test]
    fn multiple_writes_latest_wins() {
        let mut buf = alloc_register(4);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 4) };
        let mut reader = unsafe { Reader::new(base as *const u8, 4) };

        writer.write(&[1, 0, 0, 0]);
        writer.write(&[2, 0, 0, 0]);
        writer.write(&[3, 0, 0, 0]);

        let mut out = [0u8; 4];

        assert!(reader.try_read(&mut out));
        assert_eq!(out[0], 3);
    }

    #[test]
    fn generation_increments_by_two() {
        let mut buf = alloc_register(4);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 4) };

        assert_eq!(writer.generation(), 0);

        writer.write(&[0; 4]);

        assert_eq!(writer.generation(), 2);

        writer.write(&[0; 4]);

        assert_eq!(writer.generation(), 4);
    }

    #[test]
    fn read_unconditional() {
        let mut buf = alloc_register(4);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 4) };
        let mut reader = unsafe { Reader::new(base as *const u8, 4) };

        writer.write(&[42, 0, 0, 0]);

        let mut out = [0u8; 4];

        reader.read(&mut out);

        assert_eq!(out[0], 42);

        // Read again without new write — still returns the value.
        let mut out2 = [0u8; 4];

        reader.read(&mut out2);

        assert_eq!(out2[0], 42);
    }

    #[test]
    fn has_changed() {
        let mut buf = alloc_register(4);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 4) };
        let mut reader = unsafe { Reader::new(base as *const u8, 4) };

        assert!(!reader.has_changed());

        writer.write(&[1, 0, 0, 0]);

        assert!(reader.has_changed());

        let mut out = [0u8; 4];

        reader.try_read(&mut out);

        assert!(!reader.has_changed());
    }

    #[test]
    fn required_size_aligned() {
        assert_eq!(required_size(1), HEADER_SIZE + 8);
        assert_eq!(required_size(7), HEADER_SIZE + 8);
        assert_eq!(required_size(8), HEADER_SIZE + 8);
        assert_eq!(required_size(9), HEADER_SIZE + 16);
        assert_eq!(required_size(16), HEADER_SIZE + 16);
    }

    #[test]
    fn short_write_zero_pads() {
        let mut buf = alloc_register(8);
        let base = aligned_ptr(&mut buf);
        let mut writer = unsafe { Writer::new(base, 8) };
        let mut reader = unsafe { Reader::new(base as *const u8, 8) };

        writer.write(&[1, 2]);

        let mut out = [0xFF; 8];

        reader.try_read(&mut out);

        assert_eq!(out, [1, 2, 0, 0, 0, 0, 0, 0]);
    }
}
