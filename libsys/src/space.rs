//! Address space syscall wrappers.

use crate::{
    raw::{self, check, num},
    types::{Handle, SyscallError},
};

pub fn create() -> Result<Handle, SyscallError> {
    check(raw::syscall(num::SPACE_CREATE, 0, 0, 0, 0, 0, 0)).map(|v| Handle(v as u32))
}

pub fn destroy(handle: Handle) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::SPACE_DESTROY,
        handle.0 as u64,
        0,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}
