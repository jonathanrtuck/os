//! Document service protocol — editing operations and change notifications.
//!
//! Inbound (editor → document): sync call/reply on the document endpoint.
//! Outbound (document → presenter): event signal on a shared event object.
//!
//! The document service owns a shared VMO (the document buffer) with a
//! 64-byte header followed by content bytes. Clients receive an RO
//! handle to this VMO via SETUP and read it directly — no IPC needed
//! for reads. Writes go through sync IPC to the document service.

#![no_std]

/// IPC payload capacity (128-byte message minus 8-byte header).
pub const MAX_PAYLOAD: usize = 120;

// ── Methods served by the document service ─────────────────────────

pub const SETUP: u32 = 1;
pub const INSERT: u32 = 2;
pub const DELETE: u32 = 3;
pub const CURSOR_MOVE: u32 = 4;
pub const SELECT: u32 = 5;
pub const UNDO: u32 = 6;
pub const REDO: u32 = 7;
pub const GET_INFO: u32 = 8;

// ── Document buffer header ─────────────────────────────────────────

pub const DOC_HEADER_SIZE: usize = 64;

pub const DOC_OFFSET_LEN: usize = 0;
pub const DOC_OFFSET_CURSOR: usize = 8;
pub const DOC_OFFSET_GENERATION: usize = 16;
pub const DOC_OFFSET_FORMAT: usize = 20;

pub const FORMAT_PLAIN: u32 = 0;
pub const FORMAT_RICH: u32 = 1;

// ── Setup reply ────────────────────────────────────────────────────

/// SETUP reply: document metadata. The doc buffer VMO handle is
/// transferred via IPC handle slot 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupReply {
    pub content_len: u64,
    pub cursor_pos: u64,
    pub format: u32,
    pub file_id: u64,
}

impl SetupReply {
    pub const SIZE: usize = 28;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.content_len.to_le_bytes());
        buf[8..16].copy_from_slice(&self.cursor_pos.to_le_bytes());
        buf[16..20].copy_from_slice(&self.format.to_le_bytes());
        buf[20..28].copy_from_slice(&self.file_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            content_len: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            cursor_pos: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            format: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            file_id: u64::from_le_bytes(buf[20..28].try_into().unwrap()),
        }
    }
}

// ── Insert ─────────────────────────────────────────────────────────

/// Insert text at byte offset. Inline data follows after the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertHeader {
    pub offset: u64,
}

impl InsertHeader {
    pub const SIZE: usize = 8;
    pub const MAX_INLINE: usize = crate::MAX_PAYLOAD - Self::SIZE;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.offset.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            offset: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

// ── Delete ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteRequest {
    pub offset: u64,
    pub len: u64,
}

impl DeleteRequest {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..16].copy_from_slice(&self.len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            offset: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            len: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

// ── Cursor move ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorMove {
    pub position: u64,
}

impl CursorMove {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.position.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            position: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

// ── Selection ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: u64,
    pub cursor: u64,
}

impl Selection {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.anchor.to_le_bytes());
        buf[8..16].copy_from_slice(&self.cursor.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            anchor: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            cursor: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

// ── Edit reply ─────────────────────────────────────────────────────

/// Reply to INSERT, DELETE, UNDO, REDO: updated cursor position and
/// content length so the editor can update its local state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditReply {
    pub content_len: u64,
    pub cursor_pos: u64,
}

impl EditReply {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.content_len.to_le_bytes());
        buf[8..16].copy_from_slice(&self.cursor_pos.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            content_len: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            cursor_pos: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

// ── Info reply ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub content_len: u64,
    pub cursor_pos: u64,
    pub format: u32,
    pub file_id: u64,
    pub snapshot_count: u32,
}

impl InfoReply {
    pub const SIZE: usize = 32;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.content_len.to_le_bytes());
        buf[8..16].copy_from_slice(&self.cursor_pos.to_le_bytes());
        buf[16..20].copy_from_slice(&self.format.to_le_bytes());
        buf[20..28].copy_from_slice(&self.file_id.to_le_bytes());
        buf[28..32].copy_from_slice(&self.snapshot_count.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            content_len: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            cursor_pos: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            format: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            file_id: u64::from_le_bytes(buf[20..28].try_into().unwrap()),
            snapshot_count: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_reply_round_trip() {
        let reply = SetupReply {
            content_len: 42,
            cursor_pos: 10,
            format: FORMAT_PLAIN,
            file_id: 7,
        };
        let mut buf = [0u8; SetupReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(SetupReply::read_from(&buf), reply);
    }

    #[test]
    fn insert_header_round_trip() {
        let header = InsertHeader { offset: 42 };
        let mut buf = [0u8; InsertHeader::SIZE];

        header.write_to(&mut buf);

        assert_eq!(InsertHeader::read_from(&buf), header);
    }

    #[test]
    fn insert_max_inline_correct() {
        assert_eq!(InsertHeader::MAX_INLINE, 112);
    }

    #[test]
    fn delete_round_trip() {
        let req = DeleteRequest {
            offset: 100,
            len: 50,
        };
        let mut buf = [0u8; DeleteRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(DeleteRequest::read_from(&buf), req);
    }

    #[test]
    fn cursor_move_round_trip() {
        let cursor = CursorMove { position: u64::MAX };
        let mut buf = [0u8; CursorMove::SIZE];

        cursor.write_to(&mut buf);

        assert_eq!(CursorMove::read_from(&buf), cursor);
    }

    #[test]
    fn selection_round_trip() {
        let sel = Selection {
            anchor: 10,
            cursor: 50,
        };
        let mut buf = [0u8; Selection::SIZE];

        sel.write_to(&mut buf);

        assert_eq!(Selection::read_from(&buf), sel);
    }

    #[test]
    fn edit_reply_round_trip() {
        let reply = EditReply {
            content_len: 1024,
            cursor_pos: 42,
        };
        let mut buf = [0u8; EditReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(EditReply::read_from(&buf), reply);
    }

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            content_len: 500,
            cursor_pos: 100,
            format: FORMAT_PLAIN,
            file_id: 3,
            snapshot_count: 7,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(InfoReply::read_from(&buf), reply);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [
            SETUP,
            INSERT,
            DELETE,
            CURSOR_MOVE,
            SELECT,
            UNDO,
            REDO,
            GET_INFO,
        ];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(SetupReply::SIZE <= crate::MAX_PAYLOAD);
        assert!(InsertHeader::SIZE <= crate::MAX_PAYLOAD);
        assert!(DeleteRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(CursorMove::SIZE <= crate::MAX_PAYLOAD);
        assert!(Selection::SIZE <= crate::MAX_PAYLOAD);
        assert!(EditReply::SIZE <= crate::MAX_PAYLOAD);
        assert!(InfoReply::SIZE <= crate::MAX_PAYLOAD);
    }
}
