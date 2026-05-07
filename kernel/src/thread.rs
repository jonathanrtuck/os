//! Thread and Scheduler — execution contexts and fixed-priority scheduling.
//!
//! Threads are schedulable execution contexts independent of address spaces.
//! The scheduler is fixed-priority preemptive with round-robin within the
//! same priority tier. Idle threads (Priority::Idle) are the fallback when
//! all user-priority queues are empty.

#[cfg(any(target_os = "none", test))]
use alloc::boxed::Box;
use alloc::vec::Vec;

#[cfg(any(target_os = "none", test))]
use crate::frame::arch::register_state::RegisterState;
use crate::types::{AddressSpaceId, EventId, Priority, SyscallError, ThreadId, TopologyHint};

/// Bitmap words needed for 256 priority levels.
const BITMAP_WORDS: usize = Priority::NUM_LEVELS / 64;

/// Thread execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRunState {
    Ready,
    Running,
    Blocked,
    Exited,
}

/// Saved RECV buffer state for IPC direct transfer.
///
/// When a server blocks in RECV, it saves its buffer addresses here.
/// If a client calls CALL while this server is waiting, CALL reads
/// these addresses to deliver the message directly — skipping the
/// endpoint's priority send queue entirely.
#[derive(Debug, Clone, Copy)]
pub struct RecvState {
    pub endpoint_id: u32,
    pub space_id: AddressSpaceId,
    pub out_buf: usize,
    pub out_cap: usize,
    pub handles_out: usize,
    pub handles_cap: usize,
    pub reply_cap_out: usize,
}

/// A thread — schedulable execution context.
pub struct Thread {
    pub id: ThreadId,
    state: ThreadRunState,
    address_space: Option<AddressSpaceId>,
    priority: Priority,
    effective_priority: Priority,
    affinity: TopologyHint,
    exit_event: Option<EventId>,
    entry_point: usize,
    stack_top: usize,
    arg: usize,
    kernel_stack_base: usize,
    kernel_sp: usize,
    exit_code: Option<u32>,
    fp_dirty: bool,
    wait_events: [u32; crate::config::MAX_MULTI_WAIT],
    wait_count: u8,
    wakeup_error: Option<SyscallError>,
    wakeup_value: Option<u64>,
    recv_state: Option<RecvState>,
    space_next: Option<u32>,
    space_prev: Option<u32>,
    #[cfg(any(target_os = "none", test))]
    register_state: Option<Box<RegisterState>>,
}

const _: () = {
    assert!(core::mem::size_of::<Thread>() <= 576);
};

#[allow(clippy::new_without_default)]
impl Thread {
    pub fn new(
        id: ThreadId,
        address_space: Option<AddressSpaceId>,
        priority: Priority,
        entry_point: usize,
        stack_top: usize,
        arg: usize,
    ) -> Self {
        Thread {
            id,
            state: ThreadRunState::Ready,
            address_space,
            priority,
            effective_priority: priority,
            affinity: TopologyHint::Any,
            exit_event: None,
            entry_point,
            stack_top,
            arg,
            kernel_stack_base: 0,
            kernel_sp: 0,
            exit_code: None,
            fp_dirty: false,
            wait_events: [0; crate::config::MAX_MULTI_WAIT],
            wait_count: 0,
            wakeup_error: None,
            wakeup_value: None,
            recv_state: None,
            space_next: None,
            space_prev: None,
            #[cfg(any(target_os = "none", test))]
            register_state: None,
        }
    }

    pub fn state(&self) -> ThreadRunState {
        self.state
    }

