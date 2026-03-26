//! Filesystem service — COW filesystem over virtio-blk.
//!
//! Owns the virtio-blk device directly (maps MMIO, does DMA I/O) and
//! builds the COW filesystem on top. On first boot, formats the device.
//! On subsequent boots, mounts the existing filesystem.
//!
//! Self-test: creates a file, writes data, commits, reads back, verifies.
//!
//! Phase B4 will add an IPC loop for core to call `Files` operations.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use core::cell::RefCell;
use protocol::device::MSG_DEVICE_CONFIG;

mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

const PAGE_SIZE: usize = system_config::PAGE_SIZE as usize;

const SECTOR_SIZE: usize = 512;

// virtio-blk request types.
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;

// virtio-blk feature bits.
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

// virtio-blk status codes.
const VIRTIO_BLK_S_OK: u8 = 0;

const VIRTQ_REQUEST: u32 = 0;
const DATA_OFFSET: usize = 16;

/// Block request header (16 bytes, device-readable).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

// ── VirtioBlockDevice ────────────────────────────────────────────────

/// Mutable I/O state — borrowed via `RefCell` for interior mutability.
struct IoState {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    buf_va: usize,
    buf_pa: u64,
}

/// `BlockDevice` implementation over virtio-blk transport.
///
/// Translates 16 KiB filesystem block operations into virtio sector I/O.
/// Uses `RefCell` for interior mutability: `read_block(&self)` needs to
/// mutate virtqueue state, but the `BlockDevice` trait takes `&self` for
/// reads (matching `pread` semantics on the host prototype).
struct VirtioBlockDevice {
    io: RefCell<IoState>,
    capacity_sectors: u64,
    has_flush: bool,
}

impl IoState {
    /// Submit a virtio-blk request and wait for completion.
    fn submit(&mut self, req_type: u32, sector: u64, data_bytes: u32) -> u8 {
        let buf_ptr = self.buf_va as *mut u8;

        // SAFETY: buf_va points to DMA allocation with at least 16 bytes for header.
        unsafe {
            let header = buf_ptr as *mut BlkReqHeader;
            (*header).req_type = req_type;
            (*header).reserved = 0;
            (*header).sector = sector;
        }

        let header_pa = self.buf_pa;
        let status_offset = DATA_OFFSET + data_bytes as usize;
        let status_pa = self.buf_pa + status_offset as u64;

        // SAFETY: status_offset is within the DMA buffer.
        unsafe { *buf_ptr.add(status_offset) = 0xFF };

        if data_bytes == 0 {
            self.vq.push_chain(&[
                (header_pa, 16, false),
                (status_pa, 1, true),
            ]);
        } else {
            let data_pa = self.buf_pa + DATA_OFFSET as u64;
            let data_writable = req_type == VIRTIO_BLK_T_IN;
            self.vq.push_chain(&[
                (header_pa, 16, false),
                (data_pa, data_bytes, data_writable),
                (status_pa, 1, true),
            ]);
        }

        self.device.notify(VIRTQ_REQUEST);
        let _ = sys::wait(&[self.irq_handle.0], u64::MAX);
        self.device.ack_interrupt();
        self.vq.pop_used();
        let _ = sys::interrupt_ack(self.irq_handle);

        // SAFETY: device has written the status byte.
        unsafe { *buf_ptr.add(status_offset) }
    }

    /// Pointer to the data area within the DMA buffer.
    fn data_ptr(&self) -> *mut u8 {
        (self.buf_va + DATA_OFFSET) as *mut u8
    }
}

impl fs::BlockDevice for VirtioBlockDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), fs::FsError> {
        let mut io = self.io.borrow_mut();
        let sectors_per_block = fs::BLOCK_SIZE / SECTOR_SIZE as u32;
        let sector = index as u64 * sectors_per_block as u64;
        let status = io.submit(VIRTIO_BLK_T_IN, sector, fs::BLOCK_SIZE);
        if status != VIRTIO_BLK_S_OK {
            return Err(fs::FsError::Io);
        }

