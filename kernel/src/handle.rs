//! Handle table — per-address-space capability table with free-list allocation.
//!
//! O(1) allocate (pop from free stack), O(1) close (push to free stack),
//! O(1) lookup (direct index). Generation-count revocation is tracked by
//! ObjectTable, not here.

use crate::{
    config,
    types::{HandleId, ObjectType, Rights, SyscallError},
};

const EMPTY: u32 = u32::MAX;

/// A handle is a capability: a reference to a kernel object with rights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    pub object_type: ObjectType,
    pub object_id: u32,
    pub rights: Rights,
    pub generation: u64,
    pub badge: u32,
}

/// Per-address-space handle table. Fixed-size array, O(1) lookup and alloc.
pub struct HandleTable {
    entries: [Option<Handle>; config::MAX_HANDLES],
    free_head: u32,
    free_next: [u32; config::MAX_HANDLES],
    count: usize,
}

#[allow(clippy::new_without_default)]
impl HandleTable {
    pub fn new() -> Self {
        let mut free_next = [EMPTY; config::MAX_HANDLES];
        for (i, slot) in free_next.iter_mut().enumerate().take(config::MAX_HANDLES.saturating_sub(1)) {
            *slot = (i + 1) as u32;
        }

        HandleTable {
            entries: core::array::from_fn(|_| None),
            free_head: if config::MAX_HANDLES > 0 { 0 } else { EMPTY },
            free_next,
            count: 0,
        }
    }

    /// Allocate a new handle. Returns OutOfMemory if the table is full.
    pub fn allocate(
        &mut self,
        object_type: ObjectType,
        object_id: u32,
        rights: Rights,
        generation: u64,
    ) -> Result<HandleId, SyscallError> {
        self.allocate_with_badge(object_type, object_id, rights, generation, 0)
    }

    pub fn allocate_with_badge(
        &mut self,
        object_type: ObjectType,
        object_id: u32,
        rights: Rights,
        generation: u64,
        badge: u32,
    ) -> Result<HandleId, SyscallError> {
        if self.free_head == EMPTY {
            return Err(SyscallError::OutOfMemory);
        }

        let idx = self.free_head as usize;
        self.free_head = self.free_next[idx];
        self.free_next[idx] = EMPTY;
        self.entries[idx] = Some(Handle {
            object_type,
            object_id,
            rights,
            generation,
            badge,
        });
        self.count += 1;

        Ok(HandleId(idx as u32))
    }

    /// Allocate a handle at a specific index (for bootstrap initial_handles).
    pub fn allocate_at(&mut self, index: usize, handle: Handle) -> Result<HandleId, SyscallError> {
        if index >= config::MAX_HANDLES {
            return Err(SyscallError::InvalidArgument);
        }
        if self.entries[index].is_some() {
            return Err(SyscallError::InvalidArgument);
        }

        self.unlink_from_free_list(index);
        self.entries[index] = Some(handle);
        self.count += 1;

        Ok(HandleId(index as u32))
    }

    fn unlink_from_free_list(&mut self, target: usize) {
        let target_u32 = target as u32;

        if self.free_head == target_u32 {
            self.free_head = self.free_next[target];
            self.free_next[target] = EMPTY;
            return;
        }

        let mut cur = self.free_head;
        while cur != EMPTY {
            let cur_idx = cur as usize;
            if self.free_next[cur_idx] == target_u32 {
                self.free_next[cur_idx] = self.free_next[target];
                self.free_next[target] = EMPTY;
                return;
            }
            cur = self.free_next[cur_idx];
        }
    }

    /// Look up a handle. Checks existence only (generation checked by caller
    /// against the object's current generation).
    pub fn lookup(&self, id: HandleId) -> Result<&Handle, SyscallError> {
        let idx = id.as_usize();

        if idx >= config::MAX_HANDLES {
            return Err(SyscallError::InvalidHandle);
        }

        self.entries[idx]
            .as_ref()
            .ok_or(SyscallError::InvalidHandle)
    }

    /// Duplicate a handle with reduced rights. The new handle must have a
    /// subset of the original's rights.
    pub fn duplicate(
        &mut self,
        id: HandleId,
        new_rights: Rights,
    ) -> Result<HandleId, SyscallError> {
        let original = self.lookup(id)?.clone();

        if !new_rights.is_subset_of(original.rights) {
            return Err(SyscallError::InsufficientRights);
        }

        self.allocate_with_badge(
            original.object_type,
            original.object_id,
            new_rights,
            original.generation,
            original.badge,
        )
    }

    /// Close a handle, freeing the slot.
    pub fn close(&mut self, id: HandleId) -> Result<Handle, SyscallError> {
        let idx = id.as_usize();

        if idx >= config::MAX_HANDLES {
            return Err(SyscallError::InvalidHandle);
        }

        let handle = self.entries[idx]
            .take()
            .ok_or(SyscallError::InvalidHandle)?;

        self.free_next[idx] = self.free_head;
        self.free_head = idx as u32;
        self.count -= 1;

        Ok(handle)
    }

