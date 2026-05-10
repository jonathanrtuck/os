//! JPEG decoder service — decodes JPEG images to BGRA8888 pixels.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered as "jpeg-decoder")
//!
//! Enters an IPC serve loop on the pre-registered endpoint. Clients send a VMO containing JPEG file data along with the
//! file size; the service maps it, decodes, creates an output VMO with
//! BGRA pixels, and replies with the output VMO handle + dimensions.

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_SVC_EP: Handle = Handle(3);

const PAGE_SIZE: usize = 16384;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE001;

struct JpegDecoder {
    _console_ep: Handle,
}

impl Dispatch for JpegDecoder {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            jpeg_decoder::DECODE => self.handle_decode(msg),
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

impl JpegDecoder {
    fn handle_decode(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < jpeg_decoder::DecodeRequest::SIZE || msg.handles.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);
            return;
        }

        let req = jpeg_decoder::DecodeRequest::read_from(msg.payload);
        let jpeg_vmo = Handle(msg.handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);
        let jpeg_va = match abi::vmo::map(jpeg_vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(jpeg_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        // SAFETY: kernel mapped the VMO at jpeg_va, file_size is within bounds.
        let jpeg_data =
            unsafe { core::slice::from_raw_parts(jpeg_va as *const u8, req.file_size as usize) };
        let buf_size = match jpeg::jpeg_decode_buf_size(jpeg_data) {
            Ok(s) => s,
            Err(_) => {
                let _ = abi::vmo::unmap(jpeg_va);
                let _ = abi::handle::close(jpeg_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        let decode_buf_vmo_size = buf_size.next_multiple_of(PAGE_SIZE);
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let decode_vmo = match abi::vmo::create(decode_buf_vmo_size, 0) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::vmo::unmap(jpeg_va);
                let _ = abi::handle::close(jpeg_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let decode_va = match abi::vmo::map(decode_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::vmo::unmap(jpeg_va);
                let _ = abi::handle::close(jpeg_vmo);
                let _ = abi::handle::close(decode_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        // SAFETY: decode_vmo is mapped RW at decode_va with buf_size usable bytes.
        let output = unsafe { core::slice::from_raw_parts_mut(decode_va as *mut u8, buf_size) };
        let header = match jpeg::jpeg_decode(jpeg_data, output) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::vmo::unmap(jpeg_va);
                let _ = abi::handle::close(jpeg_vmo);
                let _ = abi::vmo::unmap(decode_va);
                let _ = abi::handle::close(decode_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        // Done with input VMO.
        let _ = abi::vmo::unmap(jpeg_va);
        let _ = abi::handle::close(jpeg_vmo);
        let pixel_size = header.width as usize * header.height as usize * 4;
        // Create a pixel-sized output VMO and copy BGRA data.
        let pixel_vmo_size = pixel_size.next_multiple_of(PAGE_SIZE);
        let pixel_vmo = match abi::vmo::create(pixel_vmo_size, 0) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::vmo::unmap(decode_va);
                let _ = abi::handle::close(decode_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let pixel_va = match abi::vmo::map(pixel_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::vmo::unmap(decode_va);
                let _ = abi::handle::close(decode_vmo);
                let _ = abi::handle::close(pixel_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };

        // SAFETY: both mappings are valid and non-overlapping.
        unsafe {
            core::ptr::copy_nonoverlapping(decode_va as *const u8, pixel_va as *mut u8, pixel_size);
        }

        // Clean up the decode buffer.
        let _ = abi::vmo::unmap(decode_va);
        let _ = abi::handle::close(decode_vmo);
        // Unmap the pixel VMO locally — the handle is transferred to the caller.
        let _ = abi::vmo::unmap(pixel_va);
        let mut reply_buf = [0u8; jpeg_decoder::DecodeReply::SIZE];
        let reply = jpeg_decoder::DecodeReply {
            width: header.width,
            height: header.height,
            pixel_size: pixel_size as u32,
        };

        reply.write_to(&mut reply_buf);

        let _ = msg.reply_ok(&reply_buf, &[pixel_vmo.0]);
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_NOT_FOUND),
    };

    console::write(console_ep, b"  jpeg-decoder: starting\n");

    console::write(console_ep, b"  jpeg-decoder: ready\n");

    let mut decoder = JpegDecoder {
        _console_ep: console_ep,
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut decoder);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