        // Copy from DMA buffer to caller's buffer.
        // SAFETY: data area has fs::BLOCK_SIZE bytes after a successful read.
        unsafe {
            core::ptr::copy_nonoverlapping(
                io.data_ptr(),
                buf.as_mut_ptr(),
                fs::BLOCK_SIZE as usize,
            );
        }
        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), fs::FsError> {
        let mut io = self.io.borrow_mut();
        let sectors_per_block = fs::BLOCK_SIZE / SECTOR_SIZE as u32;
        let sector = index as u64 * sectors_per_block as u64;

        // Copy caller's data into DMA buffer.
        // SAFETY: data area has space for fs::BLOCK_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                io.data_ptr(),
                fs::BLOCK_SIZE as usize,
            );
        }

        let status = io.submit(VIRTIO_BLK_T_OUT, sector, fs::BLOCK_SIZE);
        if status != VIRTIO_BLK_S_OK {
            return Err(fs::FsError::Io);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), fs::FsError> {
        if !self.has_flush {
            return Ok(()); // No flush support — best effort.
        }
        let mut io = self.io.borrow_mut();
        let status = io.submit(VIRTIO_BLK_T_FLUSH, 0, 0);
        if status != VIRTIO_BLK_S_OK {
            return Err(fs::FsError::Io);
        }
        Ok(())
    }

    fn block_count(&self) -> u32 {
        let sectors_per_block = fs::BLOCK_SIZE / SECTOR_SIZE as u32;
        (self.capacity_sectors / sectors_per_block as u64) as u32
    }
}

// ── Self-test ────────────────────────────────────────────────────────

