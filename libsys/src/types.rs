//! Shared types for the syscall interface.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Handle(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Rights(pub u32);

impl Rights {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const MAP: Self = Self(1 << 3);
    pub const SIGNAL: Self = Self(1 << 4);
    pub const WAIT: Self = Self(1 << 5);
    pub const SPAWN: Self = Self(1 << 6);
    pub const MANAGE: Self = Self(1 << 7);
    pub const DUPLICATE: Self = Self(1 << 8);
    pub const ALL: Self = Self(0x1FF);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Priority {
    Idle = 0,
    Low = 1,
    Medium = 2,
    High = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallError {
    InvalidHandle = 1,
    InvalidArgument = 2,
    OutOfMemory = 3,
    WrongHandleType = 4,
    InsufficientRights = 5,
    BufferFull = 6,
    PeerClosed = 7,
    NotFound = 8,
}

impl SyscallError {
    pub fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::InvalidHandle),
            2 => Some(Self::InvalidArgument),
            3 => Some(Self::OutOfMemory),
            4 => Some(Self::WrongHandleType),
            5 => Some(Self::InsufficientRights),
            6 => Some(Self::BufferFull),
            7 => Some(Self::PeerClosed),
            8 => Some(Self::NotFound),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ObjectType {
    Vmo = 0,
    Endpoint = 1,
    Event = 2,
    Thread = 3,
    AddressSpace = 4,
}

pub const MSG_SIZE: usize = 128;
