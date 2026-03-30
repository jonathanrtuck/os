//! Layout protocol — shared memory format and IPC messages.
//!
//! Defines the shared memory layouts for:
//! - Layout results (layout writes, presenter reads): line info, visible runs, glyph data
//! - Viewport state register (presenter writes, layout reads): scroll, viewport, page geometry
//!
//! IPC signals (no payload, signal-only):
//! - `MSG_LAYOUT_RECOMPUTE` (C → B): viewport or document changed
//! - `MSG_LAYOUT_READY` (B → C): layout results written

// ── Config (init → B) ──────────────────────────────────────────────

/// Config message type sent by init to the layout engine.
pub const MSG_LAYOUT_ENGINE_CONFIG: u32 = 120;

/// Layout engine process configuration.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutEngineConfig {
    /// VA of the shared document buffer (read-only for B).
    pub doc_va: u64,
    /// VA of the Content Region (read-only for B — fonts).
    pub content_va: u64,
    /// VA of the layout results shared memory (read-write for B).
    pub layout_results_va: u64,
    /// VA of the viewport state register (read-only for B).
    pub viewport_state_va: u64,
    /// Document buffer capacity (content area, excluding 64-byte header).
    pub doc_capacity: u32,
    /// Content Region size in bytes.
    pub content_size: u32,
    /// Layout results region capacity in bytes.
    pub layout_results_capacity: u32,
    /// Kernel channel handle for the core (presenter) channel.
    pub core_handle: u8,
    pub _pad: [u8; 3],
}

const _: () = assert!(core::mem::size_of::<LayoutEngineConfig>() <= 60);

// ── Core layout config (init → C, separate message after CoreConfig) ─

/// Additional layout-related config sent to core after CoreConfig.
pub const MSG_CORE_LAYOUT_CONFIG: u32 = 53;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreLayoutConfig {
    /// VA of the layout results shared memory (read-only for C).
    pub layout_results_va: u64,
    /// Layout results region capacity in bytes.
    pub layout_results_capacity: u32,
    /// VA of the viewport state register (read-write for C).
    pub viewport_state_va: u64,
    /// Kernel channel handle for the input driver channel.
    pub input_handle: u8,
    /// Kernel channel handle for the compositor (render service) channel.
    pub compositor_handle: u8,
    /// Kernel channel handle for the editor channel.
    pub editor_handle: u8,
    /// Kernel channel handle for the document channel.
    pub docmodel_handle: u8,
    /// Kernel channel handle for the layout engine channel.
    pub layout_handle: u8,
    /// Kernel channel handle for the second input device (tablet).
    /// 0xFF if no second input device is present.
    pub input2_handle: u8,
    pub _pad: [u8; 2],
}

const _: () = assert!(core::mem::size_of::<CoreLayoutConfig>() <= 60);

// ── IPC signals ─────────────────────────────────────────────────────

/// Signal: C → B, recompute layout (viewport or document changed).
pub const MSG_LAYOUT_RECOMPUTE: u32 = 130;

/// Signal: B → C, layout results written to shared memory.
pub const MSG_LAYOUT_READY: u32 = 131;

// ── Layout results shared memory format ─────────────────────────────

/// Layout results region size: 256 KiB (16 pages).
pub const LAYOUT_RESULTS_SIZE: usize = 256 * 1024;

/// Number of pages for the layout results region.
pub const LAYOUT_RESULTS_PAGES: u64 = (LAYOUT_RESULTS_SIZE as u64) / 16384;

/// Layout results header at offset 0 of the shared memory region.
/// B writes with store-release on `generation`. C reads with load-acquire.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LayoutResultsHeader {
    /// Generation counter (atomic). Incremented after each write.
    pub generation: u32,
    /// Total number of logical lines (all lines, not just visible).
    pub total_line_count: u32,
    /// Number of visible runs (shaped, ready for scene building).
    pub visible_run_count: u32,
    /// Total content height in points.
    pub content_height_pt: i32,
    /// Characters per line (monospace) or 0 for rich text.
    pub chars_per_line: u32,
    /// Text-area width within the page surface (points).
    pub doc_width_pt: u32,
    /// Line height in points (monospace uniform height).
    pub line_height_pt: u32,
    /// Bytes of glyph data used (ShapedGlyph arrays).
    pub glyph_data_used: u32,
    /// Document format: 0 = Plain, 1 = Rich.
    pub doc_format: u32,
    /// Style registry size in bytes (written after glyph data).
    pub style_registry_size: u32,
    /// Number of style entries in the registry.
    pub style_count: u32,
    /// Reserved for future use.
    pub _reserved: [u32; 5],
}

