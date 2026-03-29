//! View protocol — C → compositor (cursor state) and A ↔ C notifications.
//!
//! Merges the cursor shared state protocol and the document-model ↔
//! view-engine notification messages into a single view boundary module.

// ── Cursor state (shared memory: C writes, compositor reads) ────────

/// Byte offset where path command data begins in the cursor state page.
pub const CURSOR_DATA_OFFSET: usize = 40;

/// Shared cursor state — lives in an init-allocated page.
///
/// Core writes path commands + metadata when the cursor shape changes,
/// then bumps `shape_generation` with a store-release. Render service
/// load-acquires `shape_generation` each frame; if changed, it re-reads
/// all metadata fields and rasterizes the new cursor image via the
/// normal GPU path pipeline.
///
/// `opacity` is updated independently via atomic store/load — fade
/// animation changes opacity without re-rasterization.
///
/// Path command data (same binary format as Content::Path contours)
/// follows this header at byte offset `CURSOR_DATA_OFFSET`.
#[repr(C)]
pub struct CursorState {
    /// Bumped after core writes new path data + metadata.
    /// Accessed via AtomicU32 (store-release by core, load-acquire by render).
    pub shape_generation: u32,
    /// 0 = hidden, 255 = fully visible.
    /// Accessed via AtomicU32 independently of shape_generation.
    pub opacity: u32,
    /// Icon viewbox size (e.g. 24.0 for Tabler icons).
    pub viewbox: f32,
    /// Stroke width in viewbox units (e.g. 2.0 for Tabler default).
    pub stroke_width: f32,
    /// Hotspot x in viewbox units (arrow tip / I-beam center).
    pub hotspot_x: f32,
    /// Hotspot y in viewbox units.
    pub hotspot_y: f32,
    /// Fill color (RGBA packed: `(r << 24) | (g << 16) | (b << 8) | a`).
    pub fill_color: u32,
    /// Stroke color (RGBA packed).
    pub stroke_color: u32,
    /// Number of bytes of path command data at `CURSOR_DATA_OFFSET`.
    pub data_len: u32,
    /// Bit 0 (`FLAG_STROKE_ONLY`): render body as narrow stroke, not fill.
    pub flags: u32,
}

impl CursorState {
    /// Stroke-only mode: render body as a narrower inner stroke instead of
    /// filling the path interior.
    pub const FLAG_STROKE_ONLY: u32 = 1;

    /// Pack RGBA into a u32 for `fill_color` / `stroke_color`.
    pub const fn pack_color(r: u8, g: u8, b: u8, a: u8) -> u32 {
        ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32)
    }

    pub const fn unpack_color(packed: u32) -> (u8, u8, u8, u8) {
        (
            (packed >> 24) as u8,
            (packed >> 16) as u8,
            (packed >> 8) as u8,
            packed as u8,
        )
    }
}

const _: () = assert!(core::mem::size_of::<CursorState>() == CURSOR_DATA_OFFSET);

// ── Document-model ↔ view-engine notifications ──────────────────────

/// Initial document loaded during boot (A → C).
pub const MSG_DOC_LOADED: u32 = 111;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocLoaded {
    /// Number of content bytes in the document buffer.
    pub doc_len: u32,
    /// Cursor position after load.
    pub cursor_pos: u32,
    /// FileId of the loaded document.
    pub doc_file_id: u64,
    /// Document format: 0 = Plain, 1 = Rich.
    pub format: u8,
    pub _pad: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<DocLoaded>() <= 60);

/// Document buffer changed (A → C, after edit or undo/redo).
pub const MSG_DOC_CHANGED: u32 = 112;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocChanged {
    /// New content length.
    pub doc_len: u32,
    /// New cursor position.
    pub cursor_pos: u32,
    /// Flags: bit 0 = clear_selection (after undo/redo).
    pub flags: u8,
    pub _pad: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<DocChanged>() <= 60);

/// Flag: core should clear selection (used after undo/redo).
pub const DOC_CHANGED_CLEAR_SELECTION: u8 = 1;

/// Undo request (C → A).
pub const MSG_UNDO_REQUEST: u32 = 113;

/// Redo request (C → A).
pub const MSG_REDO_REQUEST: u32 = 114;

/// Image decoded and registered in Content Region (A → C).
pub const MSG_IMAGE_DECODED: u32 = 115;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageDecoded {
    /// Content ID in the Content Region registry.
    pub content_id: u32,
    /// Image width in pixels.
    pub width: u16,
    /// Image height in pixels.
    pub height: u16,
}
const _: () = assert!(core::mem::size_of::<ImageDecoded>() <= 60);

// ── Decode ──────────────────────────────────────────────────────────

/// A ↔ C notification messages.
#[derive(Clone, Copy, Debug)]
pub enum Message {
    DocLoaded(DocLoaded),
    DocChanged(DocChanged),
    UndoRequest,
    RedoRequest,
    ImageDecoded(ImageDecoded),
}

pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
    match msg_type {
        MSG_DOC_LOADED => Some(Message::DocLoaded(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_CHANGED => Some(Message::DocChanged(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_UNDO_REQUEST => Some(Message::UndoRequest),
        MSG_REDO_REQUEST => Some(Message::RedoRequest),
        MSG_IMAGE_DECODED => Some(Message::ImageDecoded(unsafe {
            crate::decode_payload(payload)
        })),
        _ => None,
    }
}
