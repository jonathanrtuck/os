//! Protocol definitions — message types for every IPC boundary.
//!
//! Single source of truth for message layouts. One module per protocol
//! boundary. All types are `no_std` and zero-dependency — pure data
//! definitions with manual little-endian serialization.
//!
//! ## Transport model
//!
//! Every IPC boundary uses one of three patterns (see `ipc` crate):
//!
//! - **Sync call/reply** — transactional request/response over endpoints
//! - **Event ring** — continuous unidirectional stream (SPSC ring buffer)
//! - **State register** — latest-value-wins (seqlock in shared memory)
//!
//! This crate defines the byte layouts for each. Shared-memory region
//! layouts (document buffer, scene graph, layout results) are defined
//! in their respective library crates, not here.

#![no_std]
#![allow(clippy::missing_panics_doc)]

pub mod blk;
pub mod bootstrap;
pub mod decode;
pub mod edit;
pub mod input;
pub mod name_service;
pub mod store;
pub mod view;

/// IPC payload capacity (128-byte message minus 8-byte header).
pub const MAX_PAYLOAD: usize = 120;

// Reply status codes — used in the IPC header's status field.
// Zero means success; nonzero means error. Services may define
// additional codes above STATUS_LAST.
pub const STATUS_OK: u16 = 0;
pub const STATUS_NOT_FOUND: u16 = 1;
pub const STATUS_ALREADY_EXISTS: u16 = 2;
pub const STATUS_INVALID: u16 = 3;
pub const STATUS_NO_SPACE: u16 = 4;
pub const STATUS_IO_ERROR: u16 = 5;
pub const STATUS_UNSUPPORTED: u16 = 6;
pub const STATUS_LAST: u16 = 6;

pub const MAX_NAME_LEN: usize = 32;

fn null_terminated_len(buf: &[u8]) -> usize {
    buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
}