    pub fn address_space(&self) -> Option<AddressSpaceId> {
        self.address_space
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    pub fn effective_priority(&self) -> Priority {
        self.effective_priority
    }

    pub fn affinity(&self) -> TopologyHint {
        self.affinity
    }

    pub fn exit_event(&self) -> Option<EventId> {
        self.exit_event
    }

    pub fn space_next(&self) -> Option<u32> {
        self.space_next
    }

    pub fn set_space_next(&mut self, next: Option<u32>) {
        self.space_next = next;
    }

    pub fn space_prev(&self) -> Option<u32> {
        self.space_prev
    }

    pub fn set_space_prev(&mut self, prev: Option<u32>) {
        self.space_prev = prev;
    }

    pub fn entry_point(&self) -> usize {
        self.entry_point
    }

    pub fn stack_top(&self) -> usize {
        self.stack_top
    }

    pub fn arg(&self) -> usize {
        self.arg
    }

    pub fn kernel_stack_base(&self) -> usize {
        self.kernel_stack_base
    }

    pub fn kernel_sp(&self) -> usize {
        self.kernel_sp
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }

    pub fn is_idle(&self) -> bool {
        self.priority == Priority::Idle
    }

    pub fn set_state(&mut self, new: ThreadRunState) {
        debug_assert!(
            matches!(
                (self.state, new),
                (ThreadRunState::Ready, ThreadRunState::Running)
                    | (ThreadRunState::Ready, ThreadRunState::Blocked)
                    | (ThreadRunState::Ready, ThreadRunState::Exited)
                    | (ThreadRunState::Running, ThreadRunState::Running)
                    | (ThreadRunState::Running, ThreadRunState::Blocked)
                    | (ThreadRunState::Running, ThreadRunState::Exited)
                    | (ThreadRunState::Running, ThreadRunState::Ready)
                    | (ThreadRunState::Blocked, ThreadRunState::Ready)
                    | (ThreadRunState::Blocked, ThreadRunState::Running)
                    | (ThreadRunState::Blocked, ThreadRunState::Exited)
            ),
            "invalid state transition: {:?} -> {:?}",
            self.state,
            new
        );

        self.state = new;
    }

    pub fn set_priority(&mut self, priority: Priority) {
        let was_boosted = self.effective_priority > self.priority;

        self.priority = priority;

        if was_boosted {
            self.effective_priority = self.effective_priority.max(priority);
        } else {
            self.effective_priority = priority;
        }
    }

    /// Boost effective priority for priority inheritance.
    pub fn boost_priority(&mut self, priority: Priority) {
        if priority > self.effective_priority {
            self.effective_priority = priority;
        }
    }

    /// Release priority boost, returning to base priority.
    pub fn release_boost(&mut self) {
        self.effective_priority = self.priority;
    }

    pub fn set_affinity(&mut self, hint: TopologyHint) {
        self.affinity = hint;
    }

    pub fn set_exit_event(&mut self, event: EventId) {
        self.exit_event = Some(event);
    }

    pub fn set_kernel_stack(&mut self, base: usize, sp: usize) {
        self.kernel_stack_base = base;
        self.kernel_sp = sp;
    }

    pub fn set_kernel_sp(&mut self, sp: usize) {
        self.kernel_sp = sp;
    }

    pub fn set_fp_dirty(&mut self, dirty: bool) {
        self.fp_dirty = dirty;
    }

    pub fn set_wait_events(&mut self, ids: &[u32]) {
        let count = ids.len().min(crate::config::MAX_MULTI_WAIT);

        self.wait_count = count as u8;
        self.wait_events = [0; crate::config::MAX_MULTI_WAIT];
        self.wait_events[..count].copy_from_slice(&ids[..count]);
    }

    pub fn take_wait_events(&mut self) -> ([u32; crate::config::MAX_MULTI_WAIT], u8) {
        let result = (self.wait_events, self.wait_count);

        self.wait_events = [0; crate::config::MAX_MULTI_WAIT];
        self.wait_count = 0;

        result
    }

    pub fn set_wakeup_error(&mut self, error: SyscallError) {
        self.wakeup_error = Some(error);
    }

    pub fn take_wakeup_error(&mut self) -> Option<SyscallError> {
        self.wakeup_error.take()
    }

    pub fn set_wakeup_value(&mut self, val: u64) {
        self.wakeup_value = Some(val);
    }

    pub fn take_wakeup_value(&mut self) -> Option<u64> {
        self.wakeup_value.take()
    }

    pub fn set_recv_state(&mut self, state: RecvState) {
        self.recv_state = Some(state);
    }

    pub fn take_recv_state(&mut self) -> Option<RecvState> {
        self.recv_state.take()
    }

    /// Terminate the thread with an exit code.
    pub fn exit(&mut self, code: u32) {
        self.state = ThreadRunState::Exited;
        self.exit_code = Some(code);
    }

    #[cfg(any(target_os = "none", test))]
    pub fn register_state(&self) -> Option<&RegisterState> {
        self.register_state.as_deref()
    }

    #[cfg(any(target_os = "none", test))]
    pub fn register_state_mut(&mut self) -> Option<&mut RegisterState> {
        self.register_state.as_deref_mut()
    }

    #[cfg(any(target_os = "none", test))]
    pub fn init_register_state(&mut self) -> &mut RegisterState {
        self.register_state
            .get_or_insert_with(|| Box::new(RegisterState::zeroed()))
    }
}

/// Doubly-linked list link for scheduler queues. Stored in a parallel array
/// inside the Scheduler, indexed by thread ID. Keeping links in the Scheduler
/// (not the Thread) avoids borrow conflicts between `Kernel.scheduler` and
/// `Kernel.threads`.
#[derive(Clone, Copy)]
struct SchedLink {
    next: Option<u32>,
    prev: Option<u32>,
}

impl SchedLink {
    const EMPTY: Self = SchedLink {
        next: None,
        prev: None,
    };
}

/// Per-core bitmap-indexed multi-level queue.
///
/// 256 priority levels, each a doubly-linked list of thread IDs. The bitmap
/// tracks which levels are non-empty, giving O(1) `pick_next` via CLZ on
/// ARM64. All list state is in the Scheduler's `links` array — RunQueue
/// stores only head/tail pointers and the bitmap.
pub struct RunQueue {
    bitmap: [u64; BITMAP_WORDS],
    heads: [Option<u32>; Priority::NUM_LEVELS],
    tails: [Option<u32>; Priority::NUM_LEVELS],
    current: Option<ThreadId>,
    count: usize,
}

const _: () = {
    assert!(core::mem::size_of::<[u64; BITMAP_WORDS]>() == 32);
};

#[allow(clippy::new_without_default)]
impl RunQueue {
    pub fn new() -> Self {
        RunQueue {
            bitmap: [0; BITMAP_WORDS],
            heads: [None; Priority::NUM_LEVELS],
            tails: [None; Priority::NUM_LEVELS],
            current: None,
            count: 0,
        }
    }

