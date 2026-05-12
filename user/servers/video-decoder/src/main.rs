//! Video decoder service — decodes AVI and MP4 video via hardware VideoToolbox.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered as "video-decoder")
//!
//! Protocol:
//!   OPEN: client sends a VMO containing file data (AVI or MP4). Service
//!         parses the container, builds a frame index, creates a hardware
//!         decode session, and replies with video metadata + output VMO.
//!   DECODE_FRAME: client sends a frame index. Service decodes that
//!         frame via VideoToolbox and replies with pixel size.
//!   CLOSE: releases the current video state.

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use alloc::{vec, vec::Vec};
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

const SHARED_VMO_SIZE: usize = PAGE_SIZE * 64;

const RIFF_MAGIC: [u8; 4] = *b"RIFF";

struct VideoDecoder {
    console_ep: Handle,
    file_va: usize,
    file_size: usize,
    frame_index: Vec<avi::FrameRef>,
    frame_pts_ns: Vec<u64>,
    output_vmo: Handle,
    output_va: usize,
    output_buf_size: usize,
    width: u32,
    height: u32,
    output_gen: u64,
    playing: bool,
    current_frame: u32,
    ns_per_frame: u64,
    play_start_ns: u64,
    codec_ep: Handle,
    codec_session_id: u32,
    shared_vmo: Handle,
    shared_va: usize,
    codec: u8,
    texture_handle: u32,
    stats: PlaybackStats,
}

struct PlaybackStats {
    frames_decoded: u32,
    frames_skipped: u32,
    total_decode_ns: u64,
    max_decode_ns: u64,
    play_start_ns: u64,
}

