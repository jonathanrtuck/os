//! Virtio block driver.
//!
//! Supports synchronous sector-level read and write via a single request
//! virtqueue (index 0). Each operation uses a 3-descriptor chain:
//! header (device-readable) → data → status (device-writable).

use super::virtqueue::{Virtqueue, DEFAULT_QUEUE_SIZE};
use super::Device;
use crate::memory;
use crate::mmio;
use crate::page_alloc;
use crate::uart;

const SECTOR_SIZE: usize = 512;
// Block request types.
const VIRTIO_BLK_T_IN: u32 = 0; // Read
const VIRTQ_REQUEST: u32 = 0;

/// Block request header (16 bytes, device-readable).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}
/// Virtio block device.
pub struct Block {
    device: Device,
    vq: Virtqueue,
    capacity: u64,
}

impl Block {
    /// Initialize the block device and its request virtqueue.
    pub fn new(device: Device) -> Option<Self> {
        device.negotiate().ok()?;

        // Capacity is at config offset 0, as a le64 (in 512-byte sectors).
        let capacity = device.config_read64(0);
        let max_size = device.queue_max_size(VIRTQ_REQUEST);
        let size = core::cmp::min(max_size, DEFAULT_QUEUE_SIZE);
        let vq = Virtqueue::new(size)?;

        device.setup_queue(
            VIRTQ_REQUEST,
            size,
            vq.desc_pa(),
            vq.avail_pa(),
            vq.used_pa(),
        );
        device.driver_ok();

        Some(Self {
            device,
            vq,
            capacity,
        })
    }

    /// Capacity in 512-byte sectors.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
    /// Read a single 512-byte sector into `buf`.
    ///
    /// `buf` must be at least 512 bytes.
    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        if sector >= self.capacity {
            return Err("sector out of range");
        }
        if buf.len() < SECTOR_SIZE {
            return Err("buffer too small");
        }

        // Allocate a DMA page. Layout:
        //   [0..16)    BlkReqHeader  (device-readable)
        //   [16..528)  sector data   (device-writable)
        //   [528]      status byte   (device-writable)
        let pa = page_alloc::alloc_frame().ok_or("out of frames")?;
        let va = memory::phys_to_virt(pa);
        let header_pa = pa.as_u64();
        let data_pa = pa.as_u64() + 16;
        let status_pa = pa.as_u64() + 16 + SECTOR_SIZE as u64;

        // SAFETY: pa is a freshly allocated zeroed frame.
        unsafe {
            let header = va as *mut BlkReqHeader;

            (*header).req_type = VIRTIO_BLK_T_IN;
            (*header).reserved = 0;
            (*header).sector = sector;

            // Sentinel — device will overwrite with 0 on success.
            *((va + 16 + SECTOR_SIZE) as *mut u8) = 0xFF;
        }

        // Clean DMA buffers: flush dirty cache lines to RAM so the device
        // sees our header. ARM caches are not coherent with DMA by default.
        mmio::cache_clean_invalidate_range(va, 4096);

        // 3-descriptor chain: header (read) → data (write) → status (write).
        self.vq
            .push_chain(&[
                (header_pa, 16, false),
                (data_pa, SECTOR_SIZE as u32, true),
                (status_pa, 1, true),
            ])
            .ok_or("no free descriptors")?;
        self.device.notify(VIRTQ_REQUEST);
        self.vq.wait_used();

        // Invalidate DMA buffers: discard stale cache lines so we read
        // what the device wrote (data + status), not stale cached values.
        mmio::cache_clean_invalidate_range(va, 4096);

        // Check status.
        let status = unsafe { *((va + 16 + SECTOR_SIZE) as *const u8) };

        if status != 0 {
            page_alloc::free_frame(pa);

            return Err("device error");
        }

        // Copy data to caller's buffer.
        unsafe {
            core::ptr::copy_nonoverlapping((va + 16) as *const u8, buf.as_mut_ptr(), SECTOR_SIZE);
        }

        page_alloc::free_frame(pa);

        Ok(())
    }
}

/// Demonstrate block I/O by reading sector 0 and logging its first 16 bytes.
pub fn demo(device: Device) {
    let mut blk = match Block::new(device) {
        Some(b) => b,
        None => {
            uart::puts("  🔌 virtio: blk init failed\n");

            return;
        }
    };

    uart::puts("  🔌 virtio: blk capacity=");
    uart::put_u64(blk.capacity());
    uart::puts(" sectors\n");

    let mut buf = [0u8; SECTOR_SIZE];

    match blk.read_sector(0, &mut buf) {
        Ok(()) => {
            uart::puts("     sector 0: ");

            // Print first 16 bytes as ASCII where printable, '.' otherwise.
            for &b in &buf[..16] {
                if b >= 0x20 && b < 0x7F {
                    uart::putc(b);
                } else {
                    uart::putc(b'.');
                }
            }

            uart::puts("\n");
        }
        Err(e) => {
            uart::puts("     read failed: ");
            uart::puts(e);
            uart::puts("\n");
        }
    }
}