/// Size of the header in bytes.
pub const LAYOUT_HEADER_SIZE: usize = core::mem::size_of::<LayoutResultsHeader>();
const _: () = assert!(LAYOUT_HEADER_SIZE == 64);

/// Line information for cursor/selection positioning.
/// Array follows immediately after the header.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LineInfo {
    /// Byte offset into the document buffer.
    pub byte_offset: u32,
    /// Byte length of this line's content.
    pub byte_length: u32,
    /// Y position in document coordinates (points).
    pub y_pt: i32,
    /// Line height in points.
    pub line_height_pt: u32,
}

const _: () = assert!(core::mem::size_of::<LineInfo>() == 16);

/// A visible text run with pre-shaped glyph data.
/// Array follows after the LineInfo array.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VisibleRun {
    /// Offset into the glyph data section (bytes from glyph data start).
    pub glyph_data_offset: u32,
    /// Number of ShapedGlyph entries.
    pub glyph_count: u16,
    /// Font size in points.
    pub font_size: u16,
    /// Y position in document coordinates (millipoints, 1/1024 pt).
    /// Includes sub-point baseline alignment offset for mixed-size lines.
    pub y_mpt: i32,
    /// Style ID for the renderer's style registry.
    pub style_id: u32,
    /// RGBA color packed as `(r << 24) | (g << 16) | (b << 8) | a`.
    pub color_rgba: u32,
    /// Byte offset into the document text where this run starts.
    pub byte_offset: u32,
    /// Byte length of this run's text content.
    pub byte_length: u16,
    /// Style flags (underline, strikethrough, italic) from the piece table.
    pub flags: u8,
    pub _pad: u8,
    /// Pen X position at the start of this run (millipoints).
    pub x_mpt: i32,
}

const _: () = assert!(core::mem::size_of::<VisibleRun>() == 32);

/// Compute the byte offset of the LineInfo array.
#[inline]
pub const fn line_info_offset() -> usize {
    LAYOUT_HEADER_SIZE
}

/// Compute the byte offset of the VisibleRun array given line count.
#[inline]
pub const fn visible_run_offset(line_count: u32) -> usize {
    line_info_offset() + (line_count as usize) * core::mem::size_of::<LineInfo>()
}

/// Compute the byte offset of the glyph data section.
#[inline]
pub const fn glyph_data_offset(line_count: u32, run_count: u32) -> usize {
    visible_run_offset(line_count) + (run_count as usize) * core::mem::size_of::<VisibleRun>()
}

/// Compute the byte offset of the style registry (after glyph data).
#[inline]
pub const fn style_registry_offset(line_count: u32, run_count: u32, glyph_bytes: u32) -> usize {
    glyph_data_offset(line_count, run_count) + glyph_bytes as usize
}

// ── Viewport state register ─────────────────────────────────────────

/// Viewport state written by C, read by B.
/// Single page (16 KiB), only first 64 bytes used.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ViewportState {
    /// Generation counter (atomic). C increments after each write.
    pub generation: u32,
    /// Vertical scroll offset in millipoints.
    pub scroll_y_mpt: i32,
    /// Viewport width in points.
    pub viewport_width_pt: u32,
    /// Viewport height in points (content area, below title bar).
    pub viewport_height_pt: u32,
    /// Page width in points (A4 proportions).
    pub page_width_pt: u32,
    /// Page height in points.
    pub page_height_pt: u32,
    /// Text inset from page edge (points).
    pub text_inset_x: u32,
    /// Font size in points.
    pub font_size: u16,
    pub _pad0: u16,
    /// Character advance in 16.16 fixed-point points (monospace).
    pub char_width_fx: i32,
    /// Line height in points.
    pub line_height: u32,
    /// Document format: 0 = Plain, 1 = Rich.
    pub doc_format: u32,
    /// Current document content length in bytes.
    pub doc_len: u32,
    /// Reserved for future use.
    pub _reserved: [u32; 4],
}

const _: () = assert!(core::mem::size_of::<ViewportState>() == 64);

// ── Message decode ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum Message {
    LayoutEngineConfig(LayoutEngineConfig),
    CoreLayoutConfig(CoreLayoutConfig),
    LayoutRecompute,
    LayoutReady,
}

pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
    match msg_type {
        MSG_LAYOUT_ENGINE_CONFIG => Some(Message::LayoutEngineConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_CORE_LAYOUT_CONFIG => Some(Message::CoreLayoutConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_LAYOUT_RECOMPUTE => Some(Message::LayoutRecompute),
        MSG_LAYOUT_READY => Some(Message::LayoutReady),
        _ => None,
    }
}
