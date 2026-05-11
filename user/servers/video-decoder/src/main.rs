//! Video decoder service — decodes MJPEG AVI files to BGRA frames.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered as "video-decoder")
//!
//! Protocol:
//!   OPEN: client sends a VMO containing AVI file data. Service parses
//!         the container, builds a frame index, allocates a reusable
//!         output VMO for decoded BGRA frames, and replies with video
//!         metadata + the output VMO handle.
//!   DECODE_FRAME: client sends a frame index. Service decodes that
//!         MJPEG frame into the output VMO and replies with pixel size.
//!   CLOSE: releases the current video state.

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use alloc::vec::Vec;
use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicU64, Ordering},
};

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_SVC_EP: Handle = Handle(3);

const PAGE_SIZE: usize = 16384;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE001;

struct VideoDecoder {
    console_ep: Handle,
    file_va: usize,
    file_size: usize,
    frame_index: Vec<avi::FrameRef>,
    output_vmo: Handle,
    output_va: usize,
    output_buf_size: usize,
    decode_buf_vmo: Handle,
    decode_buf_va: usize,
    decode_buf_size: usize,
    width: u32,
    height: u32,
    output_gen: u64,
    playing: bool,
    current_frame: u32,
    ns_per_frame: u64,
    next_frame_ns: u64,
}

