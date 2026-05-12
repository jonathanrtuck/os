//! Virtio video decode driver — hardware-accelerated video decoding.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!   Handle 5: service endpoint (pre-registered by init as "codec-decode")
//!
//! Probes the virtio MMIO region for a video decode device (device ID 30).
//! If not found, exits silently (the device is optional). Otherwise sets
//! up control and decode virtqueues, then enters an IPC serve loop
//! accepting session and decode requests from clients.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);
const HANDLE_SVC_EP: Handle = Handle(5);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;

const CONTROLQ: u32 = 0;
const DECODEQ: u32 = 1;

/// Virtio device ID for video decode (custom, device ID 30, slot 8).
const DEVICE_VIDEO_DECODE: u32 = 30;

/// Compressed data buffer size: 64 pages = 1 MiB.
const COMPRESSED_BUF_PAGES: usize = 64;

// Control queue request codes
const CTRL_CREATE_SESSION: u32 = 1;
const CTRL_DESTROY_SESSION: u32 = 2;
const CTRL_FLUSH_SESSION: u32 = 3;

struct VideoDriver {
    device: virtio::Device,
    ctrl_vq: virtio::Virtqueue,
    decode_vq: virtio::Virtqueue,
    irq_event: Handle,
    ctrl_buf_va: usize,
    ctrl_buf_pa: u64,
    frame_hdr_va: usize,
    frame_hdr_pa: u64,
    compressed_va: usize,
    compressed_pa: u64,
    compressed_len: usize,
    status_buf_va: usize,
    status_buf_pa: u64,
    supported_codecs: u32,
    max_width: u32,
    max_height: u32,
    shared_va: usize,
    shared_len: usize,
    next_session_id: u32,
}

impl VideoDriver {
    /// Send a control-queue request and wait for a response.
    ///
    /// Writes `request` into the control DMA buffer, pushes a 2-descriptor
    /// chain (request readable, response writable), notifies the device,
    /// waits for IRQ, and returns the response bytes.
    fn ctrl_request(&mut self, request: &[u8], response_len: u32) {
        let buf = self.ctrl_buf_va as *mut u8;
        let req_len = request.len();

        // SAFETY: ctrl_buf is a PAGE_SIZE DMA buffer, request fits.
        unsafe {
            core::ptr::copy_nonoverlapping(request.as_ptr(), buf, req_len);
            core::ptr::write_bytes(buf.add(req_len), 0, response_len as usize);
        }

        let req_pa = self.ctrl_buf_pa;
        let resp_pa = self.ctrl_buf_pa + req_len as u64;

        self.ctrl_vq.push_chain(&[
            (req_pa, req_len as u32, false),
            (resp_pa, response_len, true),
        ]);

        self.device.notify(CONTROLQ);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.ctrl_vq.pop_used();
    }

    fn create_session(&mut self, codec: u8, width: u32, height: u32) -> (u32, u32, u32) {
        let session_id = self.next_session_id;

        self.next_session_id += 1;

        // Protocol layout (24 bytes):
        //   0: request_type (u32) = 0x01
        //   4: session_id   (u32)
        //   8: codec         (u8)
        //   9: pixel_format  (u8) = 0 (BGRA8)
        //  10: reserved      (u16)
        //  12: width         (u32)
        //  16: height        (u32)
        //  20: codec_data_size (u32) = 0 (no codec data for V1)
        let mut req = [0u8; 24];

        req[0..4].copy_from_slice(&CTRL_CREATE_SESSION.to_le_bytes());
        req[4..8].copy_from_slice(&session_id.to_le_bytes());
        req[8] = codec;
        req[9] = 0; // pixel_format = BGRA8
        req[12..16].copy_from_slice(&width.to_le_bytes());
        req[16..20].copy_from_slice(&height.to_le_bytes());
        // codec_data_size = 0 (bytes 20..24 already zero)

        self.ctrl_request(&req, 12);

        // Response (12 bytes): [status: u32][texture_handle: u32][reserved: u32]
        let resp_va = self.ctrl_buf_va + 24;

        // SAFETY: device has written 12-byte response at resp_va.
        unsafe {
            let status = core::ptr::read_volatile(resp_va as *const u32);
            let texture_handle = core::ptr::read_volatile((resp_va + 4) as *const u32);

            (status, session_id, texture_handle)
        }
    }

    fn destroy_session(&mut self, session_id: u32) -> u32 {
        let mut req = [0u8; 8];

        req[0..4].copy_from_slice(&CTRL_DESTROY_SESSION.to_le_bytes());
        req[4..8].copy_from_slice(&session_id.to_le_bytes());

        self.ctrl_request(&req, 4);

        // SAFETY: device has written 4-byte status response.
        unsafe { core::ptr::read_volatile((self.ctrl_buf_va + 8) as *const u32) }
    }

