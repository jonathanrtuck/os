//! Generic decoder service harness.
//!
//! Handles all IPC plumbing, config reading, bounds checking, and response
//! building. Format-specific decoders supply two functions:
//!
//! - `header(data) → Option<(width, height)>` — parse dimensions without decoding
//! - `decode(data, output) → bool` — decode into caller-provided BGRA buffer
//!
//! Usage in a decoder service's `main.rs`:
//!
//! ```ignore
//! #[path = "../../decoders/harness.rs"]
//! mod harness;
//! mod png;
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn _start() -> ! {
//!     harness::run(png::header, png::decode, b"png-decode");
//! }
//! ```

use protocol::decode::{
    DecodeResponse, DecodeStatus, DECODE_FLAG_HEADER_ONLY, MSG_DECODE_RESPONSE,
};

const INIT_HANDLE: u8 = 0;
const CORE_HANDLE: u8 = 1;

/// Run the decoder service event loop. Never returns.
///
/// `header_fn`: parse encoded data, return `Some((width, height))` or `None`.
/// `decode_fn`: decode encoded data into `output` (BGRA pixels), return success.
/// `name`: service name for diagnostic output (e.g., `b"png-decode"`).
pub fn run(
    header_fn: fn(&[u8]) -> Option<(u32, u32)>,
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
    header_fn: fn(&[u8]) -> Option<(u32, u32)>,
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

    // Parse header for dimensions.
    let (width, height) = match header_fn(data) {
        Some(dims) => dims,
        None => return error_response(req.request_id, DecodeStatus::InvalidData),
    };

    // Header-only query: report dimensions, don't decode.
    if req.flags & DECODE_FLAG_HEADER_ONLY != 0 {
        return DecodeResponse {
            request_id: req.request_id,
            status: DecodeStatus::HeaderOk as u8,
            _pad: [0; 3],
            width,
            height,
            bytes_written: 0,
        };
    }

    // Full decode: check output buffer.
    let pixel_bytes = width as usize * height as usize * 4;
    if pixel_bytes > req.max_output as usize {
        return error_response(req.request_id, DecodeStatus::BufferTooSmall);
    }

    let content_end = req.content_offset as usize + pixel_bytes;
    if content_end > content_size {
        return error_response(req.request_id, DecodeStatus::BufferTooSmall);
    }

    // SAFETY: content_va..+content_size is a valid read-write mapping
    // provided by init. content_offset..content_end is within bounds.
    let output = unsafe {
        core::slice::from_raw_parts_mut(
            (content_va + req.content_offset as usize) as *mut u8,
            pixel_bytes,
        )
    };

    if decode_fn(data, output) {
        DecodeResponse {
            request_id: req.request_id,
            status: DecodeStatus::Ok as u8,
            _pad: [0; 3],
            width,
            height,
            bytes_written: pixel_bytes as u32,
        }
    } else {
        error_response(req.request_id, DecodeStatus::InvalidData)
    }
}

fn error_response(request_id: u32, status: DecodeStatus) -> DecodeResponse {
    DecodeResponse {
        request_id,
        status: status as u8,
        _pad: [0; 3],
        width: 0,
        height: 0,
        bytes_written: 0,
    }
}