    pub fn current(&self) -> Option<ThreadId> {
        self.current
    }

    pub fn set_current(&mut self, thread: Option<ThreadId>) {
        self.current = thread;
    }

    fn set_bit(&mut self, level: u8) {
        self.bitmap[(level >> 6) as usize] |= 1u64 << (level & 63);
    }

    fn clear_bit(&mut self, level: u8) {
        self.bitmap[(level >> 6) as usize] &= !(1u64 << (level & 63));
    }

    fn highest_set_bit(&self) -> Option<u8> {
        for word_idx in (0..BITMAP_WORDS).rev() {
            let word = self.bitmap[word_idx];

            if word != 0 {
                let bit = 63 - word.leading_zeros() as u8;

                return Some((word_idx as u8) * 64 + bit);
            }
        }

        None
    }

    pub fn has_higher_priority_than(&self, threshold: Priority) -> bool {
        let Some(start_level) = threshold.0.checked_add(1) else {
            return false;
        };
        let start_word = (start_level >> 6) as usize;
        let start_bit = start_level & 63;

        if start_word < BITMAP_WORDS {
            if (self.bitmap[start_word] >> start_bit) != 0 {
                return true;
            }

            for word_idx in (start_word + 1)..BITMAP_WORDS {
                if self.bitmap[word_idx] != 0 {
                    return true;
                }
            }
        }

        false
    }

    pub fn total_ready(&self) -> usize {
        self.count
    }
}

/// Per-core scheduler state: run queue + linked-list links.
///
/// Links are per-core (not global) so each core's enqueue/dequeue/pick_next
/// touches only its own state. A thread is in at most one core's queue at a
/// time, so per-core links cannot conflict. This is the unit of locking for
/// SMP: each core's `PerCoreState` lives behind its own `SpinLock` so
/// independent cores never contend.
pub struct PerCoreState {
    queue: RunQueue,
    links: Vec<SchedLink>,
}

impl Default for PerCoreState {
    fn default() -> Self {
        Self::new()
    }
}

impl PerCoreState {
    pub fn new() -> Self {
        PerCoreState {
            queue: RunQueue::new(),
            links: alloc::vec![SchedLink::EMPTY; crate::config::MAX_THREADS],
        }
    }

