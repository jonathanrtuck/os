//! Thread syscall wrappers.

use crate::{
    raw::{self, check, num},
    types::{Handle, Priority, SyscallError},
};

pub fn create(entry: usize, stack_top: usize, arg: usize) -> Result<Handle, SyscallError> {
    check(raw::syscall(
        num::THREAD_CREATE,
        entry as u64,
        stack_top as u64,
        arg as u64,
        0,
        0,
        0,
    ))
    .map(|v| Handle(v as u32))
}

pub fn create_in(
    space: Handle,
    entry: usize,
    stack_top: usize,
    arg: usize,
    handles: &[u32],
) -> Result<Handle, SyscallError> {
    check(raw::syscall(
        num::THREAD_CREATE_IN,
        space.0 as u64,
        entry as u64,
        stack_top as u64,
        arg as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
    ))
    .map(|v| Handle(v as u32))
}

pub fn exit(code: u32) -> ! {
    loop {
        raw::syscall(num::THREAD_EXIT, code as u64, 0, 0, 0, 0, 0);
    }
}

pub fn set_priority(handle: Handle, priority: Priority) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::THREAD_SET_PRIORITY,
        handle.0 as u64,
        priority as u64,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}

pub fn set_affinity(handle: Handle, hint: u64) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::THREAD_SET_AFFINITY,
        handle.0 as u64,
        hint,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}
