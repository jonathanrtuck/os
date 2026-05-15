//! Handle management syscall wrappers.

use crate::{
    raw::{self, check, num},
    types::{Handle, ObjectType, Rights, SyscallError},
};

pub fn dup(handle: Handle, rights: Rights) -> Result<Handle, SyscallError> {
    check(raw::syscall(
        num::HANDLE_DUP,
        handle.0 as u64,
        rights.0 as u64,
        0,
        0,
        0,
        0,
    ))
    .map(|v| Handle(v as u32))
}

pub fn close(handle: Handle) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::HANDLE_CLOSE,
        handle.0 as u64,
        0,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}

pub struct HandleInfo {
    pub object_type: ObjectType,
    pub rights: Rights,
}

pub fn info(handle: Handle) -> Result<HandleInfo, SyscallError> {
    let packed = check(raw::syscall(
        num::HANDLE_INFO,
        handle.0 as u64,
        0,
        0,
        0,
        0,
        0,
    ))?;
    let type_val = packed >> 32;
    let rights_val = (packed & 0xFFFF_FFFF) as u32;
    let object_type = match type_val {
        0 => ObjectType::Vmo,
        1 => ObjectType::Endpoint,
        2 => ObjectType::Event,
        3 => ObjectType::Thread,
        4 => ObjectType::AddressSpace,
        _ => return Err(SyscallError::InvalidArgument),
    };

    Ok(HandleInfo {
        object_type,
        rights: Rights(rights_val),
    })
}