    pub fn enqueue(&mut self, thread: ThreadId, priority: Priority) {
        let level = priority.0;
        let id = thread.0 as usize;
        let old_tail = self.queue.tails[level as usize];

        self.links[id] = SchedLink {
            prev: old_tail,
            next: None,
        };

        if let Some(t) = old_tail {
            self.links[t as usize].next = Some(thread.0);
        } else {
            self.queue.heads[level as usize] = Some(thread.0);
        }

        self.queue.tails[level as usize] = Some(thread.0);
        self.queue.set_bit(level);
        self.queue.count += 1;
    }

    fn unlink(&mut self, thread_id: u32, level: u8) {
        let link = self.links[thread_id as usize];

        if let Some(p) = link.prev {
            self.links[p as usize].next = link.next;
        } else {
            self.queue.heads[level as usize] = link.next;
        }
        if let Some(n) = link.next {
            self.links[n as usize].prev = link.prev;
        } else {
            self.queue.tails[level as usize] = link.prev;
        }

        self.links[thread_id as usize] = SchedLink::EMPTY;

        if self.queue.heads[level as usize].is_none() {
            self.queue.clear_bit(level);
        }

        self.queue.count -= 1;
    }

    pub fn pick_next(&mut self) -> Option<ThreadId> {
        let level = self.queue.highest_set_bit()?;
        let head = self.queue.heads[level as usize]?;

        self.unlink(head, level);

        Some(ThreadId(head))
    }

    pub fn dequeue(&mut self, thread: ThreadId, priority: Priority) -> bool {
        if self.queue.heads[priority.0 as usize].is_none() {
            return false;
        }

        let mut cursor = self.queue.heads[priority.0 as usize];

        while let Some(id) = cursor {
            if id == thread.0 {
                self.unlink(id, priority.0);

                return true;
            }

            cursor = self.links[id as usize].next;
        }

        false
    }

    pub fn rotate_current(&mut self, priority: Priority) {
        let current = self.queue.current.take();

        if let Some(tid) = current {
            self.enqueue(tid, priority);
        }
    }

    pub fn remove_if_present(&mut self, thread: ThreadId) -> bool {
        if self.queue.current == Some(thread) {
            self.queue.current = None;

            return true;
        }

        for level in 0..Priority::NUM_LEVELS {
            let mut cursor = self.queue.heads[level];

            while let Some(id) = cursor {
                if id == thread.0 {
                    self.unlink(id, level as u8);

                    return true;
                }

                cursor = self.links[id as usize].next;
            }
        }

        false
    }

    pub fn current(&self) -> Option<ThreadId> {
        self.queue.current()
    }

    pub fn set_current(&mut self, thread: Option<ThreadId>) {
        self.queue.set_current(thread);
    }

    pub fn total_ready(&self) -> usize {
        self.queue.total_ready()
    }

    pub fn has_higher_priority_than(&self, threshold: Priority) -> bool {
        self.queue.has_higher_priority_than(threshold)
    }

    #[cfg(any(test, fuzzing, debug_assertions))]
    pub fn all_queued(&self) -> alloc::vec::Vec<ThreadId> {
        let mut ids = alloc::vec::Vec::new();

        for level in 0..Priority::NUM_LEVELS {
            let mut cursor = self.queue.heads[level];

            while let Some(id) = cursor {
                ids.push(ThreadId(id));

                cursor = self.links[id as usize].next;
            }
        }

        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::state::Schedulers;

    fn make_thread(id: u32, priority: Priority) -> Thread {
        Thread::new(
            ThreadId(id),
            Some(AddressSpaceId(0)),
            priority,
            0x1000,
            0x2000,
            0,
        )
    }

    fn make_idle_thread(id: u32) -> Thread {
        Thread::new(ThreadId(id), None, Priority::Idle, 0, 0, 0)
    }

    // -- Thread lifecycle --

    #[test]
    fn thread_created_in_ready_state() {
        let t = make_thread(0, Priority::Medium);

        assert_eq!(t.state(), ThreadRunState::Ready);
        assert_eq!(t.priority(), Priority::Medium);
        assert_eq!(t.address_space(), Some(AddressSpaceId(0)));
        assert_eq!(t.entry_point(), 0x1000);
    }

    #[test]
    fn thread_exit_sets_code() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.exit(42);

        assert_eq!(t.state(), ThreadRunState::Exited);
        assert_eq!(t.exit_code(), Some(42));
    }

