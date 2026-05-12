//! Userspace virtio MMIO transport and split virtqueue.
//!
//! Pure library — no syscalls, no allocator. Drivers allocate DMA memory
//! and map MMIO via the `abi` crate, then hand the addresses here.

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

const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_FAILED: u32 = 128;

pub const PAGE_SIZE: usize = 16384;

pub const DEFAULT_QUEUE_SIZE: u32 = 128;

pub const DESC_F_NEXT: u16 = 1;
pub const DESC_F_WRITE: u16 = 2;

pub const VIRTIO_MAGIC: u32 = 0x7472_6976;

pub const DEVICE_NET: u32 = 1;
pub const DEVICE_BLK: u32 = 2;
pub const DEVICE_RNG: u32 = 4;
pub const DEVICE_9P: u32 = 9;
pub const DEVICE_INPUT: u32 = 18;
pub const DEVICE_METAL: u32 = 22;
pub const DEVICE_SND: u32 = 25;
pub const DEVICE_VIDEO_DECODE: u32 = 30;

pub const MMIO_STRIDE: usize = 0x200;
pub const MAX_DEVICES: usize = 10;
pub const SPI_BASE_INTID: u32 = 48;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}

/// Scan the virtio MMIO region for a device with the given ID.
///
/// Returns the `Device` and its slot index (0-based), or `None` if not found.
pub fn find_device(virtio_va: usize, device_id: u32) -> Option<(Device, u32)> {
    find_device_from(virtio_va, device_id, 0)
}

pub fn find_device_from(virtio_va: usize, device_id: u32, start: usize) -> Option<(Device, u32)> {
    for i in start..MAX_DEVICES {
        let base = virtio_va + i * MMIO_STRIDE;
        let dev = Device::new(base);

        if dev.is_valid() && dev.device_id() == device_id {
            return Some((dev, i as u32));
        }
    }

    None
}

/// A virtio-mmio device backed by a mapped MMIO region.
pub struct Device {
    base: usize,
}

impl Device {
    pub fn new(base: usize) -> Self {
        Self { base }
    }

    fn read(&self, offset: usize) -> u32 {
        read32(self.base + offset)
    }

    fn write(&self, offset: usize, val: u32) {
        write32(self.base + offset, val);
    }

    pub fn magic(&self) -> u32 {
        self.read(REG_MAGIC)
    }

    pub fn version(&self) -> u32 {
        self.read(REG_VERSION)
    }

    pub fn device_id(&self) -> u32 {
        self.read(REG_DEVICE_ID)
    }

    pub fn is_valid(&self) -> bool {
        self.magic() == VIRTIO_MAGIC && self.version() == 2
    }

    pub fn read_isr(&self) -> u32 {
        self.read(REG_INTERRUPT_STATUS)
    }

    pub fn ack_interrupt(&self) -> u32 {
        let status = self.read(REG_INTERRUPT_STATUS);

        self.write(REG_INTERRUPT_ACK, status);

        status
    }

    pub fn config_read8(&self, offset: usize) -> u8 {
        read8(self.base + REG_CONFIG + offset)
    }

    pub fn config_read32(&self, offset: usize) -> u32 {
        read32(self.base + REG_CONFIG + offset)
    }

    pub fn config_write32(&self, offset: usize, val: u32) {
        write32(self.base + REG_CONFIG + offset, val);
    }

    pub fn config_read64(&self, offset: usize) -> u64 {
        let lo = self.config_read32(offset) as u64;
        let hi = self.config_read32(offset + 4) as u64;

        lo | (hi << 32)
    }

    pub fn driver_ok(&self) {
        let status = self.read(REG_STATUS);

        self.write(REG_STATUS, status | STATUS_DRIVER_OK);
    }

    pub fn read_device_features(&self) -> u64 {
        self.write(REG_DEVICE_FEATURES_SEL, 0);

        let lo = self.read(REG_DEVICE_FEATURES) as u64;

        self.write(REG_DEVICE_FEATURES_SEL, 1);

        let hi = self.read(REG_DEVICE_FEATURES) as u64;

        lo | (hi << 32)
    }

    pub fn write_driver_features(&self, features: u64) {
        self.write(REG_DRIVER_FEATURES_SEL, 0);
        self.write(REG_DRIVER_FEATURES, features as u32);
        self.write(REG_DRIVER_FEATURES_SEL, 1);
        self.write(REG_DRIVER_FEATURES, (features >> 32) as u32);
    }

    pub fn negotiate(&self) -> bool {
        self.negotiate_features(0).0
    }

