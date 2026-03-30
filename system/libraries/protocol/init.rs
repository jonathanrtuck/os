//! Init → service configuration protocol.
//!
//! All config messages sent by init during boot to configure services.
//! Consolidates gpu, core_config, compose, editor, and document_model
//! config into a single module organized by target service.

// ── GPU / render driver config ──────────────────────────────────────

pub const MSG_GPU_CONFIG: u32 = 2;
pub const MSG_DISPLAY_INFO: u32 = 5;
pub const MSG_GPU_READY: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuConfig {
    pub mmio_pa: u64,
    pub irq: u32,
    pub _pad: u32,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_size: u32,
    /// Number of chunks per buffer (total entries = chunks_per_buf * 2).
    pub chunks_per_buf: u16,
    /// Each chunk is 2^chunk_order pages.
    pub chunk_order: u8,
    pub _pad2: u8,
}
const _: () = assert!(core::mem::size_of::<GpuConfig>() <= 60);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayInfoMsg {
    pub width: u32,
    pub height: u32,
    /// Display refresh rate in Hz. 0 = unknown (default to 60).
    pub refresh_rate: u32,
}
const _: () = assert!(core::mem::size_of::<DisplayInfoMsg>() <= 60);

// ── View-engine (core) config ───────────────────────────────────────

pub const MSG_CORE_CONFIG: u32 = 50;
/// Signal-only message (no payload). Core sends this after publishing a
/// new scene graph frame to notify the render service to read it.
pub const MSG_SCENE_UPDATED: u32 = 51;
/// Display refresh rate, sent as a separate message after CoreConfig.
pub const MSG_FRAME_RATE: u32 = 52;

/// View-engine process configuration.
///
/// `fb_width` / `fb_height` are dimensions in points (physical / scale).
/// The core lays out in point coordinates; the render service scales to pixels.
/// `content_va` is the Content Region base (read-write). Core reads font
/// data via the registry and writes decoded image pixels.
/// `content_size` is the total Content Region size in bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreConfig {
    pub doc_va: u64,
    pub scene_va: u64,
    pub content_va: u64,
    /// VA of the shared PointerState register (input driver → core).
    /// 0 if no input device is present.
    pub input_state_va: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub doc_capacity: u32,
    pub content_size: u32,
    /// VA of the shared CursorState page (core writes, render reads).
    pub cursor_state_va: u64,
}
const _: () = assert!(core::mem::size_of::<CoreConfig>() <= 60);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRateMsg {
    /// Display refresh rate in Hz. 0 = use default (60 Hz).
    pub frame_rate: u32,
}
const _: () = assert!(core::mem::size_of::<FrameRateMsg>() <= 60);

// ── Compositor / render service config ──────────────────────────────

pub const MSG_COMPOSITOR_CONFIG: u32 = 3;
pub const MSG_IMAGE_CONFIG: u32 = 6;
pub const MSG_RTC_CONFIG: u32 = 15;

/// Render service configuration.
///
/// `fb_width` / `fb_height` are physical framebuffer dimensions in pixels.
/// `scale_factor` is the fractional display scale (1.0, 1.25, 1.5, 2.0).
/// `font_size` is the font size in points (e.g. 18).
/// `screen_dpi` is the display DPI (e.g. 96).
/// `frame_rate` is the target frames per second (e.g. 60).
/// `content_va` is the base VA of the Content Region shared memory.
/// `content_size` is the total Content Region size in bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompositorConfig {
    pub scene_va: u64,
    pub content_va: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub content_size: u32,
    pub scale_factor: f32,
    pub frame_rate: u16,
    pub font_size: u16,
    pub screen_dpi: u16,
    pub _pad: u16,
    /// Pointer state register VA (atomic u64, read-only).
    pub pointer_state_va: u64,
    /// Cursor state page VA (read-only).
    pub cursor_state_va: u64,
}
const _: () = assert!(core::mem::size_of::<CompositorConfig>() <= 60);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageConfig {
    /// Byte offset of the encoded image within the File Store.
    pub file_store_offset: u32,
    /// Byte length of the encoded image in the File Store.
    pub file_store_length: u32,
}
const _: () = assert!(core::mem::size_of::<ImageConfig>() <= 60);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtcConfig {
    pub mmio_pa: u64,
}
const _: () = assert!(core::mem::size_of::<RtcConfig>() <= 60);

// ── Editor config ───────────────────────────────────────────────────

