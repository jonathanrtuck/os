//! Userspace virtio console driver.
//!
//! Receives device info (MMIO PA, IRQ) from the kernel via channel shared
//! memory, maps the device, and writes a test string to validate the
//! userspace driver model.
//!
//! # Shared memory layout (written by kernel before start)
//!
//! ```text
//! offset 0:  mmio_pa (u64) — physical address of the MMIO region
//! offset 8:  irq     (u32) — GIC IRQ number
//! ```

#![no_std]
#![no_main]

/// Channel shared memory base (handle 0).
const SHM: *const u8 = 0x4000_0000 as *const u8; // must match kernel paging::CHANNEL_SHM_BASE
const VIRTQ_TX: u32 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read device info from shared memory.
    let mmio_pa = unsafe { core::ptr::read_volatile(SHM as *const u64) };
    let irq = unsafe { core::ptr::read_volatile(SHM.add(8) as *const u32) };
    // Map the 4K page containing the MMIO region. Virtio-mmio slots have
    // 0x200 stride, so most sit at sub-page offsets within a 4K page.
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000).unwrap_or_else(|_| {
        sys::print(b"virtio-console: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Negotiate features.
    if !device.negotiate() {
        sys::print(b"virtio-console: negotiate failed\n");
        sys::exit();
    }

    // Register for device interrupt before driver_ok.
    let irq_handle = sys::interrupt_register(irq).unwrap_or_else(|_| {
        sys::print(b"virtio-console: interrupt_register failed\n");
        sys::exit();
    });
    // Allocate DMA for the TX virtqueue.
    let queue_size = core::cmp::min(device.queue_max_size(VIRTQ_TX), virtio::DEFAULT_QUEUE_SIZE);
    let order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-console: dma_alloc (vq) failed\n");
        sys::exit();
    });
    // Zero the virtqueue memory.
    let vq_bytes = (1usize << order) * 4096;

    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };

    let mut tx = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        VIRTQ_TX,
        queue_size,
        tx.desc_pa(),
        tx.avail_pa(),
        tx.used_pa(),
    );
    device.driver_ok();

    // Allocate a DMA buffer for the data payload.
    let mut buf_pa: u64 = 0;
    let buf_va = sys::dma_alloc(0, &mut buf_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-console: dma_alloc (buf) failed\n");
        sys::exit();
    });
    // Copy message into DMA buffer and submit.
    let msg = b"virtio console ok\n";

    unsafe {
        core::ptr::copy_nonoverlapping(msg.as_ptr(), buf_va as *mut u8, msg.len());
    }

    tx.push(buf_pa, msg.len() as u32, false);
    device.notify(VIRTQ_TX);

    // Wait for completion interrupt (blocks instead of spinning).
    let _ = sys::wait(&[irq_handle], u64::MAX);

    device.ack_interrupt();
    tx.pop_used();

    let _ = sys::interrupt_ack(irq_handle);
    let _ = sys::dma_free(buf_va as u64, 0);
    // Signal the kernel channel to indicate we're done.
    let _ = sys::channel_signal(0);

    sys::exit();
}
