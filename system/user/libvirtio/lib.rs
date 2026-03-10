//! Userspace virtio MMIO transport and split virtqueue.
//!
//! Pure library — no syscalls. Drivers allocate DMA memory and map MMIO
//! via `libsys` syscalls, then hand the addresses to this library.
//!
//! # Usage
//!
//! ```text
//! let mmio_va = sys::device_map(mmio_pa, 0x200);
//! let device = Device::new(mmio_va as usize);
//! device.negotiate();
//!
//! let mut vq_pa: u64 = 0;
//! let vq_va = sys::dma_alloc(0, &mut vq_pa);
//! let vq = Virtqueue::new(128, vq_va as usize, vq_pa);
//! device.setup_queue(0, vq.size(), vq.desc_pa(), vq.avail_pa(), vq.used_pa());
//! device.driver_ok();
//! ```

#![no_std]

const REG_MAGIC: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_QUEUE_SEL: usize = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize = 0x038;
const REG_QUEUE_READY: usize = 0x044;
const REG_QUEUE_NOTIFY: usize = 0x050;
const REG_INTERRUPT_STATUS: usize = 0x060;
const REG_INTERRUPT_ACK: usize = 0x064;
const REG_STATUS: usize = 0x070;
const REG_QUEUE_DESC_LOW: usize = 0x080;
const REG_QUEUE_DESC_HIGH: usize = 0x084;
const REG_QUEUE_DRIVER_LOW: usize = 0x090;
const REG_QUEUE_DRIVER_HIGH: usize = 0x094;
const REG_QUEUE_DEVICE_LOW: usize = 0x0A0;
const REG_QUEUE_DEVICE_HIGH: usize = 0x0A4;
const REG_CONFIG: usize = 0x100;
// Device status bits.
const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_FAILED: u32 = 128;

/// Default queue size. At 128 entries, all three regions fit in one 4 KiB page.
pub const DEFAULT_QUEUE_SIZE: u32 = 128;
/// Buffer continues in the next descriptor.
pub const DESC_F_NEXT: u16 = 1;
/// Device writes to this buffer (vs. reads).
pub const DESC_F_WRITE: u16 = 2;

