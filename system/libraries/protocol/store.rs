//! Store service protocol — init/document <-> store service.
//!
//! The store service is the metadata-aware persistence layer over virtio-blk.
//! It provides commit, query, snapshot/restore via the store library.

// ── Message types ────────────────────────────────────────────────────

/// Config message: init → store service (device config + doc buffer).
pub const MSG_STORE_CONFIG: u32 = 80;
/// Ready signal: store service → init.
pub const MSG_STORE_READY: u32 = 81;
/// Commit request: document → store service.
pub const MSG_STORE_COMMIT: u32 = 82;
/// Query request: document → store service.
pub const MSG_STORE_QUERY: u32 = 83;
/// Query result: store service → document.
pub const MSG_STORE_QUERY_RESULT: u32 = 84;
/// Read request: document → store service.
pub const MSG_STORE_READ: u32 = 85;
/// Read done: store service → document.
pub const MSG_STORE_READ_DONE: u32 = 86;
/// Snapshot request: document → store service.
pub const MSG_STORE_SNAPSHOT: u32 = 87;
/// Restore request: document → store service.
pub const MSG_STORE_RESTORE: u32 = 88;
/// Boot done signal: init → store service (end boot-query phase).
pub const MSG_STORE_BOOT_DONE: u32 = 89;
/// Create document request: document → store service.
pub const MSG_STORE_CREATE: u32 = 90;
/// Create document result: store service → document.
pub const MSG_STORE_CREATE_RESULT: u32 = 91;
/// Snapshot result: store service → document.
pub const MSG_STORE_SNAPSHOT_RESULT: u32 = 92;
/// Restore result: store service → document.
pub const MSG_STORE_RESTORE_RESULT: u32 = 93;
/// Delete snapshot request: document → store service (fire-and-forget).
pub const MSG_STORE_DELETE_SNAPSHOT: u32 = 94;

// ── Payload structs ──────────────────────────────────────────────────

/// Store service configuration (init → store service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreConfig {
    /// Physical address of the virtio-blk MMIO region.
    pub mmio_pa: u64,
    /// IRQ number for the virtio-blk device.
    pub irq: u32,
    pub _pad: u32,
    /// VA of the shared document buffer (read-only for store service).
    pub doc_va: u64,
    /// Document buffer capacity in bytes (content area, excluding header).
    pub doc_capacity: u32,
    pub _pad2: u32,
    /// VA of the Content Region (read-write for boot font loading).
    /// 0 if no Content Region is shared.
    pub content_va: u64,
    /// Content Region size in bytes.
    pub content_size: u32,
    /// Kernel channel handle for signaling init.
    pub init_handle: u8,
    /// Kernel channel handle for the document service channel.
    pub core_handle: u8,
    pub _pad3: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<StoreConfig>() <= 60);

/// Query request payload (document/init → store service).
///
/// `query_type`:
///   0 = media type exact match (data = UTF-8 string)
///   1 = type prefix match (data = UTF-8 string)
///   2 = attribute match (data = "key\0value" UTF-8)
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreQuery {
    pub query_type: u32,
    pub data_len: u32,
    pub data: [u8; 48],
}
const _: () = assert!(core::mem::size_of::<StoreQuery>() <= 60);

/// Query result payload (store service → document).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreQueryResult {
    pub count: u32,
    pub _pad: u32,
    pub file_ids: [u64; 6],
}
const _: () = assert!(core::mem::size_of::<StoreQueryResult>() <= 60);

/// Commit request payload (document → store service).
/// Includes the FileId of the document to commit.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreCommit {
    pub file_id: u64,
}
const _: () = assert!(core::mem::size_of::<StoreCommit>() <= 60);

/// Create document request payload (document → store service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreCreate {
    pub media_type_len: u32,
    pub _pad: u32,
    pub media_type: [u8; 52],
}
const _: () = assert!(core::mem::size_of::<StoreCreate>() <= 60);

/// Create document result payload (store service → document).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreCreateResult {
    pub file_id: u64,
    /// 0 = success, non-zero = error.
    pub status: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<StoreCreateResult>() <= 60);

/// Read request payload (document → store service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreRead {
    pub file_id: u64,
    pub target_va: u64,
    pub capacity: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<StoreRead>() <= 60);

/// Read done payload (store service → document).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreReadDone {
    pub file_id: u64,
    pub len: u32,
    /// 0 = success, non-zero = error.
    pub status: u32,
}
const _: () = assert!(core::mem::size_of::<StoreReadDone>() <= 60);

