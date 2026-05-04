//! Handle table — per-address-space capability table with lock-free allocation.
//!
//! O(1) allocate (CAS pop from atomic free stack), O(1) close (CAS push),
//! O(1) lookup (direct index). Generation-count revocation is tracked by
//! ObjectTable, not here. Single-threaded CAS degenerates to unconditional
//! success.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

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
    free_head: AtomicU32,
    free_next: [AtomicU32; config::MAX_HANDLES],
    count: AtomicUsize,
}

#[allow(clippy::new_without_default)]
impl HandleTable {
    pub fn new() -> Self {
        HandleTable {
            entries: core::array::from_fn(|_| None),
            free_head: AtomicU32::new(if config::MAX_HANDLES > 0 { 0 } else { EMPTY }),
            free_next: core::array::from_fn(|i| {
                AtomicU32::new(if i + 1 < config::MAX_HANDLES {
                    (i + 1) as u32
                } else {
                    EMPTY
                })
            }),
            count: AtomicUsize::new(0),
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
        loop {
            let head = self.free_head.load(Ordering::Acquire);

            if head == EMPTY {
                return Err(SyscallError::OutOfMemory);
            }

            let next = self.free_next[head as usize].load(Ordering::Relaxed);

            if self
                .free_head
                .compare_exchange_weak(head, next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.free_next[head as usize].store(EMPTY, Ordering::Relaxed);
                self.entries[head as usize] = Some(Handle {
                    object_type,
                    object_id,
                    rights,
                    generation,
                    badge,
                });
                self.count.fetch_add(1, Ordering::Relaxed);

                return Ok(HandleId(head));
            }
        }
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
        self.count.fetch_add(1, Ordering::Relaxed);

        Ok(HandleId(index as u32))
    }

    fn unlink_from_free_list(&mut self, target: usize) {
        let target_u32 = target as u32;
        let head = self.free_head.load(Ordering::Relaxed);

        if head == target_u32 {
            let next = self.free_next[target].load(Ordering::Relaxed);

            self.free_head.store(next, Ordering::Relaxed);
            self.free_next[target].store(EMPTY, Ordering::Relaxed);

            return;
        }

        let mut cur = head;

        while cur != EMPTY {
            let cur_idx = cur as usize;
            let next = self.free_next[cur_idx].load(Ordering::Relaxed);

            if next == target_u32 {
                let target_next = self.free_next[target].load(Ordering::Relaxed);

                self.free_next[cur_idx].store(target_next, Ordering::Relaxed);
                self.free_next[target].store(EMPTY, Ordering::Relaxed);

                return;
            }

            cur = next;
        }
    }

    pub fn lookup(&self, id: HandleId) -> Result<&Handle, SyscallError> {
        let idx = id.as_usize();

        if idx >= config::MAX_HANDLES {
            return Err(SyscallError::InvalidHandle);
        }

        self.entries[idx]
            .as_ref()
            .ok_or(SyscallError::InvalidHandle)
    }

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

    pub fn close(&mut self, id: HandleId) -> Result<Handle, SyscallError> {
        let idx = id.as_usize();

        if idx >= config::MAX_HANDLES {
            return Err(SyscallError::InvalidHandle);
        }

        let handle = self.entries[idx]
            .take()
            .ok_or(SyscallError::InvalidHandle)?;

        loop {
            let head = self.free_head.load(Ordering::Relaxed);

            self.free_next[idx].store(head, Ordering::Relaxed);

            if self
                .free_head
                .compare_exchange_weak(head, idx as u32, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        self.count.fetch_sub(1, Ordering::Relaxed);

        Ok(handle)
    }

    pub fn info(&self, id: HandleId) -> Result<(ObjectType, Rights), SyscallError> {
        let h = self.lookup(id)?;

        Ok((h.object_type, h.rights))
    }

    pub fn remove(&mut self, id: HandleId) -> Result<Handle, SyscallError> {
        self.close(id)
    }

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
        self.count.load(Ordering::Relaxed)
    }

    pub fn iter_handles(&self) -> impl Iterator<Item = (HandleId, &Handle)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|h| (HandleId(i as u32), h)))
    }

    pub fn free_slot_count(&self) -> usize {
        config::MAX_HANDLES - self.count()
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
