//! System information and clock syscall wrappers.

use crate::{
    raw::{self, check, num},
    types::SyscallError,
};

pub fn clock_read() -> Result<u64, SyscallError> {
    check(raw::syscall(num::CLOCK_READ, 0, 0, 0, 0, 0, 0))
}

pub fn info(key: u64) -> Result<u64, SyscallError> {
    check(raw::syscall(num::SYSTEM_INFO, key, 0, 0, 0, 0, 0))
}

pub const INFO_PAGE_SIZE: u64 = 0;
pub const INFO_MSG_SIZE: u64 = 1;
pub const INFO_NUM_CORES: u64 = 2;