/// Format, create a file, write, commit, read back, verify.
fn self_test(device: VirtioBlockDevice) {
    use fs::Files;

    sys::print(b"     formatting filesystem...\n");
    let mut filesystem = match fs::Filesystem::format(device) {
        Ok(f) => f,
        Err(_) => {
            sys::print(b"     FAIL: format failed\n");
            return;
        }
    };

    // Create a file (Files trait returns FileId).
    let file_id = match filesystem.create() {
        Ok(id) => id,
        Err(_) => {
            sys::print(b"     FAIL: create failed\n");
            return;
        }
    };

    // Write test data (use raw u64 for Filesystem's internal API).
    let test_data = b"Hello from the COW filesystem!";
    if let Err(_) = filesystem.write(file_id.0, 0, test_data) {
        sys::print(b"     FAIL: write failed\n");
        return;
    }

    // Commit (two-flush protocol).
    if let Err(_) = filesystem.commit() {
        sys::print(b"     FAIL: commit failed\n");
        return;
    }

    // Read back.
    let mut read_buf = vec![0u8; test_data.len()];
    match filesystem.read(file_id.0, 0, &mut read_buf) {
        Ok(n) if n == test_data.len() => {}
        _ => {
            sys::print(b"     FAIL: read failed\n");
            return;
        }
    }

    // Verify.
    if read_buf == test_data {
        sys::print(b"     filesystem self-test: OK\n");
    } else {
        sys::print(b"     FAIL: data mismatch\n");
    }

    // Verify size.
    match filesystem.size(file_id) {
        Ok(s) if s == test_data.len() as u64 => {
            sys::print(b"     file size: OK\n");
        }
        _ => {
            sys::print(b"     FAIL: size mismatch\n");
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────

/// Format a u64 in decimal.
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
    sys::print(b"  \xF0\x9F\x92\xBE filesystem - starting\n");

    // Read device config from init.
    let ch = unsafe { ipc::Channel::from_base(protocol::CHANNEL_SHM_BASE, ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"filesystem: no config message\n");
        sys::exit();
    }

    let config = if let Some(protocol::device::Message::DeviceConfig(c)) =
        protocol::device::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"filesystem: bad device config\n");
        sys::exit();
    };

    // Map MMIO region.
    let mmio_pa = config.mmio_pa;
    let page_offset = mmio_pa & (PAGE_SIZE as u64 - 1);
    let page_pa = mmio_pa & !(PAGE_SIZE as u64 - 1);
    let page_va = sys::device_map(page_pa, PAGE_SIZE as u64).unwrap_or_else(|_| {
        sys::print(b"filesystem: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Negotiate features (request FLUSH).
    let (ok, accepted) = device.negotiate_features(VIRTIO_BLK_F_FLUSH);
    if !ok {
        sys::print(b"filesystem: negotiate failed\n");
        sys::exit();
    }
    let has_flush = accepted & VIRTIO_BLK_F_FLUSH != 0;

    // Register for device interrupt.
    let irq_handle: sys::InterruptHandle =
        sys::interrupt_register(config.irq).unwrap_or_else(|_| {
            sys::print(b"filesystem: interrupt_register failed\n");
            sys::exit();
        });

    // Read capacity.
    let capacity_sectors = device.config_read64(0);

    // Allocate virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_REQUEST),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"filesystem: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_pages = 1usize << vq_order;
    // SAFETY: vq_va is a valid DMA allocation; zeroing before use.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_pages * PAGE_SIZE) };

    let vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);
    device.setup_queue(VIRTQ_REQUEST, queue_size, vq.desc_pa(), vq.avail_pa(), vq.used_pa());
    device.driver_ok();

    // Allocate DMA buffer (2 pages for block-sized operations).
    let mut buf_pa: u64 = 0;
    let buf_va = sys::dma_alloc(1, &mut buf_pa).unwrap_or_else(|_| {
        sys::print(b"filesystem: dma_alloc (buf) failed\n");
        sys::exit();
    });
    // SAFETY: buf_va is a valid DMA allocation of 2 pages; zeroing before use.
    unsafe { core::ptr::write_bytes(buf_va as *mut u8, 0, 2 * PAGE_SIZE) };

    let blk = VirtioBlockDevice {
        io: RefCell::new(IoState {
            device,
            vq,
            irq_handle,
            buf_va,
            buf_pa,
        }),
        capacity_sectors,
        has_flush,
    };

    // Print capacity.
    let block_count = fs::BlockDevice::block_count(&blk);
    {
        let mut buf = [0u8; 64];
        let prefix = b"     capacity=";
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        pos += format_u64(block_count as u64, &mut buf[pos..]);
        let suffix = b" blocks\n";
        buf[pos..pos + suffix.len()].copy_from_slice(suffix);
        pos += suffix.len();
        sys::print(&buf[..pos]);
    }

    // Format the filesystem.
    sys::print(b"     formatting...\n");
    let mut filesystem = match fs::Filesystem::format(blk) {
        Ok(f) => f,
        Err(_) => {
            sys::print(b"     FAIL: format failed\n");
            let _ = sys::channel_signal(sys::ChannelHandle(0));
            sys::exit();
        }
    };

    // Create a file for the document.
    let file_id = match filesystem.create_file() {
        Ok(id) => id,
        Err(_) => {
            sys::print(b"     FAIL: create failed\n");
            let _ = sys::channel_signal(sys::ChannelHandle(0));
            sys::exit();
        }
    };

    // Initial commit (empty file).
    if let Err(_) = filesystem.commit() {
        sys::print(b"     FAIL: initial commit failed\n");
        let _ = sys::channel_signal(sys::ChannelHandle(0));
        sys::exit();
    }

    sys::print(b"     filesystem ready\n");

    // Read FS config from init (doc buffer VA).
    let doc_va: usize;
    let doc_capacity: usize;
    if ch.try_recv(&mut msg) && msg.msg_type == protocol::blkfs::MSG_FS_CONFIG {
        if let Some(protocol::blkfs::Message::FsConfig(cfg)) =
            protocol::blkfs::decode(msg.msg_type, &msg.payload)
        {
            doc_va = cfg.doc_va as usize;
            doc_capacity = cfg.doc_capacity as usize;
        } else {
            sys::print(b"filesystem: bad fs config\n");
            let _ = sys::channel_signal(sys::ChannelHandle(0));
            sys::exit();
        }
    } else {
        // No filesystem config — run without document persistence.
        sys::print(b"     no fs config, running standalone\n");
        let _ = sys::channel_signal(sys::ChannelHandle(0));
        sys::exit();
    }

    sys::print(b"     doc buffer mapped, entering IPC loop\n");

    // Signal init that we're ready.
    let _ = sys::channel_signal(sys::ChannelHandle(0));

    // ── IPC loop: handle commit requests from core ──────────────────

    // Core channel: handle 1 (sent by init via handle_send).
    let core_ch = unsafe {
        ipc::Channel::from_base(protocol::channel_shm_va(1), ipc::PAGE_SIZE, 1)
    };

    loop {
        // Wait for signal on the core channel (handle 1).
        let _ = sys::wait(&[1], u64::MAX);

        while core_ch.try_recv(&mut msg) {
            if msg.msg_type == protocol::blkfs::MSG_FS_COMMIT {
                // Read document content from shared buffer.
                // Header layout: [0..8) = content_len (u64), [8..16) = cursor_pos, [16..64) = reserved, [64..) = content
                let content_len = unsafe {
                    core::ptr::read_volatile(doc_va as *const u64) as usize
                };

                let actual_len = if content_len > doc_capacity {
                    doc_capacity
                } else {
                    content_len
                };

                // SAFETY: doc_va + 64 points to content area in shared memory,
                // mapped read-only by init. actual_len is bounded by doc_capacity.
                let content = unsafe {
                    core::slice::from_raw_parts((doc_va + 64) as *const u8, actual_len)
                };

                // Write to filesystem and commit.
                let _ = filesystem.write(file_id, 0, content);
                let _ = filesystem.truncate(file_id, actual_len as u64);
                let _ = filesystem.commit();
            }
        }
    }
}
