// AUDIT: 2026-04-01 — 0 unsafe blocks (pure safe Rust). Two-level table.
// Handle lifecycle verified: create returns first free slot (base then overflow),
// close clears slot and returns (object, rights), use-after-close returns
// InvalidHandle, double-close returns InvalidHandle. Table growth: base 256 inline
// + overflow pages on demand (256 entries each, heap-allocated). Max 4096 handles.
// Concurrent access: table is per-process, accessed only under scheduler lock.
// insert_at for rollback semantics verified correct (SlotOccupied on conflict).
// drain iterator correctly yields all occupied slots and clears table.

//! Per-process handle table (two-level).
//!
//! Each user process owns a handle table. A handle is a u16 index into this
//! table. Each slot holds a reference to a kernel object plus a rights
//! bitfield. The kernel validates handles and rights on every operation.
//!
//! The table has two levels:
//! - **Base** (inline): 256 slots, no allocation. Handles 0-255.
//! - **Overflow** (heap): additional 256-entry pages allocated on demand.
//!
//! This gives O(1) lookup for common cases (handles 0-255) and bounded
//! growth for compound documents that need many channels.

#[cfg(any(test, not(test)))]
extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

use super::{
    event::EventId, interrupt::InterruptId, process::ProcessId,
    scheduling_context::SchedulingContextId, thread::ThreadId, timer::TimerId, vmo::VmoId,
};

const BASE_SIZE: usize = super::paging::HANDLE_TABLE_BASE_SIZE as usize;
const OVERFLOW_PAGE_SIZE: usize = 256;

pub const MAX_HANDLES: usize = super::paging::MAX_HANDLES as usize;

type OverflowPage = Box<[Option<HandleEntry>; OVERFLOW_PAGE_SIZE]>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleObject {
    Channel(ChannelId),
    Event(EventId),
    Interrupt(InterruptId),
    Process(ProcessId),
    SchedulingContext(SchedulingContextId),
    Thread(ThreadId),
    Timer(TimerId),
    Vmo(VmoId),
}

#[derive(Clone, Copy)]
struct HandleEntry {
    object: HandleObject,
    rights: Rights,
    badge: u64,
}
#[repr(i64)]
#[derive(Debug)]
pub enum HandleError {
    InvalidHandle = -10,
    InsufficientRights = -12,
    TableFull = -13,
    /// Returned by `insert_at` when the specific slot is already occupied.
    /// Semantically distinct from `TableFull` (no free slots anywhere).
    SlotOccupied = -14,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelId(pub u32);
pub struct DrainHandles<'a> {
    table: &'a mut HandleTable,
    index: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(pub u16);
pub struct HandleTable {
    base: [Option<HandleEntry>; BASE_SIZE],
    overflow: Vec<OverflowPage>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(u32);

impl HandleTable {
    pub const fn new() -> Self {
        Self {
            base: [None; BASE_SIZE],
            overflow: Vec::new(),
        }
    }

    /// Total capacity: base + all allocated overflow pages.
    fn capacity(&self) -> usize {
        BASE_SIZE + self.overflow.len() * OVERFLOW_PAGE_SIZE
    }
    /// Get a reference to the slot at `index`, or None if beyond allocated range.
    fn slot(&self, index: usize) -> Option<&Option<HandleEntry>> {
        if index < BASE_SIZE {
            Some(&self.base[index])
        } else {
            let overflow_idx = index - BASE_SIZE;
            let page = overflow_idx / OVERFLOW_PAGE_SIZE;
            let offset = overflow_idx % OVERFLOW_PAGE_SIZE;

            self.overflow.get(page).map(|p| &p[offset])
        }
    }
    /// Get a mutable reference to the slot at `index`, or None if beyond allocated range.
    fn slot_mut(&mut self, index: usize) -> Option<&mut Option<HandleEntry>> {
        if index < BASE_SIZE {
            Some(&mut self.base[index])
        } else {
            let overflow_idx = index - BASE_SIZE;
            let page = overflow_idx / OVERFLOW_PAGE_SIZE;
            let offset = overflow_idx % OVERFLOW_PAGE_SIZE;

            self.overflow.get_mut(page).map(|p| &mut p[offset])
        }
    }

    /// Close a handle (clear the slot). Returns the object, rights, and badge.
    pub fn close(&mut self, handle: Handle) -> Result<(HandleObject, Rights, u64), HandleError> {
        let slot = self
            .slot_mut(handle.0 as usize)
            .ok_or(HandleError::InvalidHandle)?;
        let entry = slot.ok_or(HandleError::InvalidHandle)?;
        let result = (entry.object, entry.rights, entry.badge);

        *slot = None;

        Ok(result)
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
        let entry = self
            .slot(handle.0 as usize)
            .and_then(|s| s.as_ref())
            .ok_or(HandleError::InvalidHandle)?;

        if !entry.rights.contains(required) {
            return Err(HandleError::InsufficientRights);
        }

        Ok(entry.object)
    }
    /// Read the badge on a handle.
    pub fn get_badge(&self, handle: Handle) -> Result<u64, HandleError> {
        let entry = self
            .slot(handle.0 as usize)
            .and_then(|s| s.as_ref())
            .ok_or(HandleError::InvalidHandle)?;

        Ok(entry.badge)
    }
    /// Look up a handle, verify rights, and return both the object and its rights.
    #[allow(dead_code)] // used by test crate and handle_send
    pub fn get_entry(
        &self,
        handle: Handle,
        required: Rights,
    ) -> Result<(HandleObject, Rights), HandleError> {
        let entry = self
            .slot(handle.0 as usize)
            .and_then(|s| s.as_ref())
            .ok_or(HandleError::InvalidHandle)?;

        if !entry.rights.contains(required) {
            return Err(HandleError::InsufficientRights);
        }

        Ok((entry.object, entry.rights))
    }
    /// Insert a new handle. Returns the handle index, or TableFull.
    pub fn insert(&mut self, object: HandleObject, rights: Rights) -> Result<Handle, HandleError> {
        // Scan base for free slot.
        for (i, slot) in self.base.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(HandleEntry {
                    object,
                    rights,
                    badge: 0,
                });

                return Ok(Handle(i as u16));
            }
        }

