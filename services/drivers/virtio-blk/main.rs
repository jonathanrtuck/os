//! Userspace virtio block driver.
//!
//! Receives device config via IPC from init, maps the device, negotiates
//! features (including flush), and provides block-level read/write/flush
//! operations over virtio-blk transport.
//!
//! # Self-test
//!
//! On startup, writes a test pattern to block 1, reads it back, verifies
//! the round-trip, and flushes. Reports capacity and test results via serial.
//!
//! # Future: IPC service (Phase B3)
//!
//! Will be converted from a self-test into a long-lived IPC service that
//! accepts read/write/flush requests from the filesystem service.

#![no_std]
#![no_main]

use protocol::device::MSG_DEVICE_CONFIG;

mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

const PAGE_SIZE: usize = system_config::PAGE_SIZE as usize;

const SECTOR_SIZE: usize = 512;
/// Filesystem block size (matches kernel page size).
const BLOCK_SIZE: usize = 16_384;
/// Number of 512-byte sectors per filesystem block.
const SECTORS_PER_BLOCK: u32 = (BLOCK_SIZE / SECTOR_SIZE) as u32;

// virtio-blk request types (virtio spec §5.2.6).
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;

// virtio-blk feature bits (virtio spec §5.2.3).
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

// virtio-blk status codes (virtio spec §5.2.6.1).
const VIRTIO_BLK_S_OK: u8 = 0;

const VIRTQ_REQUEST: u32 = 0;

/// Byte offset of the data area in the DMA buffer (after BlkReqHeader).
const DATA_OFFSET: usize = 16;

/// Block request header (16 bytes, device-readable).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

/// Encapsulates a virtio-blk device with DMA buffer for block operations.
///
/// The DMA buffer is 2 pages (32 KiB) to accommodate a full block operation:
/// 16 bytes header + 16,384 bytes data + 1 byte status = 16,401 bytes.
struct BlkDevice {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    /// DMA buffer VA (2 pages).
    buf_va: usize,
    /// DMA buffer PA.
    buf_pa: u64,
    /// Device capacity in 512-byte sectors.
    capacity: u64,
    /// Whether the device supports VIRTIO_BLK_F_FLUSH.
    has_flush: bool,
}

impl BlkDevice {
    /// Submit a virtio-blk request and wait for completion.
    ///
    /// For reads (`VIRTIO_BLK_T_IN`), the data descriptor is device-writable
    /// (device writes data into our buffer). For writes (`VIRTIO_BLK_T_OUT`),
    /// it is device-readable (device reads data from our buffer). For flush,
    /// `data_bytes` is 0 and no data descriptor is emitted.
    ///
    /// Returns the status byte (0 = `VIRTIO_BLK_S_OK`).
    fn submit(&mut self, req_type: u32, sector: u64, data_bytes: u32) -> u8 {
        let buf_ptr = self.buf_va as *mut u8;

        // Write request header.
        // SAFETY: buf_va points to DMA allocation with at least 16 bytes for BlkReqHeader.
        unsafe {
            let header = buf_ptr as *mut BlkReqHeader;
            (*header).req_type = req_type;
            (*header).reserved = 0;
            (*header).sector = sector;
        }

        let header_pa = self.buf_pa;
        let status_offset = DATA_OFFSET + data_bytes as usize;
        let status_pa = self.buf_pa + status_offset as u64;

        // Sentinel status byte — device overwrites with 0 on success.
        // SAFETY: status_offset is within the DMA buffer (16 + data_bytes < 2 pages).
        unsafe { *buf_ptr.add(status_offset) = 0xFF };

        if data_bytes == 0 {
            // Flush: 2-descriptor chain (header + status, no data).
            self.vq.push_chain(&[
                (header_pa, 16, false), // header: device-readable
                (status_pa, 1, true),   // status: device-writable
            ]);
        } else {
            let data_pa = self.buf_pa + DATA_OFFSET as u64;
            let data_writable = req_type == VIRTIO_BLK_T_IN;
            // 3-descriptor chain: header → data → status.
            self.vq.push_chain(&[
                (header_pa, 16, false),               // header: device-readable
                (data_pa, data_bytes, data_writable), // data
                (status_pa, 1, true),                 // status: device-writable
            ]);
        }

        self.device.notify(VIRTQ_REQUEST);

        // Block until the device signals completion.
        let _ = sys::wait(&[self.irq_handle.0], u64::MAX);
        self.device.ack_interrupt();
        self.vq.pop_used();
        let _ = sys::interrupt_ack(self.irq_handle);

        // SAFETY: status_offset is within the DMA buffer; device has written the status.
        unsafe { *buf_ptr.add(status_offset) }
    }