impl VideoDecoder {
    fn handle_open(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < video_decoder::OpenRequest::SIZE || msg.handles.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);
            return;
        }

        self.close_current();

        let req = video_decoder::OpenRequest::read_from(msg.payload);
        let file_vmo = Handle(msg.handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);
        let file_va = match abi::vmo::map(file_vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);
                return;
            }
        };
        // SAFETY: kernel mapped the VMO at file_va for file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(file_va as *const u8, req.file_size as usize) };
        let info = match avi::parse(file_data) {
            Ok(i) => i,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        if info.codec != avi::FourCC::MJPG && info.codec != avi::FourCC::MJPEG {
            let _ = abi::vmo::unmap(file_va);
            let _ = abi::handle::close(file_vmo);
            let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);

            return;
        }

        let frame_index: Vec<avi::FrameRef> = match avi::VideoFrameIter::new(file_data) {
            Ok(iter) => iter.collect(),
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        let pixel_size = info.width as usize * info.height as usize * 4;
        let output_buf_size =
            (video_decoder::GEN_HEADER_SIZE + pixel_size).next_multiple_of(PAGE_SIZE);
        let output_vmo = match abi::vmo::create(output_buf_size, 0) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let output_va = match abi::vmo::map(output_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = abi::handle::close(output_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let first_frame_data = frame_index
            .first()
            .and_then(|f| avi::frame_data(file_data, f));
        let decode_buf_size = match first_frame_data {
            Some(jpeg_data) => jpeg::jpeg_decode_buf_size(jpeg_data).unwrap_or(0),
            None => 0,
        };

        if decode_buf_size == 0 {
            let _ = abi::vmo::unmap(file_va);
            let _ = abi::handle::close(file_vmo);
            let _ = abi::vmo::unmap(output_va);
            let _ = abi::handle::close(output_vmo);
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let decode_buf_size = decode_buf_size.next_multiple_of(PAGE_SIZE);
        let decode_buf_vmo = match abi::vmo::create(decode_buf_size, 0) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = abi::vmo::unmap(output_va);
                let _ = abi::handle::close(output_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let decode_buf_va = match abi::vmo::map(decode_buf_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = abi::vmo::unmap(output_va);
                let _ = abi::handle::close(output_vmo);
                let _ = abi::handle::close(decode_buf_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let output_dup = match abi::handle::dup(
            output_vmo,
            Rights(Rights::READ.0 | Rights::MAP.0 | Rights::DUP.0),
        ) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = abi::vmo::unmap(output_va);
                let _ = abi::handle::close(output_vmo);
                let _ = abi::vmo::unmap(decode_buf_va);
                let _ = abi::handle::close(decode_buf_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        self.file_va = file_va;
        self.file_size = req.file_size as usize;
        self.frame_index = frame_index;
        self.output_vmo = output_vmo;
        self.output_va = output_va;
        self.output_buf_size = output_buf_size;
        self.decode_buf_vmo = decode_buf_vmo;
        self.decode_buf_va = decode_buf_va;
        self.decode_buf_size = decode_buf_size;
        self.width = info.width;
        self.height = info.height;
        self.output_gen = 0;
        self.playing = false;
        self.current_frame = 0;
        self.ns_per_frame = info.ns_per_frame();
        self.next_frame_ns = 0;

        self.decode_and_publish(0);

        let total = self.frame_index.len() as u32;

        console::write(self.console_ep, b"  video-decoder: opened AVI\n");

        let mut reply_buf = [0u8; video_decoder::OpenReply::SIZE];
        let reply = video_decoder::OpenReply {
            width: info.width,
            height: info.height,
            ns_per_frame: info.ns_per_frame(),
            total_frames: total,
        };

        reply.write_to(&mut reply_buf);

        let _ = msg.reply_ok(&reply_buf, &[output_dup.0]);
    }

    fn decode_and_publish(&mut self, idx: u32) {
        if self.file_va == 0 || self.frame_index.is_empty() {
            return;
        }

        let idx = idx as usize;

        if idx >= self.frame_index.len() {
            return;
        }

        let frame_ref = &self.frame_index[idx];
        // SAFETY: file_va is a valid mapping of file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(self.file_va as *const u8, self.file_size) };
        let jpeg_data = match avi::frame_data(file_data, frame_ref) {
            Some(d) => d,
            None => return,
        };
        // SAFETY: decode_buf_va is a valid RW mapping of decode_buf_size bytes.
        let decode_buf = unsafe {
            core::slice::from_raw_parts_mut(self.decode_buf_va as *mut u8, self.decode_buf_size)
        };
        let header = match jpeg::jpeg_decode(jpeg_data, decode_buf) {
            Ok(h) => h,
            Err(_) => return,
        };
        let pixel_size = header.width as usize * header.height as usize * 4;
        let max_pixels = self
            .output_buf_size
            .saturating_sub(video_decoder::GEN_HEADER_SIZE);

        // SAFETY: output_va is a valid RW mapping. Pixels written at offset 8
        // (after the generation counter header).
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.decode_buf_va as *const u8,
                (self.output_va + video_decoder::GEN_HEADER_SIZE) as *mut u8,
                pixel_size.min(max_pixels),
            );
        }

        self.output_gen = self.output_gen.wrapping_add(1);
        self.current_frame = idx as u32;

        // SAFETY: output_va is 8-byte aligned (page-aligned VMO). Release
        // ordering ensures pixel writes above are visible before the gen bump.
        unsafe {
            let gen_ptr = self.output_va as *const AtomicU64;

            (*gen_ptr).store(self.output_gen, Ordering::Release);
        }
    }

    fn handle_decode_frame(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < video_decoder::DecodeFrameRequest::SIZE {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        if self.file_va == 0 || self.frame_index.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let req = video_decoder::DecodeFrameRequest::read_from(msg.payload);

        if req.frame_index as usize >= self.frame_index.len() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        self.decode_and_publish(req.frame_index);

        let pixel_size = self.width * self.height * 4;
        let mut reply_buf = [0u8; video_decoder::DecodeFrameReply::SIZE];

        video_decoder::DecodeFrameReply { pixel_size }.write_to(&mut reply_buf);

        let _ = msg.reply_ok(&reply_buf, &[]);
    }

    fn close_current(&mut self) {
        if self.file_va != 0 {
            let _ = abi::vmo::unmap(self.file_va);

            self.file_va = 0;
        }
        if self.output_va != 0 {
            let _ = abi::vmo::unmap(self.output_va);

            self.output_va = 0;
        }
        if self.output_vmo.0 != 0 {
            let _ = abi::handle::close(self.output_vmo);

            self.output_vmo = Handle(0);
        }
        if self.decode_buf_va != 0 {
            let _ = abi::vmo::unmap(self.decode_buf_va);

            self.decode_buf_va = 0;
        }
        if self.decode_buf_vmo.0 != 0 {
            let _ = abi::handle::close(self.decode_buf_vmo);

            self.decode_buf_vmo = Handle(0);
        }

        self.frame_index.clear();
        self.file_size = 0;
        self.output_buf_size = 0;
        self.decode_buf_size = 0;
        self.width = 0;
        self.height = 0;
    }
}

impl Dispatch for VideoDecoder {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            video_decoder::OPEN => self.handle_open(msg),
            video_decoder::DECODE_FRAME => self.handle_decode_frame(msg),
            video_decoder::CLOSE => {
                self.close_current();

                let _ = msg.reply_empty();
            }
            video_decoder::PLAY => {
                self.playing = true;
                self.next_frame_ns = abi::system::clock_read().unwrap_or(0);

                let _ = msg.reply_empty();
            }
            video_decoder::PAUSE => {
                self.playing = false;

                let _ = msg.reply_empty();
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_NOT_FOUND),
    };

    console::write(console_ep, b"  video-decoder: starting\n");
    console::write(console_ep, b"  video-decoder: ready\n");

    let mut decoder = VideoDecoder {
        console_ep,
        file_va: 0,
        file_size: 0,
        frame_index: Vec::new(),
        output_vmo: Handle(0),
        output_va: 0,
        output_buf_size: 0,
        decode_buf_vmo: Handle(0),
        decode_buf_va: 0,
        decode_buf_size: 0,
        width: 0,
        height: 0,
        output_gen: 0,
        playing: false,
        current_frame: 0,
        ns_per_frame: 0,
        next_frame_ns: 0,
    };

    loop {
        if decoder.playing && decoder.ns_per_frame > 0 {
            let deadline = decoder.next_frame_ns;

            match ipc::server::serve_one_timed(HANDLE_SVC_EP, &mut decoder, deadline) {
                Ok(()) | Err(abi::types::SyscallError::TimedOut) => {}
                Err(_) => break,
            }
        } else {
            match ipc::server::serve_one(HANDLE_SVC_EP, &mut decoder) {
                Ok(()) => {}
                Err(_) => break,
            }
        }

        if decoder.playing && decoder.ns_per_frame > 0 {
            let now = abi::system::clock_read().unwrap_or(0);

            if now >= decoder.next_frame_ns {
                let total = decoder.frame_index.len() as u32;

                if total > 0 {
                    let next = (decoder.current_frame + 1) % total;

                    decoder.decode_and_publish(next);
                }

                decoder.next_frame_ns = now + decoder.ns_per_frame;
            }
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
