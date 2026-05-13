//! Audio mixer service — accepts playback requests, forwards PCM to snd driver.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered by init as "audio")
//!
//! Looks up the "snd" driver via name service. Runs an event-driven
//! loop: when idle, blocks on recv. During playback, writes one chunk
//! to the snd driver per iteration, polling for STOP between chunks.

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

struct PlaybackState {
    src: usize,
    remaining: usize,
    client_va: usize,
    client_vmo: Handle,
    conv_va: usize,
    conv_vmo: Handle,
}

struct AudioServer {
    snd_ep: Handle,
    _shared_vmo: Handle,
    shared_va: usize,
    playback: Option<PlaybackState>,
}

impl AudioServer {
    fn write_next_chunk(&mut self) {
        let pb = match self.playback.as_mut() {
            Some(pb) => pb,
            None => return,
        };

        let chunk = pb.remaining.min(SHARED_SIZE);

        if chunk == 0 {
            self.stop_playback();

            return;
        }

        // SAFETY: src points into a mapped VMO; shared_va is SHARED_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(pb.src as *const u8, self.shared_va as *mut u8, chunk);
        }

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
            self.stop_playback();

            return;
        }

        let pb = self.playback.as_mut().unwrap();

        pb.src += chunk;
        pb.remaining -= chunk;

        if pb.remaining == 0 {
            self.stop_playback();
        }
    }

    fn stop_playback(&mut self) {
        if let Some(pb) = self.playback.take() {
            if pb.client_va != 0 {
                let _ = abi::vmo::unmap(pb.client_va);
            }
            if pb.client_vmo.0 != 0 {
                let _ = abi::handle::close(pb.client_vmo);
            }
            if pb.conv_va != 0 {
                let _ = abi::vmo::unmap(pb.conv_va);
            }
            if pb.conv_vmo.0 != 0 {
                let _ = abi::handle::close(pb.conv_vmo);
            }
        }
    }

    fn handle_play(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < audio_service::PlayRequest::SIZE || msg.handles.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        self.stop_playback();

        let req = audio_service::PlayRequest::read_from(msg.payload);
        let data_vmo = Handle(msg.handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);
        let data_va = match abi::vmo::map(data_vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(data_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        match req.format {
            FORMAT_F32_STEREO_48K => {
                let offset = req.data_offset as usize;
                let len = req.data_len as usize;

                self.playback = Some(PlaybackState {
                    src: data_va + offset,
                    remaining: len,
                    client_va: data_va,
                    client_vmo: data_vmo,
                    conv_va: 0,
                    conv_vmo: Handle(0),
                });
            }
            FORMAT_WAV => {
                let wav_data = unsafe {
                    core::slice::from_raw_parts(data_va as *const u8, req.data_len as usize)
                };

                if let Ok(info) = wav::parse(wav_data) {
                    let frame_count = wav::frame_count(&info);
                    let f32_samples = frame_count * 2;
                    let f32_bytes = f32_samples * 4;
                    let f32_pages = f32_bytes.div_ceil(PAGE_SIZE);

                    if let Ok(conv_vmo) = abi::vmo::create(f32_pages * PAGE_SIZE, 0) {
                        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                        if let Ok(conv_va) = abi::vmo::map(conv_vmo, 0, rw) {
                            let out = unsafe {
                                core::slice::from_raw_parts_mut(conv_va as *mut f32, f32_samples)
                            };

                            wav::to_f32_stereo_48k(wav_data, &info, out);

                            let _ = abi::vmo::unmap(data_va);
                            let _ = abi::handle::close(data_vmo);

                            self.playback = Some(PlaybackState {
                                src: conv_va,
                                remaining: f32_bytes,
                                client_va: 0,
                                client_vmo: Handle(0),
                                conv_va,
                                conv_vmo,
                            });
                        } else {
                            let _ = abi::handle::close(conv_vmo);
                        }
                    }
                }

                if self.playback.is_none() {
                    let _ = abi::vmo::unmap(data_va);
                    let _ = abi::handle::close(data_vmo);
                }
            }
            _ => {
                let _ = abi::vmo::unmap(data_va);
                let _ = abi::handle::close(data_vmo);
            }
        }

        let _ = msg.reply_empty();
    }
}

impl Dispatch for AudioServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            audio_service::PLAY => self.handle_play(msg),
            audio_service::STOP => {
                self.stop_playback();

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
        playback: None,
    };

    loop {
        if server.playback.is_some() {
            server.write_next_chunk();

            let _ = ipc::server::serve_one_timed(HANDLE_SVC_EP, &mut server, 1);
        } else {
            let _ = ipc::server::serve_one(HANDLE_SVC_EP, &mut server);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