    fn flush_session(&mut self, session_id: u32) -> u32 {
        let mut req = [0u8; 8];

        req[0..4].copy_from_slice(&CTRL_FLUSH_SESSION.to_le_bytes());
        req[4..8].copy_from_slice(&session_id.to_le_bytes());

        self.ctrl_request(&req, 4);

        // SAFETY: device has written 4-byte status response.
        unsafe { core::ptr::read_volatile((self.ctrl_buf_va + 8) as *const u32) }
    }

    fn decode_frame(
        &mut self,
        session_id: u32,
        compressed_data: &[u8],
        timestamp_ns: u64,
    ) -> video::DecodeFrameReply {
        let data_len = compressed_data.len().min(self.compressed_len);

        // Copy compressed data into DMA buffer.
        // SAFETY: compressed_va is a DMA buffer of compressed_len bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                compressed_data.as_ptr(),
                self.compressed_va as *mut u8,
                data_len,
            );
        }

        // Build 20-byte frame header in frame_hdr_buf:
        // [session_id: u32][flags: u32][compressed_size: u32][reserved: u32][timestamp_ns: u64]
        let hdr = self.frame_hdr_va as *mut u8;

        // SAFETY: frame_hdr_va is a PAGE_SIZE DMA buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(session_id.to_le_bytes().as_ptr(), hdr, 4);
            // flags = 1 (new frame)
            core::ptr::copy_nonoverlapping(1u32.to_le_bytes().as_ptr(), hdr.add(4), 4);
            core::ptr::copy_nonoverlapping((data_len as u32).to_le_bytes().as_ptr(), hdr.add(8), 4);
            // reserved = 0
            core::ptr::write_bytes(hdr.add(12), 0, 4);
            core::ptr::copy_nonoverlapping(timestamp_ns.to_le_bytes().as_ptr(), hdr.add(16), 8);
        }

        // Zero the status buffer before submission.
        // SAFETY: status_buf_va is a PAGE_SIZE DMA buffer.
        unsafe { core::ptr::write_bytes(self.status_buf_va as *mut u8, 0, 24) };

        // Push 3-descriptor chain on decodeq:
        //   header (readable, 20 bytes)
        //   compressed data (readable, data_len bytes)
        //   status (writable, 24 bytes)
        self.decode_vq.push_chain(&[
            (self.frame_hdr_pa, 20, false),
            (self.compressed_pa, data_len as u32, false),
            (self.status_buf_pa, 24, true),
        ]);

        self.device.notify(DECODEQ);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.decode_vq.pop_used();

        // Read 24-byte status response:
        // [status: u32][bytes_written: u32][timestamp_ns: u64][duration_ns: u64]
        // SAFETY: device has written 24-byte response at status_buf_va.
        unsafe {
            let va = self.status_buf_va;

            video::DecodeFrameReply {
                status: core::ptr::read_volatile(va as *const u32),
                bytes_written: core::ptr::read_volatile((va + 4) as *const u32),
                timestamp_ns: core::ptr::read_volatile((va + 8) as *const u64),
                duration_ns: core::ptr::read_volatile((va + 16) as *const u64),
            }
        }
    }
}

struct VideoServer {
    driver: VideoDriver,
}

