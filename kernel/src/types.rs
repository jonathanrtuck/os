//! Shared type definitions used across all kernel modules.
//!
//! ID newtypes, error enum, rights bitfield. No logic — just types.

/// Syscall error codes. Uniform across all 30 syscalls.
/// x0 = error code (0 = success).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallError {
    Success = 0,
    InvalidHandle = 1,
    WrongHandleType = 2,
    InsufficientRights = 3,
    OutOfMemory = 4,
    InvalidArgument = 5,
    PeerClosed = 6,
    TimedOut = 7,
    BufferFull = 8,
    WouldDeadlock = 9,
    AlreadySealed = 10,
    GenerationMismatch = 11,
    NotFound = 12,
}

/// Object types that handles can reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    Vmo = 0,
    Endpoint = 1,
    Event = 2,
    Thread = 3,
    AddressSpace = 4,
    Resource = 5,
}

/// Handle rights bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const READ: Rights = Rights(1 << 0);
    pub const WRITE: Rights = Rights(1 << 1);
    pub const EXECUTE: Rights = Rights(1 << 2);
    pub const MAP: Rights = Rights(1 << 3);
    pub const DUP: Rights = Rights(1 << 4);
    pub const TRANSFER: Rights = Rights(1 << 5);
    pub const SIGNAL: Rights = Rights(1 << 6);
    pub const WAIT: Rights = Rights(1 << 7);
    pub const SPAWN: Rights = Rights(1 << 8);

    pub const ALL: Rights = Rights(0x1FF);
    pub const NONE: Rights = Rights(0);

    pub fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersection(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }

    pub fn is_subset_of(self, other: Rights) -> bool {
        other.contains(self)
    }
}

/// Thread priority — 256 levels (0 = idle, 255 = max).
///
/// Named constants provide semantic anchors. Any u8 value is valid.
/// The scheduler uses a bitmap-indexed multi-level queue for O(1) operations
/// across all 256 levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub u8);

impl Default for Priority {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl Priority {
    pub const IDLE: Self = Priority(0);
    pub const LOW: Self = Priority(64);
    pub const NORMAL: Self = Priority(128);
    pub const HIGH: Self = Priority(192);
    pub const MAX: Self = Priority(255);

    pub const NUM_LEVELS: usize = 256;

    /// Map to one of 4 IPC priority buckets (0-3) for endpoint send queues.
    pub fn ipc_bucket(self) -> usize {
        (self.0 >> 6) as usize
    }
}

// Aliases matching the old 4-level enum for migration convenience.
#[allow(non_upper_case_globals)]
impl Priority {
    pub const Idle: Self = Self::IDLE;
    pub const Low: Self = Self::LOW;
    pub const Medium: Self = Self::NORMAL;
    pub const High: Self = Self::HIGH;
}

/// Topology placement hints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TopologyHint {
    #[default]
    Any,
    Performance,
    Efficiency,
    SameClusterAs(ThreadId),
}

/// Reason a thread is blocked — stored for debugging and timeout handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    EventWait,
    EndpointRecv,
    EndpointCall,
}

// ---------------------------------------------------------------------------
// ID newtypes — small, Copy, used as array indices
// ---------------------------------------------------------------------------

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            pub fn as_usize(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_newtype!(VmoId);
id_newtype!(EndpointId);
id_newtype!(EventId);
id_newtype!(ThreadId);
id_newtype!(AddressSpaceId);
id_newtype!(HandleId);
id_newtype!(ResourceId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rights_contains() {
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);

        assert!(rw.contains(Rights::READ));
        assert!(rw.contains(Rights::WRITE));
        assert!(!rw.contains(Rights::EXECUTE));
    }

    #[test]
    fn rights_intersection() {
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let rx = Rights(Rights::READ.0 | Rights::EXECUTE.0);
        let r = rw.intersection(rx);

        assert!(r.contains(Rights::READ));
        assert!(!r.contains(Rights::WRITE));
        assert!(!r.contains(Rights::EXECUTE));
    }

    #[test]
    fn rights_subset() {
        let r = Rights::READ;
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);

        assert!(r.is_subset_of(rw));
        assert!(!rw.is_subset_of(r));
    }

    #[test]
    fn syscall_error_repr() {
        assert_eq!(SyscallError::Success as u64, 0);
        assert_eq!(SyscallError::InvalidHandle as u64, 1);
        assert_eq!(SyscallError::NotFound as u64, 12);
    }

    #[test]
    fn priority_ordering() {
        assert!(Priority::HIGH > Priority::NORMAL);
        assert!(Priority::NORMAL > Priority::LOW);
        assert!(Priority::LOW > Priority::IDLE);
        assert!(Priority::MAX > Priority::HIGH);
        assert!(Priority(100) > Priority(99));
    }

    #[test]
    fn priority_ipc_bucket() {
        assert_eq!(Priority::IDLE.ipc_bucket(), 0);
        assert_eq!(Priority(63).ipc_bucket(), 0);
        assert_eq!(Priority::LOW.ipc_bucket(), 1);
        assert_eq!(Priority(127).ipc_bucket(), 1);
        assert_eq!(Priority::NORMAL.ipc_bucket(), 2);
        assert_eq!(Priority(191).ipc_bucket(), 2);
        assert_eq!(Priority::HIGH.ipc_bucket(), 3);
        assert_eq!(Priority::MAX.ipc_bucket(), 3);
    }

    #[test]
    fn priority_default_is_normal() {
        assert_eq!(Priority::default(), Priority::NORMAL);
    }

    #[test]
    fn priority_aliases_match() {
        assert_eq!(Priority::Idle, Priority::IDLE);
        assert_eq!(Priority::Low, Priority::LOW);
        assert_eq!(Priority::Medium, Priority::NORMAL);
        assert_eq!(Priority::High, Priority::HIGH);
    }
}
