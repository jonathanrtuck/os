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

pub const MAX_PAYLOAD: usize = 120;

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
pub const RESULTS_VALUE_SIZE: usize = LayoutHeader::SIZE + MAX_LINES * LineInfo::SIZE;

/// Header at the start of the results value (after seqlock gen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutHeader {
    /// Number of laid-out lines.
    pub line_count: u32,
    /// Total layout height in points.
    pub total_height: i32,
    /// Document content length at time of layout.
    pub content_len: u32,
    pub _reserved: u32,
}

impl LayoutHeader {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.line_count.to_le_bytes());
        buf[4..8].copy_from_slice(&self.total_height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.content_len.to_le_bytes());
        buf[12..16].copy_from_slice(&0u32.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            line_count: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            total_height: i32::from_le_bytes(buf[4..8].try_into().unwrap()),
            content_len: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            _reserved: 0,
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
            _reserved: 0,
        };
        let mut buf = [0u8; LayoutHeader::SIZE];

        header.write_to(&mut buf);

        assert_eq!(LayoutHeader::read_from(&buf), header);
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
        assert!(SetupReply::SIZE <= MAX_PAYLOAD);
        assert!(InfoReply::SIZE <= MAX_PAYLOAD);
    }

    #[test]
    fn results_value_size_fits_page() {
        let total = SEQLOCK_HEADER_SIZE + RESULTS_VALUE_SIZE;

        assert!(
            total <= 16384,
            "results VMO ({total}) exceeds one page (16384)"
        );
    }

    #[test]
    fn max_lines_capacity() {
        assert_eq!(MAX_LINES, 512);
        assert_eq!(
            RESULTS_VALUE_SIZE,
            LayoutHeader::SIZE + 512 * LineInfo::SIZE
        );
    }
}
