//! RAII typed handles — automatic close on drop, type-safe handle passing.

use core::marker::PhantomData;

use abi::types::{Handle, Rights, SyscallError};

pub struct EndpointTag;
pub struct EventTag;
pub struct VmoTag;
pub struct ThreadTag;
pub struct SpaceTag;

pub struct OwnedHandle<T> {
    raw: u32,
    _marker: PhantomData<T>,
}

pub type Endpoint = OwnedHandle<EndpointTag>;
pub type Event = OwnedHandle<EventTag>;
pub type Vmo = OwnedHandle<VmoTag>;
pub type Thread = OwnedHandle<ThreadTag>;
pub type Space = OwnedHandle<SpaceTag>;

impl<T> OwnedHandle<T> {
    /// Take ownership of a raw handle value.
    ///
    /// # Safety
    /// The raw value must be a valid kernel handle of the correct type.
    /// The caller transfers sole ownership — no other code may close
    /// this handle.
    pub unsafe fn from_raw(raw: u32) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }

    pub fn raw(&self) -> Handle {
        Handle(self.raw)
    }

    /// Surrender ownership, returning the raw handle value for IPC transfer.
    /// The handle will NOT be closed on drop.
    pub fn into_raw(self) -> u32 {
        let raw = self.raw;

        core::mem::forget(self);

        raw
    }

    pub fn dup(&self, rights: Rights) -> Result<Self, SyscallError> {
        let h = abi::handle::dup(Handle(self.raw), rights)?;

        Ok(Self {
            raw: h.0,
            _marker: PhantomData,
        })
    }
}

impl<T> Drop for OwnedHandle<T> {
    fn drop(&mut self) {
        let _ = abi::handle::close(Handle(self.raw));
    }
}

impl Endpoint {
    pub fn create() -> Result<Self, SyscallError> {
        let h = abi::ipc::endpoint_create()?;

        Ok(unsafe { Self::from_raw(h.0) })
    }

    pub fn bind_event(&self, event: &Event) -> Result<(), SyscallError> {
        abi::ipc::endpoint_bind_event(Handle(self.raw), Handle(event.raw))
    }
}

impl Event {
    pub fn create() -> Result<Self, SyscallError> {
        let h = abi::event::create()?;

        Ok(unsafe { Self::from_raw(h.0) })
    }

    pub fn signal(&self, bits: u64) -> Result<(), SyscallError> {
        abi::event::signal(Handle(self.raw), bits)
    }

    pub fn clear(&self, bits: u64) -> Result<(), SyscallError> {
        abi::event::clear(Handle(self.raw), bits)
    }
}

impl Vmo {
    pub fn create(size: usize) -> Result<Self, SyscallError> {
        let h = abi::vmo::create(size, 0)?;

        Ok(unsafe { Self::from_raw(h.0) })
    }

    pub fn map(&self, addr_hint: usize, perms: Rights) -> Result<usize, SyscallError> {
        abi::vmo::map(Handle(self.raw), addr_hint, perms)
    }

    pub fn seal(&self) -> Result<(), SyscallError> {
        abi::vmo::seal(Handle(self.raw))
    }

    pub fn snapshot(&self) -> Result<Self, SyscallError> {
        let h = abi::vmo::snapshot(Handle(self.raw))?;

        Ok(unsafe { Self::from_raw(h.0) })
    }

    pub fn resize(&self, new_size: usize) -> Result<(), SyscallError> {
        abi::vmo::resize(Handle(self.raw), new_size)
    }
}
