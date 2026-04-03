//! Intrusive doubly-linked list and thread pool for O(1) scheduler operations.
//!
//! Threads stay in their pool slot for their entire lifetime (Box address never
//! moves). Lists link threads via `list_next`/`list_prev` fields embedded in the
//! Thread struct. All list operations are O(1).
//!
//! # Design
//!
//! The pool owns all `Box<Thread>` allocations via `ThreadSlots`. Lists are just
//! head/tail/len metadata — they don't own threads. A thread is in exactly one
//! list at a time (tracked by `ThreadLocation`).
//!
//! `ThreadSlots` (the slot array) lives as a separate field from `PoolMeta`
//! (free list + generations) in the scheduler's `State` struct. This lets Rust
//! split borrows: list operations can take `&mut ThreadSlots` independently from
//! borrows of other State fields (blocked, local_queues, etc.).
//!
//! Idle threads live outside the pool (in `PerCoreState::idle`).

use alloc::{boxed::Box, vec::Vec};

use super::thread::{Thread, ThreadId};

/// The slot array holding all thread `Box`es.
///
/// Separated from `PoolMeta` so the scheduler can borrow `slots` independently
/// from other State fields (lists, cores, etc.).
pub struct ThreadSlots {
    pub inner: Vec<Option<Box<Thread>>>,
}

impl ThreadSlots {
    pub const fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// Direct slot access (no generation check). Used when the slot index
    /// is known-valid (e.g., from a list or `current_slot`).
    pub fn get(&self, slot: u16) -> Option<&Thread> {
        self.inner.get(slot as usize)?.as_deref()
    }

    /// Mutable direct slot access.
    pub fn get_mut(&mut self, slot: u16) -> Option<&mut Thread> {
        self.inner.get_mut(slot as usize)?.as_deref_mut()
    }
}

/// Intrusive doubly-linked list of threads, linked by pool slot indices.
///
/// O(1) push_back, remove, pop_front, is_empty. O(n) iteration.
/// Does not own threads — the `ThreadSlots` owns the `Box<Thread>`s.
pub struct IntrusiveList {
    head: Option<u16>,
    tail: Option<u16>,
    len: u32,
}

impl IntrusiveList {
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
        }
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append a thread to the back of the list. O(1).
    pub fn push_back(&mut self, slot: u16, slots: &mut ThreadSlots) {
        let thread = slots.inner[slot as usize]
            .as_mut()
            .expect("push_back: empty slot");

        thread.list_next = None;
        thread.list_prev = self.tail;

        if let Some(old_tail) = self.tail {
            slots.inner[old_tail as usize]
                .as_mut()
                .expect("push_back: stale tail")
                .list_next = Some(slot);
        } else {
            self.head = Some(slot);
        }

        self.tail = Some(slot);
        self.len += 1;
    }

    /// Remove a thread from anywhere in the list. O(1).
    ///
    /// The caller must ensure `slot` is in this list (checked by ThreadLocation).
    pub fn remove(&mut self, slot: u16, slots: &mut ThreadSlots) {
        let thread = slots.inner[slot as usize]
            .as_ref()
            .expect("remove: empty slot");
        let prev = thread.list_prev;
        let next = thread.list_next;

        // Update predecessor.
        if let Some(p) = prev {
            slots.inner[p as usize]
                .as_mut()
                .expect("remove: stale prev")
                .list_next = next;
        } else {
            self.head = next;
        }

        // Update successor.
        if let Some(n) = next {
            slots.inner[n as usize]
                .as_mut()
                .expect("remove: stale next")
                .list_prev = prev;
        } else {
            self.tail = prev;
        }

        // Clear links on the removed thread.
        let thread = slots.inner[slot as usize]
            .as_mut()
            .expect("remove: empty slot");

        thread.list_next = None;
        thread.list_prev = None;
        self.len -= 1;
    }

    /// Remove and return the first thread in the list. O(1).
    pub fn pop_front(&mut self, slots: &mut ThreadSlots) -> Option<u16> {
        let head = self.head?;

        self.remove(head, slots);

        Some(head)
    }

    /// Iterate over slot indices in list order (head to tail).
    ///
    /// The returned iterator borrows `slots` immutably. Do not modify the list
    /// during iteration — collect slot indices first if you need to mutate.
    pub fn iter<'a>(&self, slots: &'a ThreadSlots) -> ListIter<'a> {
        ListIter {
            current: self.head,
            slots,
        }
    }
}