    /// Read `count` contiguous sectors starting at `sector` into the data area.
    fn read_sectors(&mut self, sector: u64, count: u32) -> u8 {
        self.submit(VIRTIO_BLK_T_IN, sector, count * SECTOR_SIZE as u32)
    }

    /// Write `count` contiguous sectors starting at `sector` from the data area.
    ///
    /// Caller must fill the data area before calling.
    fn write_sectors(&mut self, sector: u64, count: u32) -> u8 {
        self.submit(VIRTIO_BLK_T_OUT, sector, count * SECTOR_SIZE as u32)
    }

    /// Flush all previously written data to stable storage.
    ///
    /// Returns `VIRTIO_BLK_S_OK` on success. If the device does not support
    /// flush (feature not negotiated), returns 0xFF (sentinel, no request sent).
    fn flush(&mut self) -> u8 {
        if !self.has_flush {
            return 0xFF;
        }
        self.submit(VIRTIO_BLK_T_FLUSH, 0, 0)
    }

    /// Read a full 16 KiB filesystem block into the data area.
    fn read_block(&mut self, block_index: u32) -> u8 {
        let sector = block_index as u64 * SECTORS_PER_BLOCK as u64;
        self.read_sectors(sector, SECTORS_PER_BLOCK)
    }

    /// Write a full 16 KiB filesystem block from the data area.
    fn write_block(&mut self, block_index: u32) -> u8 {
        let sector = block_index as u64 * SECTORS_PER_BLOCK as u64;
        self.write_sectors(sector, SECTORS_PER_BLOCK)
    }

    /// Mutable pointer to the data area within the DMA buffer.
    fn data_mut(&self) -> *mut u8 {
        // SAFETY: buf_va + DATA_OFFSET is within the 2-page DMA allocation.
        (self.buf_va + DATA_OFFSET) as *mut u8
    }

    /// Device capacity in filesystem blocks (16 KiB each).
    fn capacity_blocks(&self) -> u32 {
        (self.capacity / SECTORS_PER_BLOCK as u64) as u32
    }
}

/// Self-test: write a 16 KiB block, read it back, verify, flush.
fn self_test(blk: &mut BlkDevice) {
    const TEST_BLOCK: u32 = 1;

    // Fill data area with a counting pattern: byte[i] = i & 0xFF.
    let data = blk.data_mut();
    for i in 0..BLOCK_SIZE {
        // SAFETY: data points to BLOCK_SIZE bytes within the DMA buffer.
        unsafe { *data.add(i) = (i & 0xFF) as u8 };
    }

    // Write block 1.
    let status = blk.write_block(TEST_BLOCK);
    if status != VIRTIO_BLK_S_OK {
        sys::print(b"     FAIL: write_block status=");
        sys::print_u32(status as u32);
        sys::print(b"\n");
        return;
    }

    // Clear data area to ensure the read is real.
    // SAFETY: data points to BLOCK_SIZE bytes within the DMA buffer.
    unsafe { core::ptr::write_bytes(data, 0, BLOCK_SIZE) };

    // Read block 1 back.
    let status = blk.read_block(TEST_BLOCK);
    if status != VIRTIO_BLK_S_OK {
        sys::print(b"     FAIL: read_block status=");
        sys::print_u32(status as u32);
        sys::print(b"\n");
        return;
    }

    // Verify every byte.
    let mut mismatches: u32 = 0;
    for i in 0..BLOCK_SIZE {
        let expected = (i & 0xFF) as u8;
        // SAFETY: data points to BLOCK_SIZE bytes; device has written the read data.
        let actual = unsafe { *data.add(i) };
        if actual != expected {
            mismatches += 1;
        }
    }

    if mismatches == 0 {
        sys::print(b"     write+read 16K block: OK\n");
    } else {
        sys::print(b"     FAIL: ");
        sys::print_u32(mismatches);
        sys::print(b" byte mismatches in 16K block\n");
        return;
    }

    // Test flush.
    let status = blk.flush();
    if status == VIRTIO_BLK_S_OK {
        sys::print(b"     flush: OK\n");
    } else if status == 0xFF {
        sys::print(b"     flush: not supported (feature not negotiated)\n");
    } else {
        sys::print(b"     FAIL: flush status=");
        sys::print_u32(status as u32);
        sys::print(b"\n");
    }
}