/// Snapshot request payload (document → store service).
/// Contains file IDs to include in the snapshot.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreSnapshot {
    pub file_count: u32,
    pub _pad: u32,
    pub file_ids: [u64; 6],
}
const _: () = assert!(core::mem::size_of::<StoreSnapshot>() <= 60);

/// Snapshot result payload (store service → document).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreSnapshotResult {
    pub snapshot_id: u64,
    /// 0 = success, non-zero = error.
    pub status: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<StoreSnapshotResult>() <= 60);

/// Restore request payload (document → store service).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreRestore {
    pub snapshot_id: u64,
}
const _: () = assert!(core::mem::size_of::<StoreRestore>() <= 60);

/// Restore result payload (store service → document).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreRestoreResult {
    /// 0 = success, non-zero = error.
    pub status: u32,
    pub _pad: u32,
}
const _: () = assert!(core::mem::size_of::<StoreRestoreResult>() <= 60);

/// Delete snapshot request payload (document → store service).
/// Fire-and-forget — no response message.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreDeleteSnapshot {
    pub snapshot_id: u64,
}
const _: () = assert!(core::mem::size_of::<StoreDeleteSnapshot>() <= 60);

// ── Decode ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum Message {
    StoreConfig(StoreConfig),
    StoreReady,
    StoreCommit(StoreCommit),
    StoreQuery(StoreQuery),
    StoreQueryResult(StoreQueryResult),
    StoreRead(StoreRead),
    StoreReadDone(StoreReadDone),
    StoreSnapshot(StoreSnapshot),
    StoreSnapshotResult(StoreSnapshotResult),
    StoreRestore(StoreRestore),
    StoreRestoreResult(StoreRestoreResult),
    StoreBootDone,
    StoreCreate(StoreCreate),
    StoreCreateResult(StoreCreateResult),
    StoreDeleteSnapshot(StoreDeleteSnapshot),
}

pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
    match msg_type {
        MSG_STORE_CONFIG => Some(Message::StoreConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_READY => Some(Message::StoreReady),
        MSG_STORE_COMMIT => Some(Message::StoreCommit(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_QUERY => Some(Message::StoreQuery(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_QUERY_RESULT => Some(Message::StoreQueryResult(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_READ => Some(Message::StoreRead(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_READ_DONE => Some(Message::StoreReadDone(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_SNAPSHOT => Some(Message::StoreSnapshot(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_RESTORE => Some(Message::StoreRestore(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_BOOT_DONE => Some(Message::StoreBootDone),
        MSG_STORE_CREATE => Some(Message::StoreCreate(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_CREATE_RESULT => Some(Message::StoreCreateResult(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_SNAPSHOT_RESULT => Some(Message::StoreSnapshotResult(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_RESTORE_RESULT => Some(Message::StoreRestoreResult(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_STORE_DELETE_SNAPSHOT => Some(Message::StoreDeleteSnapshot(unsafe {
            crate::decode_payload(payload)
        })),
        _ => None,
    }
}

// ── Legacy filesystem service (blkfs) ───────────────────────────────
//
// These types were originally in the `blkfs` module. They are used by
// the filesystem service which is the predecessor of the store service.

/// Config message: init → filesystem service.
pub const MSG_FS_CONFIG: u32 = 70;
/// Commit request: core → filesystem service.
pub const MSG_FS_COMMIT: u32 = 71;
/// Ready signal: filesystem → core (via init channel, not direct).
pub const MSG_FS_READY: u32 = 72;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FsConfig {
    /// VA of the shared document buffer (read-only for filesystem).
    pub doc_va: u64,
    /// Document buffer capacity in bytes (content area, excluding header).
    pub doc_capacity: u32,
    /// Kernel channel handle for signaling init.
    pub init_handle: u8,
    /// Kernel channel handle for the core (docmodel) channel.
    pub core_handle: u8,
    pub _pad: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<FsConfig>() <= 60);

#[derive(Clone, Copy, Debug)]
pub enum FsMessage {
    FsConfig(FsConfig),
    FsCommit,
    FsReady,
}

pub fn decode_fs(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<FsMessage> {
    match msg_type {
        MSG_FS_CONFIG => Some(FsMessage::FsConfig(unsafe {
            crate::decode_payload(payload)
        })),
        MSG_FS_COMMIT => Some(FsMessage::FsCommit),
        MSG_FS_READY => Some(FsMessage::FsReady),
        _ => None,
    }
}
