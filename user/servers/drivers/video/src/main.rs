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
const CTRL_DECODE_AUDIO: u32 = 4;
const CTRL_STOP_AUDIO: u32 = 5;

struct VideoDriver {
    device: virtio::Device,
    ctrl_vq: virtio::Virtqueue,
    decode_vq: virtio::Virtqueue,
    irq_event: Handle,
    ctrl_dma: init::DmaBuf,
    frame_hdr_dma: init::DmaBuf,
    compressed_dma: init::DmaBuf,
    compressed_len: usize,
    status_dma: init::DmaBuf,
    supported_codecs: u32,
    max_width: u32,
    max_height: u32,
    shared_va: usize,
    shared_len: usize,
    next_session_id: u32,
    output_va: usize,
    output_len: usize,
    pixel_dma: Option<init::DmaBuf>,
    pixel_dma_len: usize,
    session_width: u32,
    session_height: u32,
}

impl VideoDriver {
    /// Send a control-queue request and wait for a response.
    ///
    /// Writes `request` into the control DMA buffer, pushes a 2-descriptor
    /// chain (request readable, response writable), notifies the device,
    /// waits for IRQ, and returns the response bytes.
    fn ctrl_request(&mut self, request: &[u8], response_len: u32) {
        let buf = self.ctrl_dma.va as *mut u8;
        let req_len = request.len();

        // SAFETY: ctrl_buf is a PAGE_SIZE DMA buffer, request fits.
        unsafe {
            core::ptr::copy_nonoverlapping(request.as_ptr(), buf, req_len);
            core::ptr::write_bytes(buf.add(req_len), 0, response_len as usize);
        }

        let req_pa = self.ctrl_dma.pa;
        let resp_pa = self.ctrl_dma.pa + req_len as u64;

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

    fn create_session(
        &mut self,
        codec: u8,
        width: u32,
        height: u32,
        codec_data: &[u8],
    ) -> (u32, u32, u32) {
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
        //  20: codec_data_size (u32)
        let codec_data_size = codec_data.len() as u32;
        let mut req = [0u8; 24];

        req[0..4].copy_from_slice(&CTRL_CREATE_SESSION.to_le_bytes());
        req[4..8].copy_from_slice(&session_id.to_le_bytes());
        req[8] = codec;
        req[9] = 0;
        req[12..16].copy_from_slice(&width.to_le_bytes());
        req[16..20].copy_from_slice(&height.to_le_bytes());
        req[20..24].copy_from_slice(&codec_data_size.to_le_bytes());

        let buf = self.ctrl_dma.va as *mut u8;

        // SAFETY: ctrl_buf is a PAGE_SIZE DMA buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(req.as_ptr(), buf, 24);

            if !codec_data.is_empty() {
                core::ptr::copy_nonoverlapping(codec_data.as_ptr(), buf.add(24), codec_data.len());
                core::ptr::write_bytes(buf.add(24 + codec_data.len()), 0, 12);
            } else {
                core::ptr::write_bytes(buf.add(24), 0, 12);
            }
        }

        let req_pa = self.ctrl_dma.pa;
        let resp_offset = if codec_data.is_empty() {
            24u64
        } else {
            24 + codec_data.len() as u64
        };
        let resp_pa = self.ctrl_dma.pa + resp_offset;

        if codec_data.is_empty() {
            self.ctrl_vq
                .push_chain(&[(req_pa, 24, false), (resp_pa, 12, true)]);
        } else {
            let codec_pa = self.ctrl_dma.pa + 24;

            self.ctrl_vq.push_chain(&[
                (req_pa, 24, false),
                (codec_pa, codec_data_size, false),
                (resp_pa, 12, true),
            ]);
        }

        self.device.notify(CONTROLQ);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.ctrl_vq.pop_used();
        let resp_va = self.ctrl_dma.va + resp_offset as usize;

        // SAFETY: device has written 12-byte response.
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
        unsafe { core::ptr::read_volatile((self.ctrl_dma.va + 8) as *const u32) }
    }

