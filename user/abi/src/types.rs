//! Shared types for the syscall interface.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Handle(pub u32);

impl Handle {
    pub const SELF: Self = Self(u32::MAX);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Rights(pub u32);

impl Rights {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const MAP: Self = Self(1 << 3);
    pub const DUP: Self = Self(1 << 4);
    pub const TRANSFER: Self = Self(1 << 5);
    pub const SIGNAL: Self = Self(1 << 6);
    pub const WAIT: Self = Self(1 << 7);
    pub const SPAWN: Self = Self(1 << 8);
    pub const ALL: Self = Self(0x1FF);

    pub const READ_MAP: Self = Self(Self::READ.0 | Self::MAP.0);
    pub const READ_WRITE_MAP: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::MAP.0);
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

impl SyscallError {
    pub fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::InvalidHandle),
            2 => Some(Self::WrongHandleType),
            3 => Some(Self::InsufficientRights),
            4 => Some(Self::OutOfMemory),
            5 => Some(Self::InvalidArgument),
            6 => Some(Self::PeerClosed),
            7 => Some(Self::TimedOut),
            8 => Some(Self::BufferFull),
            9 => Some(Self::WouldDeadlock),
            10 => Some(Self::AlreadySealed),
            11 => Some(Self::GenerationMismatch),
            12 => Some(Self::NotFound),
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
    Resource = 5,
}

pub const MSG_SIZE: usize = 128;
