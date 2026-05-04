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

/// Priority levels for threads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    Low = 0,
    #[default]
    Medium = 1,
    High = 2,
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

// ---------------------------------------------------------------------------
// ID newtypes — small, Copy, used as array indices
// ---------------------------------------------------------------------------

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
        assert!(Priority::High > Priority::Medium);
        assert!(Priority::Medium > Priority::Low);
    }
}
