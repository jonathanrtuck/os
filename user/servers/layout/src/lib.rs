//! Layout service protocol — text layout computation.
//!
//! Transport: sync call/reply for SETUP/RECOMPUTE/GET_INFO.
//! Data plane: seqlock registers for viewport state (in) and layout
//! results (out).
//!
//! The layout service is a pure function: (document content + viewport
//! state + font metrics) → positioned text runs. It reads the document
//! buffer (RO shared VMO from the document service) and viewport state
//! (seqlock register from the presenter), computes line breaks and
//! positions, and writes results to a dedicated seqlock-protected VMO.

#![no_std]

pub use ipc::MAX_PAYLOAD;

// ── Methods served by the layout service ─────────────────────────

/// Presenter sends viewport state VMO handle → receives layout
/// results VMO handle (RO) via IPC handle slot 0.
pub const SETUP: u32 = 1;

/// Trigger immediate relayout. Replies when layout is complete.
pub const RECOMPUTE: u32 = 2;

/// Returns current layout statistics.
pub const GET_INFO: u32 = 3;

// ── Viewport state (seqlock register, written by presenter) ──────

/// Viewport parameters controlling layout computation. Stored in a
/// seqlock register (ipc::register) in a shared VMO owned by the
/// presenter. The layout service reads this to determine wrapping
/// width, font metrics, and scroll position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportState {
    /// Vertical scroll offset in points.
    pub scroll_y: i32,
    /// Available width for text wrapping, in points.
    pub viewport_width: u32,
    /// Visible area height, in points.
    pub viewport_height: u32,
    /// Monospace character width, fixed-point 16.16.
    pub char_width_fp: u32,
    /// Line height in points.
    pub line_height: u32,
}

impl ViewportState {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.scroll_y.to_le_bytes());
        buf[4..8].copy_from_slice(&self.viewport_width.to_le_bytes());
        buf[8..12].copy_from_slice(&self.viewport_height.to_le_bytes());
        buf[12..16].copy_from_slice(&self.char_width_fp.to_le_bytes());
        buf[16..20].copy_from_slice(&self.line_height.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            scroll_y: i32::from_le_bytes(buf[0..4].try_into().unwrap()),
            viewport_width: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            viewport_height: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            char_width_fp: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            line_height: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        }
    }

    /// Convert fixed-point char width to f32.
    #[must_use]
    pub fn char_width(&self) -> f32 {
        (self.char_width_fp as f32) / 65536.0
    }

    /// Encode an f32 char width as fixed-point 16.16.
    #[must_use]
    pub fn encode_char_width(w: f32) -> u32 {
        (w * 65536.0) as u32
    }
}

// ── Layout results VMO format ────────────────────────────────────
//
// The results VMO uses the seqlock protocol (ipc::register):
//   [0..8]   seqlock generation (AtomicU64)
//   [8..24]  LayoutHeader (16 bytes)
//   [24..]   LineInfo × line_count (20 bytes each)
//
// The layout service writes directly using odd/even generation
// bumps. Readers use ipc::register::Reader for consistency.

pub const RESULTS_HEADER_OFFSET: usize = 0;
/// Seqlock generation counter size (AtomicU64).
pub const SEQLOCK_HEADER_SIZE: usize = 8;
pub const RESULTS_VALUE_OFFSET: usize = SEQLOCK_HEADER_SIZE;
pub const MAX_LINES: usize = 512;
pub const MAX_VISIBLE_RUNS: usize = 256;
pub const VISIBLE_RUNS_OFFSET: usize = LayoutHeader::SIZE + MAX_LINES * LineInfo::SIZE;
pub const RESULTS_VALUE_SIZE: usize =
    LayoutHeader::SIZE + MAX_LINES * LineInfo::SIZE + MAX_VISIBLE_RUNS * VisibleRun::SIZE;

/// Header at the start of the results value (after seqlock gen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutHeader {
    /// Number of laid-out lines.
    pub line_count: u32,
    /// Total layout height in points.
    pub total_height: i32,
    /// Document content length at time of layout.
    pub content_len: u32,
    /// Document format (0=plain, 1=rich). Mirrors the document buffer format.
    pub format: u8,
    pub _pad: u8,
    /// Number of VisibleRun entries (rich text only, 0 for plain).
    pub visible_run_count: u16,
}

impl LayoutHeader {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.line_count.to_le_bytes());
        buf[4..8].copy_from_slice(&self.total_height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.content_len.to_le_bytes());
        buf[12] = self.format;
        buf[13] = 0;
        buf[14..16].copy_from_slice(&self.visible_run_count.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            line_count: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            total_height: i32::from_le_bytes(buf[4..8].try_into().unwrap()),
            content_len: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            format: buf[12],
            _pad: 0,
            visible_run_count: u16::from_le_bytes(buf[14..16].try_into().unwrap()),
        }
    }
}

