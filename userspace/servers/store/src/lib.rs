//! Store service protocol — persistence layer over the COW filesystem.
//!
//! Transport: sync call/reply (document → store).
//!
//! The store service owns the filesystem and document catalog. It
//! provides file CRUD, snapshots (for undo), and commit. Data transfer
//! for reads and writes uses a shared VMO established via SETUP.

#![no_std]

pub const MAX_PAYLOAD: usize = 120;

pub const SETUP: u32 = 1;
pub const CREATE: u32 = 2;
pub const WRITE_DOC: u32 = 3;
pub const READ_DOC: u32 = 4;
pub const TRUNCATE: u32 = 5;
pub const COMMIT: u32 = 6;
pub const SNAPSHOT: u32 = 7;
pub const RESTORE: u32 = 8;
pub const DELETE_SNAPSHOT: u32 = 9;
pub const GET_INFO: u32 = 10;

/// Create request — media type as inline bytes after the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateRequest {
    pub media_type_len: u16,
}

impl CreateRequest {
    pub const SIZE: usize = 2;
    pub const MAX_MEDIA_TYPE: usize = crate::MAX_PAYLOAD - Self::SIZE;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.media_type_len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            media_type_len: u16::from_le_bytes(buf[0..2].try_into().unwrap()),
        }
    }
}

/// Create reply — returns the new file ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateReply {
    pub file_id: u64,
}

impl CreateReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

/// Write request — offset + length of data in the shared VMO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRequest {
    pub file_id: u64,
    pub offset: u64,
    pub vmo_offset: u32,
    pub len: u32,
}

impl WriteRequest {
    pub const SIZE: usize = 24;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.vmo_offset.to_le_bytes());
        buf[20..24].copy_from_slice(&self.len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            offset: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            vmo_offset: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            len: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        }
    }
}

/// Read request — file ID + offset + max bytes to read.
/// Store writes data to the shared VMO at vmo_offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    pub file_id: u64,
    pub offset: u64,
    pub vmo_offset: u32,
    pub max_len: u32,
}

impl ReadRequest {
    pub const SIZE: usize = 24;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.vmo_offset.to_le_bytes());
        buf[20..24].copy_from_slice(&self.max_len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            offset: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            vmo_offset: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            max_len: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        }
    }
}

/// Read reply — actual bytes written to the shared VMO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadReply {
    pub bytes_read: u32,
}

impl ReadReply {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.bytes_read.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            bytes_read: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

/// Truncate request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncateRequest {
    pub file_id: u64,
    pub len: u64,
}

impl TruncateRequest {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            len: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

/// Commit request — commit all pending changes to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitRequest {
    pub file_id: u64,
}

impl CommitRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

/// Snapshot request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRequest {
    pub file_id: u64,
}

impl SnapshotRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

/// Snapshot reply — returns the snapshot ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotReply {
    pub snapshot_id: u64,
}

impl SnapshotReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.snapshot_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            snapshot_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

/// Restore request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreRequest {
    pub file_id: u64,
    pub snapshot_id: u64,
}

impl RestoreRequest {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.snapshot_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            snapshot_id: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

/// Delete snapshot request (fire-and-forget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteSnapshotRequest {
    pub snapshot_id: u64,
}

impl DeleteSnapshotRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.snapshot_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            snapshot_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        }
    }
}

/// Get info reply — document metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub file_id: u64,
    pub size: u64,
}

impl InfoReply {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.file_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            size: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_request_round_trip() {
        let req = CreateRequest { media_type_len: 10 };
        let mut buf = [0u8; CreateRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(CreateRequest::read_from(&buf), req);
    }

    #[test]
    fn create_reply_round_trip() {
        let reply = CreateReply { file_id: 42 };
        let mut buf = [0u8; CreateReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(CreateReply::read_from(&buf), reply);
    }

    #[test]
    fn write_request_round_trip() {
        let req = WriteRequest {
            file_id: 1,
            offset: 100,
            vmo_offset: 0,
            len: 512,
        };
        let mut buf = [0u8; WriteRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(WriteRequest::read_from(&buf), req);
    }

    #[test]
    fn read_request_round_trip() {
        let req = ReadRequest {
            file_id: 1,
            offset: 0,
            vmo_offset: 0,
            max_len: 4096,
        };
        let mut buf = [0u8; ReadRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(ReadRequest::read_from(&buf), req);
    }

    #[test]
    fn read_reply_round_trip() {
        let reply = ReadReply { bytes_read: 256 };
        let mut buf = [0u8; ReadReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(ReadReply::read_from(&buf), reply);
    }

    #[test]
    fn truncate_round_trip() {
        let req = TruncateRequest {
            file_id: 5,
            len: 1024,
        };
        let mut buf = [0u8; TruncateRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(TruncateRequest::read_from(&buf), req);
    }

    #[test]
    fn commit_round_trip() {
        let req = CommitRequest { file_id: 1 };
        let mut buf = [0u8; CommitRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(CommitRequest::read_from(&buf), req);
    }

    #[test]
    fn snapshot_round_trip() {
        let req = SnapshotRequest { file_id: 3 };
        let mut buf = [0u8; SnapshotRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(SnapshotRequest::read_from(&buf), req);
    }

    #[test]
    fn snapshot_reply_round_trip() {
        let reply = SnapshotReply { snapshot_id: 99 };
        let mut buf = [0u8; SnapshotReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(SnapshotReply::read_from(&buf), reply);
    }

    #[test]
    fn restore_round_trip() {
        let req = RestoreRequest {
            file_id: 1,
            snapshot_id: 7,
        };
        let mut buf = [0u8; RestoreRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(RestoreRequest::read_from(&buf), req);
    }

    #[test]
    fn delete_snapshot_round_trip() {
        let req = DeleteSnapshotRequest { snapshot_id: 42 };
        let mut buf = [0u8; DeleteSnapshotRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(DeleteSnapshotRequest::read_from(&buf), req);
    }

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            file_id: 1,
            size: 8192,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(InfoReply::read_from(&buf), reply);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [
            SETUP,
            CREATE,
            WRITE_DOC,
            READ_DOC,
            TRUNCATE,
            COMMIT,
            SNAPSHOT,
            RESTORE,
            DELETE_SNAPSHOT,
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
        assert!(CreateRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(CreateReply::SIZE <= crate::MAX_PAYLOAD);
        assert!(WriteRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(ReadRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(ReadReply::SIZE <= crate::MAX_PAYLOAD);
        assert!(TruncateRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(CommitRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(SnapshotRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(SnapshotReply::SIZE <= crate::MAX_PAYLOAD);
        assert!(RestoreRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(DeleteSnapshotRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(InfoReply::SIZE <= crate::MAX_PAYLOAD);
    }
}