    pub fn negotiate_features(&self, requested: u64) -> (bool, u64) {
        self.reset();
        self.write(REG_STATUS, STATUS_ACKNOWLEDGE);

        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER;

        self.write(REG_STATUS, status);

        let offered = self.read_device_features();
        let accepted = offered & requested;

        self.write_driver_features(accepted);

        let status = status | STATUS_FEATURES_OK;

        self.write(REG_STATUS, status);

        if self.read(REG_STATUS) & STATUS_FEATURES_OK == 0 {
            self.write(REG_STATUS, STATUS_FAILED);

            return (false, 0);
        }

        (true, accepted)
    }

    /// Notify the device that virtqueue `index` has new buffers.
    ///
    /// DSB SY ensures all prior writes to virtqueue memory are visible to
    /// the hypervisor before the MMIO write triggers device processing.
    pub fn notify(&self, index: u32) {
        // SAFETY: DSB SY is a barrier hint with no side effects beyond
        // ordering. Required before MMIO write for hypervisor visibility.
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };

        self.write(REG_QUEUE_NOTIFY, index);
    }

    pub fn queue_max_size(&self, index: u32) -> u32 {
        self.write(REG_QUEUE_SEL, index);
        self.read(REG_QUEUE_NUM_MAX)
    }

    pub fn reset(&self) {
        self.write(REG_STATUS, 0);
    }

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

/// Split virtqueue — descriptor table, available ring, and used ring.
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

impl Virtqueue {
    /// Create a virtqueue over a pre-allocated, zeroed DMA buffer.
    ///
    /// `va` and `pa` must point to the same physical memory (identity-mapped
    /// DMA VMO). The buffer must be at least `total_bytes(size)` bytes.
    pub fn new(size: u32, va: usize, pa: u64) -> Self {
        let desc_bytes = size as usize * core::mem::size_of::<Descriptor>();
        let avail_bytes = 6 + size as usize * 2;
        let avail_offset = desc_bytes;
        let used_offset = (avail_offset + avail_bytes + 3) & !3;
        let desc_va = va;
        let avail_va = va + avail_offset;
        let used_va = va + used_offset;
        let desc_ptr = desc_va as *mut Descriptor;

        for i in 0..size {
            // SAFETY: desc_ptr + i is within the DMA allocation.
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

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn desc_pa(&self) -> u64 {
        self.desc_pa
    }

    pub fn avail_pa(&self) -> u64 {
        self.avail_pa
    }

    pub fn used_pa(&self) -> u64 {
        self.used_pa
    }

    /// Total bytes needed for a virtqueue of `size` entries.
    pub fn total_bytes(size: u32) -> usize {
        let desc_bytes = size as usize * core::mem::size_of::<Descriptor>();
        let avail_bytes = 6 + size as usize * 2;
        let avail_offset = desc_bytes;
        let used_offset = (avail_offset + avail_bytes + 3) & !3;
        let used_bytes = 6 + size as usize * core::mem::size_of::<UsedElem>();

        used_offset + used_bytes
    }

    /// Number of 16 KiB pages needed for a virtqueue of `size` entries.
    pub fn pages_needed(size: u32) -> usize {
        (Self::total_bytes(size) + PAGE_SIZE - 1) / PAGE_SIZE
    }

    fn free_descriptor_chain(&mut self, mut idx: u16) {
        loop {
            if idx as u32 >= self.size {
                return;
            }

            // SAFETY: idx < self.size, desc_va is within the DMA allocation.
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

    /// Poll for a completed request from the used ring.
    pub fn pop_used(&mut self) -> Option<UsedElem> {
        // SAFETY: used_va + 2 is the used ring index, within DMA allocation.
        let used_idx = unsafe {
            let idx_ptr = (self.used_va + 2) as *const u16;

            core::ptr::read_volatile(idx_ptr)
        };

        if self.last_used_idx == used_idx {
            return None;
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        // SAFETY: used_va + 4 is the used ring elements array.
        let elem = unsafe {
            let ring_ptr = (self.used_va + 4) as *const UsedElem;

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
    pub fn push_chain(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.is_empty() || (self.num_free as usize) < bufs.len() {
            return None;
        }

        let head = self.free_head;
        let mut current = head;

        for (i, &(pa, len, writable)) in bufs.iter().enumerate() {
            // SAFETY: current < self.size (maintained by free list invariant).
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

        // SAFETY: avail_va is within the DMA allocation.
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
}

#[inline(always)]
fn read8(addr: usize) -> u8 {
    // SAFETY: addr is within a mapped MMIO region.
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}

#[inline(always)]
fn read32(addr: usize) -> u32 {
    // SAFETY: addr is within a mapped MMIO region.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[inline(always)]
fn write32(addr: usize, val: u32) {
    // SAFETY: addr is within a mapped MMIO region.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}