/// A single laid-out line in the results VMO.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineInfo {
    /// Start byte offset in the document content.
    pub byte_offset: u32,
    /// Byte count of visible content on this line.
    pub byte_length: u32,
    /// Horizontal offset in points (for alignment).
    pub x: f32,
    /// Vertical position in points from layout top.
    pub y: i32,
    /// Rendered width of this line in points.
    pub width: f32,
}

impl LineInfo {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.byte_offset.to_le_bytes());
        buf[4..8].copy_from_slice(&self.byte_length.to_le_bytes());
        buf[8..12].copy_from_slice(&self.x.to_le_bytes());
        buf[12..16].copy_from_slice(&self.y.to_le_bytes());
        buf[16..20].copy_from_slice(&self.width.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            byte_offset: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            byte_length: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            x: f32::from_le_bytes(buf[8..12].try_into().unwrap()),
            y: i32::from_le_bytes(buf[12..16].try_into().unwrap()),
            width: f32::from_le_bytes(buf[16..20].try_into().unwrap()),
        }
    }
}

/// A positioned styled run in the layout results (rich text only).
///
/// Each run has a position, a byte range in the document's logical text,
/// font/style info, and color. The presenter shapes the glyphs itself
/// using the font data identified by `font_family` and `style_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleRun {
    /// Horizontal position in points.
    pub x: f32,
    /// Vertical position in points (top of line).
    pub y: i32,
    /// Start byte offset in the document's logical text.
    pub byte_offset: u32,
    /// Byte count of this run's text.
    pub byte_length: u16,
    /// Font size in points.
    pub font_size: u16,
    /// Piecetable style index.
    pub style_id: u8,
    /// Font family (0=mono, 1=sans, 2=serif).
    pub font_family: u8,
    /// Decoration flags (bit 0=italic, bit 1=underline, bit 2=strikethrough).
    pub flags: u8,
    pub _pad: u8,
    /// Packed RGBA color: `(r << 24) | (g << 16) | (b << 8) | a`.
    pub color_rgba: u32,
    /// Font weight (100-900).
    pub weight: u16,
    /// Which layout line this run belongs to.
    pub line_index: u16,
}

impl VisibleRun {
    pub const SIZE: usize = 28;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.x.to_le_bytes());
        buf[4..8].copy_from_slice(&self.y.to_le_bytes());
        buf[8..12].copy_from_slice(&self.byte_offset.to_le_bytes());
        buf[12..14].copy_from_slice(&self.byte_length.to_le_bytes());
        buf[14..16].copy_from_slice(&self.font_size.to_le_bytes());
        buf[16] = self.style_id;
        buf[17] = self.font_family;
        buf[18] = self.flags;
        buf[19] = 0;
        buf[20..24].copy_from_slice(&self.color_rgba.to_le_bytes());
        buf[24..26].copy_from_slice(&self.weight.to_le_bytes());
        buf[26..28].copy_from_slice(&self.line_index.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            x: f32::from_le_bytes(buf[0..4].try_into().unwrap()),
            y: i32::from_le_bytes(buf[4..8].try_into().unwrap()),
            byte_offset: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            byte_length: u16::from_le_bytes(buf[12..14].try_into().unwrap()),
            font_size: u16::from_le_bytes(buf[14..16].try_into().unwrap()),
            style_id: buf[16],
            font_family: buf[17],
            flags: buf[18],
            _pad: 0,
            color_rgba: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
            weight: u16::from_le_bytes(buf[24..26].try_into().unwrap()),
            line_index: u16::from_le_bytes(buf[26..28].try_into().unwrap()),
        }
    }
}

// ── SETUP reply ──────────────────────────────────────────────────

/// SETUP reply: layout metadata. The layout results VMO handle is
/// transferred via IPC handle slot 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupReply {
    pub max_lines: u32,
}

impl SetupReply {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.max_lines.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            max_lines: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

// ── GET_INFO reply ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub line_count: u32,
    pub total_height: i32,
    pub content_len: u32,
    pub viewport_width: u32,
    pub line_height: u32,
}

