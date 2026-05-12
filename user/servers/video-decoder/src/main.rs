//! Video decoder service — decodes MJPEG AVI and H.264 MP4 files to BGRA frames.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered as "video-decoder")
//!
//! Protocol:
//!   OPEN: client sends a VMO containing file data (AVI or MP4). Service
//!         parses the container, builds a frame index, allocates a reusable
//!         output VMO for decoded BGRA frames, and replies with video
//!         metadata + the output VMO handle.
//!   DECODE_FRAME: client sends a frame index. Service decodes that
//!         frame into the output VMO and replies with pixel size.
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

const SHARED_VMO_SIZE: usize = PAGE_SIZE * 64; // 1 MiB for compressed data

/// AVI RIFF magic: first 4 bytes of any AVI file.
const RIFF_MAGIC: [u8; 4] = *b"RIFF";

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
    codec_ep: Handle,
    codec_session_id: u32,
    shared_vmo: Handle,
    shared_va: usize,
    is_mp4: bool,
    nal_length_size: u8,
}

/// Reformat raw avcC configuration record into the driver's codec data format.
///
/// Input (ISO 14496-15 avcC box body):
///   0: configurationVersion (1)
///   1: AVCProfileIndication
///   2: profile_compatibility
///   3: AVCLevelIndication
///   4: 0xFC | (lengthSizeMinusOne & 0x03)
///   5: 0xE0 | (numSPS & 0x1F)
///   For each SPS: u16 spsLength, [spsLength bytes]
///   u8 numPPS
///   For each PPS: u16 ppsLength, [ppsLength bytes]
///
/// Output (driver codec data protocol):
///   0: nal_length_size (u8)
///   1: num_parameter_sets (u8)
///   2: reserved (u16, zeroed)
///   Per parameter set: size (u32 LE) + data
///
/// Returns the number of bytes written to `out`, or 0 on malformed input.
fn reformat_avcc(avcc: &[u8], out: &mut [u8]) -> usize {
    if avcc.len() < 7 {
        return 0;
    }

    let nal_length_size = (avcc[4] & 0x03) + 1;
    let num_sps = (avcc[5] & 0x1F) as usize;
    let mut read_pos = 6;
    // Collect all parameter set (offset, length) pairs first to count them and
    // validate before writing.
    let mut params: Vec<(usize, usize)> = Vec::new();

    // SPS entries
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

    // PPS count
    if read_pos >= avcc.len() {
        return 0;
    }

    let num_pps = avcc[read_pos] as usize;

    read_pos += 1;

    // PPS entries
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
    // Header: 1 + 1 + 2 = 4 bytes, then per param: 4 + data_len
    let needed = 4 + params.iter().map(|(_, len)| 4 + len).sum::<usize>();

    if needed > out.len() {
        return 0;
    }

    // Write header
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
        // Detect container format by magic bytes.
        let is_avi = file_data.len() >= 4 && file_data[0..4] == RIFF_MAGIC;

        if is_avi {
            self.open_avi(msg, file_va, file_data, file_vmo);
        } else {
            self.open_mp4(msg, file_va, file_data, file_vmo);
        }
    }

    /// Open an AVI/MJPEG file. Existing path, unchanged.
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
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        self.finish_open(
            msg,
            file_va,
            file_data.len(),
            file_vmo,
            info.width,
            info.height,
            info.ns_per_frame(),
            frame_index,
            decode_buf_size,
            false,
            0,
            b"  video-decoder: opened AVI\n",
        );
    }

    /// Open an MP4/H.264 file.
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

        if mp4_info.total_samples == 0 {
            let _ = abi::vmo::unmap(file_va);
            let _ = abi::handle::close(file_vmo);
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let frame_index: Vec<avi::FrameRef> = mp4_info
            .samples()
            .map(|s| avi::FrameRef {
                offset: s.offset as u32,
                size: s.size,
            })
            .collect();
        let (nal_length_size, _avcc_body) = match mp4_info.avc_config() {
            Some(cfg) => cfg,
            None => {
                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };
        let ns_per_frame = mp4_info.ns_per_frame();

        // H.264 uses hardware decode only; software decode buffer not needed.
        // Set a minimal decode_buf_size (0 means the finish_open path will skip
        // software buffer allocation for MP4).
        self.finish_open(
            msg,
            file_va,
            file_data.len(),
            file_vmo,
            mp4_info.width,
            mp4_info.height,
            ns_per_frame,
            frame_index,
            0, // no software decode buffer for H.264
            true,
            nal_length_size,
            b"  video-decoder: opened MP4\n",
        );
    }

    /// Common open completion: allocate output VMO, set up state, reply.
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
        decode_buf_size: usize,
        is_mp4: bool,
        nal_length_size: u8,
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
        // Allocate software decode buffer (MJPEG only — MP4 uses hardware path).
        let (decode_buf_vmo, decode_buf_va, final_decode_buf_size) = if decode_buf_size > 0 {
            let aligned = decode_buf_size.next_multiple_of(PAGE_SIZE);
            let vmo = match abi::vmo::create(aligned, 0) {
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
            let va = match abi::vmo::map(vmo, 0, rw) {
                Ok(va) => va,
                Err(_) => {
                    let _ = abi::vmo::unmap(file_va);
                    let _ = abi::handle::close(file_vmo);
                    let _ = abi::vmo::unmap(output_va);
                    let _ = abi::handle::close(output_vmo);
                    let _ = abi::handle::close(vmo);
                    let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                    return;
                }
            };
            (vmo, va, aligned)
        } else {
            (Handle(0), 0, 0)
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

                if decode_buf_va != 0 {
                    let _ = abi::vmo::unmap(decode_buf_va);
                }
                if decode_buf_vmo.0 != 0 {
                    let _ = abi::handle::close(decode_buf_vmo);
                }

                let _ = msg.reply_error(ipc::STATUS_INVALID);

                return;
            }
        };

        self.file_va = file_va;
        self.file_size = file_size;
        self.frame_index = frame_index;
        self.output_vmo = output_vmo;
        self.output_va = output_va;
        self.output_buf_size = output_buf_size;
        self.decode_buf_vmo = decode_buf_vmo;
        self.decode_buf_va = decode_buf_va;
        self.decode_buf_size = final_decode_buf_size;
        self.width = width;
        self.height = height;
        self.output_gen = 0;
        self.playing = false;
        self.current_frame = 0;
        self.ns_per_frame = ns_per_frame;
        self.next_frame_ns = 0;
        self.is_mp4 = is_mp4;
        self.nal_length_size = nal_length_size;

        if is_mp4 {
            self.setup_hardware_session();

            if self.codec_session_id == 0 {
                self.close_current();

                let _ = abi::vmo::unmap(file_va);
                let _ = abi::handle::close(file_vmo);
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);

                return;
            }
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

        // Create shared VMO for compressed data transfer
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

            // Send shared VMO to driver via SETUP
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

        // For H.264: write reformatted avcC data into the shared VMO so the
        // driver can read it during session creation.
        let codec_data_size = if self.is_mp4 && self.shared_va != 0 {
            self.write_codec_data()
        } else {
            0
        };
        // Create decode session, sending output VMO to the driver
        let output_dup = match abi::handle::dup(
            self.output_vmo,
            Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0 | Rights::DUP.0),
        ) {
            Ok(h) => h,
            Err(_) => return,
        };
        let req = video::CreateSessionRequest {
            codec: if self.is_mp4 {
                video::CODEC_H264
            } else {
                video::CODEC_MJPEG
            },
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
    }

    /// Write reformatted avcC codec data into the shared VMO for the driver.
    /// Returns the number of bytes written, or 0 on failure.
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
        // Use a stack buffer for reformatting. avcC data is small (typically
        // < 256 bytes: a few SPS/PPS NAL units).
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
        let frame_data = match avi::frame_data(file_data, frame_ref) {
            Some(d) => d,
            None => return,
        };

        if self.codec_session_id != 0 {
            self.decode_hardware(frame_data, idx);
        } else if !self.is_mp4 {
            // Software fallback is MJPEG only — H.264 requires hardware decode.
            self.decode_software(frame_data);
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

    fn decode_hardware(&mut self, frame_data: &[u8], frame_idx: usize) {
        if self.shared_va == 0 || frame_data.len() > SHARED_VMO_SIZE {
            if !self.is_mp4 {
                self.decode_software(frame_data);
            }
            return;
        }

        // Copy compressed data to shared VMO
        // SAFETY: shared_va is a valid RW mapping of SHARED_VMO_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame_data.as_ptr(),
                self.shared_va as *mut u8,
                frame_data.len(),
            );
        }

        let ts_ns = frame_idx as u64 * self.ns_per_frame;
        let req = video::DecodeFrameRequest {
            session_id: self.codec_session_id,
            offset: 0,
            size: frame_data.len() as u32,
            timestamp_ns: ts_ns,
        };
        let mut req_buf = [0u8; video::DecodeFrameRequest::SIZE];

        req.write_to(&mut req_buf);

        match ipc::client::call_simple(self.codec_ep, video::DECODE_FRAME, &req_buf) {
            Ok((0, _payload)) => {}
            _ => {
                // Fallback to software decode on hardware failure (MJPEG only)
                if !self.is_mp4 {
                    self.decode_software(frame_data);
                }
            }
        }
    }

    fn decode_software(&mut self, jpeg_data: &[u8]) {
        if self.decode_buf_va == 0 {
            return;
        }

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

        // SAFETY: output_va is a valid RW mapping.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.decode_buf_va as *const u8,
                (self.output_va + video_decoder::GEN_HEADER_SIZE) as *mut u8,
                pixel_size.min(max_pixels),
            );
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
        self.is_mp4 = false;
        self.nal_length_size = 0;
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
        codec_ep: Handle(0),
        codec_session_id: 0,
        shared_vmo: Handle(0),
        shared_va: 0,
        is_mp4: false,
        nal_length_size: 0,
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
