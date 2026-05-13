//! Audio mixer service — accepts playback requests, forwards PCM to snd driver.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered by init as "audio")
//!
//! Looks up the "snd" driver via name service. On PLAY: maps the
//! client's data VMO, decodes if WAV, converts to F32 stereo 48 kHz,
//! and forwards to the snd driver via shared VMO + WRITE.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use audio_service::{FORMAT_F32_STEREO_48K, FORMAT_WAV};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_SVC_EP: Handle = Handle(3);

const PAGE_SIZE: usize = 16384;
const SHARED_PAGES: usize = 4;
const SHARED_SIZE: usize = PAGE_SIZE * SHARED_PAGES;

struct AudioServer {
    snd_ep: Handle,
    _shared_vmo: Handle,
    shared_va: usize,
}

impl AudioServer {
    fn forward_f32(&mut self, f32_data: &[f32]) {
        let byte_len = f32_data.len() * 4;
        let mut written = 0;

        while written < byte_len {
            let chunk = (byte_len - written).min(SHARED_SIZE);
            let src = unsafe {
                core::slice::from_raw_parts((f32_data.as_ptr() as *const u8).add(written), chunk)
            };
            let dst = self.shared_va as *mut u8;

            // SAFETY: shared_va is a mapped VMO of SHARED_SIZE bytes.
            unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, chunk) };

            let req = snd::WriteRequest {
                offset: 0,
                len: chunk as u32,
            };
            let mut payload = [0u8; snd::WriteRequest::SIZE];

            req.write_to(&mut payload);

            let mut buf = [0u8; ipc::message::MSG_SIZE];
            let total = ipc::message::write_request(&mut buf, snd::WRITE, &payload);
            let _ = abi::ipc::call(self.snd_ep, &mut buf, total, &[], &mut []);
            let reply = ipc::message::Header::read_from(&buf);

            if reply.is_error() {
                break;
            }

            written += chunk;
        }
    }
}

impl Dispatch for AudioServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            audio_service::PLAY => {
                if msg.payload.len() < audio_service::PlayRequest::SIZE || msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = audio_service::PlayRequest::read_from(msg.payload);
                let data_vmo = Handle(msg.handles[0]);
                let ro = Rights(Rights::READ.0 | Rights::MAP.0);
                let data_va = match abi::vmo::map(data_vmo, 0, ro) {
                    Ok(va) => va,
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);

                        return;
                    }
                };
                let data_offset = req.data_offset as usize;
                let data_len = req.data_len as usize;

                match req.format {
                    FORMAT_F32_STEREO_48K => {
                        let start = data_offset / 4;
                        let f32_count = data_len / 4;
                        let total = start + f32_count;
                        let f32_data =
                            unsafe { core::slice::from_raw_parts(data_va as *const f32, total) };

                        self.forward_f32(&f32_data[start..]);
                    }
                    FORMAT_WAV => {
                        let wav_data =
                            unsafe { core::slice::from_raw_parts(data_va as *const u8, data_len) };

                        if let Ok(info) = wav::parse(wav_data) {
                            let frame_count = wav::frame_count(&info);
                            let f32_samples = frame_count * 2;
                            let f32_bytes = f32_samples * 4;
                            let f32_pages = f32_bytes.div_ceil(PAGE_SIZE);

                            if let Ok(conv_vmo) = abi::vmo::create(f32_pages * PAGE_SIZE, 0) {
                                let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                                if let Ok(conv_va) = abi::vmo::map(conv_vmo, 0, rw) {
                                    let out = unsafe {
                                        core::slice::from_raw_parts_mut(
                                            conv_va as *mut f32,
                                            f32_samples,
                                        )
                                    };

                                    wav::to_f32_stereo_48k(wav_data, &info, out);

                                    self.forward_f32(out);

                                    let _ = abi::vmo::unmap(conv_va);
                                }

                                let _ = abi::handle::close(conv_vmo);
                            }
                        }
                    }
                    _ => {}
                }

                let _ = abi::vmo::unmap(data_va);
                let _ = abi::handle::close(data_vmo);
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
        Err(_) => abi::thread::exit(1),
    };

    console::write(console_ep, b"audio: starting\n");

    let snd_ep = match name::watch(HANDLE_NS_EP, b"snd") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"audio: no snd driver, exiting\n");

            abi::thread::exit(0);
        }
    };
    let shared_vmo = match abi::vmo::create(SHARED_SIZE, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(2),
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let shared_va = match abi::vmo::map(shared_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(3),
    };
    let dup_vmo = match abi::handle::dup(shared_vmo, rw) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(4),
    };
    let mut setup_buf = [0u8; ipc::message::MSG_SIZE];
    let total = ipc::message::write_request(&mut setup_buf, snd::SETUP, &[]);
    let _ = abi::ipc::call(snd_ep, &mut setup_buf, total, &[dup_vmo.0], &mut []);

    console::write(console_ep, b"audio: ready\n");

    let mut server = AudioServer {
        snd_ep,
        _shared_vmo: shared_vmo,
        shared_va,
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
