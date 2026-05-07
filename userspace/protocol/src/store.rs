//! Store protocol — persistence operations for the document service.
//!
//! Transport: sync call/reply (document → store).
//!
//! The store service owns the COW block filesystem. It provides
//! commit, snapshot (for undo), restore, and read/write operations
//! keyed by opaque 64-bit document IDs.

pub const COMMIT: u32 = 1;
pub const SNAPSHOT: u32 = 2;
pub const RESTORE: u32 = 3;
pub const READ_DOC: u32 = 4;
pub const WRITE_DOC: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitRequest {
    pub doc_id: DocId,
}

impl CommitRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.doc_id.0.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            doc_id: DocId(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRequest {
    pub doc_id: DocId,
}

impl SnapshotRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.doc_id.0.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            doc_id: DocId(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotReply {
    pub snapshot_id: SnapshotId,
}

impl SnapshotReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.snapshot_id.0.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            snapshot_id: SnapshotId(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreRequest {
    pub doc_id: DocId,
    pub snapshot_id: SnapshotId,
}

impl RestoreRequest {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.doc_id.0.to_le_bytes());
        buf[8..16].copy_from_slice(&self.snapshot_id.0.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            doc_id: DocId(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
            snapshot_id: SnapshotId(u64::from_le_bytes(buf[8..16].try_into().unwrap())),
        }
    }
}

/// Read a range of a document's persisted content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    pub doc_id: DocId,
    pub offset: u64,
    pub len: u32,
}

impl ReadRequest {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.doc_id.0.to_le_bytes());
        buf[8..16].copy_from_slice(&self.offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            doc_id: DocId(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
            offset: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            len: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        }
    }
}

/// Write inline data to a document. Data follows after the header
/// in the IPC payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRequest {
    pub doc_id: DocId,
    pub offset: u64,
}

impl WriteRequest {
    pub const SIZE: usize = 16;
    pub const MAX_INLINE: usize = crate::MAX_PAYLOAD - Self::SIZE;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.doc_id.0.to_le_bytes());
        buf[8..16].copy_from_slice(&self.offset.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            doc_id: DocId(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
            offset: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_round_trip() {
        let req = CommitRequest {
            doc_id: DocId(0xDEAD),
        };
        let mut buf = [0u8; CommitRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = CommitRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn snapshot_round_trip() {
        let req = SnapshotRequest { doc_id: DocId(42) };
        let mut buf = [0u8; SnapshotRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = SnapshotRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn snapshot_reply_round_trip() {
        let reply = SnapshotReply {
            snapshot_id: SnapshotId(999),
        };
        let mut buf = [0u8; SnapshotReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = SnapshotReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn restore_round_trip() {
        let req = RestoreRequest {
            doc_id: DocId(1),
            snapshot_id: SnapshotId(7),
        };
        let mut buf = [0u8; RestoreRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = RestoreRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn read_request_round_trip() {
        let req = ReadRequest {
            doc_id: DocId(1),
            offset: 4096,
            len: 512,
        };
        let mut buf = [0u8; ReadRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = ReadRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn write_request_round_trip() {
        let req = WriteRequest {
            doc_id: DocId(1),
            offset: 0,
        };
        let mut buf = [0u8; WriteRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = WriteRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn write_max_inline_correct() {
        assert_eq!(WriteRequest::MAX_INLINE, 104);
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(CommitRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(SnapshotRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(SnapshotReply::SIZE <= crate::MAX_PAYLOAD);
        assert!(RestoreRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(ReadRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(WriteRequest::SIZE <= crate::MAX_PAYLOAD);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [COMMIT, SNAPSHOT, RESTORE, READ_DOC, WRITE_DOC];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn doc_id_zero() {
        let req = CommitRequest { doc_id: DocId(0) };
        let mut buf = [0u8; CommitRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(buf, [0; 8]);
    }

    #[test]
    fn doc_id_max() {
        let req = CommitRequest {
            doc_id: DocId(u64::MAX),
        };
        let mut buf = [0u8; CommitRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = CommitRequest::read_from(&buf);

        assert_eq!(decoded.doc_id.0, u64::MAX);
    }
}
