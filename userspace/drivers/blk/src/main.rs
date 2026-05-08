//! Virtio-blk driver — block device I/O.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!
//! Probes the virtio MMIO region for a block device (device ID 2).
//! Negotiates features (including FLUSH), reads capacity, allocates
//! DMA buffers, runs a self-test, then enters an IPC serve loop
//! accepting read/write/flush requests from the store service.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights, SyscallError};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;
const MSG_SIZE: usize = 128;

const BLOCK_SIZE: usize = protocol::blk::BLOCK_SIZE as usize;
const SECTORS_PER_BLOCK: u32 = protocol::blk::SECTORS_PER_BLOCK;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;

const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const VIRTIO_BLK_S_OK: u8 = 0;
const STATUS_NOT_SUPPORTED: u8 = 0xFF;

const VIRTQ_REQUEST: u32 = 0;
const DATA_OFFSET: usize = 16;

#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

struct BlkDevice {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_event: Handle,
    buf_va: usize,
    buf_pa: u64,
    capacity_sectors: u64,
    has_flush: bool,
}

impl BlkDevice {
    fn submit(&mut self, req_type: u32, sector: u64, data_bytes: u32) -> u8 {
        let buf_ptr = self.buf_va as *mut u8;

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

        // SAFETY: status_offset is within the DMA buffer (16 + data_bytes < 2 pages).
        unsafe { *buf_ptr.add(status_offset) = STATUS_NOT_SUPPORTED };

        if data_bytes == 0 {
            self.vq
                .push_chain(&[(header_pa, 16, false), (status_pa, 1, true)]);
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

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);
        let _ = abi::event::clear(self.irq_event, 0x1);

        self.device.ack_interrupt();
        self.vq.pop_used();

        // SAFETY: status_offset is within the DMA buffer; device has written the status byte.
        unsafe { *buf_ptr.add(status_offset) }
    }

    fn read_block(&mut self, block_index: u32) -> u8 {
        let sector = block_index as u64 * SECTORS_PER_BLOCK as u64;

        self.submit(VIRTIO_BLK_T_IN, sector, BLOCK_SIZE as u32)
    }

    fn write_block(&mut self, block_index: u32) -> u8 {
        let sector = block_index as u64 * SECTORS_PER_BLOCK as u64;

        self.submit(VIRTIO_BLK_T_OUT, sector, BLOCK_SIZE as u32)
    }

    fn flush(&mut self) -> u8 {
        if !self.has_flush {
            return STATUS_NOT_SUPPORTED;
        }

        self.submit(VIRTIO_BLK_T_FLUSH, 0, 0)
    }

    fn data_ptr(&self) -> *mut u8 {
        (self.buf_va + DATA_OFFSET) as *mut u8
    }

    fn capacity_blocks(&self) -> u32 {
        (self.capacity_sectors / SECTORS_PER_BLOCK as u64) as u32
    }
}

struct BlkServer {
    blk: BlkDevice,
    shared_va: usize,
    shared_len: usize,
}

