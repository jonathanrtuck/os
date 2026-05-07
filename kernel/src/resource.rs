//! Resource — kernel-created authority token for privileged operations.
//!
//! A Resource handle gates access to operations that only specific
//! processes should perform. The kernel creates Resources at boot and
//! installs them in init's handle table. Init passes or delegates them
//! to child processes as needed.
//!
//! Resources have no mutable state and no operations of their own —
//! they exist solely to be presented to syscalls as proof of authority.

use crate::types::ResourceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ResourceKind {
    Dma = 0,
}

pub struct Resource {
    pub id: ResourceId,
    pub kind: ResourceKind,
}

impl Resource {
    pub fn new(id: ResourceId, kind: ResourceKind) -> Self {
        Self { id, kind }
    }
}