/// Iterator over slot indices in an IntrusiveList.
pub struct ListIter<'a> {
    current: Option<u16>,
    slots: &'a ThreadSlots,
}

impl Iterator for ListIter<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        let slot = self.current?;
        let thread = self.slots.inner[slot as usize]
            .as_ref()
            .expect("iter: empty slot");

        self.current = thread.list_next;

        Some(slot)
    }
}

/// Pool metadata: free list and generation counters.
///
/// The actual slot storage is in `ThreadSlots` (a sibling field in State).
/// This separation enables Rust's borrow checker to split borrows between
/// the slot array and pool metadata.
pub struct PoolMeta {
    /// Free slot indices available for allocation.
    free_slots: Vec<u16>,
    /// Generation counter per slot. Incremented on free. Prevents stale
    /// ThreadId from aliasing a reused slot.
    generations: Vec<u64>,
}

impl PoolMeta {
    pub const fn new() -> Self {
        Self {
            free_slots: Vec::new(),
            generations: Vec::new(),
        }
    }

    /// Pre-allocate pool to full capacity. Called once at boot.
    pub fn init(&mut self, slots: &mut ThreadSlots, capacity: usize) {
        slots.inner.reserve(capacity);
        self.free_slots.reserve(capacity);
        self.generations.resize(capacity, 0);

        // Pre-fill slots and free list so push() never allocates at runtime.
        for i in 0..capacity {
            slots.inner.push(None);
            // Push in reverse order so pop() yields low indices first.
            self.free_slots.push((capacity - 1 - i) as u16);
        }
    }

    /// Allocate a slot for a new thread. Returns the slot index and ThreadId,
    /// or None if the pool is full.
    ///
    /// Sets the thread's `pool_slot` and `id` fields.
    pub fn alloc(
        &mut self,
        slots: &mut ThreadSlots,
        mut thread: Box<Thread>,
    ) -> Option<(u16, ThreadId)> {
        let slot = self.free_slots.pop()?;
        let gen = self.generations[slot as usize];
        let id = ThreadId::new(slot, gen);

        thread.pool_slot = slot;
        thread.set_id(id);
        slots.inner[slot as usize] = Some(thread);

        Some((slot, id))
    }

    /// Free a slot, dropping the `Box<Thread>` (which frees the kernel stack).
    /// Increments the generation so stale ThreadIds won't match.
    ///
    /// Returns the dropped thread's process_id (for cleanup bookkeeping).
    pub fn free(
        &mut self,
        slots: &mut ThreadSlots,
        slot: u16,
    ) -> Option<super::process::ProcessId> {
        let thread = slots.inner[slot as usize].take()?;
        let pid = thread.process_id;

        // Increment generation AFTER taking the thread. The next alloc on this
        // slot will use generation+1, so old ThreadIds with the old generation
        // will fail the generation check in get/get_mut.
        self.generations[slot as usize] += 1;
        self.free_slots.push(slot);

        // Thread is dropped here — kernel stack is freed by Thread::drop.
        drop(thread);

        pid
    }

    /// Look up a thread by ThreadId. Returns None if the slot is empty or
    /// the generation doesn't match (stale ID).
    pub fn get<'a>(&self, slots: &'a ThreadSlots, id: ThreadId) -> Option<&'a Thread> {
        let slot = id.slot() as usize;

        if slot >= slots.inner.len() {
            return None;
        }

        let thread = slots.inner[slot].as_deref()?;

        if thread.id() != id {
            return None;
        }

        Some(thread)
    }

    /// Mutable lookup by ThreadId.
    pub fn get_mut<'a>(&self, slots: &'a mut ThreadSlots, id: ThreadId) -> Option<&'a mut Thread> {
        let slot = id.slot() as usize;

        if slot >= slots.inner.len() {
            return None;
        }

        let thread = slots.inner[slot].as_deref_mut()?;

        if thread.id() != id {
            return None;
        }

        Some(thread)
    }
}