impl Dispatch for BlkServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            protocol::blk::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let vmo = Handle(msg.handles[0]);
                let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                match abi::vmo::map(vmo, 0, rw) {
                    Ok(va) => {
                        self.shared_va = va;
                        self.shared_len = BLOCK_SIZE * 4;

                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(protocol::STATUS_INVALID);
                    }
                }
            }
            protocol::blk::READ_BLOCK => {
                if msg.payload.len() < protocol::blk::BlockRequest::SIZE {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let req = protocol::blk::BlockRequest::read_from(msg.payload);

                if req.block_index >= self.blk.capacity_blocks() {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let status = self.blk.read_block(req.block_index);

                if status != VIRTIO_BLK_S_OK {
                    let _ = msg.reply_error(protocol::STATUS_IO_ERROR);

                    return;
                }

                if self.shared_va != 0 {
                    let offset = req.vmo_offset as usize;

                    if offset + BLOCK_SIZE > self.shared_len {
                        let _ = msg.reply_error(protocol::STATUS_INVALID);

                        return;
                    }

                    let dst = (self.shared_va + offset) as *mut u8;
                    let src = self.blk.data_ptr();

                    // SAFETY: offset + BLOCK_SIZE <= shared_len, checked above.
                    unsafe { core::ptr::copy_nonoverlapping(src, dst, BLOCK_SIZE) };
                }

                let _ = msg.reply_empty();
            }
            protocol::blk::WRITE_BLOCK => {
                if msg.payload.len() < protocol::blk::BlockRequest::SIZE {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let req = protocol::blk::BlockRequest::read_from(msg.payload);

                if req.block_index >= self.blk.capacity_blocks() {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                if self.shared_va == 0 {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let offset = req.vmo_offset as usize;

                if offset + BLOCK_SIZE > self.shared_len {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let src = (self.shared_va + offset) as *const u8;
                let dst = self.blk.data_ptr();

                // SAFETY: offset + BLOCK_SIZE <= shared_len, checked above.
                unsafe { core::ptr::copy_nonoverlapping(src, dst, BLOCK_SIZE) };

                let status = self.blk.write_block(req.block_index);

                if status != VIRTIO_BLK_S_OK {
                    let _ = msg.reply_error(protocol::STATUS_IO_ERROR);
                } else {
                    let _ = msg.reply_empty();
                }
            }
            protocol::blk::FLUSH => {
                let status = self.blk.flush();

                if status == VIRTIO_BLK_S_OK {
                    let _ = msg.reply_empty();
                } else if status == STATUS_NOT_SUPPORTED {
                    let _ = msg.reply_error(protocol::STATUS_UNSUPPORTED);
                } else {
                    let _ = msg.reply_error(protocol::STATUS_IO_ERROR);
                }
            }
            protocol::blk::GET_INFO => {
                let reply = protocol::blk::InfoReply {
                    capacity_blocks: self.blk.capacity_blocks(),
                    has_flush: u8::from(self.blk.has_flush),
                };
                let mut data = [0u8; protocol::blk::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            _ => {
                let _ = msg.reply_error(protocol::STATUS_UNSUPPORTED);
            }
        }
    }
}

fn request_dma(init_ep: Handle, size: usize) -> Result<(Handle, usize), SyscallError> {
    let mut msg = [0u8; MSG_SIZE];
    let method = protocol::bootstrap::DMA_ALLOC;

    msg[0..4].copy_from_slice(&method.to_le_bytes());

    let req = protocol::bootstrap::DmaAllocRequest { size: size as u32 };

    req.write_to(&mut msg[4..8]);

    let mut recv_handles = [0u32; 4];
    let result = abi::ipc::call(init_ep, &mut msg, 8, &[], &mut recv_handles)?;

    if result.handle_count == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let vmo = Handle(recv_handles[0]);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let va = abi::vmo::map(vmo, 0, rw)?;

    Ok((vmo, va))
}

fn lookup_service(ns_ep: Handle, name: &[u8]) -> Result<Handle, SyscallError> {
    let req = protocol::name_service::NameRequest::new(name);
    let mut buf = [0u8; MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, protocol::name_service::LOOKUP, &req.name);
    let mut recv_handles = [0u32; 4];
    let result = abi::ipc::call(ns_ep, &mut buf, total, &[], &mut recv_handles)?;

    if result.handle_count == 0 {
        return Err(SyscallError::NotFound);
    }

    Ok(Handle(recv_handles[0]))
}

fn console_write(console_ep: Handle, text: &[u8]) {
    let mut buf = [0u8; MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, 1, text);
    let _ = abi::ipc::call(console_ep, &mut buf, total, &[], &mut []);
}

fn console_write_u32(console_ep: Handle, prefix: &[u8], n: u32) {
    let mut text = [0u8; 80];
    let plen = prefix.len().min(60);

    text[..plen].copy_from_slice(&prefix[..plen]);

    let nlen = format_u32(n, &mut text[plen..]);

    text[plen + nlen] = b'\n';

    console_write(console_ep, &text[..plen + nlen + 1]);
}

fn format_u32(mut n: u32, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }

    let mut tmp = [0u8; 10];
    let mut i = 10;

    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    let len = 10 - i;

    buf[..len].copy_from_slice(&tmp[i..]);

    len
}

fn self_test(blk: &mut BlkDevice, console_ep: Handle) {
    const TEST_BLOCK: u32 = 1;

    let data = blk.data_ptr();

    for i in 0..BLOCK_SIZE {
        // SAFETY: data points to BLOCK_SIZE bytes within the DMA buffer.
        unsafe { *data.add(i) = (i & 0xFF) as u8 };
    }

    let status = blk.write_block(TEST_BLOCK);

    if status != VIRTIO_BLK_S_OK {
        console_write_u32(console_ep, b"blk: FAIL write status=", status as u32);

        return;
    }

    // SAFETY: data points to BLOCK_SIZE bytes within the DMA buffer.
    unsafe { core::ptr::write_bytes(data, 0, BLOCK_SIZE) };

    let status = blk.read_block(TEST_BLOCK);

    if status != VIRTIO_BLK_S_OK {
        console_write_u32(console_ep, b"blk: FAIL read status=", status as u32);

        return;
    }

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
        console_write(console_ep, b"blk: write+read 16K: OK\n");
    } else {
        console_write_u32(console_ep, b"blk: FAIL mismatches=", mismatches);
        return;
    }

    let status = blk.flush();

    if status == VIRTIO_BLK_S_OK {
        console_write(console_ep, b"blk: flush: OK\n");
    } else if status == STATUS_NOT_SUPPORTED {
        console_write(console_ep, b"blk: flush: not supported\n");
    } else {
        console_write_u32(console_ep, b"blk: FAIL flush status=", status as u32);
    }
}

fn register_with_name_service(ns_ep: Handle, name: &[u8], own_ep: Handle) {
    let dup = match abi::handle::dup(own_ep, abi::types::Rights::ALL) {
        Ok(h) => h,
        Err(_) => return,
    };
    let req = protocol::name_service::NameRequest::new(name);
    let mut buf = [0u8; MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, protocol::name_service::REGISTER, &req.name);
    let _ = abi::ipc::call(ns_ep, &mut buf, total, &[dup.0], &mut []);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };

    let (device, blk_slot) = match virtio::find_device(virtio_va, virtio::DEVICE_BLK) {
        Some(d) => d,
        None => abi::thread::exit(0xB0),
    };

    let (ok, accepted) = device.negotiate_features(VIRTIO_BLK_F_FLUSH);

    if !ok {
        abi::thread::exit(3);
    }

    let has_flush = accepted & VIRTIO_BLK_F_FLUSH != 0;
    let capacity_sectors = device.config_read64(0);

    let queue_size = device
        .queue_max_size(VIRTQ_REQUEST)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let vq_bytes = virtio::Virtqueue::total_bytes(queue_size);
    let vq_alloc = vq_bytes.next_multiple_of(PAGE_SIZE);
    let (_vq_vmo, vq_va) = match request_dma(HANDLE_INIT_EP, vq_alloc) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(4),
    };

    // SAFETY: vq_va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_alloc) };

    let vq_pa = vq_va as u64;
    let vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    let buf_alloc = PAGE_SIZE * 2;
    let (_buf_vmo, buf_va) = match request_dma(HANDLE_INIT_EP, buf_alloc) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: buf_va is a valid DMA allocation of 2 pages; zeroing before use.
    unsafe { core::ptr::write_bytes(buf_va as *mut u8, 0, buf_alloc) };

    let buf_pa = buf_va as u64;

    device.driver_ok();

    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(6),
    };

    let irq_num = virtio::SPI_BASE_INTID + blk_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(7);
    }

    let mut blk = BlkDevice {
        device,
        vq,
        irq_event,
        buf_va,
        buf_pa,
        capacity_sectors,
        has_flush,
    };

    let console_ep = match lookup_service(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    console_write_u32(console_ep, b"blk: capacity=", blk.capacity_blocks());

    if blk.capacity_blocks() >= 2 {
        self_test(&mut blk, console_ep);
    } else {
        console_write(console_ep, b"blk: skip self-test (< 2 blocks)\n");
    }

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(9),
    };

    register_with_name_service(HANDLE_NS_EP, b"blk", own_ep);

    console_write(console_ep, b"blk: ready\n");

    let mut server = BlkServer {
        blk,
        shared_va: 0,
        shared_len: 0,
    };

    ipc::server::serve(own_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
