//! Protocol definitions for future services.
//!
//! These modules define wire formats for services that don't exist yet.
//! When a server is created, its protocol moves into the server's lib.rs.
//!
//! Implemented protocols have moved to their servers:
//! - bootstrap → servers/init
//! - name_service → servers/name
//! - blk → servers/drivers/blk
//! - input → servers/drivers/input
//! - metal → servers/drivers/render
//! - store → servers/store
//! - edit, view → servers/document

#![no_std]
#![allow(clippy::missing_panics_doc)]

pub mod decode;

/// IPC payload capacity (128-byte message minus 8-byte header).
pub const MAX_PAYLOAD: usize = 120;

pub const MAX_NAME_LEN: usize = 32;

fn null_terminated_len(buf: &[u8]) -> usize {
    buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
}
