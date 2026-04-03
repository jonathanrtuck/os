//! Syscall error type and decoding.

/// Error codes returned by kernel syscalls.
///
/// Unifies the kernel's `syscall::Error` and `handle::HandleError` enums into
/// one flat enum for userspace. The numeric values match the kernel's `repr(i64)`
/// discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
    InvalidArgument = -4,
    AlreadyBorrowing = -5,
    NotBorrowing = -6,
    AlreadyBound = -7,
    WouldBlock = -8,
    OutOfMemory = -9,
    InvalidHandle = -10,
    // -11 is unused in the kernel.
    InsufficientRights = -12,
    TableFull = -13,
    SlotOccupied = -14,
    SyscallBlocked = -15,
}

impl SyscallError {
    /// Decode a raw negative return value into a `SyscallError`.
    ///
    /// Returns `UnknownSyscall` for unrecognized codes (defensive — kernel
    /// shouldn't produce codes outside this set, but userspace shouldn't panic
    /// if it does).
    pub fn from_raw(val: i64) -> Self {
        match val {
            -1 => Self::UnknownSyscall,
            -2 => Self::BadAddress,
            -3 => Self::BadLength,
            -4 => Self::InvalidArgument,
            -5 => Self::AlreadyBorrowing,
            -6 => Self::NotBorrowing,
            -7 => Self::AlreadyBound,
            -8 => Self::WouldBlock,
            -9 => Self::OutOfMemory,
            -10 => Self::InvalidHandle,
            -12 => Self::InsufficientRights,
            -13 => Self::TableFull,
            -14 => Self::SlotOccupied,
            -15 => Self::SyscallBlocked,
            _ => Self::UnknownSyscall,
        }
    }
}
