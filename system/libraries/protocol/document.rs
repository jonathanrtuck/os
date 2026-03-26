//! Document service protocol — init/core <-> document service.
//!
//! The document service replaces the filesystem service, adding metadata
//! (media types, attributes) and snapshot/restore (undo/redo) via the
//! store library.

// ── Message types ────────────────────────────────────────────────────

/// Config message: init → document service (device config + doc buffer).
pub const MSG_DOC_CONFIG: u32 = 80;
/// Ready signal: document service → init.
pub const MSG_DOC_READY: u32 = 81;
/// Commit request: core → document service.
pub const MSG_DOC_COMMIT: u32 = 82;
/// Query request: core → document service.
pub const MSG_DOC_QUERY: u32 = 83;
/// Query result: document service → core.
pub const MSG_DOC_QUERY_RESULT: u32 = 84;
/// Read request: core → document service.
pub const MSG_DOC_READ: u32 = 85;
/// Read done: document service → core.
pub const MSG_DOC_READ_DONE: u32 = 86;
/// Snapshot request: core → document service.
pub const MSG_DOC_SNAPSHOT: u32 = 87;
/// Restore request: core → document service.
pub const MSG_DOC_RESTORE: u32 = 88;
/// Boot done signal: init → document service (end boot-query phase).
pub const MSG_DOC_BOOT_DONE: u32 = 89;
/// Create document request: core → document service.
pub const MSG_DOC_CREATE: u32 = 90;
/// Create document result: document service → core.
pub const MSG_DOC_CREATE_RESULT: u32 = 91;

// ── Payload structs ──────────────────────────────────────────────────

/// Document service configuration (init → document service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocConfig {
    /// Physical address of the virtio-blk MMIO region.
    pub mmio_pa: u64,
    /// IRQ number for the virtio-blk device.
    pub irq: u32,
    pub _pad: u32,
    /// VA of the shared document buffer (read-only for document service).
    pub doc_va: u64,
    /// Document buffer capacity in bytes (content area, excluding header).
    pub doc_capacity: u32,
    pub _pad2: u32,
    /// VA of the Content Region (read-write for boot font loading).
    /// 0 if no Content Region is shared.
    pub content_va: u64,
    /// Content Region size in bytes.
    pub content_size: u32,
    pub _pad3: u32,
}
const _: () = assert!(core::mem::size_of::<DocConfig>() <= 60);

/// Query request payload (core/init → document service).
///
/// `query_type`:
///   0 = media type exact match (data = UTF-8 string)
///   1 = type prefix match (data = UTF-8 string)
///   2 = attribute match (data = "key\0value" UTF-8)
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocQuery {
    pub query_type: u32,
    pub data_len: u32,
    pub data: [u8; 48],
}
const _: () = assert!(core::mem::size_of::<DocQuery>() <= 60);

/// Query result payload (document service → core).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocQueryResult {
    pub count: u32,
    pub _pad: u32,
    pub file_ids: [u64; 6],
}
const _: () = assert!(core::mem::size_of::<DocQueryResult>() <= 60);

/// Commit request payload (core → document service).
/// Includes the FileId of the document to commit.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocCommit {
    pub file_id: u64,
}
const _: () = assert!(core::mem::size_of::<DocCommit>() <= 60);

/// Create document request payload (core → document service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocCreate {
    pub media_type_len: u32,
    pub _pad: u32,
    pub media_type: [u8; 52],
}
const _: () = assert!(core::mem::size_of::<DocCreate>() <= 60);

/// Create document result payload (document service → core).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocCreateResult {
    pub file_id: u64,
    /// 0 = success, non-zero = error.
    pub status: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<DocCreateResult>() <= 60);

/// Read request payload (core → document service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocRead {
    pub file_id: u64,
    pub target_va: u64,
    pub capacity: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<DocRead>() <= 60);

/// Read done payload (document service → core).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocReadDone {
    pub file_id: u64,
    pub len: u32,
    /// 0 = success, non-zero = error.
    pub status: u32,
}
const _: () = assert!(core::mem::size_of::<DocReadDone>() <= 60);

/// Snapshot request payload (core → document service).
/// Contains file IDs to include in the snapshot.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocSnapshot {
    pub file_count: u32,
    pub _pad: u32,
    pub file_ids: [u64; 6],
}
const _: () = assert!(core::mem::size_of::<DocSnapshot>() <= 60);

/// Snapshot result payload (document service → core).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocSnapshotResult {
    pub snapshot_id: u64,
    /// 0 = success, non-zero = error.
    pub status: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<DocSnapshotResult>() <= 60);

/// Restore request payload (core → document service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocRestore {
    pub snapshot_id: u64,
}
const _: () = assert!(core::mem::size_of::<DocRestore>() <= 60);

/// Restore result payload (document service → core).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocRestoreResult {
    /// 0 = success, non-zero = error.
    pub status: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<DocRestoreResult>() <= 60);

// ── Decode ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum Message {
    DocConfig(DocConfig),
    DocReady,
    DocCommit(DocCommit),
    DocQuery(DocQuery),
    DocQueryResult(DocQueryResult),
    DocRead(DocRead),
    DocReadDone(DocReadDone),
    DocSnapshot(DocSnapshot),
    DocSnapshotResult(DocSnapshotResult),
    DocRestore(DocRestore),
    DocRestoreResult(DocRestoreResult),
    DocBootDone,
    DocCreate(DocCreate),
    DocCreateResult(DocCreateResult),
}

pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
    match msg_type {
        MSG_DOC_CONFIG => Some(Message::DocConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_READY => Some(Message::DocReady),
        MSG_DOC_COMMIT => Some(Message::DocCommit(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_QUERY => Some(Message::DocQuery(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_QUERY_RESULT => Some(Message::DocQueryResult(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_READ => Some(Message::DocRead(unsafe { crate::decode_payload(payload) })),
        MSG_DOC_READ_DONE => Some(Message::DocReadDone(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_SNAPSHOT => Some(Message::DocSnapshot(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_RESTORE => Some(Message::DocRestore(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_BOOT_DONE => Some(Message::DocBootDone),
        MSG_DOC_CREATE => Some(Message::DocCreate(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_DOC_CREATE_RESULT => Some(Message::DocCreateResult(unsafe {
            crate::decode_payload(payload)
        })),
        _ => None,
    }
}