impl InfoReply {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.line_count.to_le_bytes());
        buf[4..8].copy_from_slice(&self.total_height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.content_len.to_le_bytes());
        buf[12..16].copy_from_slice(&self.viewport_width.to_le_bytes());
        buf[16..20].copy_from_slice(&self.line_height.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            line_count: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            total_height: i32::from_le_bytes(buf[4..8].try_into().unwrap()),
            content_len: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            viewport_width: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            line_height: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_state_round_trip() {
        let state = ViewportState {
            scroll_y: -100,
            viewport_width: 800,
            viewport_height: 600,
            char_width_fp: ViewportState::encode_char_width(10.0),
            line_height: 20,
        };
        let mut buf = [0u8; ViewportState::SIZE];

        state.write_to(&mut buf);

        assert_eq!(ViewportState::read_from(&buf), state);
    }

    #[test]
    fn viewport_char_width_encoding() {
        let fp = ViewportState::encode_char_width(10.0);
        let state = ViewportState {
            scroll_y: 0,
            viewport_width: 0,
            viewport_height: 0,
            char_width_fp: fp,
            line_height: 0,
        };

        assert!((state.char_width() - 10.0).abs() < 0.01);
    }

    #[test]
    fn layout_header_round_trip() {
        let header = LayoutHeader {
            line_count: 42,
            total_height: 840,
            content_len: 1024,
            format: 1,
            _pad: 0,
            visible_run_count: 15,
        };
        let mut buf = [0u8; LayoutHeader::SIZE];

        header.write_to(&mut buf);

        let decoded = LayoutHeader::read_from(&buf);

        assert_eq!(decoded.line_count, 42);
        assert_eq!(decoded.total_height, 840);
        assert_eq!(decoded.content_len, 1024);
        assert_eq!(decoded.format, 1);
        assert_eq!(decoded.visible_run_count, 15);
    }

    #[test]
    fn visible_run_round_trip() {
        let run = VisibleRun {
            x: 15.5,
            y: 200,
            byte_offset: 100,
            byte_length: 50,
            font_size: 14,
            style_id: 3,
            font_family: 1,
            flags: 0b011,
            _pad: 0,
            color_rgba: 0xFF_00_00_FF,
            weight: 700,
            line_index: 5,
        };
        let mut buf = [0u8; VisibleRun::SIZE];

        run.write_to(&mut buf);

        let decoded = VisibleRun::read_from(&buf);

        assert!((decoded.x - 15.5).abs() < 0.01);
        assert_eq!(decoded.y, 200);
        assert_eq!(decoded.byte_offset, 100);
        assert_eq!(decoded.byte_length, 50);
        assert_eq!(decoded.font_size, 14);
        assert_eq!(decoded.style_id, 3);
        assert_eq!(decoded.font_family, 1);
        assert_eq!(decoded.flags, 0b011);
        assert_eq!(decoded.color_rgba, 0xFF_00_00_FF);
        assert_eq!(decoded.weight, 700);
        assert_eq!(decoded.line_index, 5);
    }

    #[test]
    fn line_info_round_trip() {
        let info = LineInfo {
            byte_offset: 100,
            byte_length: 50,
            x: 15.5,
            y: 200,
            width: 450.0,
        };
        let mut buf = [0u8; LineInfo::SIZE];

        info.write_to(&mut buf);

        let decoded = LineInfo::read_from(&buf);

        assert_eq!(decoded.byte_offset, 100);
        assert_eq!(decoded.byte_length, 50);
        assert!((decoded.x - 15.5).abs() < 0.01);
        assert_eq!(decoded.y, 200);
        assert!((decoded.width - 450.0).abs() < 0.01);
    }

    #[test]
    fn setup_reply_round_trip() {
        let reply = SetupReply { max_lines: 512 };
        let mut buf = [0u8; SetupReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(SetupReply::read_from(&buf), reply);
    }

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            line_count: 10,
            total_height: 200,
            content_len: 500,
            viewport_width: 800,
            line_height: 20,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(InfoReply::read_from(&buf), reply);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [SETUP, RECOMPUTE, GET_INFO];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(ViewportState::SIZE <= MAX_PAYLOAD);
        assert!(LayoutHeader::SIZE <= MAX_PAYLOAD);
        assert!(LineInfo::SIZE <= MAX_PAYLOAD);
        assert!(VisibleRun::SIZE <= MAX_PAYLOAD);
        assert!(SetupReply::SIZE <= MAX_PAYLOAD);
        assert!(InfoReply::SIZE <= MAX_PAYLOAD);
    }

    #[test]
    fn results_value_size_fits_two_pages() {
        let total = SEQLOCK_HEADER_SIZE + RESULTS_VALUE_SIZE;

        assert!(
            total <= 32768,
            "results VMO ({total}) exceeds two pages (32768)"
        );
    }

    #[test]
    fn max_lines_capacity() {
        assert_eq!(MAX_LINES, 512);
    }

    #[test]
    fn visible_run_size_correct() {
        assert_eq!(VisibleRun::SIZE, 28);
    }

    #[test]
    fn visible_runs_offset_correct() {
        assert_eq!(
            VISIBLE_RUNS_OFFSET,
            LayoutHeader::SIZE + MAX_LINES * LineInfo::SIZE
        );
    }
}
