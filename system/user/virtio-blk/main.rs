//! Userspace virtio block driver.
//!
//! Receives device info (MMIO PA, IRQ) from the kernel via channel shared
//! memory, maps the device, reads sector 0, and prints its first 16 bytes.
//!
//! # Shared memory layout (written by kernel before start)
//!
//! ```text
//! offset 0:  mmio_pa (u64) — physical address of the MMIO region
//! offset 8:  irq     (u32) — GIC IRQ number
//! ```

#![no_std]
#![no_main]

const SECTOR_SIZE: usize = 512;
/// Channel shared memory base (handle 0).
const SHM: *const u8 = 0x4000_0000 as *const u8;
const VIRTIO_BLK_T_IN: u32 = 0; // Read
const VIRTQ_REQUEST: u32 = 0;

/// Block request header (16 bytes, device-readable).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

/// Print a u64 in decimal (simple, no alloc).
fn print_u64(mut n: u64) {
    if n == 0 {
        sys::write(b"0");

        return;
    }

    let mut buf = [0u8; 20];
    let mut i = 20;

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    sys::write(&buf[i..]);
}
/// Read a sector and print its first 16 bytes as ASCII.
fn read_and_print_sector(device: &virtio::Device, vq: &mut virtio::Virtqueue, sector: u64) {
    // Allocate a DMA page. Layout:
    //   [0..16)    BlkReqHeader  (device-readable)
    //   [16..528)  sector data   (device-writable)
    //   [528]      status byte   (device-writable)
    let mut buf_pa: u64 = 0;
    let buf_va = sys::dma_alloc(0, &mut buf_pa);

    if buf_va < 0 {
        sys::write(b"virtio-blk: dma_alloc (buf) failed\n");

        return;
    }

    let buf_ptr = buf_va as *mut u8;

    // Zero the buffer.
    unsafe { core::ptr::write_bytes(buf_ptr, 0, 4096) };

    let header_pa = buf_pa;
    let data_pa = buf_pa + 16;
    let status_pa = buf_pa + 16 + SECTOR_SIZE as u64;

    // Write request header.
    unsafe {
        let header = buf_ptr as *mut BlkReqHeader;

        (*header).req_type = VIRTIO_BLK_T_IN;
        (*header).reserved = 0;
        (*header).sector = sector;
        // Sentinel status byte — device overwrites with 0 on success.
        *buf_ptr.add(16 + SECTOR_SIZE) = 0xFF;
    }

    // 3-descriptor chain: header (read) → data (write) → status (write).
    vq.push_chain(&[
        (header_pa, 16, false),
        (data_pa, SECTOR_SIZE as u32, true),
        (status_pa, 1, true),
    ]);
    device.notify(VIRTQ_REQUEST);
    vq.wait_used();

    // Check status.
    let status = unsafe { *buf_ptr.add(16 + SECTOR_SIZE) };

    if status != 0 {
        sys::write(b"     sector 0 - read failed\n");
    } else {
        sys::write(b"     sector 0 - ");

        // Print first 16 bytes as ASCII where printable, '.' otherwise.
        let data = unsafe { core::slice::from_raw_parts(buf_ptr.add(16), 16) };
        let mut ascii = [b'.'; 16];

        for (i, &b) in data.iter().enumerate() {
            if b >= 0x20 && b < 0x7F {
                ascii[i] = b;
            }
        }

        sys::write(&ascii);
        sys::write(b"\n");
    }

    sys::dma_free(buf_va as u64, 0);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read device info from shared memory.
    let mmio_pa = unsafe { core::ptr::read_volatile(SHM as *const u64) };
    let _irq = unsafe { core::ptr::read_volatile(SHM.add(8) as *const u32) };
    // Map the 4K page containing the MMIO region. Virtio-mmio slots have
    // 0x200 stride, so most sit at sub-page offsets within a 4K page.
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000);

    if page_va < 0 {
        sys::write(b"virtio-blk: device_map failed\n");
        sys::exit();
    }

    let device = virtio::Device::new(page_va as usize + page_offset as usize);

    // Negotiate features.
    if !device.negotiate() {
        sys::write(b"virtio-blk: negotiate failed\n");
        sys::exit();
    }

    // Read capacity from device config.
    let capacity = device.config_read64(0);
    // Allocate DMA for the request virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_REQUEST),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(order, &mut vq_pa);

    if vq_va < 0 {
        sys::write(b"virtio-blk: dma_alloc (vq) failed\n");
        sys::exit();
    }

    // Zero the virtqueue memory.
    let vq_bytes = (1usize << order) * 4096;

    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };

    let mut vq = virtio::Virtqueue::new(queue_size, vq_va as usize, vq_pa);

    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    // Print capacity.
    sys::write(b"  \xF0\x9F\x94\x8C virtio - blk capacity=");

    print_u64(capacity);

    sys::write(b" sectors\n");

    // Read sector 0 if the device has any capacity.
    if capacity > 0 {
        read_and_print_sector(&device, &mut vq, 0);
    }

    // Signal the kernel channel to indicate we're done.
    sys::channel_signal(0);
    sys::exit();
}