        // Scan existing overflow pages.
        for (page_idx, page) in self.overflow.iter_mut().enumerate() {
            for (offset, slot) in page.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = Some(HandleEntry {
                        object,
                        rights,
                        badge: 0,
                    });

                    let index = BASE_SIZE + page_idx * OVERFLOW_PAGE_SIZE + offset;

                    return Ok(Handle(index as u16));
                }
            }
        }

        // All existing pages full — allocate a new overflow page if within limit.
        let new_capacity = self.capacity() + OVERFLOW_PAGE_SIZE;

        if new_capacity > MAX_HANDLES {
            return Err(HandleError::TableFull);
        }

        let mut page = Box::new([None; OVERFLOW_PAGE_SIZE]);

        page[0] = Some(HandleEntry {
            object,
            rights,
            badge: 0,
        });

        let index = BASE_SIZE + self.overflow.len() * OVERFLOW_PAGE_SIZE;

        self.overflow.push(page);

        Ok(Handle(index as u16))
    }
    /// Insert at a specific slot. The slot must be empty. Used for rollback
    /// when a handle move fails and the handle must be restored to its
    /// original position.
    pub fn insert_at(
        &mut self,
        handle: Handle,
        object: HandleObject,
        rights: Rights,
        badge: u64,
    ) -> Result<(), HandleError> {
        let index = handle.0 as usize;

        // Ensure overflow pages exist up to the required index.
        if index >= BASE_SIZE {
            let required_page = (index - BASE_SIZE) / OVERFLOW_PAGE_SIZE;

            while self.overflow.len() <= required_page {
                if self.capacity() + OVERFLOW_PAGE_SIZE > MAX_HANDLES {
                    return Err(HandleError::TableFull);
                }

                self.overflow.push(Box::new([None; OVERFLOW_PAGE_SIZE]));
            }
        }

        let slot = self.slot_mut(index).ok_or(HandleError::InvalidHandle)?;

        if slot.is_some() {
            return Err(HandleError::SlotOccupied);
        }

        *slot = Some(HandleEntry {
            object,
            rights,
            badge,
        });

        Ok(())
    }
    /// Insert with an explicit badge (used when transferring a badged handle).
    pub fn insert_with_badge(
        &mut self,
        object: HandleObject,
        rights: Rights,
        badge: u64,
    ) -> Result<Handle, HandleError> {
        let handle = self.insert(object, rights)?;

        // Overwrite the badge (insert sets it to 0).
        if let Some(slot) = self.slot_mut(handle.0 as usize) {
            if let Some(entry) = slot.as_mut() {
                entry.badge = badge;
            }
        }

        Ok(handle)
    }
    /// Set the badge on a handle.
    pub fn set_badge(&mut self, handle: Handle, badge: u64) -> Result<(), HandleError> {
        let slot = self
            .slot_mut(handle.0 as usize)
            .ok_or(HandleError::InvalidHandle)?;
        let entry = slot.as_mut().ok_or(HandleError::InvalidHandle)?;

        entry.badge = badge;

        Ok(())
    }
}

impl Iterator for DrainHandles<'_> {
    type Item = (HandleObject, Rights, u64);

    fn next(&mut self) -> Option<Self::Item> {
        let total = self.table.capacity();

        while self.index < total {
            let i = self.index;

            self.index += 1;

            if let Some(slot) = self.table.slot_mut(i) {
                if let Some(entry) = slot.take() {
                    return Some((entry.object, entry.rights, entry.badge));
                }
            }
        }

        None
    }
}

impl Rights {
    pub const ALL: Self = Self(0x3FF);
    pub const APPEND: Self = Self(1 << 8);
    pub const CREATE: Self = Self(1 << 6);
    pub const KILL: Self = Self(1 << 7);
    pub const MAP: Self = Self(1 << 4);
    pub const NONE: Self = Self(0);
    pub const READ_WRITE: Self = Self((1 << 0) | (1 << 1));
    pub const READ: Self = Self(1 << 0);
    pub const SEAL: Self = Self(1 << 9);
    pub const SIGNAL: Self = Self(1 << 2);
    pub const TRANSFER: Self = Self(1 << 5);
    pub const WAIT: Self = Self(1 << 3);
    pub const WRITE: Self = Self(1 << 1);

    /// Reduce rights: result has only the bits present in both self and mask.
    /// This is the core capability attenuation operation — rights can only be
    /// removed, never added.
    pub const fn attenuate(self, mask: Self) -> Self {
        Self(self.0 & mask.0)
    }
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }
    /// Construct from a raw u32 (e.g., from a syscall argument). Masks to
    /// defined bits only — undefined bits are silently dropped.
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw & 0x3FF)
    }
    /// Combine two rights sets (bitwise OR).
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}