pub const MSG_EDITOR_CONFIG: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorConfig {
    pub doc_va: u64,
    pub doc_capacity: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<EditorConfig>() <= 60);

// ── Document-model config ───────────────────────────────────────────

/// Config message sent by init to the document process.
pub const MSG_DOC_CONFIG: u32 = 110;

/// Document process configuration.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocConfig {
    /// VA of the shared document buffer (read-write for A).
    pub doc_va: u64,
    /// Document buffer capacity (content area, excluding 64-byte header).
    pub doc_capacity: u32,
    /// VA of the Content Region (read-write for image decode allocation).
    pub content_va: u64,
    /// Content Region size in bytes.
    pub content_size: u32,
    /// Byte offset of the encoded image within the File Store.
    pub img_file_store_offset: u32,
    /// Byte length of the encoded image in the File Store.
    pub img_file_store_length: u32,
    /// Kernel channel handle for the editor channel.
    pub editor_handle: u8,
    /// Kernel channel handle for the decoder channel.
    pub decoder_handle: u8,
    /// Kernel channel handle for the document service (filesystem) channel.
    pub fs_handle: u8,
    /// Kernel channel handle for the core (presenter) channel.
    pub core_handle: u8,
}
const _: () = assert!(core::mem::size_of::<DocConfig>() <= 60);

// ── Legacy 9P filesystem (init ↔ virtio-9p driver) ──────────────────

/// FS read request. Sent by init to the 9p driver.
pub const MSG_FS_READ_REQUEST: u32 = 40;
/// FS read response. Sent by the 9p driver back to init.
pub const MSG_FS_READ_RESPONSE: u32 = 41;

// ── Decode helpers ──────────────────────────────────────────────────

/// GPU protocol messages (init ↔ render driver).
#[derive(Clone, Copy, Debug)]
pub enum GpuMessage {
    GpuConfig(GpuConfig),
    DisplayInfo(DisplayInfoMsg),
    GpuReady,
}

pub fn decode_gpu(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<GpuMessage> {
    match msg_type {
        MSG_GPU_CONFIG => Some(GpuMessage::GpuConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DISPLAY_INFO => Some(GpuMessage::DisplayInfo(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_GPU_READY => Some(GpuMessage::GpuReady),
        _ => None,
    }
}

/// View-engine config messages (init → presenter).
#[derive(Clone, Copy, Debug)]
pub enum CoreMessage {
    CoreConfig(CoreConfig),
    FrameRate(FrameRateMsg),
    SceneUpdated,
}

pub fn decode_core(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<CoreMessage> {
    match msg_type {
        MSG_CORE_CONFIG => Some(CoreMessage::CoreConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_FRAME_RATE => Some(CoreMessage::FrameRate(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_SCENE_UPDATED => Some(CoreMessage::SceneUpdated),
        _ => None,
    }
}

/// Compositor config messages (init → render service).
#[derive(Clone, Copy, Debug)]
pub enum ComposeMessage {
    CompositorConfig(CompositorConfig),
    ImageConfig(ImageConfig),
    RtcConfig(RtcConfig),
}

pub fn decode_compose(
    msg_type: u32,
    payload: &[u8; crate::PAYLOAD_SIZE],
) -> Option<ComposeMessage> {
    match msg_type {
        MSG_COMPOSITOR_CONFIG => Some(ComposeMessage::CompositorConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_IMAGE_CONFIG => Some(ComposeMessage::ImageConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_RTC_CONFIG => Some(ComposeMessage::RtcConfig(unsafe {
            crate::decode_payload(payload)
        })),
        _ => None,
    }
}

/// Editor config messages (init → editor).
#[derive(Clone, Copy, Debug)]
pub enum EditorMessage {
    EditorConfig(EditorConfig),
}

pub fn decode_editor(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<EditorMessage> {
    match msg_type {
        MSG_EDITOR_CONFIG => Some(EditorMessage::EditorConfig(unsafe {
            crate::decode_payload(payload)
        })),
        _ => None,
    }
}

/// Document config messages (init → document).
#[derive(Clone, Copy, Debug)]
pub enum DocMessage {
    DocConfig(DocConfig),
}

pub fn decode_doc(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<DocMessage> {
    match msg_type {
        MSG_DOC_CONFIG => Some(DocMessage::DocConfig(unsafe {
            crate::decode_payload(payload)
        })),
        _ => None,
    }
}