fn reformat_avcc(avcc: &[u8], out: &mut [u8]) -> usize {
    if avcc.len() < 7 {
        return 0;
    }

    let nal_length_size = (avcc[4] & 0x03) + 1;
    let num_sps = (avcc[5] & 0x1F) as usize;
    let mut read_pos = 6;
    let mut params: Vec<(usize, usize)> = Vec::new();

    for _ in 0..num_sps {
        if read_pos + 2 > avcc.len() {
            return 0;
        }

        let len = u16::from_be_bytes([avcc[read_pos], avcc[read_pos + 1]]) as usize;

        read_pos += 2;

        if read_pos + len > avcc.len() {
            return 0;
        }

        params.push((read_pos, len));

        read_pos += len;
    }

    if read_pos >= avcc.len() {
        return 0;
    }

    let num_pps = avcc[read_pos] as usize;

    read_pos += 1;

    for _ in 0..num_pps {
        if read_pos + 2 > avcc.len() {
            return 0;
        }

        let len = u16::from_be_bytes([avcc[read_pos], avcc[read_pos + 1]]) as usize;

        read_pos += 2;

        if read_pos + len > avcc.len() {
            return 0;
        }

        params.push((read_pos, len));

        read_pos += len;
    }

    let total_params = params.len();
    let needed = 4 + params.iter().map(|(_, len)| 4 + len).sum::<usize>();

    if needed > out.len() {
        return 0;
    }

    out[0] = nal_length_size;
    out[1] = total_params as u8;
    out[2] = 0;
    out[3] = 0;

    let mut write_pos = 4;

    for &(offset, len) in &params {
        out[write_pos..write_pos + 4].copy_from_slice(&(len as u32).to_le_bytes());

        write_pos += 4;

        out[write_pos..write_pos + len].copy_from_slice(&avcc[offset..offset + len]);

        write_pos += len;
    }

    write_pos
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
            Ok(va) => {
                let _ = abi::handle::close(file_vmo);

                va
            }
            Err(_) => {
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        // SAFETY: kernel mapped the VMO at file_va for file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(file_va as *const u8, req.file_size as usize) };
        let is_avi = file_data.len() >= 4 && file_data[0..4] == RIFF_MAGIC;

        if is_avi {
            self.open_avi(msg, file_va, file_data);
        } else {
            self.open_mp4(msg, file_va, file_data);
        }
    }

    fn open_avi(&mut self, msg: Incoming<'_>, file_va: usize, file_data: &[u8]) {
        let info = match avi::parse(file_data) {
            Ok(i) => i,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        if info.codec != avi::FourCC::MJPG && info.codec != avi::FourCC::MJPEG {
            let _ = abi::vmo::unmap(file_va);
            let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);

            return;
        }

        let frame_index: Vec<avi::FrameRef> = match avi::VideoFrameIter::new(file_data) {
            Ok(iter) => iter.collect(),
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        let ns_per = info.ns_per_frame();
        let frame_pts_ns: Vec<u64> = (0..frame_index.len() as u64).map(|i| i * ns_per).collect();

        self.finish_open(
            msg,
            file_va,
            file_data.len(),
            info.width,
            info.height,
            ns_per,
            frame_index,
            frame_pts_ns,
            video::CODEC_MJPEG,
            b"  video-decoder: opened AVI\n",
        );
    }

    fn open_mp4(&mut self, msg: Incoming<'_>, file_va: usize, file_data: &[u8]) {
        let mp4_info = match mp4::parse(file_data) {
            Ok(m) => m,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        if mp4_info.total_samples == 0 || mp4_info.avc_config().is_none() {
            let _ = abi::vmo::unmap(file_va);
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let timescale = mp4_info.timescale as u64;
        let mut frame_index: Vec<avi::FrameRef> = Vec::new();
        let mut frame_pts_ns: Vec<u64> = Vec::new();

        for s in mp4_info.samples() {
            frame_index.push(avi::FrameRef {
                offset: s.offset as u32,
                size: s.size,
            });

            let pts_ns = (s.pts_ticks * 1_000_000_000)
                .checked_div(timescale)
                .unwrap_or(0);

            frame_pts_ns.push(pts_ns);
        }

        self.finish_open(
            msg,
            file_va,
            file_data.len(),
            mp4_info.width,
            mp4_info.height,
            mp4_info.ns_per_frame(),
            frame_index,
            frame_pts_ns,
            video::CODEC_H264,
            b"  video-decoder: opened MP4\n",
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_open(
        &mut self,
        msg: Incoming<'_>,
        file_va: usize,
        file_size: usize,
        width: u32,
        height: u32,
        ns_per_frame: u64,
        frame_index: Vec<avi::FrameRef>,
        frame_pts_ns: Vec<u64>,
        codec: u8,
        log_msg: &[u8],
    ) {
        self.file_va = file_va;
        self.file_size = file_size;
        self.frame_index = frame_index;
        self.frame_pts_ns = frame_pts_ns;
        self.width = width;
        self.height = height;
        self.output_gen = 0;
        self.playing = false;
        self.current_frame = 0;
        self.ns_per_frame = ns_per_frame;
        self.play_start_ns = 0;
        self.codec = codec;

        self.setup_hardware_session();

        if self.codec_session_id == 0 {
            self.close_current();

            let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);

            return;
        }

        let output_buf_size = video_decoder::GEN_HEADER_SIZE.next_multiple_of(PAGE_SIZE);
        let output_vmo = match abi::vmo::create(output_buf_size, 0) {
            Ok(h) => h,
            Err(_) => {
                self.close_current();

                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let output_va = match abi::vmo::map(output_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(output_vmo);

                self.close_current();

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
                let _ = abi::vmo::unmap(output_va);
                let _ = abi::handle::close(output_vmo);

                self.close_current();

                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        self.output_vmo = output_vmo;
        self.output_va = output_va;
        self.output_buf_size = output_buf_size;

        self.decode_and_publish(0);

        let total = self.frame_index.len() as u32;

        console::write(self.console_ep, log_msg);

        let mut reply_buf = [0u8; video_decoder::OpenReply::SIZE];
        let reply = video_decoder::OpenReply {
            width,
            height,
            ns_per_frame,
            total_frames: total,
            texture_handle: self.texture_handle,
        };

        reply.write_to(&mut reply_buf);

        let _ = msg.reply_ok(&reply_buf, &[output_dup.0]);
    }

    fn setup_hardware_session(&mut self) {
        if self.width == 0 || self.height == 0 {
            return;
        }

        if self.codec_ep.0 == 0 {
            self.codec_ep = name::lookup(HANDLE_NS_EP, b"codec-decode").unwrap_or(Handle(0));

            if self.codec_ep.0 == 0 {
                return;
            }
        }

        if self.shared_vmo.0 == 0 {
            let vmo = match abi::vmo::create(SHARED_VMO_SIZE, 0) {
                Ok(h) => h,
                Err(_) => return,
            };
            let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
            let va = match abi::vmo::map(vmo, 0, rw) {
                Ok(va) => va,
                Err(_) => {
                    let _ = abi::handle::close(vmo);
                    return;
                }
            };

            self.shared_vmo = vmo;
            self.shared_va = va;

            let shared_dup =
                match abi::handle::dup(vmo, Rights(Rights::READ.0 | Rights::MAP.0 | Rights::DUP.0))
                {
                    Ok(h) => h,
                    Err(_) => return,
                };
            let mut setup_buf = [0u8; ipc::message::MSG_SIZE];
            let _ = ipc::client::call(
                self.codec_ep,
                video::SETUP,
                &[],
                &[shared_dup.0],
                &mut [],
                &mut setup_buf,
            );
        }

        let codec_data_size = if self.codec == video::CODEC_H264 && self.shared_va != 0 {
            self.write_codec_data()
        } else {
            0
        };
        let req = video::CreateSessionRequest {
            codec: self.codec,
            width: self.width,
            height: self.height,
            codec_data_offset: 0,
            codec_data_size: codec_data_size as u32,
        };
        let mut req_buf = [0u8; video::CreateSessionRequest::SIZE];

        req.write_to(&mut req_buf);

        let mut call_buf = [0u8; ipc::message::MSG_SIZE];
        let reply = match ipc::client::call(
            self.codec_ep,
            video::CREATE_SESSION,
            &req_buf,
            &[],
            &mut [],
            &mut call_buf,
        ) {
            Ok(r) => r,
            Err(_) => return,
        };

        if reply.is_error() || reply.payload.len() < video::CreateSessionReply::SIZE {
            return;
        }

        let cr = video::CreateSessionReply::read_from(reply.payload);

        self.codec_session_id = cr.session_id;
        self.texture_handle = cr.texture_handle;
    }

    fn write_codec_data(&self) -> usize {
        if self.file_va == 0 || self.shared_va == 0 {
            return 0;
        }

        // SAFETY: file_va is a valid mapping of file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(self.file_va as *const u8, self.file_size) };
        let mp4_info = match mp4::parse(file_data) {
            Ok(m) => m,
            Err(_) => return 0,
        };
        let (_nal_len, avcc_body) = match mp4_info.avc_config() {
            Some(cfg) => cfg,
            None => return 0,
        };
        let mut reformat_buf = vec![0u8; 1024];
        let written = reformat_avcc(avcc_body, &mut reformat_buf);

        if written == 0 || written > SHARED_VMO_SIZE {
            return 0;
        }

        // SAFETY: shared_va is a valid RW mapping of SHARED_VMO_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                reformat_buf.as_ptr(),
                self.shared_va as *mut u8,
                written,
            );
        }

        written
    }

    fn decode_and_publish(&mut self, idx: u32) {
        if self.file_va == 0 || self.frame_index.is_empty() || self.codec_session_id == 0 {
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
        let frame_data = match avi::frame_data(file_data, frame_ref) {
            Some(d) => d,
            None => return,
        };
        if self.shared_va != 0 && frame_data.len() <= SHARED_VMO_SIZE {
            // SAFETY: shared_va is a valid RW mapping of SHARED_VMO_SIZE bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    frame_data.as_ptr(),
                    self.shared_va as *mut u8,
                    frame_data.len(),
                );
            }

            let pts_ns = self
                .frame_pts_ns
                .get(idx)
                .copied()
                .unwrap_or(idx as u64 * self.ns_per_frame);
            let req = video::DecodeFrameRequest {
                session_id: self.codec_session_id,
                offset: 0,
                size: frame_data.len() as u32,
                timestamp_ns: pts_ns,
                output_pixel_offset: 0,
            };
            let mut req_buf = [0u8; video::DecodeFrameRequest::SIZE];

            req.write_to(&mut req_buf);

            let _ = ipc::client::call_simple(self.codec_ep, video::DECODE_FRAME, &req_buf);
        }

        self.output_gen = self.output_gen.wrapping_add(1);
        self.current_frame = idx as u32;

        // SAFETY: output_va is page-aligned. Release ordering ensures the
        // host-side texture update is sequenced before the gen bump signals
        // the render driver to re-composite.
        unsafe {
            let gen_ptr = self.output_va as *const AtomicU64;

            (*gen_ptr).store(self.output_gen, Ordering::Release);
        }
    }

    fn publish_status(&self) {
        if self.output_va == 0 {
            return;
        }

        // SAFETY: output_va + 8 is within the 16-byte header (page-aligned VMO).
        unsafe {
            let flags_ptr = (self.output_va + 8) as *const AtomicU64;

            (*flags_ptr).store(u64::from(self.playing), Ordering::Release);
        }
    }

    fn report_stats(&self) {
        if self.stats.frames_decoded == 0 {
            return;
        }

        let avg_us = (self.stats.total_decode_ns / self.stats.frames_decoded as u64) / 1000;
        let max_us = self.stats.max_decode_ns / 1000;
        let elapsed_ms = if self.stats.play_start_ns > 0 {
            let now = abi::system::clock_read().unwrap_or(0);

            now.saturating_sub(self.stats.play_start_ns) / 1_000_000
        } else {
            0
        };
        let target_us = self.ns_per_frame / 1000;

        console::write(self.console_ep, b"  video-decoder: ");
        console::write_u32(self.console_ep, b"decoded=", self.stats.frames_decoded);
        console::write_u32(self.console_ep, b" skipped=", self.stats.frames_skipped);
        console::write_u32(self.console_ep, b" avg=", avg_us as u32);
        console::write_u32(self.console_ep, b"us max=", max_us as u32);
        console::write_u32(self.console_ep, b"us budget=", target_us as u32);
        console::write_u32(self.console_ep, b"us wall=", elapsed_ms as u32);
        console::write(self.console_ep, b"ms\n");
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

        self.frame_index.clear();
        self.frame_pts_ns.clear();
        self.file_size = 0;
        self.output_buf_size = 0;
        self.width = 0;
        self.height = 0;
        self.codec = 0;
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
            video_decoder::TOGGLE => {
                if self.playing {
                    self.report_stats();
                }

                self.playing = !self.playing;

                if self.playing {
                    let now = abi::system::clock_read().unwrap_or(0);

                    self.play_start_ns = now;
                    self.stats = PlaybackStats {
                        frames_decoded: 0,
                        frames_skipped: 0,
                        total_decode_ns: 0,
                        max_decode_ns: 0,
                        play_start_ns: now,
                    };
                }

                self.publish_status();

                let mut reply_buf = [0u8; video_decoder::ToggleReply::SIZE];

                video_decoder::ToggleReply {
                    playing: u8::from(self.playing),
                }
                .write_to(&mut reply_buf);

                let _ = msg.reply_ok(&reply_buf, &[]);
            }
            video_decoder::PAUSE => {
                self.playing = false;

                self.publish_status();

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

    let _ = abi::thread::set_priority(Handle::SELF, abi::types::Priority::High);

    console::write(console_ep, b"  video-decoder: starting\n");
    console::write(console_ep, b"  video-decoder: ready\n");

    let mut decoder = VideoDecoder {
        console_ep,
        file_va: 0,
        file_size: 0,
        frame_index: Vec::new(),
        frame_pts_ns: Vec::new(),
        output_vmo: Handle(0),
        output_va: 0,
        output_buf_size: 0,
        width: 0,
        height: 0,
        output_gen: 0,
        playing: false,
        current_frame: 0,
        ns_per_frame: 0,
        play_start_ns: 0,
        codec_ep: Handle(0),
        codec_session_id: 0,
        shared_vmo: Handle(0),
        shared_va: 0,
        codec: 0,
        texture_handle: 0,
        stats: PlaybackStats {
            frames_decoded: 0,
            frames_skipped: 0,
            total_decode_ns: 0,
            max_decode_ns: 0,
            play_start_ns: 0,
        },
    };

    loop {
        if decoder.playing && !decoder.frame_pts_ns.is_empty() {
            let next = (decoder.current_frame + 1) as usize;
            let deadline = if next < decoder.frame_pts_ns.len() {
                decoder.play_start_ns + decoder.frame_pts_ns[next]
            } else {
                decoder.play_start_ns
                    + decoder.frame_pts_ns.last().copied().unwrap_or(0)
                    + decoder.ns_per_frame
            };

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

        if decoder.playing && !decoder.frame_pts_ns.is_empty() {
            let now = abi::system::clock_read().unwrap_or(0);
            let elapsed = now.saturating_sub(decoder.play_start_ns);
            let total = decoder.frame_pts_ns.len() as u32;
            let mut target = decoder.current_frame;

            while (target + 1) < total {
                if decoder.frame_pts_ns[(target + 1) as usize] > elapsed {
                    break;
                }

                target += 1;
            }

            if target > decoder.current_frame {
                let skipped = target - decoder.current_frame - 1;

                if skipped > 0 {
                    decoder.stats.frames_skipped += skipped;
                }

                let t0 = abi::system::clock_read().unwrap_or(0);

                decoder.decode_and_publish(target);

                let dt = abi::system::clock_read().unwrap_or(0).saturating_sub(t0);

                decoder.stats.frames_decoded += 1;
                decoder.stats.total_decode_ns += dt;

                if dt > decoder.stats.max_decode_ns {
                    decoder.stats.max_decode_ns = dt;
                }
            }

            if decoder.current_frame >= total - 1 {
                decoder.playing = false;

                decoder.decode_and_publish(0);
                decoder.publish_status();
                decoder.report_stats();
            }
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
