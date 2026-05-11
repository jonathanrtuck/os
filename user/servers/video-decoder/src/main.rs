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
use core::panic::PanicInfo;

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
        let output_buf_size = pixel_size.next_multiple_of(PAGE_SIZE);
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
        let decode_buf_size = self.estimate_decode_buf(info.width, info.height);
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
        let output_dup = match abi::handle::dup(output_vmo, Rights(Rights::READ.0 | Rights::MAP.0))
        {
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
        let idx = req.frame_index as usize;

        if idx >= self.frame_index.len() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let frame_ref = &self.frame_index[idx];
        // SAFETY: file_va is a valid mapping of file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(self.file_va as *const u8, self.file_size) };
        let jpeg_data = match avi::frame_data(file_data, frame_ref) {
            Some(d) => d,
            None => {
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        // SAFETY: decode_buf_va is a valid RW mapping of decode_buf_size bytes.
        let decode_buf = unsafe {
            core::slice::from_raw_parts_mut(self.decode_buf_va as *mut u8, self.decode_buf_size)
        };
        let header = match jpeg::jpeg_decode(jpeg_data, decode_buf) {
            Ok(h) => h,
            Err(_) => {
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        let pixel_size = header.width as usize * header.height as usize * 4;

        // SAFETY: output_va is a valid RW mapping, pixel_size fits within output_buf_size.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.decode_buf_va as *const u8,
                self.output_va as *mut u8,
                pixel_size.min(self.output_buf_size),
            );
        }

        let mut reply_buf = [0u8; video_decoder::DecodeFrameReply::SIZE];

        video_decoder::DecodeFrameReply {
            pixel_size: pixel_size as u32,
        }
        .write_to(&mut reply_buf);

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

    fn estimate_decode_buf(&self, width: u32, height: u32) -> usize {
        let pixel_size = width as usize * height as usize * 4;

        // Progressive JPEG needs coefficient buffer (~2x pixel size).
        // Rotation scratch needs 2x pixel size.
        // Allocate 3x to be safe.
        (pixel_size * 3).next_multiple_of(PAGE_SIZE)
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
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut decoder);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