/// Format a u64 in decimal into `buf`, returning the number of bytes written.
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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read device config from init (first message on channel 0).
    let ch = unsafe { ipc::Channel::from_base(protocol::channel_shm_base(), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virtio-blk: no config message\n");
        sys::exit();
    }

    let config = if let Some(protocol::device::Message::DeviceConfig(c)) =
        protocol::device::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"virtio-blk: bad device config\n");
        sys::exit();
    };
    let init_handle = sys::ChannelHandle(config.init_handle);

    // Map the MMIO region. virtio-mmio slots have 0x200 stride, so most
    // sit at sub-page offsets within a page.
    let mmio_pa = config.mmio_pa;
    let page_offset = mmio_pa & (PAGE_SIZE as u64 - 1);
    let page_pa = mmio_pa & !(PAGE_SIZE as u64 - 1);
    let page_va = sys::device_map(page_pa, PAGE_SIZE as u64).unwrap_or_else(|_| {
        sys::print(b"virtio-blk: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Negotiate features. Request FLUSH for crash-consistent writes.
    let (ok, accepted) = device.negotiate_features(VIRTIO_BLK_F_FLUSH);
    if !ok {
        sys::print(b"virtio-blk: negotiate failed\n");
        sys::exit();
    }
    let has_flush = accepted & VIRTIO_BLK_F_FLUSH != 0;

    // Register for device interrupt.
    let irq_handle: sys::InterruptHandle =
        sys::interrupt_register(config.irq).unwrap_or_else(|_| {
            sys::print(b"virtio-blk: interrupt_register failed\n");
            sys::exit();
        });

    // Read capacity from device config space (offset 0, 8 bytes).
    let capacity = device.config_read64(0);

    // Allocate virtqueue DMA.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_REQUEST),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-blk: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_pages = 1usize << vq_order;
    // SAFETY: vq_va is a valid DMA allocation of vq_pages * PAGE_SIZE bytes; zeroing before use.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_pages * PAGE_SIZE) };

    let mut vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);
    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    // Allocate DMA buffer for block operations: 2 pages (32 KiB).
    // Layout: [0..16) header, [16..16400) data, [16400] status.
    let mut buf_pa: u64 = 0;
    let buf_va = sys::dma_alloc(1, &mut buf_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-blk: dma_alloc (buf) failed\n");
        sys::exit();
    });
    // SAFETY: buf_va is a valid DMA allocation of 2 * PAGE_SIZE bytes; zeroing before use.
    unsafe { core::ptr::write_bytes(buf_va as *mut u8, 0, 2 * PAGE_SIZE) };

    let mut blk = BlkDevice {
        device,
        vq,
        irq_handle,
        buf_va,
        buf_pa,
        capacity,
        has_flush,
    };

    // Print capacity.
    {
        let mut buf = [0u8; 80];
        let prefix = b"  \xF0\x9F\x94\x8C virtio-blk capacity=";
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();

        pos += format_u64(capacity, &mut buf[pos..]);

        let mid = b" sectors (";
        buf[pos..pos + mid.len()].copy_from_slice(mid);
        pos += mid.len();

        pos += format_u64(blk.capacity_blocks() as u64, &mut buf[pos..]);

        let suffix = b" blocks)\n";
        buf[pos..pos + suffix.len()].copy_from_slice(suffix);
        pos += suffix.len();

        sys::print(&buf[..pos]);
    }

    // Self-test: write/read/verify a full 16 KiB block + flush.
    if blk.capacity_blocks() >= 2 {
        self_test(&mut blk);
    } else {
        sys::print(b"     skipping self-test: need >= 2 blocks\n");
    }

    // Signal init that we're done.
    let _ = sys::channel_signal(init_handle);
    sys::exit();
}