    fn flush_session(&mut self, session_id: u32) -> u32 {
        let mut req = [0u8; 8];

        req[0..4].copy_from_slice(&CTRL_FLUSH_SESSION.to_le_bytes());
        req[4..8].copy_from_slice(&session_id.to_le_bytes());

        self.ctrl_request(&req, 4);

        // SAFETY: device has written 4-byte status response.
        unsafe { core::ptr::read_volatile((self.ctrl_dma.va + 8) as *const u32) }
    }

    fn stop_audio(&mut self) {
        let mut req = [0u8; 8];

        req[0..4].copy_from_slice(&CTRL_STOP_AUDIO.to_le_bytes());

        self.ctrl_request(&req, 4);
    }

    fn decode_audio(
        &mut self,
        req: &video::DecodeAudioRequest,
        audio_data: &[u8],
        pcm_dma_pa: u64,
        pcm_dma_len: usize,
    ) -> (u32, u32) {
        let total_input = audio_data.len();

        if total_input > self.compressed_len {
            return (1, 0);
        }

        // SAFETY: compressed_va is a DMA buffer of compressed_len bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                audio_data.as_ptr(),
                self.compressed_dma.va as *mut u8,
                total_input,
            );
        }

        let hdr = self.ctrl_dma.va as *mut u8;

        // SAFETY: ctrl_buf is a PAGE_SIZE DMA buffer.
        unsafe {
            core::ptr::write_bytes(hdr, 0, 32);
            core::ptr::copy_nonoverlapping(CTRL_DECODE_AUDIO.to_le_bytes().as_ptr(), hdr, 4);

            *hdr.add(4) = req.codec;
            *hdr.add(5) = req.channels;

            core::ptr::copy_nonoverlapping(req.sample_rate.to_le_bytes().as_ptr(), hdr.add(8), 4);
            core::ptr::copy_nonoverlapping(req.config_size.to_le_bytes().as_ptr(), hdr.add(12), 4);
            core::ptr::copy_nonoverlapping(req.num_frames.to_le_bytes().as_ptr(), hdr.add(16), 4);
            core::ptr::copy_nonoverlapping(req.data_size.to_le_bytes().as_ptr(), hdr.add(20), 4);
        }
        // Zero the status area (at end of ctrl_buf after header)
        // SAFETY: ctrl_buf is PAGE_SIZE.
        unsafe { core::ptr::write_bytes(hdr.add(24), 0, 8) };

        let hdr_pa = self.ctrl_dma.pa;
        let status_pa = self.ctrl_dma.pa + 24;

        // 4-descriptor chain: header, audio data, PCM output, status
        self.ctrl_vq.push_chain(&[
            (hdr_pa, 24, false),
            (self.compressed_dma.pa, total_input as u32, false),
            (pcm_dma_pa, pcm_dma_len as u32, true),
            (status_pa, 8, true),
        ]);
        self.device.notify(CONTROLQ);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.ctrl_vq.pop_used();

        // SAFETY: device has written 8-byte status+pcm_bytes.
        unsafe {
            let status = core::ptr::read_volatile((self.ctrl_dma.va + 24) as *const u32);
            let pcm_bytes = core::ptr::read_volatile((self.ctrl_dma.va + 28) as *const u32);

            (status, pcm_bytes)
        }
    }

    fn decode_frame(
        &mut self,
        session_id: u32,
        compressed_data: &[u8],
        timestamp_ns: u64,
        output_pixel_offset: usize,
    ) -> video::DecodeFrameReply {
        let data_len = compressed_data.len().min(self.compressed_len);

        // Copy compressed data into DMA buffer.
        // SAFETY: compressed_va is a DMA buffer of compressed_len bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                compressed_data.as_ptr(),
                self.compressed_dma.va as *mut u8,
                data_len,
            );
        }

        // Build 20-byte frame header in frame_hdr_buf:
        // [session_id: u32][flags: u32][compressed_size: u32][reserved: u32][timestamp_ns: u64]
        let hdr = self.frame_hdr_dma.va as *mut u8;

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
        unsafe { core::ptr::write_bytes(self.status_dma.va as *mut u8, 0, 24) };

        let pixel_size = self.session_width as usize * self.session_height as usize * 4;
        let use_pixel_output = self.pixel_dma_len >= pixel_size && pixel_size > 0;

        if use_pixel_output {
            // 4-descriptor chain: header, compressed, pixel output, status
            self.decode_vq.push_chain(&[
                (self.frame_hdr_dma.pa, 20, false),
                (self.compressed_dma.pa, data_len as u32, false),
                (self.pixel_dma.as_ref().unwrap().pa, pixel_size as u32, true),
                (self.status_dma.pa, 24, true),
            ]);
        } else {
            // 3-descriptor chain: header, compressed, status (zero-copy only)
            self.decode_vq.push_chain(&[
                (self.frame_hdr_dma.pa, 20, false),
                (self.compressed_dma.pa, data_len as u32, false),
                (self.status_dma.pa, 24, true),
            ]);
        }
        self.device.notify(DECODEQ);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.decode_vq.pop_used();
        // Read 24-byte status response:
        // [status: u32][bytes_written: u32][timestamp_ns: u64][duration_ns: u64]
        // SAFETY: device has written 24-byte response at status_buf_va.
        let reply = unsafe {
            let va = self.status_dma.va;

            video::DecodeFrameReply {
                status: core::ptr::read_volatile(va as *const u32),
                bytes_written: core::ptr::read_volatile((va + 4) as *const u32),
                timestamp_ns: core::ptr::read_volatile((va + 8) as *const u64),
                duration_ns: core::ptr::read_volatile((va + 16) as *const u64),
            }
        };

        if reply.status == 0 && use_pixel_output && self.output_va != 0 {
            let out_off = output_pixel_offset;
            let copy_len = pixel_size.min(self.output_len.saturating_sub(out_off));

            // SAFETY: pixel_dma holds decoded BGRA from the host.
            // output_va + out_off is within the mapped output VMO.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.pixel_dma.as_ref().unwrap().va as *const u8,
                    (self.output_va + out_off) as *mut u8,
                    copy_len,
                );
            }
        }

        reply
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
                let ro = Rights(Rights::READ.0 | Rights::MAP.0);

                match abi::vmo::map(vmo, 0, ro) {
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

                // Map the output VMO if the client sent one
                if !msg.handles.is_empty() {
                    let output_vmo = Handle(msg.handles[0]);
                    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                    if let Ok(va) = abi::vmo::map(output_vmo, 0, rw) {
                        let pixel_size = req.width as usize * req.height as usize * 4;
                        let output_len =
                            (video::PIXEL_OFFSET + pixel_size).next_multiple_of(PAGE_SIZE);

                        self.driver.output_va = va;
                        self.driver.output_len = output_len;
                        self.driver.session_width = req.width;
                        self.driver.session_height = req.height;

                        // Allocate DMA buffer for decoded pixel output
                        let dma_size = pixel_size.next_multiple_of(PAGE_SIZE);

                        if let Ok(dma) = init::request_dma(HANDLE_INIT_EP, dma_size) {
                            // SAFETY: DMA allocation is valid; zeroing before use.
                            unsafe { core::ptr::write_bytes(dma.va as *mut u8, 0, dma_size) };

                            self.driver.pixel_dma_len = dma_size;
                            self.driver.pixel_dma = Some(dma);
                        }
                    }
                }

                let codec_data_size = req.codec_data_size as usize;
                let codec_data_offset = req.codec_data_offset as usize;
                let codec_data: &[u8] = if codec_data_size > 0
                    && self.driver.shared_va != 0
                    && codec_data_offset + codec_data_size <= self.driver.shared_len
                {
                    // SAFETY: shared_va is valid, bounds checked above.
                    unsafe {
                        core::slice::from_raw_parts(
                            (self.driver.shared_va + codec_data_offset) as *const u8,
                            codec_data_size,
                        )
                    }
                } else {
                    &[]
                };
                let (status, session_id, texture_handle) = self
                    .driver
                    .create_session(req.codec, req.width, req.height, codec_data);

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
                let reply = self.driver.decode_frame(
                    req.session_id,
                    compressed,
                    req.timestamp_ns,
                    req.output_pixel_offset as usize,
                );
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
            video::DECODE_AUDIO => {
                if msg.payload.len() < video::DecodeAudioRequest::SIZE || msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = video::DecodeAudioRequest::read_from(msg.payload);
                let total_input =
                    req.config_size as usize + req.num_frames as usize * 4 + req.data_size as usize;

                if self.driver.shared_va == 0 || total_input > self.driver.shared_len {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                // SAFETY: shared_va is valid, total_input checked above.
                let audio_data = unsafe {
                    core::slice::from_raw_parts(self.driver.shared_va as *const u8, total_input)
                };
                let pcm_vmo = Handle(msg.handles[0]);
                let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
                let pcm_va = match abi::vmo::map(pcm_vmo, 0, rw) {
                    Ok(va) => va,
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                        return;
                    }
                };
                let pcm_dma_size =
                    (req.num_frames as usize * 1024 * 2 * 4 * 2).next_multiple_of(PAGE_SIZE);
                let pcm_dma = match init::request_dma(HANDLE_INIT_EP, pcm_dma_size) {
                    Ok(d) => d,
                    Err(_) => {
                        let _ = abi::vmo::unmap(pcm_va);
                        let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                        return;
                    }
                };

                // SAFETY: DMA allocation is valid; zeroing before use.
                unsafe { core::ptr::write_bytes(pcm_dma.va as *mut u8, 0, pcm_dma_size) };

                let (status, pcm_bytes) =
                    self.driver
                        .decode_audio(&req, audio_data, pcm_dma.va as u64, pcm_dma_size);

                if status != 0 {
                    let _ = abi::vmo::unmap(pcm_va);
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                    return;
                }

                let copy_len = (pcm_bytes as usize).min(pcm_dma_size);

                // SAFETY: pcm_dma.va and pcm_va are valid mappings.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        pcm_dma.va as *const u8,
                        pcm_va as *mut u8,
                        copy_len,
                    );
                }

                let _ = abi::vmo::unmap(pcm_va);
                let reply = video::DecodeAudioReply {
                    status: 0,
                    pcm_bytes,
                };
                let mut data = [0u8; video::DecodeAudioReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            video::STOP_AUDIO => {
                self.driver.stop_audio();

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
) -> Option<(virtio::Virtqueue, init::DmaBuf)> {
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

    let vq = virtio::Virtqueue::new(queue_size, vq_dma.va, vq_dma.pa);

    device.setup_queue(
        queue_idx,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    Some((vq, vq_dma))
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

    let (ctrl_vq, _ctrl_vq_dma) = match setup_virtqueue(&device, CONTROLQ, HANDLE_INIT_EP) {
        Some(pair) => pair,
        None => abi::thread::exit(4),
    };
    let (decode_vq, _decode_vq_dma) = match setup_virtqueue(&device, DECODEQ, HANDLE_INIT_EP) {
        Some(pair) => pair,
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

    // Self-register: create endpoint and register with the name service.
    // Only reached after confirming the device exists — no dangling endpoint.
    let svc_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE020),
    };
    let svc_dup = match abi::handle::dup(svc_ep, Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE021),
    };

    name::register(HANDLE_NS_EP, b"codec-decode", svc_dup);

    console::write(console_ep, &log_buf[..plen + hex_len + 2]);

    let mut server = VideoServer {
        driver: VideoDriver {
            device,
            ctrl_vq,
            decode_vq,
            irq_event,
            ctrl_dma,
            frame_hdr_dma,
            compressed_dma,
            compressed_len: compressed_size,
            status_dma,
            supported_codecs,
            max_width,
            max_height,
            shared_va: 0,
            shared_len: 0,
            next_session_id: 1,
            output_va: 0,
            output_len: 0,
            pixel_dma: None,
            pixel_dma_len: 0,
            session_width: 0,
            session_height: 0,
        },
    };

    ipc::server::serve(svc_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
