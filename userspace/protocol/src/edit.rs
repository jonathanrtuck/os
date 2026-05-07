//! Edit protocol — editing operations from editor to document service.
//!
//! Transport: sync call/reply (editor → document).
//!
//! Insert carries inline text data after the header (up to 112 bytes).
//! For larger inserts, the editor passes a VMO handle containing the
//! data. Undo/Redo have no payload — the document service manages the
//! snapshot ring internally.

pub const INSERT: u32 = 1;
pub const DELETE: u32 = 2;
pub const CURSOR_MOVE: u32 = 3;
pub const SELECT: u32 = 4;
pub const UNDO: u32 = 5;
pub const REDO: u32 = 6;

/// Insert header — byte offset where text is inserted.
/// Inline data follows immediately after this header in the IPC payload.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_header_round_trip() {
        let header = InsertHeader { offset: 42 };
        let mut buf = [0u8; InsertHeader::SIZE];

        header.write_to(&mut buf);

        let decoded = InsertHeader::read_from(&buf);

        assert_eq!(header, decoded);
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

        let decoded = DeleteRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn cursor_move_round_trip() {
        let cursor = CursorMove { position: u64::MAX };
        let mut buf = [0u8; CursorMove::SIZE];

        cursor.write_to(&mut buf);

        let decoded = CursorMove::read_from(&buf);

        assert_eq!(cursor, decoded);
    }

    #[test]
    fn selection_round_trip() {
        let sel = Selection {
            anchor: 10,
            cursor: 50,
        };
        let mut buf = [0u8; Selection::SIZE];

        sel.write_to(&mut buf);

        let decoded = Selection::read_from(&buf);

        assert_eq!(sel, decoded);
    }

    #[test]
    fn selection_reversed() {
        let sel = Selection {
            anchor: 100,
            cursor: 10,
        };
        let mut buf = [0u8; Selection::SIZE];

        sel.write_to(&mut buf);

        let decoded = Selection::read_from(&buf);

        assert_eq!(decoded.anchor, 100);
        assert_eq!(decoded.cursor, 10);
    }

    #[test]
    fn delete_zero_length() {
        let req = DeleteRequest { offset: 0, len: 0 };
        let mut buf = [0u8; DeleteRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = DeleteRequest::read_from(&buf);

        assert_eq!(decoded.len, 0);
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(InsertHeader::SIZE <= crate::MAX_PAYLOAD);
        assert!(DeleteRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(CursorMove::SIZE <= crate::MAX_PAYLOAD);
        assert!(Selection::SIZE <= crate::MAX_PAYLOAD);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [INSERT, DELETE, CURSOR_MOVE, SELECT, UNDO, REDO];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }
}