    #[test]
    fn thread_exit_event_stored() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_exit_event(EventId(7));

        assert_eq!(t.exit_event(), Some(EventId(7)));
    }

    #[test]
    fn thread_state_transitions() {
        let mut t = make_thread(0, Priority::Medium);

        assert_eq!(t.state(), ThreadRunState::Ready);

        t.set_state(ThreadRunState::Running);

        assert_eq!(t.state(), ThreadRunState::Running);

        t.set_state(ThreadRunState::Blocked);

        assert_eq!(t.state(), ThreadRunState::Blocked);

        t.set_state(ThreadRunState::Ready);

        assert_eq!(t.state(), ThreadRunState::Ready);
    }

    #[test]
    fn idle_thread_properties() {
        let t = make_idle_thread(0);

        assert!(t.is_idle());
        assert!(t.address_space().is_none());
        assert_eq!(t.priority(), Priority::Idle);
    }

    #[test]
    fn kernel_stack_tracking() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_kernel_stack(0xDEAD_0000, 0xDEAD_8000);

        assert_eq!(t.kernel_stack_base(), 0xDEAD_0000);
        assert_eq!(t.kernel_sp(), 0xDEAD_8000);
    }

    #[test]
    fn priority_boost_and_release() {
        let mut t = make_thread(0, Priority::Low);

        assert_eq!(t.effective_priority(), Priority::Low);

        t.boost_priority(Priority::High);

        assert_eq!(t.effective_priority(), Priority::High);
        assert_eq!(t.priority(), Priority::Low);

        t.release_boost();

        assert_eq!(t.effective_priority(), Priority::Low);
    }

    #[test]
    fn thread_register_state_starts_none() {
        let t = make_thread(0, Priority::Medium);

        assert!(t.register_state().is_none());
    }

    #[test]
    fn thread_init_register_state() {
        let mut t = make_thread(0, Priority::Medium);
        let rs = t.init_register_state();

        rs.pc = 0x1000;
        rs.sp = 0x2000;
        rs.pstate = 0;

        assert_eq!(t.register_state().unwrap().pc, 0x1000);
        assert_eq!(t.register_state().unwrap().sp, 0x2000);
    }

    #[test]
    fn set_priority_updates_effective_if_higher() {
        let mut t = make_thread(0, Priority::Low);

        t.set_priority(Priority::High);

        assert_eq!(t.priority(), Priority::High);
        assert_eq!(t.effective_priority(), Priority::High);
    }

    // -- Scheduler priority ordering --

    #[test]
    fn pick_next_returns_highest_priority() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Low);
        scheds.core(0).lock().enqueue(ThreadId(2), Priority::High);
        scheds.core(0).lock().enqueue(ThreadId(3), Priority::Medium);

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(2)));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(3)));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(1)));
        assert_eq!(scheds.core(0).lock().pick_next(), None);
    }

    // -- Round-robin --

    #[test]
    fn round_robin_same_priority() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);
        scheds.core(0).lock().enqueue(ThreadId(2), Priority::Medium);
        scheds.core(0).lock().enqueue(ThreadId(3), Priority::Medium);

        let first = scheds.core(0).lock().pick_next().unwrap();

        assert_eq!(first, ThreadId(1));

        scheds.core(0).lock().set_current(Some(first));
        scheds.core(0).lock().rotate_current(Priority::Medium);

        let second = scheds.core(0).lock().pick_next().unwrap();

        assert_eq!(second, ThreadId(2));

        scheds.core(0).lock().set_current(Some(second));
        scheds.core(0).lock().rotate_current(Priority::Medium);

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(3)));
    }

    // -- Idle thread --

    #[test]
    fn idle_thread_selected_last() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(100), Priority::Idle);
        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Low);

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(1)));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(100)));
    }

    #[test]
    fn idle_thread_when_all_empty() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(100), Priority::Idle);

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(100)));
    }

    // -- Preemption detection --

    #[test]
    fn detects_higher_priority_ready() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(2), Priority::High);

        assert!(
            scheds
                .core(0)
                .lock()
                .has_higher_priority_than(Priority::Low)
        );
        assert!(
            !scheds
                .core(0)
                .lock()
                .has_higher_priority_than(Priority::High)
        );
    }

    // -- Dequeue --

    #[test]
    fn dequeue_removes_thread() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);
        scheds.core(0).lock().enqueue(ThreadId(2), Priority::Medium);

        assert!(scheds.core(0).lock().dequeue(ThreadId(1), Priority::Medium));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(2)));
        assert!(scheds.core(0).lock().pick_next().is_none());
    }

    // -- Multi-core --

    #[test]
    fn multi_core_isolation() {
        let scheds = Schedulers::new(2);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);
        scheds.core(1).lock().enqueue(ThreadId(2), Priority::Medium);

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(1)));
        assert_eq!(scheds.core(1).lock().pick_next(), Some(ThreadId(2)));
        assert!(scheds.core(0).lock().pick_next().is_none());
        assert!(scheds.core(1).lock().pick_next().is_none());
    }

    #[test]
    fn least_loaded_core_picks_empty() {
        let scheds = Schedulers::new(4);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);
        scheds.core(0).lock().enqueue(ThreadId(2), Priority::Medium);
        scheds.core(1).lock().enqueue(ThreadId(3), Priority::Medium);

        let least = scheds.least_loaded_core();

        assert!(least == 2 || least == 3);
    }

    // -- Scheduler edge cases --

    #[test]
    fn round_robin_all_priority_levels() {
        for pri in [
            Priority::Idle,
            Priority::Low,
            Priority::Medium,
            Priority::High,
        ] {
            let scheds = Schedulers::new(1);

            scheds.core(0).lock().enqueue(ThreadId(1), pri);
            scheds.core(0).lock().enqueue(ThreadId(2), pri);

            assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(1)));

            scheds.core(0).lock().set_current(Some(ThreadId(1)));
            scheds.core(0).lock().rotate_current(pri);

            assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(2)));
        }
    }

    #[test]
    fn preemption_detection_at_every_boundary() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::High);

        assert!(
            scheds
                .core(0)
                .lock()
                .has_higher_priority_than(Priority::Medium)
        );
        assert!(
            scheds
                .core(0)
                .lock()
                .has_higher_priority_than(Priority::Low)
        );
        assert!(
            scheds
                .core(0)
                .lock()
                .has_higher_priority_than(Priority::Idle)
        );
        assert!(
            !scheds
                .core(0)
                .lock()
                .has_higher_priority_than(Priority::High)
        );
    }

    #[test]
    fn remove_thread_from_middle_of_queue() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);
        scheds.core(0).lock().enqueue(ThreadId(2), Priority::Medium);
        scheds.core(0).lock().enqueue(ThreadId(3), Priority::Medium);
        scheds.core(0).lock().dequeue(ThreadId(2), Priority::Medium);

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(1)));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(3)));
        assert!(scheds.core(0).lock().pick_next().is_none());
    }

    #[test]
    fn remove_nonexistent_thread_is_noop() {
        let scheds = Schedulers::new(1);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);

        assert!(
            !scheds
                .core(0)
                .lock()
                .dequeue(ThreadId(99), Priority::Medium)
        );
        assert_eq!(scheds.core(0).lock().total_ready(), 1);
    }

    #[test]
    fn remove_from_global_scheduler_finds_correct_core() {
        let scheds = Schedulers::new(3);

        scheds.core(0).lock().enqueue(ThreadId(1), Priority::Medium);
        scheds.core(1).lock().enqueue(ThreadId(2), Priority::Medium);
        scheds.core(2).lock().enqueue(ThreadId(3), Priority::Medium);
        scheds.remove(ThreadId(2));

        assert_eq!(scheds.core(0).lock().total_ready(), 1);
        assert_eq!(scheds.core(1).lock().total_ready(), 0);
        assert_eq!(scheds.core(2).lock().total_ready(), 1);
    }

    #[test]
    fn empty_queue_pick_returns_none() {
        let scheds = Schedulers::new(1);

        assert!(scheds.core(0).lock().pick_next().is_none());
        assert!(scheds.core(0).lock().current().is_none());
    }

    #[test]
    fn set_priority_preserves_boost() {
        let mut t = make_thread(0, Priority::Low);

        t.boost_priority(Priority::High);
        t.set_priority(Priority::Medium);

        assert_eq!(t.priority(), Priority::Medium);
        assert_eq!(
            t.effective_priority(),
            Priority::High,
            "boost should be preserved when base priority changes"
        );
    }

    #[test]
    fn set_priority_below_boost_keeps_boost() {
        let mut t = make_thread(0, Priority::Low);

        t.boost_priority(Priority::High);
        t.set_priority(Priority::Low);

        assert_eq!(t.effective_priority(), Priority::High);
    }

    #[test]
    fn boost_below_current_effective_is_noop() {
        let mut t = make_thread(0, Priority::High);

        t.boost_priority(Priority::Low);

        assert_eq!(t.effective_priority(), Priority::High);
    }

    // ── State machine: every valid transition ──

    #[test]
    fn state_ready_to_running() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);

        assert_eq!(t.state(), ThreadRunState::Running);
    }

    #[test]
    fn state_ready_to_blocked() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Blocked);

        assert_eq!(t.state(), ThreadRunState::Blocked);
    }

    #[test]
    fn state_ready_to_exited() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Exited);

        assert_eq!(t.state(), ThreadRunState::Exited);
    }

    #[test]
    fn state_running_to_blocked() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Blocked);

        assert_eq!(t.state(), ThreadRunState::Blocked);
    }

    #[test]
    fn state_running_to_ready() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Ready);

        assert_eq!(t.state(), ThreadRunState::Ready);
    }

    #[test]
    fn state_running_to_exited() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Exited);

        assert_eq!(t.state(), ThreadRunState::Exited);
    }

    #[test]
    fn state_blocked_to_ready() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Blocked);
        t.set_state(ThreadRunState::Ready);

        assert_eq!(t.state(), ThreadRunState::Ready);
    }

    #[test]
    fn state_blocked_to_exited() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Blocked);
        t.set_state(ThreadRunState::Exited);

        assert_eq!(t.state(), ThreadRunState::Exited);
    }

    // ── State machine: invalid transitions (debug_assert) ──

    #[test]
    #[should_panic(expected = "invalid state transition")]
    fn state_exited_to_running_panics() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Exited);
        t.set_state(ThreadRunState::Running);
    }

    #[test]
    #[should_panic(expected = "invalid state transition")]
    fn state_exited_to_ready_panics() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Exited);
        t.set_state(ThreadRunState::Ready);
    }

    #[test]
    fn state_blocked_to_running_via_direct_switch() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Blocked);
        t.set_state(ThreadRunState::Running);

        assert_eq!(t.state(), ThreadRunState::Running);
    }

    // ── FixedRing edge cases ──

    #[test]
    fn fixed_ring_fill_to_capacity() {
        let scheds = Schedulers::new(1);

        for i in 0..128 {
            scheds.core(0).lock().enqueue(ThreadId(i), Priority::Medium);
        }

        assert_eq!(scheds.core(0).lock().total_ready(), 128);

        for i in 0..128 {
            assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(i)));
        }

        assert!(scheds.core(0).lock().pick_next().is_none());
    }

    #[test]
    fn fixed_ring_remove_first_middle_last() {
        let scheds = Schedulers::new(1);

        for i in 0..5 {
            scheds.core(0).lock().enqueue(ThreadId(i), Priority::Medium);
        }

        assert!(scheds.core(0).lock().dequeue(ThreadId(0), Priority::Medium));
        assert!(scheds.core(0).lock().dequeue(ThreadId(2), Priority::Medium));
        assert!(scheds.core(0).lock().dequeue(ThreadId(4), Priority::Medium));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(1)));
        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(3)));
        assert!(scheds.core(0).lock().pick_next().is_none());
    }

    #[test]
    fn fixed_ring_wraparound() {
        let scheds = Schedulers::new(1);

        for i in 0..120 {
            scheds.core(0).lock().enqueue(ThreadId(i), Priority::Medium);
        }

        for _ in 0..120 {
            scheds.core(0).lock().pick_next().unwrap();
        }

        for i in 200..210 {
            scheds.core(0).lock().enqueue(ThreadId(i), Priority::Medium);
        }

        assert_eq!(scheds.core(0).lock().pick_next(), Some(ThreadId(200)));
        assert_eq!(scheds.core(0).lock().total_ready(), 9);
    }
}
