//! Generic decoder service harness.
//!
//! Handles all IPC plumbing, config reading, bounds checking, and response
//! building. Format-specific decoders supply two functions:
//!
//! - `header(data) → Option<(width, height, bits_per_pixel)>` — parse dimensions
//! - `decode(data, output) → bool` — decode into caller-provided buffer
//!
//! The harness heap-allocates a decode buffer (BGRA + decompression scratch),
//! calls the decoder, then copies only the BGRA pixels into the Content Region.
//! This keeps scratch memory private to the decoder service — the Content Region
//! holds only final pixel data.

extern crate alloc;

use alloc::vec;

use protocol::decode::{
    DecodeResponse, DecodeStatus, DECODE_FLAG_HEADER_ONLY, MSG_DECODE_RESPONSE,
};

const INIT_HANDLE: u8 = 0;
const CORE_HANDLE: u8 = 1;

/// Run the decoder service event loop. Never returns.
///
/// `header_fn`: parse encoded data, return `Some((width, height, bits_per_pixel))` or `None`.
/// `decode_fn`: decode encoded data into `output` (BGRA + scratch), return success.
/// `name`: service name for diagnostic output (e.g., `b"png-decode"`).
pub fn run(
    header_fn: fn(&[u8]) -> Option<(u32, u32, u8)>,
    decode_fn: fn(&[u8], &mut [u8]) -> bool,
    name: &[u8],
) -> ! {
    sys::print(b"  ");
    sys::print(name);
    sys::print(b": starting\n");

    // Read config from init (channel 0). Pre-buffered before process start.
    let init_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) {
        sys::print(b"  ");
        sys::print(name);
        sys::print(b": no config message\n");
        sys::exit();
    }
    let config = if let Some(protocol::decode::Message::Config(c)) =
        protocol::decode::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"  ");
        sys::print(name);
        sys::print(b": bad config payload\n");
        sys::exit();
    };

    let file_store_va = config.file_store_va as usize;
    let file_store_size = config.file_store_size as usize;
    let content_va = config.content_va as usize;
    let content_size = config.content_size as usize;

    sys::print(b"  ");
    sys::print(name);
    sys::print(b": config received, entering decode loop\n");

    // Core channel (channel 1) for decode requests.
    let core_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    loop {
        let _ = sys::wait(&[CORE_HANDLE], u64::MAX);

        while core_ch.try_recv(&mut msg) {
            if let Some(protocol::decode::Message::Request(req)) =
                protocol::decode::decode(msg.msg_type, &msg.payload)
            {
                let response = handle_request(
                    &req,
                    file_store_va,
                    file_store_size,
                    content_va,
                    content_size,
                    header_fn,
                    decode_fn,
                );

                // SAFETY: DecodeResponse is repr(C) and fits in 60-byte payload.
                let resp_msg =
                    unsafe { ipc::Message::from_payload(MSG_DECODE_RESPONSE, &response) };
                core_ch.send(&resp_msg);
                let _ = sys::channel_signal(sys::ChannelHandle(CORE_HANDLE));
            }
        }
    }
}

fn handle_request(
    req: &protocol::decode::DecodeRequest,
    file_store_va: usize,
    file_store_size: usize,
    content_va: usize,
    content_size: usize,
    header_fn: fn(&[u8]) -> Option<(u32, u32, u8)>,
    decode_fn: fn(&[u8], &mut [u8]) -> bool,
) -> DecodeResponse {
    let file_end = req.file_offset as usize + req.file_length as usize;
    if file_end > file_store_size || req.file_length == 0 {
        return error_response(req.request_id, DecodeStatus::OutOfBounds);
    }

    // SAFETY: file_store_va..+file_store_size is a valid read-only mapping
    // provided by init. file_offset..file_end is within bounds (checked above).
    let data = unsafe {
        core::slice::from_raw_parts(
            (file_store_va + req.file_offset as usize) as *const u8,
            req.file_length as usize,
        )
    };

    // Parse header for dimensions and format.
    let (width, height, bpp) = match header_fn(data) {
        Some(info) => info,
        None => return error_response(req.request_id, DecodeStatus::InvalidData),
    };

    // Header-only query: report dimensions and format, don't decode.
    if req.flags & DECODE_FLAG_HEADER_ONLY != 0 {
        return DecodeResponse {
            request_id: req.request_id,
            status: DecodeStatus::HeaderOk as u8,
            bits_per_pixel: bpp,
            _pad: [0; 2],
            width,
            height,
            bytes_written: 0,
        };
    }

    // Full decode.
    let pixel_bytes = width as usize * height as usize * 4;
    if pixel_bytes > req.max_output as usize {
        return error_response(req.request_id, DecodeStatus::BufferTooSmall);
    }
    let content_end = req.content_offset as usize + pixel_bytes;
    if content_end > content_size {
        return error_response(req.request_id, DecodeStatus::BufferTooSmall);
    }

    // Compute decompression scratch size from format info.
    // Raw scanline = ceil(width * bpp / 8) bytes + 1 filter byte per row.
    // Add margin for Adam7 interlace overhead (extra filter bytes per pass).
    let bpp_val = (bpp as usize).max(1);
    let raw_row = (width as usize * bpp_val + 7) / 8;
    let scratch = height as usize * (raw_row + 1) + height as usize * 8;
    let decode_buf_size = pixel_bytes + scratch;

    // Heap-allocate the full decode buffer (BGRA output + scratch).
    // The decoder writes decompressed data into the scratch area and
    // final BGRA pixels into the first pixel_bytes. After decode, we
    // copy only the BGRA pixels to the Content Region.
    let mut decode_buf = vec![0u8; decode_buf_size];

    if !decode_fn(data, &mut decode_buf) {
        return error_response(req.request_id, DecodeStatus::InvalidData);
    }

    // Copy BGRA pixels from heap buffer to Content Region.
    // SAFETY: content_va..+content_size is a valid read-write mapping.
    // content_offset..content_end is within bounds (checked above).
    let output = unsafe {
        core::slice::from_raw_parts_mut(
            (content_va + req.content_offset as usize) as *mut u8,
            pixel_bytes,
        )
    };
    output.copy_from_slice(&decode_buf[..pixel_bytes]);
    // decode_buf is freed here (Vec drop).

    DecodeResponse {
        request_id: req.request_id,
        status: DecodeStatus::Ok as u8,
        bits_per_pixel: bpp,
        _pad: [0; 2],
        width,
        height,
        bytes_written: pixel_bytes as u32,
    }
}

fn error_response(request_id: u32, status: DecodeStatus) -> DecodeResponse {
    DecodeResponse {
        request_id,
        status: status as u8,
        bits_per_pixel: 0,
        _pad: [0; 2],
        width: 0,
        height: 0,
        bytes_written: 0,
    }
}
