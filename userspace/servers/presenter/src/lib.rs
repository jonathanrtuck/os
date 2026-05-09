//! Presenter service protocol — scene graph builder and view state manager.
//!
//! The presenter is the OS Service from architecture.md: it compiles
//! document state + layout results into a scene graph for the compositor.
//!
//! Transport: sync call/reply for SETUP/BUILD/GET_INFO.
//! Data plane: scene graph in shared VMO (writer), viewport state in
//! seqlock register (writer), layout results in seqlock register (reader).

#![no_std]

pub use ipc::MAX_PAYLOAD;

// ── Methods served by the presenter ─────────────────────────────

/// Returns scene graph VMO handle (RO) via IPC handle slot 0.
pub const SETUP: u32 = 1;

/// Trigger full scene graph rebuild from document + layout state.
/// Replies with current stats when build is complete.
pub const BUILD: u32 = 2;

/// Returns current presenter statistics.
pub const GET_INFO: u32 = 3;

/// Key event from input driver. Payload: KeyDispatch (4 bytes).
/// Presenter forwards to the active editor, rebuilds scene, re-renders.
pub const KEY_EVENT: u32 = 4;

/// Scroll event from input driver. Payload: ScrollEvent (8 bytes).
/// Presenter adjusts scroll_y, rebuilds scene, re-renders.
pub const SCROLL_EVENT: u32 = 5;

// ── Visual constants ────────────────────────────────────────────

pub const DEFAULT_WIDTH: u32 = 1440;
pub const DEFAULT_HEIGHT: u32 = 900;

pub const FONT_SIZE: u16 = 14;
pub const CHAR_WIDTH_F32: f32 = 10.0;
pub const LINE_HEIGHT: u32 = 20;

pub const MARGIN_LEFT: i32 = 16;
pub const MARGIN_TOP: i32 = 12;

pub const BG_R: u8 = 30;
pub const BG_G: u8 = 30;
pub const BG_B: u8 = 32;

pub const TEXT_R: u8 = 30;
pub const TEXT_G: u8 = 30;
pub const TEXT_B: u8 = 32;

pub const CURSOR_R: u8 = 30;
pub const CURSOR_G: u8 = 30;
pub const CURSOR_B: u8 = 30;

pub const CURSOR_WIDTH: u32 = 2;

pub const SEL_R: u8 = 44;
pub const SEL_G: u8 = 80;
pub const SEL_B: u8 = 132;

// ── Chrome / page geometry ────────────────────────────────────────

pub const TITLE_BAR_H: u32 = 36;
pub const PAGE_MARGIN_V: u32 = 16;
pub const PAGE_PADDING: u32 = 24;

pub const CHROME_TITLE_R: u8 = 255;
pub const CHROME_TITLE_G: u8 = 255;
pub const CHROME_TITLE_B: u8 = 255;

pub const CHROME_CLOCK_R: u8 = 160;
pub const CHROME_CLOCK_G: u8 = 160;
pub const CHROME_CLOCK_B: u8 = 160;

pub const PAGE_BG_R: u8 = 255;
pub const PAGE_BG_G: u8 = 255;
pub const PAGE_BG_B: u8 = 255;

pub const SHADOW_BLUR_RADIUS: u8 = 64;
pub const SHADOW_SPREAD: i8 = 36;

// ── SETUP reply ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupReply {
    pub display_width: u32,
    pub display_height: u32,
}

impl SetupReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.display_width.to_le_bytes());
        buf[4..8].copy_from_slice(&self.display_height.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            display_width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            display_height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }
}

// ── SCROLL_EVENT payload ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollEvent {
    pub delta_x: i32,
    pub delta_y: i32,
}

impl ScrollEvent {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.delta_x.to_le_bytes());
        buf[4..8].copy_from_slice(&self.delta_y.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            delta_x: i32::from_le_bytes(buf[0..4].try_into().unwrap()),
            delta_y: i32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }
}

// ── GET_INFO / BUILD reply ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub node_count: u16,
    pub generation: u32,
    pub line_count: u32,
    pub cursor_line: u32,
    pub cursor_col: u32,
    pub content_len: u32,
    pub scroll_y: i32,
}

impl InfoReply {
    pub const SIZE: usize = 26;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.node_count.to_le_bytes());
        buf[2..6].copy_from_slice(&self.generation.to_le_bytes());
        buf[6..10].copy_from_slice(&self.line_count.to_le_bytes());
        buf[10..14].copy_from_slice(&self.cursor_line.to_le_bytes());
        buf[14..18].copy_from_slice(&self.cursor_col.to_le_bytes());
        buf[18..22].copy_from_slice(&self.content_len.to_le_bytes());
        buf[22..26].copy_from_slice(&self.scroll_y.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            node_count: u16::from_le_bytes(buf[0..2].try_into().unwrap()),
            generation: u32::from_le_bytes(buf[2..6].try_into().unwrap()),
            line_count: u32::from_le_bytes(buf[6..10].try_into().unwrap()),
            cursor_line: u32::from_le_bytes(buf[10..14].try_into().unwrap()),
            cursor_col: u32::from_le_bytes(buf[14..18].try_into().unwrap()),
            content_len: u32::from_le_bytes(buf[18..22].try_into().unwrap()),
            scroll_y: i32::from_le_bytes(buf[22..26].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_reply_round_trip() {
        let reply = SetupReply {
            display_width: 1440,
            display_height: 900,
        };
        let mut buf = [0u8; SetupReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(SetupReply::read_from(&buf), reply);
    }

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            node_count: 42,
            generation: 7,
            line_count: 30,
            cursor_line: 5,
            cursor_col: 10,
            content_len: 500,
            scroll_y: 120,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(InfoReply::read_from(&buf), reply);
    }

    #[test]
    fn scroll_event_round_trip() {
        let event = ScrollEvent {
            delta_x: 0,
            delta_y: -40,
        };
        let mut buf = [0u8; ScrollEvent::SIZE];

        event.write_to(&mut buf);

        assert_eq!(ScrollEvent::read_from(&buf), event);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [SETUP, BUILD, GET_INFO, KEY_EVENT, SCROLL_EVENT];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(SetupReply::SIZE <= MAX_PAYLOAD);
        assert!(InfoReply::SIZE <= MAX_PAYLOAD);
        assert!(ScrollEvent::SIZE <= MAX_PAYLOAD);
    }
}
