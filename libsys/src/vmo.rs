//! VMO (Virtual Memory Object) syscall wrappers.

use crate::{
    raw::{self, check, num},
    types::{Handle, Rights, SyscallError},
};

pub fn create(size: usize, flags: u64) -> Result<Handle, SyscallError> {
    check(raw::syscall(
        num::VMO_CREATE,
        size as u64,
        flags,
        0,
        0,
        0,
        0,
    ))
    .map(|v| Handle(v as u32))
}

pub fn map(handle: Handle, addr_hint: usize, perms: Rights) -> Result<usize, SyscallError> {
    check(raw::syscall(
        num::VMO_MAP,
        handle.0 as u64,
        addr_hint as u64,
        perms.0 as u64,
        0,
        0,
        0,
    ))
    .map(|v| v as usize)
}

pub fn map_into(
    vmo: Handle,
    space: Handle,
    addr: usize,
    perms: Rights,
) -> Result<usize, SyscallError> {
    check(raw::syscall(
        num::VMO_MAP_INTO,
        vmo.0 as u64,
        space.0 as u64,
        addr as u64,
        perms.0 as u64,
        0,
        0,
    ))
    .map(|v| v as usize)
}

pub fn unmap(addr: usize) -> Result<(), SyscallError> {
    check(raw::syscall(num::VMO_UNMAP, addr as u64, 0, 0, 0, 0, 0)).map(|_| ())
}

pub fn snapshot(handle: Handle) -> Result<Handle, SyscallError> {
    check(raw::syscall(
        num::VMO_SNAPSHOT,
        handle.0 as u64,
        0,
        0,
        0,
        0,
        0,
    ))
    .map(|v| Handle(v as u32))
}

pub fn seal(handle: Handle) -> Result<(), SyscallError> {
    check(raw::syscall(num::VMO_SEAL, handle.0 as u64, 0, 0, 0, 0, 0)).map(|_| ())
}

pub fn resize(handle: Handle, new_size: usize) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::VMO_RESIZE,
        handle.0 as u64,
        new_size as u64,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}

pub fn set_pager(vmo: Handle, endpoint: Handle) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::VMO_SET_PAGER,
        vmo.0 as u64,
        endpoint.0 as u64,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}