impl Dispatch for VideoServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            video::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let vmo = Handle(msg.handles[0]);
                let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                match abi::vmo::map(vmo, 0, rw) {
                    Ok(va) => {
                        self.driver.shared_va = va;
                        self.driver.shared_len = PAGE_SIZE * COMPRESSED_BUF_PAGES;

                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }
            video::GET_INFO => {
                let reply = video::InfoReply {
                    supported_codecs: self.driver.supported_codecs,
                    max_width: self.driver.max_width,
                    max_height: self.driver.max_height,
                };
                let mut data = [0u8; video::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            video::CREATE_SESSION => {
                if msg.payload.len() < video::CreateSessionRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = video::CreateSessionRequest::read_from(msg.payload);
                let (status, session_id, texture_handle) =
                    self.driver.create_session(req.codec, req.width, req.height);

                if status != 0 {
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                    return;
                }

                let reply = video::CreateSessionReply {
                    session_id,
                    texture_handle,
                };
                let mut data = [0u8; video::CreateSessionReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            video::DECODE_FRAME => {
                if msg.payload.len() < video::DecodeFrameRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = video::DecodeFrameRequest::read_from(msg.payload);
                let offset = req.offset as usize;
                let size = req.size as usize;

                if self.driver.shared_va == 0 || offset + size > self.driver.shared_len {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                if size > self.driver.compressed_len {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                // SAFETY: shared_va is a mapped VMO, offset+size checked above.
                let compressed = unsafe {
                    core::slice::from_raw_parts((self.driver.shared_va + offset) as *const u8, size)
                };
                let reply = self
                    .driver
                    .decode_frame(req.session_id, compressed, req.timestamp_ns);
                let mut data = [0u8; video::DecodeFrameReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            video::DESTROY_SESSION => {
                if msg.payload.len() < video::SessionRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = video::SessionRequest::read_from(msg.payload);
                let status = self.driver.destroy_session(req.session_id);

                if status != 0 {
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                    return;
                }

                let _ = msg.reply_empty();
            }
            video::FLUSH_SESSION => {
                if msg.payload.len() < video::SessionRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = video::SessionRequest::read_from(msg.payload);
                let status = self.driver.flush_session(req.session_id);

                if status != 0 {
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                    return;
                }

                let _ = msg.reply_empty();
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

fn setup_virtqueue(
    device: &virtio::Device,
    queue_idx: u32,
    init_ep: Handle,
) -> Option<virtio::Virtqueue> {
    let queue_size = device
        .queue_max_size(queue_idx)
        .min(virtio::DEFAULT_QUEUE_SIZE);

    if queue_size == 0 {
        return None;
    }

    let vq_bytes = virtio::Virtqueue::total_bytes(queue_size);
    let vq_alloc = vq_bytes.next_multiple_of(PAGE_SIZE);
    let vq_dma = init::request_dma(init_ep, vq_alloc).ok()?;

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(vq_dma.va as *mut u8, 0, vq_alloc) };

    let vq = virtio::Virtqueue::new(queue_size, vq_dma.va, vq_dma.va as u64);

    device.setup_queue(
        queue_idx,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    Some(vq)
}

/// Format a u32 as "0x" + lowercase hex into `buf`, returning the number
/// of bytes written.
fn format_hex(mut n: u32, buf: &mut [u8]) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    buf[0] = b'0';
    buf[1] = b'x';

    if n == 0 {
        buf[2] = b'0';

        return 3;
    }

    let mut tmp = [0u8; 8];
    let mut i = 8;

    while n > 0 {
        i -= 1;
        tmp[i] = HEX[(n & 0xF) as usize];
        n >>= 4;
    }

    let len = 8 - i;

    buf[2..2 + len].copy_from_slice(&tmp[i..]);

    2 + len
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
    let (device, video_slot) = match virtio::find_device(virtio_va, DEVICE_VIDEO_DECODE) {
        Some(d) => d,
        None => abi::thread::exit(0),
    };
    let (ok, _) = device.negotiate_features(1 << 32);

    if !ok {
        abi::thread::exit(3);
    }

    let ctrl_vq = match setup_virtqueue(&device, CONTROLQ, HANDLE_INIT_EP) {
        Some(vq) => vq,
        None => abi::thread::exit(4),
    };
    let decode_vq = match setup_virtqueue(&device, DECODEQ, HANDLE_INIT_EP) {
        Some(vq) => vq,
        None => abi::thread::exit(4),
    };

    // Allocate DMA buffers.
    let ctrl_dma = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(ctrl_dma.va as *mut u8, 0, PAGE_SIZE) };

    let frame_hdr_dma = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(frame_hdr_dma.va as *mut u8, 0, PAGE_SIZE) };

    let compressed_size = PAGE_SIZE * COMPRESSED_BUF_PAGES;
    let compressed_dma = match init::request_dma(HANDLE_INIT_EP, compressed_size) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(compressed_dma.va as *mut u8, 0, compressed_size) };

    let status_dma = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(status_dma.va as *mut u8, 0, PAGE_SIZE) };

    device.driver_ok();

    // Bind IRQ event for device notifications.
    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(6),
    };
    let irq_num = virtio::SPI_BASE_INTID + video_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(7);
    }

    // Read device config space.
    let supported_codecs = device.config_read32(0);
    let max_width = device.config_read32(4);
    let max_height = device.config_read32(8);

    // Watch for console service to log readiness.
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    // Build log message: "codec-decode: ready (codecs=0xNN)\n"
    let mut log_buf = [0u8; 64];
    let prefix = b"codec-decode: ready (codecs=";
    let plen = prefix.len();

    log_buf[..plen].copy_from_slice(prefix);

    let hex_len = format_hex(supported_codecs, &mut log_buf[plen..]);

    log_buf[plen + hex_len] = b')';
    log_buf[plen + hex_len + 1] = b'\n';

    console::write(console_ep, &log_buf[..plen + hex_len + 2]);

    let mut server = VideoServer {
        driver: VideoDriver {
            device,
            ctrl_vq,
            decode_vq,
            irq_event,
            ctrl_buf_va: ctrl_dma.va,
            ctrl_buf_pa: ctrl_dma.va as u64,
            frame_hdr_va: frame_hdr_dma.va,
            frame_hdr_pa: frame_hdr_dma.va as u64,
            compressed_va: compressed_dma.va,
            compressed_pa: compressed_dma.va as u64,
            compressed_len: compressed_size,
            status_buf_va: status_dma.va,
            status_buf_pa: status_dma.va as u64,
            supported_codecs,
            max_width,
            max_height,
            shared_va: 0,
            shared_len: 0,
            next_session_id: 1,
        },
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
