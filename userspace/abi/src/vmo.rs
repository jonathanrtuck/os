//! VMO (Virtual Memory Object) syscall wrappers and RAII mapping.

use core::ops::{Deref, DerefMut};

use crate::{
    raw::{self, check, num},
    types::{Handle, Rights, SyscallError},
};

pub const FLAG_DMA: u64 = 1 << 2;

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

pub fn create_dma(size: usize, resource: Handle) -> Result<Handle, SyscallError> {
    check(raw::syscall(
        num::VMO_CREATE,
        size as u64,
        FLAG_DMA,
        resource.0 as u64,
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

/// RAII mapping of a VMO into the current address space.
///
/// Provides safe `&[u8]` / `&mut [u8]` access to mapped memory and
/// unmaps automatically on drop. Use [`map_region`] to create, or
/// [`Mapping::from_raw_parts`] for pre-mapped addresses.
pub struct Mapping {
    addr: usize,
    len: usize,
}

impl Mapping {
    /// Wrap an existing VMO mapping.
    ///
    /// # Safety
    /// `addr` must have been returned by [`map`] and the mapped region
    /// must cover at least `len` bytes. The caller transfers sole
    /// ownership of the mapping — no other code may unmap it.
    pub unsafe fn from_raw_parts(addr: usize, len: usize) -> Self {
        Self { addr, len }
    }

    pub fn addr(&self) -> usize {
        self.addr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Consume the mapping without unmapping. Returns the base address.
    pub fn leak(self) -> usize {
        let addr = self.addr;
        core::mem::forget(self);
        addr
    }
}

impl Deref for Mapping {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: addr..addr+len is valid mapped memory per constructor contract.
        unsafe { core::slice::from_raw_parts(self.addr as *const u8, self.len) }
    }
}

impl DerefMut for Mapping {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: addr..addr+len is valid mapped memory per constructor contract.
        unsafe { core::slice::from_raw_parts_mut(self.addr as *mut u8, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let _ = unmap(self.addr);
    }
}

/// Map a VMO into the current address space with RAII lifetime.
///
/// `size` must match the VMO's mapped extent. The returned [`Mapping`]
/// provides safe slice access and unmaps on drop.
pub fn map_region(handle: Handle, size: usize, perms: Rights) -> Result<Mapping, SyscallError> {
    let addr = map(handle, 0, perms)?;
    // SAFETY: the kernel just mapped `size` bytes at `addr`.
    Ok(unsafe { Mapping::from_raw_parts(addr, size) })
}
