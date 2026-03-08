//! Virtio console driver.
//!
//! Implements transmit-only console output via virtqueue 1 (TX).
//! Used to demonstrate end-to-end virtio I/O. The PL011 UART remains
//! the primary kernel output path.

use super::virtqueue::{Virtqueue, DEFAULT_QUEUE_SIZE};
use super::Device;
use crate::memory;
use crate::mmio;
use crate::page_alloc;

const VIRTQ_TX: u32 = 1;

/// Transmit-only virtio console.
pub struct Console {
    device: Device,
    tx: Virtqueue,
}

impl Console {
    /// Initialize the console device and its TX virtqueue.
    pub fn new(device: Device) -> Option<Self> {
        device.negotiate().ok()?;

        let max_size = device.queue_max_size(VIRTQ_TX);
        let size = core::cmp::min(max_size, DEFAULT_QUEUE_SIZE);
        let tx = Virtqueue::new(size)?;

        device.setup_queue(VIRTQ_TX, size, tx.desc_pa(), tx.avail_pa(), tx.used_pa());
        device.driver_ok();

        Some(Self { device, tx })
    }

    /// Write bytes to the console. Copies data into a DMA buffer, submits
    /// it to the device, and waits for completion.
    pub fn write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let pa = match page_alloc::alloc_frame() {
            Some(pa) => pa,
            None => return,
        };
        let va = memory::phys_to_virt(pa) as *mut u8;
        let len = core::cmp::min(data.len(), 4096);

        // SAFETY: pa was just allocated (4 KiB zeroed frame).
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), va, len);
        }

        // Clean DMA buffer: flush our data from cache to RAM so the device
        // sees it. ARM caches are not coherent with DMA by default.
        mmio::cache_clean_invalidate_range(va as usize, len);

        self.tx.push(pa as u64, len as u32, false);
        self.device.notify(VIRTQ_TX);
        self.tx.wait_used();

        page_alloc::free_frame(pa);
    }
}

/// Demonstrate console I/O by writing a test string.
pub fn demo(device: Device) {
    if let Some(mut console) = Console::new(device) {
        crate::uart::puts("  🔌 virtio: console\n");

        console.write(b"virtio console ok\n");
    }
}
