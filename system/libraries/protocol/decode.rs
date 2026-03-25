//! Decode protocol — generic interface for content decoder services.
//!
//! Format-agnostic: the same request/response types work for PNG, JPEG,
//! WebP, and any future decoder. Core dispatches to the right decoder
//! service based on mimetype; the protocol carries no format information.
//!
//! Data flow: core pre-allocates Content Region space, sends a request
//! with the output offset, the decoder writes pixels there and responds
//! with dimensions and actual byte count.

/// Decode request (core → decoder service).
pub const MSG_DECODE_REQUEST: u32 = 60;

/// Decode response (decoder service → core).
pub const MSG_DECODE_RESPONSE: u32 = 61;

/// Decoder service configuration (init → decoder service).
pub const MSG_DECODER_CONFIG: u32 = 62;

// ── Decoder config ─────────────────────────────────────────────────

/// Configuration sent by init to a decoder service at startup.
/// Provides the shared memory region addresses for File Store and
/// Content Region access.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecoderConfig {
    /// File Store base VA (read-only mapping). Raw encoded bytes.
    pub file_store_va: u64,
    /// File Store total size in bytes.
    pub file_store_size: u32,
    /// Content Region base VA (read-write mapping). Decoded output.
    pub content_va: u64,
    /// Content Region total size in bytes.
    pub content_size: u32,
}

const _: () = assert!(core::mem::size_of::<DecoderConfig>() <= 60);

// ── Decode request ─────────────────────────────────────────────────

/// A decode request from core to a decoder service.
///
/// Core has already allocated space in the Content Region at
/// `content_offset`. The decoder reads raw bytes from the File Store
/// and writes decoded BGRA pixels into the Content Region.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecodeRequest {
    /// Byte offset into the File Store where the encoded file starts.
    pub file_offset: u32,
    /// Byte length of the encoded file in the File Store.
    pub file_length: u32,
    /// Byte offset into the Content Region where decoded output should
    /// be written. Pre-allocated by core via `ContentAllocator`.
    pub content_offset: u32,
    /// Maximum bytes the decoder may write at `content_offset`.
    pub max_output: u32,
    /// Request ID for matching responses (core-assigned, opaque to decoder).
    pub request_id: u32,
    /// Flags. Bit 0: header-only (report dimensions, don't decode).
    pub flags: u32,
}

const _: () = assert!(core::mem::size_of::<DecodeRequest>() <= 60);

/// Flag: report dimensions only, do not decode pixel data.
pub const DECODE_FLAG_HEADER_ONLY: u32 = 1;

// ── Decode response ────────────────────────────────────────────────

/// Decode status codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DecodeStatus {
    /// Decode succeeded. Pixels written to Content Region.
    Ok = 0,
    /// Header-only query succeeded. Dimensions valid, no pixels written.
    HeaderOk = 1,
    /// Encoded data is corrupt or unsupported.
    InvalidData = 2,
    /// Output buffer too small for the decoded image.
    BufferTooSmall = 3,
    /// File Store offset/length out of bounds.
    OutOfBounds = 4,
}

/// A decode response from a decoder service to core.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecodeResponse {
    /// Echoed from request — lets core match responses to requests.
    pub request_id: u32,
    /// Result status.
    pub status: u8,
    pub _pad: [u8; 3],
    /// Decoded image width in pixels (valid for Ok and HeaderOk).
    pub width: u32,
    /// Decoded image height in pixels (valid for Ok and HeaderOk).
    pub height: u32,
    /// Actual bytes written to the Content Region (0 for header-only).
    pub bytes_written: u32,
}

const _: () = assert!(core::mem::size_of::<DecodeResponse>() <= 60);

// ── Typed decode ───────────────────────────────────────────────────

/// Typed message for the decode protocol boundary.
#[derive(Clone, Copy, Debug)]
pub enum Message {
    Config(DecoderConfig),
    Request(DecodeRequest),
    Response(DecodeResponse),
}

/// Decode a decode-protocol message. Returns `None` for unknown msg_type.
pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
    match msg_type {
        MSG_DECODER_CONFIG => Some(Message::Config(unsafe { crate::decode_payload(payload) })),
        MSG_DECODE_REQUEST => Some(Message::Request(unsafe { crate::decode_payload(payload) })),
        MSG_DECODE_RESPONSE => {
            Some(Message::Response(unsafe { crate::decode_payload(payload) }))
        }
        _ => None,
    }
}
