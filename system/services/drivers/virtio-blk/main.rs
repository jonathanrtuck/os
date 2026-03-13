//! Userspace virtio block driver.
//!
//! Receives device config via IPC ring buffer from init, maps the device,
//! reads sector 0, and prints its first 16 bytes.

#![no_std]
#![no_main]

const SECTOR_SIZE: usize = 512;
/// Channel shared memory base (first channel in our address space).
const CHANNEL_SHM_BASE: usize = 0x4000_0000;
const VIRTIO_BLK_T_IN: u32 = 0; // Read
const VIRTQ_REQUEST: u32 = 0;
// Protocol message type (must match init's definition).
const MSG_DEVICE_CONFIG: u32 = 1;

/// Block request header (16 bytes, device-readable).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct DeviceConfig {
    mmio_pa: u64,
    irq: u32,
    _pad: u32,
}

/// Format a u64 into a buffer, returning the number of bytes written.
fn format_u64(mut n: u64, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';

        return 1;
    }

    let mut tmp = [0u8; 20];
    let mut i = 20;

    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    let len = 20 - i;

    buf[..len].copy_from_slice(&tmp[i..]);

    len
}
/// Print a u64 in decimal (simple, no alloc).
fn print_u64(mut n: u64) {
    if n == 0 {
        sys::print(b"0");

        return;
    }

    let mut buf = [0u8; 20];
    let mut i = 20;

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    sys::print(&buf[i..]);
}
/// Read a sector and print its first 16 bytes as ASCII.
fn read_and_print_sector(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    sector: u64,
    irq_handle: u8,
) {
    // Allocate a DMA page. Layout:
    //   [0..16)    BlkReqHeader  (device-readable)
    //   [16..528)  sector data   (device-writable)
    //   [528]      status byte   (device-writable)
    let mut buf_pa: u64 = 0;
    let buf_va = match sys::dma_alloc(0, &mut buf_pa) {
        Ok(va) => va,
        Err(_) => {
            sys::print(b"virtio-blk: dma_alloc (buf) failed\n");
            return;
        }
    };
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

    // Wait for completion interrupt (blocks instead of spinning).
    let _ = sys::wait(&[irq_handle], u64::MAX);

    device.ack_interrupt();
    vq.pop_used();

    let _ = sys::interrupt_ack(irq_handle);
    // Check status.
    let status = unsafe { *buf_ptr.add(16 + SECTOR_SIZE) };

    if status != 0 {
        sys::print(b"     sector 0 - read failed\n");
    } else {
        // Print first 16 bytes as ASCII where printable, '.' otherwise.
        let data = unsafe { core::slice::from_raw_parts(buf_ptr.add(16), 16) };
        let mut line = [0u8; 34]; // "     sector 0 - " (16) + 16 ascii + "\n" + pad
        let prefix = b"     sector 0 - ";

        line[..prefix.len()].copy_from_slice(prefix);

        for (i, &b) in data.iter().enumerate() {
            line[prefix.len() + i] = if b >= 0x20 && b < 0x7F { b } else { b'.' };
        }

        line[prefix.len() + 16] = b'\n';

        sys::print(&line[..prefix.len() + 17]);
    }

    let _ = sys::dma_free(buf_va as u64, 0);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read device config from ring buffer (first message, sent by init).
    let ch = unsafe { ipc::Channel::from_base(CHANNEL_SHM_BASE, ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virtio-blk: no config message\n");
        sys::exit();
    }

    let config: DeviceConfig = unsafe { msg.payload_as() };
    let mmio_pa = config.mmio_pa;
    let irq = config.irq;
    // Map the 4K page containing the MMIO region. Virtio-mmio slots have
    // 0x200 stride, so most sit at sub-page offsets within a 4K page.
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000).unwrap_or_else(|_| {
        sys::print(b"virtio-blk: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Negotiate features.
    if !device.negotiate() {
        sys::print(b"virtio-blk: negotiate failed\n");
        sys::exit();
    }

    // Register for device interrupt before driver_ok.
    let irq_handle = sys::interrupt_register(irq).unwrap_or_else(|_| {
        sys::print(b"virtio-blk: interrupt_register failed\n");
        sys::exit();
    });
    // Read capacity from device config.
    let capacity = device.config_read64(0);
    // Allocate DMA for the request virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_REQUEST),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-blk: dma_alloc (vq) failed\n");
        sys::exit();
    });
    // Zero the virtqueue memory.
    let vq_bytes = (1usize << order) * 4096;

    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };

    let mut vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    // Print capacity as a single line.
    {
        let mut buf = [0u8; 64];
        let prefix = b"  \xF0\x9F\x94\x8C virtio - blk capacity=";

        buf[..prefix.len()].copy_from_slice(prefix);

        let mut pos = prefix.len();

        pos += format_u64(capacity, &mut buf[pos..]);

        let suffix = b" sectors\n";

        buf[pos..pos + suffix.len()].copy_from_slice(suffix);

        pos += suffix.len();

        sys::print(&buf[..pos]);
    }

    // Read sector 0 if the device has any capacity.
    if capacity > 0 {
        read_and_print_sector(&device, &mut vq, 0, irq_handle);
    }

    // Signal the kernel channel to indicate we're done.
    let _ = sys::channel_signal(0);

    sys::exit();
}
