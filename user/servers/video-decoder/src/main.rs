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
    file_vmo: Handle,
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
    codec_texture_handle: u32,
    shared_vmo: Handle,
    shared_va: usize,
    codec: u8,
    notify_ep: Handle,
    stats: PlaybackStats,
    audio_pcm_vmo: Handle,
    audio_pcm_va: usize,
    audio_pcm_bytes: u32,
    audio_ep: Handle,
}

struct PlaybackStats {
    frames_decoded: u32,
    frames_skipped: u32,
    total_decode_ns: u64,
    max_decode_ns: u64,
    play_start_ns: u64,
}

fn copy_into(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());

    dst[..len].copy_from_slice(&src[..len]);

    len
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

        if self.notify_ep.0 != 0 {
            let _ = abi::handle::close(self.notify_ep);
        }

        self.notify_ep = if msg.handles.len() > 1 && msg.handles[1] != 0 {
            Handle(msg.handles[1])
        } else {
            Handle(0)
        };

        let ro = Rights(Rights::READ.0 | Rights::MAP.0);
        let file_va = match abi::vmo::map(file_vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        self.file_vmo = file_vmo;

        // SAFETY: kernel mapped the VMO at file_va for file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(file_va as *const u8, req.file_size as usize) };
        let is_avi = file_data.len() >= 4 && file_data[0..4] == RIFF_MAGIC;

        if is_avi {
            self.open_avi(msg, file_va, file_data, file_vmo);
        } else {
            self.open_mp4(msg, file_va, file_data, file_vmo);
        }
    }

    fn open_avi(&mut self, msg: Incoming<'_>, file_va: usize, file_data: &[u8], file_vmo: Handle) {
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
        let ns_per = info.ns_per_frame();
        let frame_pts_ns: Vec<u64> = (0..frame_index.len() as u64).map(|i| i * ns_per).collect();

        self.finish_open(
            msg,
            file_va,
            file_data.len(),
            file_vmo,
            info.width,
            info.height,
            ns_per,
            frame_index,
            frame_pts_ns,
            video::CODEC_MJPEG,
            b"  video-decoder: opened AVI\n",
        );
    }

    fn open_mp4(&mut self, msg: Incoming<'_>, file_va: usize, file_data: &[u8], file_vmo: Handle) {
        let mp4_info = match mp4::parse(file_data) {
            Ok(m) => m,
            Err(_) => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        if mp4_info.total_samples == 0 || mp4_info.avc_config().is_none() {
            let _ = abi::vmo::unmap(file_va);
            let _ = abi::handle::close(file_vmo);
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
            file_vmo,
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
        file_vmo: Handle,
        width: u32,
        height: u32,
        ns_per_frame: u64,
        frame_index: Vec<avi::FrameRef>,
        frame_pts_ns: Vec<u64>,
        codec: u8,
        log_msg: &[u8],
    ) {
        let pixel_size = width as usize * height as usize * 4;
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
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        self.file_va = file_va;
        self.file_size = file_size;
        self.frame_index = frame_index;
        self.frame_pts_ns = frame_pts_ns;
        self.output_vmo = output_vmo;
        self.output_va = output_va;
        self.output_buf_size = output_buf_size;
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

        if self.codec == video::CODEC_H264 {
            self.extract_audio();
        }

        self.decode_and_publish(0);

        let total = self.frame_index.len() as u32;

        console::write(self.console_ep, log_msg);

        let mut reply_buf = [0u8; video_decoder::OpenReply::SIZE];
        let reply = video_decoder::OpenReply {
            width,
            height,
            ns_per_frame,
            total_frames: total,
            host_texture_handle: self.codec_texture_handle,
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
        let output_dup = match abi::handle::dup(
            self.output_vmo,
            Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0 | Rights::DUP.0),
        ) {
            Ok(h) => h,
            Err(_) => return,
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
            &[output_dup.0],
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
        self.codec_texture_handle = cr.texture_handle;
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

    fn extract_audio(&mut self) {
        if self.file_va == 0 || self.codec_ep.0 == 0 || self.shared_va == 0 {
            return;
        }

        // SAFETY: file_va is a valid mapping of file_size bytes.
        let file_data =
            unsafe { core::slice::from_raw_parts(self.file_va as *const u8, self.file_size) };
        let mp4_info = match mp4::parse(file_data) {
            Ok(m) => m,
            Err(_) => return,
        };

        if !mp4_info.has_audio() {
            return;
        }

        let config = match mp4_info.audio_config() {
            Some(c) => c,
            None => return,
        };
        let audio_refs: Vec<mp4::SampleRef> = mp4_info.audio_samples().collect();

        if audio_refs.is_empty() {
            return;
        }

        let config_size = config.len();
        let sizes_size = audio_refs.len() * 4;
        let data_size: usize = audio_refs.iter().map(|s| s.size as usize).sum();
        let total = config_size + sizes_size + data_size;

        if total > SHARED_VMO_SIZE {
            return;
        }

        // Pack into shared VMO: [config][frame_sizes][compressed_data]
        let dst = self.shared_va as *mut u8;
        let mut pos = 0;

        // SAFETY: shared_va is a valid RW mapping of SHARED_VMO_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(config.as_ptr(), dst.add(pos), config_size);
        }

        pos += config_size;

        for r in &audio_refs {
            // SAFETY: pos + 4 <= total <= SHARED_VMO_SIZE.
            unsafe {
                core::ptr::copy_nonoverlapping(r.size.to_le_bytes().as_ptr(), dst.add(pos), 4);
            }

            pos += 4;
        }

        for r in &audio_refs {
            let sample = match mp4_info.sample_data(r) {
                Some(d) => d,
                None => return,
            };

            // SAFETY: pos + sample.len() <= total <= SHARED_VMO_SIZE.
            unsafe {
                core::ptr::copy_nonoverlapping(sample.as_ptr(), dst.add(pos), sample.len());
            }

            pos += sample.len();
        }

        // Allocate PCM output VMO: duration_ns * 48000 * 2ch * 4bytes / 1e9
        let duration_ns = mp4_info.audio_duration_ns();
        let pcm_estimate = (duration_ns as usize * 48000 * 8 / 1_000_000_000) + PAGE_SIZE;
        let pcm_vmo_size = pcm_estimate.next_multiple_of(PAGE_SIZE);
        let pcm_vmo = match abi::vmo::create(pcm_vmo_size, 0) {
            Ok(h) => h,
            Err(_) => return,
        };
        let pcm_dup = match abi::handle::dup(
            pcm_vmo,
            Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0 | Rights::DUP.0),
        ) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::handle::close(pcm_vmo);

                return;
            }
        };
        let req = video::DecodeAudioRequest {
            codec: video::AUDIO_CODEC_AAC,
            channels: mp4_info.audio_channels() as u8,
            sample_rate: mp4_info.audio_sample_rate(),
            config_size: config_size as u32,
            num_frames: audio_refs.len() as u32,
            data_size: data_size as u32,
        };
        let mut req_buf = [0u8; video::DecodeAudioRequest::SIZE];

        req.write_to(&mut req_buf);

        let mut call_buf = [0u8; ipc::message::MSG_SIZE];
        let mut recv_handles = [0u32; 4];
        let reply = match ipc::client::call(
            self.codec_ep,
            video::DECODE_AUDIO,
            &req_buf,
            &[pcm_dup.0],
            &mut recv_handles,
            &mut call_buf,
        ) {
            Ok(r) => r,
            Err(_) => {
                let _ = abi::handle::close(pcm_vmo);

                return;
            }
        };

        if reply.is_error() || reply.payload.len() < video::DecodeAudioReply::SIZE {
            let _ = abi::handle::close(pcm_vmo);

            return;
        }

        let ar = video::DecodeAudioReply::read_from(reply.payload);

        if ar.status != 0 || ar.pcm_bytes == 0 {
            let _ = abi::handle::close(pcm_vmo);

            return;
        }

        let ro = Rights(Rights::READ.0 | Rights::MAP.0);
        let pcm_va = match abi::vmo::map(pcm_vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(pcm_vmo);

                return;
            }
        };

        self.audio_pcm_vmo = pcm_vmo;
        self.audio_pcm_va = pcm_va;
        self.audio_pcm_bytes = ar.pcm_bytes;

        console::write(self.console_ep, b"  video-decoder: audio extracted\n");
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
                output_pixel_offset: video_decoder::GEN_HEADER_SIZE as u32,
            };
            let mut req_buf = [0u8; video::DecodeFrameRequest::SIZE];

            req.write_to(&mut req_buf);

            let _ = ipc::client::call_simple(self.codec_ep, video::DECODE_FRAME, &req_buf);
        }

        self.output_gen = self.output_gen.wrapping_add(1);
        self.current_frame = idx as u32;

        // SAFETY: output_va is page-aligned. Release ordering ensures pixel
        // writes are visible before the gen bump makes the buffer readable.
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
        self.stop_audio_playback();

        if self.file_va != 0 {
            let _ = abi::vmo::unmap(self.file_va);

            self.file_va = 0;
        }
        if self.file_vmo.0 != 0 {
            let _ = abi::handle::close(self.file_vmo);

            self.file_vmo = Handle(0);
        }
        if self.output_va != 0 {
            let _ = abi::vmo::unmap(self.output_va);

            self.output_va = 0;
        }
        if self.output_vmo.0 != 0 {
            let _ = abi::handle::close(self.output_vmo);

            self.output_vmo = Handle(0);
        }
        if self.audio_pcm_va != 0 {
            let _ = abi::vmo::unmap(self.audio_pcm_va);

            self.audio_pcm_va = 0;
        }
        if self.audio_pcm_vmo.0 != 0 {
            let _ = abi::handle::close(self.audio_pcm_vmo);

            self.audio_pcm_vmo = Handle(0);
        }

        self.audio_pcm_bytes = 0;
        self.frame_index.clear();
        self.frame_pts_ns.clear();
        self.file_size = 0;
        self.output_buf_size = 0;
        self.width = 0;
        self.height = 0;
        self.codec = 0;
    }

    fn stop_audio_playback(&self) {
        if self.audio_ep.0 != 0 {
            let _ = ipc::client::call_simple(self.audio_ep, audio_service::STOP, &[]);
        }
        if self.codec_ep.0 != 0 {
            let _ = ipc::client::call_simple(self.codec_ep, video::STOP_AUDIO, &[]);
        }
    }

    fn start_audio_playback(&mut self) {
        if self.audio_pcm_vmo.0 == 0 || self.audio_pcm_bytes == 0 {
            return;
        }

        if self.audio_ep.0 == 0 {
            self.audio_ep = match name::lookup(HANDLE_NS_EP, b"audio") {
                Ok(h) => h,
                Err(_) => return,
            };
        }

        let pcm_dup =
            match abi::handle::dup(self.audio_pcm_vmo, Rights(Rights::READ.0 | Rights::MAP.0)) {
                Ok(h) => h,
                Err(_) => return,
            };
        let current_pts = self
            .frame_pts_ns
            .get(self.current_frame as usize)
            .copied()
            .unwrap_or(0);
        let pcm_offset = (current_pts * 48000 * 8 / 1_000_000_000) as u32 & !7;
        let remaining = self.audio_pcm_bytes.saturating_sub(pcm_offset);
        let req = audio_service::PlayRequest {
            format: audio_service::FORMAT_F32_STEREO_48K,
            data_len: remaining,
            data_offset: pcm_offset,
        };
        let mut payload = [0u8; audio_service::PlayRequest::SIZE];

        req.write_to(&mut payload);

        let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
        let _ = ipc::client::call(
            self.audio_ep,
            audio_service::PLAY,
            &payload,
            &[pcm_dup.0],
            &mut [],
            &mut reply_buf,
        );
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
                    let current_pts = self
                        .frame_pts_ns
                        .get(self.current_frame as usize)
                        .copied()
                        .unwrap_or(0);

                    self.play_start_ns = now.saturating_sub(current_pts);
                    self.stats = PlaybackStats {
                        frames_decoded: 0,
                        frames_skipped: 0,
                        total_decode_ns: 0,
                        max_decode_ns: 0,
                        play_start_ns: now,
                    };

                    let mut buf = [0u8; 60];
                    let mut p = 0;

                    p += copy_into(&mut buf[p..], b"vdec: PLAY total=");
                    p += console::format_u32(self.frame_index.len() as u32, &mut buf[p..]);
                    p += copy_into(&mut buf[p..], b" nsf=");
                    p += console::format_u32((self.ns_per_frame / 1000) as u32, &mut buf[p..]);

                    buf[p] = b'\n';

                    p += 1;

                    console::write(self.console_ep, &buf[..p]);

                    self.start_audio_playback();
                } else {
                    self.stop_audio_playback();

                    console::write(self.console_ep, b"vdec: PAUSE\n");
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

                self.stop_audio_playback();
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
        file_vmo: Handle(0),
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
        codec_texture_handle: 0,
        shared_vmo: Handle(0),
        shared_va: 0,
        codec: 0,
        notify_ep: Handle(0),
        stats: PlaybackStats {
            frames_decoded: 0,
            frames_skipped: 0,
            total_decode_ns: 0,
            max_decode_ns: 0,
            play_start_ns: 0,
        },
        audio_pcm_vmo: Handle(0),
        audio_pcm_va: 0,
        audio_pcm_bytes: 0,
        audio_ep: Handle(0),
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
            let got_ipc = match ipc::server::serve_one_timed(HANDLE_SVC_EP, &mut decoder, deadline)
            {
                Ok(()) => true,
                Err(abi::types::SyscallError::TimedOut) => false,
                Err(_) => break,
            };

            if got_ipc {
                continue;
            }

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

                decoder.stop_audio_playback();
                decoder.decode_and_publish(0);
                decoder.publish_status();
                decoder.report_stats();

                if decoder.notify_ep.0 != 0 {
                    const PRESENTER_VIDEO_PLAYBACK_ENDED: u32 = 8;

                    let _ = ipc::client::call_simple(
                        decoder.notify_ep,
                        PRESENTER_VIDEO_PLAYBACK_ENDED,
                        &[],
                    );
                }
            }
        } else {
            match ipc::server::serve_one(HANDLE_SVC_EP, &mut decoder) {
                Ok(()) => {}
                Err(_) => break,
            }
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