    /// Get handle info (type, rights) without cloning the full handle.
    pub fn info(&self, id: HandleId) -> Result<(ObjectType, Rights), SyscallError> {
        let h = self.lookup(id)?;

        Ok((h.object_type, h.rights))
    }

    /// Remove a handle and return the full Handle struct (for IPC transfer).
    pub fn remove(&mut self, id: HandleId) -> Result<Handle, SyscallError> {
        self.close(id)
    }

    /// Install a Handle struct at the next free slot (for IPC receive).
    pub fn install(&mut self, handle: Handle) -> Result<HandleId, SyscallError> {
        self.allocate_with_badge(
            handle.object_type,
            handle.object_id,
            handle.rights,
            handle.generation,
            handle.badge,
        )
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_table() -> HandleTable {
        HandleTable::new()
    }

    #[test]
    fn allocate_and_lookup() {
        let mut t = make_table();
        let id = t.allocate(ObjectType::Vmo, 42, Rights::ALL, 0).unwrap();
        let h = t.lookup(id).unwrap();

        assert_eq!(h.object_type, ObjectType::Vmo);
        assert_eq!(h.object_id, 42);
    }

    #[test]
    fn close_frees_slot() {
        let mut t = make_table();
        let id = t.allocate(ObjectType::Vmo, 0, Rights::ALL, 0).unwrap();

        assert_eq!(t.count(), 1);

        t.close(id).unwrap();

        assert_eq!(t.count(), 0);
        assert_eq!(t.lookup(id), Err(SyscallError::InvalidHandle));
    }

    #[test]
    fn duplicate_with_reduced_rights() {
        let mut t = make_table();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let id = t.allocate(ObjectType::Vmo, 0, rw, 0).unwrap();
        let dup = t.duplicate(id, Rights::READ).unwrap();
        let h = t.lookup(dup).unwrap();

        assert!(h.rights.contains(Rights::READ));
        assert!(!h.rights.contains(Rights::WRITE));
    }

    #[test]
    fn duplicate_cannot_escalate_rights() {
        let mut t = make_table();
        let id = t.allocate(ObjectType::Vmo, 0, Rights::READ, 0).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);

        assert_eq!(t.duplicate(id, rw), Err(SyscallError::InsufficientRights));
    }

    #[test]
    fn exhaustion() {
        let mut t = make_table();

        for i in 0..config::MAX_HANDLES {
            t.allocate(ObjectType::Vmo, i as u32, Rights::ALL, 0)
                .unwrap();
        }

        assert_eq!(
            t.allocate(ObjectType::Vmo, 0, Rights::ALL, 0),
            Err(SyscallError::OutOfMemory),
        );

        // Free one, reallocate.
        t.close(HandleId(0)).unwrap();

        assert!(t.allocate(ObjectType::Vmo, 0, Rights::ALL, 0).is_ok());
    }

    #[test]
    fn invalid_handle_id() {
        let t = make_table();

        assert_eq!(t.lookup(HandleId(999)), Err(SyscallError::InvalidHandle));
    }

    #[test]
    fn remove_and_install() {
        let mut t = make_table();
        let id = t.allocate(ObjectType::Event, 7, Rights::SIGNAL, 0).unwrap();
        let handle = t.remove(id).unwrap();

        assert_eq!(handle.object_type, ObjectType::Event);
        assert_eq!(handle.object_id, 7);
        assert_eq!(t.lookup(id), Err(SyscallError::InvalidHandle));

        let new_id = t.install(handle).unwrap();
        let h = t.lookup(new_id).unwrap();

        assert_eq!(h.object_type, ObjectType::Event);
    }

    #[test]
    fn info() {
        let mut t = make_table();
        let id = t.allocate(ObjectType::Thread, 3, Rights::READ, 5).unwrap();
        let (typ, rights) = t.info(id).unwrap();

        assert_eq!(typ, ObjectType::Thread);
        assert_eq!(rights, Rights::READ);
    }

    #[test]
    fn allocate_at_specific_index() {
        let mut t = make_table();
        let h = Handle {
            object_type: ObjectType::Vmo,
            object_id: 10,
            rights: Rights::ALL,
            generation: 0,
            badge: 0,
        };

        let id = t.allocate_at(3, h).unwrap();

        assert_eq!(id, HandleId(3));
        assert_eq!(t.lookup(HandleId(3)).unwrap().object_id, 10);

        let id2 = t.allocate(ObjectType::Event, 20, Rights::ALL, 0).unwrap();

        assert_ne!(id2, HandleId(3));
    }
}
