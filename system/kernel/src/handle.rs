//! Per-process handle table.
//!
//! Each user process owns a handle table — a fixed-size array of slots.
//! A handle is an integer index into this table. Each slot holds a reference
//! to a kernel object (channel endpoint, future: document mapping) plus a
//! rights bitfield (read, write). The kernel validates handles and rights
//! on every operation.

use super::interrupt::InterruptId;
use super::process::ProcessId;
use super::scheduling_context::SchedulingContextId;
use super::thread::ThreadId;
use super::timer::TimerId;

const MAX_HANDLES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelId(pub u32);
pub struct DrainHandles<'a> {
    table: &'a mut HandleTable,
    index: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(pub u8);
pub struct HandleTable {
    entries: [Option<HandleEntry>; MAX_HANDLES],
}
#[derive(Clone, Copy)]
pub struct Rights(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleObject {
    Channel(ChannelId),
    Interrupt(InterruptId),
    Process(ProcessId),
    SchedulingContext(SchedulingContextId),
    Thread(ThreadId),
    Timer(TimerId),
}
#[derive(Clone, Copy)]
struct HandleEntry {
    object: HandleObject,
    rights: Rights,
}
#[repr(i64)]
#[derive(Debug)]
pub enum HandleError {
    InvalidHandle = -10,
    InsufficientRights = -12,
    TableFull = -13,
}

impl HandleTable {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_HANDLES],
        }
    }
    /// Close a handle (clear the slot). Returns the object that was there.
    pub fn close(&mut self, handle: Handle) -> Result<HandleObject, HandleError> {
        let slot = &mut self.entries[handle.0 as usize];
        let entry = slot.ok_or(HandleError::InvalidHandle)?;
        let object = entry.object;

        *slot = None;

        Ok(object)
    }
    /// Iterate over all occupied handles (for cleanup on process exit).
    pub fn drain(&mut self) -> DrainHandles<'_> {
        DrainHandles {
            table: self,
            index: 0,
        }
    }
    /// Look up a handle and verify it has the required rights.
    pub fn get(&self, handle: Handle, required: Rights) -> Result<HandleObject, HandleError> {
        let entry = self.entries[handle.0 as usize]
            .as_ref()
            .ok_or(HandleError::InvalidHandle)?;

        if !entry.rights.contains(required) {
            return Err(HandleError::InsufficientRights);
        }

        Ok(entry.object)
    }
    /// Look up a handle, verify rights, and return both the object and its rights.
    pub fn get_entry(
        &self,
        handle: Handle,
        required: Rights,
    ) -> Result<(HandleObject, Rights), HandleError> {
        let entry = self.entries[handle.0 as usize]
            .as_ref()
            .ok_or(HandleError::InvalidHandle)?;

        if !entry.rights.contains(required) {
            return Err(HandleError::InsufficientRights);
        }

        Ok((entry.object, entry.rights))
    }
    /// Insert a new handle. Returns the handle index, or TableFull.
    pub fn insert(&mut self, object: HandleObject, rights: Rights) -> Result<Handle, HandleError> {
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(HandleEntry { object, rights });

                return Ok(Handle(i as u8));
            }
        }

        Err(HandleError::TableFull)
    }
}
impl Iterator for DrainHandles<'_> {
    type Item = (Handle, HandleObject);

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < MAX_HANDLES {
            let i = self.index;

            self.index += 1;

            if let Some(entry) = self.table.entries[i].take() {
                return Some((Handle(i as u8), entry.object));
            }
        }

        None
    }
}
impl Rights {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const READ_WRITE: Self = Self((1 << 0) | (1 << 1));

    pub fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }
}