#[inline(always)]
fn read8(addr: usize) -> u8 {
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}
#[inline(always)]
fn read32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
#[inline(always)]
fn write32(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// A single virtqueue descriptor (16 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}
/// A virtio-mmio device. Wraps a mapped MMIO region.
pub struct Device {
    base: usize,
}
/// Element in the used ring (8 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}
/// Manages a single virtqueue's descriptor table, available ring, and used ring.
///
/// Memory layout within the DMA allocation:
/// ```text
/// [desc_table: size * 16]  [avail_ring: 6 + size * 2]  [pad]  [used_ring: 6 + size * 8]
/// ```
pub struct Virtqueue {
    size: u32,
    desc_va: usize,
    avail_va: usize,
    used_va: usize,
    desc_pa: u64,
    avail_pa: u64,
    used_pa: u64,
    free_head: u16,
    num_free: u32,
    last_used_idx: u16,
}

impl Device {
    /// Create a device from a mapped MMIO base VA.
    pub fn new(base: usize) -> Self {
        Self { base }
    }

    fn read(&self, offset: usize) -> u32 {
        read32(self.base + offset)
    }
    fn write(&self, offset: usize, val: u32) {
        write32(self.base + offset, val);
    }

    /// Acknowledge pending interrupts.
    pub fn ack_interrupt(&self) -> u32 {
        let status = self.read(REG_INTERRUPT_STATUS);

        self.write(REG_INTERRUPT_ACK, status);

        status
    }
    /// Read a byte from device-specific config space.
    pub fn config_read8(&self, offset: usize) -> u8 {
        read8(self.base + REG_CONFIG + offset)
    }
    /// Read a 32-bit word from device-specific config space.
    pub fn config_read32(&self, offset: usize) -> u32 {
        read32(self.base + REG_CONFIG + offset)
    }
    /// Read a 64-bit word from device-specific config space.
    pub fn config_read64(&self, offset: usize) -> u64 {
        let lo = self.config_read32(offset) as u64;
        let hi = self.config_read32(offset + 4) as u64;

        lo | (hi << 32)
    }
    /// Signal that the driver is fully configured.
    pub fn driver_ok(&self) {
        let status = self.read(REG_STATUS);

        self.write(REG_STATUS, status | STATUS_DRIVER_OK);
    }
    /// Perform feature negotiation. Accepts no device-specific features.
    pub fn negotiate(&self) -> bool {
        self.reset();
        self.write(REG_STATUS, STATUS_ACKNOWLEDGE);

        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER;

        self.write(REG_STATUS, status);
        // Read device features (word 0 only).
        self.write(REG_DEVICE_FEATURES_SEL, 0);

        let _features = self.read(REG_DEVICE_FEATURES);

        // Accept no features.
        self.write(REG_DRIVER_FEATURES_SEL, 0);
        self.write(REG_DRIVER_FEATURES, 0);
        self.write(REG_DRIVER_FEATURES_SEL, 1);
        self.write(REG_DRIVER_FEATURES, 0);

        let status = status | STATUS_FEATURES_OK;

        self.write(REG_STATUS, status);

        if self.read(REG_STATUS) & STATUS_FEATURES_OK == 0 {
            self.write(REG_STATUS, STATUS_FAILED);

            return false;
        }

        true
    }
    /// Notify the device that virtqueue `index` has new buffers.
    pub fn notify(&self, index: u32) {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

        self.write(REG_QUEUE_NOTIFY, index);
    }
    /// Read the maximum queue size for virtqueue `index`.
    pub fn queue_max_size(&self, index: u32) -> u32 {
        self.write(REG_QUEUE_SEL, index);
        self.read(REG_QUEUE_NUM_MAX)
    }
    /// Reset the device.
    pub fn reset(&self) {
        self.write(REG_STATUS, 0);
    }
    /// Configure a virtqueue: set size, physical addresses, and mark ready.
    pub fn setup_queue(&self, index: u32, size: u32, desc_pa: u64, avail_pa: u64, used_pa: u64) {
        self.write(REG_QUEUE_SEL, index);
        self.write(REG_QUEUE_NUM, size);
        self.write(REG_QUEUE_DESC_LOW, desc_pa as u32);
        self.write(REG_QUEUE_DESC_HIGH, (desc_pa >> 32) as u32);
        self.write(REG_QUEUE_DRIVER_LOW, avail_pa as u32);
        self.write(REG_QUEUE_DRIVER_HIGH, (avail_pa >> 32) as u32);
        self.write(REG_QUEUE_DEVICE_LOW, used_pa as u32);
        self.write(REG_QUEUE_DEVICE_HIGH, (used_pa >> 32) as u32);
        self.write(REG_QUEUE_READY, 1);
    }
}

impl Virtqueue {
    /// Create a virtqueue over a pre-allocated DMA buffer.
    ///
    /// The caller must allocate DMA memory of at least `allocation_order(size)`
    /// pages and zero the buffer before calling this.
    pub fn new(size: u32, va: usize, pa: u64) -> Self {
        let desc_bytes = size as usize * core::mem::size_of::<Descriptor>();
        let avail_bytes = 6 + size as usize * 2;
        let avail_offset = desc_bytes;
        let used_offset = (avail_offset + avail_bytes + 3) & !3;
        let desc_va = va;
        let avail_va = va + avail_offset;
        let used_va = va + used_offset;
        // Initialize free descriptor chain: 0 → 1 → 2 → ... → (size-1).
        let desc_ptr = desc_va as *mut Descriptor;

        for i in 0..size {
            unsafe {
                let d = &mut *desc_ptr.add(i as usize);
                d.next = if i + 1 < size { (i + 1) as u16 } else { 0xFFFF };
            }
        }

        Self {
            size,
            desc_va,
            avail_va,
            used_va,
            desc_pa: pa,
            avail_pa: pa + avail_offset as u64,
            used_pa: pa + used_offset as u64,
            free_head: 0,
            num_free: size,
            last_used_idx: 0,
        }
    }

    /// Calculate the DMA allocation order needed for `size` descriptors.
    pub fn allocation_order(size: u32) -> u32 {
        let desc_bytes = size as usize * core::mem::size_of::<Descriptor>();
        let avail_bytes = 6 + size as usize * 2;
        let avail_offset = desc_bytes;
        let used_offset = (avail_offset + avail_bytes + 3) & !3;
        let used_bytes = 6 + size as usize * core::mem::size_of::<UsedElem>();
        let total = used_offset + used_bytes;
        let pages_needed = (total + 4095) / 4096;

        pages_needed.next_power_of_two().trailing_zeros() as u32
    }
    pub fn avail_pa(&self) -> u64 {
        self.avail_pa
    }
    pub fn desc_pa(&self) -> u64 {
        self.desc_pa
    }
    /// Return a descriptor chain to the free list.
    fn free_descriptor_chain(&mut self, mut idx: u16) {
        loop {
            if idx as u32 >= self.size {
                return;
            }

            let desc = unsafe { &mut *(self.desc_va as *mut Descriptor).add(idx as usize) };
            let has_next = desc.flags & DESC_F_NEXT != 0;
            let next = desc.next;

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
    /// Poll for a completed request.
    pub fn pop_used(&mut self) -> Option<UsedElem> {
        let used_idx = unsafe {
            let idx_ptr = (self.used_va + 2) as *const u16;
            core::ptr::read_volatile(idx_ptr)
        };

        if self.last_used_idx == used_idx {
            return None;
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        let ring_ptr = (self.used_va + 4) as *const UsedElem;
        let elem = unsafe {
            core::ptr::read_volatile(ring_ptr.add((self.last_used_idx % self.size as u16) as usize))
        };

        self.last_used_idx = self.last_used_idx.wrapping_add(1);

        if (elem.id as u32) < self.size {
            self.free_descriptor_chain(elem.id as u16);
        }

        Some(elem)
    }
    /// Push a single buffer onto the available ring.
    pub fn push(&mut self, buf_pa: u64, buf_len: u32, device_writable: bool) -> Option<u16> {
        self.push_chain(&[(buf_pa, buf_len, device_writable)])
    }
    /// Push a descriptor chain onto the available ring.
    ///
    /// Each element is `(physical_address, length, device_writable)`.
    pub fn push_chain(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.is_empty() || (self.num_free as usize) < bufs.len() {
            return None;
        }

        let head = self.free_head;
        let mut current = head;

        for (i, &(pa, len, writable)) in bufs.iter().enumerate() {
            let desc = unsafe { &mut *(self.desc_va as *mut Descriptor).add(current as usize) };
            let next_free = desc.next;

            desc.addr = pa;
            desc.len = len;
            desc.flags = if writable { DESC_F_WRITE } else { 0 };

            if i + 1 < bufs.len() {
                desc.flags |= DESC_F_NEXT;
                desc.next = next_free;
                current = next_free;
            } else {
                desc.next = 0;
                self.free_head = next_free;
            }

            self.num_free -= 1;
        }

        // Add head to the available ring.
        unsafe {
            let avail_idx_ptr = (self.avail_va + 2) as *mut u16;
            let avail_idx = core::ptr::read_volatile(avail_idx_ptr);
            let ring_ptr = (self.avail_va + 4) as *mut u16;

            core::ptr::write_volatile(ring_ptr.add((avail_idx % self.size as u16) as usize), head);
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
}
