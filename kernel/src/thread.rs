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

/// Number of priority levels: Idle, Low, Medium, High.
const NUM_PRIORITY_LEVELS: usize = 4;

fn priority_index(p: Priority) -> usize {
    p as usize
}

/// Thread execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRunState {
    Ready,
    Running,
    Blocked,
    Exited,
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
    space_next: Option<u32>,
    space_prev: Option<u32>,
    #[cfg(any(target_os = "none", test))]
    register_state: Option<Box<RegisterState>>,
}

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

const QUEUE_CAP: usize = 128;

struct FixedRing {
    buf: [ThreadId; QUEUE_CAP],
    head: u16,
    len: u16,
}

impl FixedRing {
    const fn new() -> Self {
        FixedRing {
            buf: [ThreadId(0); QUEUE_CAP],
            head: 0,
            len: 0,
        }
    }

    fn push_back(&mut self, id: ThreadId) {
        debug_assert!((self.len as usize) < QUEUE_CAP, "run queue overflow");

        let tail = (self.head as usize + self.len as usize) % QUEUE_CAP;

        self.buf[tail] = id;
        self.len += 1;
    }

    fn pop_front(&mut self) -> Option<ThreadId> {
        if self.len == 0 {
            return None;
        }

        let id = self.buf[self.head as usize];

        self.head = ((self.head as usize + 1) % QUEUE_CAP) as u16;
        self.len -= 1;

        Some(id)
    }

    fn remove(&mut self, id: ThreadId) -> bool {
        let len = self.len as usize;

        for i in 0..len {
            let idx = (self.head as usize + i) % QUEUE_CAP;

            if self.buf[idx] == id {
                for j in i..len - 1 {
                    let src = (self.head as usize + j + 1) % QUEUE_CAP;
                    let dst = (self.head as usize + j) % QUEUE_CAP;

                    self.buf[dst] = self.buf[src];
                }

                self.len -= 1;

                return true;
            }
        }

        false
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len as usize
    }

    #[cfg(any(test, fuzzing))]
    fn iter(&self) -> impl Iterator<Item = ThreadId> + '_ {
        (0..self.len as usize).map(|i| self.buf[(self.head as usize + i) % QUEUE_CAP])
    }
}

/// Per-core run queue with priority-level FIFO queues.
pub struct RunQueue {
    queues: [FixedRing; NUM_PRIORITY_LEVELS],
    current: Option<ThreadId>,
}

#[allow(clippy::new_without_default)]
impl RunQueue {
    pub fn new() -> Self {
        RunQueue {
            queues: [const { FixedRing::new() }; NUM_PRIORITY_LEVELS],
            current: None,
        }
    }

    pub fn current(&self) -> Option<ThreadId> {
        self.current
    }

    pub fn set_current(&mut self, thread: Option<ThreadId>) {
        self.current = thread;
    }

    /// Add a thread to the back of its priority queue.
    pub fn enqueue(&mut self, thread: ThreadId, priority: Priority) {
        self.queues[priority_index(priority)].push_back(thread);
    }

    /// Remove a specific thread from its priority queue (for blocking).
    pub fn dequeue(&mut self, thread: ThreadId, priority: Priority) -> bool {
        self.queues[priority_index(priority)].remove(thread)
    }

    /// Pick the highest-priority ready thread (removes from queue).
    pub fn pick_next(&mut self) -> Option<ThreadId> {
        for q in self.queues.iter_mut().rev() {
            if let Some(thread) = q.pop_front() {
                return Some(thread);
            }
        }
        None
    }

    /// Move current thread to the back of its queue (quantum expired).
    pub fn rotate_current(&mut self, priority: Priority) {
        if let Some(current) = self.current.take() {
            self.queues[priority_index(priority)].push_back(current);
        }
    }

    /// Check if any thread at higher priority than `threshold` is ready.
    pub fn has_higher_priority_than(&self, threshold: Priority) -> bool {
        let idx = priority_index(threshold);

        self.queues[idx + 1..].iter().any(|q| !q.is_empty())
    }

    pub fn total_ready(&self) -> usize {
        self.queues.iter().map(|q| q.len()).sum()
    }

    #[cfg(any(test, fuzzing))]
    pub fn all_queued_thread_ids(&self) -> alloc::vec::Vec<ThreadId> {
        let mut ids = alloc::vec::Vec::new();

        for q in &self.queues {
            ids.extend(q.iter());
        }

        ids
    }
}

/// Multi-core fixed-priority preemptive scheduler.
pub struct Scheduler {
    cores: Vec<RunQueue>,
}

impl Scheduler {
    pub fn new(num_cores: usize) -> Self {
        let mut cores = Vec::with_capacity(num_cores);

        for _ in 0..num_cores {
            cores.push(RunQueue::new());
        }

        Scheduler { cores }
    }

    pub fn core(&self, core_id: usize) -> &RunQueue {
        &self.cores[core_id]
    }

    pub fn core_mut(&mut self, core_id: usize) -> &mut RunQueue {
        &mut self.cores[core_id]
    }

    pub fn num_cores(&self) -> usize {
        self.cores.len()
    }

    pub fn enqueue(&mut self, core_id: usize, thread: ThreadId, priority: Priority) {
        self.cores[core_id].enqueue(thread, priority);
    }

    pub fn pick_next(&mut self, core_id: usize) -> Option<ThreadId> {
        self.cores[core_id].pick_next()
    }

    /// Remove a thread from any core's run queue (for teardown).
    pub fn remove(&mut self, thread: ThreadId) {
        for core in &mut self.cores {
            if core.current == Some(thread) {
                core.current = None;

                return;
            }

            for q in &mut core.queues {
                if q.remove(thread) {
                    return;
                }
            }
        }
    }

    /// Find the core with the fewest ready threads.
    pub fn least_loaded_core(&self) -> usize {
        self.cores
            .iter()
            .enumerate()
            .min_by_key(|(_, rq)| rq.total_ready())
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(1), Priority::Low);
        sched.enqueue(0, ThreadId(2), Priority::High);
        sched.enqueue(0, ThreadId(3), Priority::Medium);

        assert_eq!(sched.pick_next(0), Some(ThreadId(2)));
        assert_eq!(sched.pick_next(0), Some(ThreadId(3)));
        assert_eq!(sched.pick_next(0), Some(ThreadId(1)));
        assert_eq!(sched.pick_next(0), None);
    }

    // -- Round-robin --

    #[test]
    fn round_robin_same_priority() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(1), Priority::Medium);
        sched.enqueue(0, ThreadId(2), Priority::Medium);
        sched.enqueue(0, ThreadId(3), Priority::Medium);

        let first = sched.pick_next(0).unwrap();

        assert_eq!(first, ThreadId(1));

        sched.core_mut(0).set_current(Some(first));
        sched.core_mut(0).rotate_current(Priority::Medium);

        let second = sched.pick_next(0).unwrap();

        assert_eq!(second, ThreadId(2));

        sched.core_mut(0).set_current(Some(second));
        sched.core_mut(0).rotate_current(Priority::Medium);

        assert_eq!(sched.pick_next(0), Some(ThreadId(3)));

        // Thread 1 wraps back to front after 3 rotates
    }

    // -- Idle thread --

    #[test]
    fn idle_thread_selected_last() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(100), Priority::Idle);
        sched.enqueue(0, ThreadId(1), Priority::Low);

        assert_eq!(sched.pick_next(0), Some(ThreadId(1)));
        assert_eq!(sched.pick_next(0), Some(ThreadId(100)));
    }

    #[test]
    fn idle_thread_when_all_empty() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(100), Priority::Idle);

        assert_eq!(sched.pick_next(0), Some(ThreadId(100)));
    }

    // -- Preemption detection --

    #[test]
    fn detects_higher_priority_ready() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(2), Priority::High);

        assert!(sched.core(0).has_higher_priority_than(Priority::Low));
        assert!(!sched.core(0).has_higher_priority_than(Priority::High));
    }

    // -- Dequeue --

    #[test]
    fn dequeue_removes_thread() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(1), Priority::Medium);
        sched.enqueue(0, ThreadId(2), Priority::Medium);

        assert!(sched.core_mut(0).dequeue(ThreadId(1), Priority::Medium));
        assert_eq!(sched.pick_next(0), Some(ThreadId(2)));
        assert!(sched.pick_next(0).is_none());
    }

    // -- Multi-core --

    #[test]
    fn multi_core_isolation() {
        let mut sched = Scheduler::new(2);

        sched.enqueue(0, ThreadId(1), Priority::Medium);
        sched.enqueue(1, ThreadId(2), Priority::Medium);

        assert_eq!(sched.pick_next(0), Some(ThreadId(1)));
        assert_eq!(sched.pick_next(1), Some(ThreadId(2)));
        assert!(sched.pick_next(0).is_none());
        assert!(sched.pick_next(1).is_none());
    }

    #[test]
    fn least_loaded_core_picks_empty() {
        let mut sched = Scheduler::new(4);

        sched.enqueue(0, ThreadId(1), Priority::Medium);
        sched.enqueue(0, ThreadId(2), Priority::Medium);
        sched.enqueue(1, ThreadId(3), Priority::Medium);

        let least = sched.least_loaded_core();

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
            let mut sched = Scheduler::new(1);

            sched.enqueue(0, ThreadId(1), pri);
            sched.enqueue(0, ThreadId(2), pri);

            assert_eq!(sched.pick_next(0), Some(ThreadId(1)));

            sched.core_mut(0).set_current(Some(ThreadId(1)));
            sched.core_mut(0).rotate_current(pri);

            assert_eq!(sched.pick_next(0), Some(ThreadId(2)));
        }
    }

    #[test]
    fn preemption_detection_at_every_boundary() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(1), Priority::High);

        assert!(sched.core(0).has_higher_priority_than(Priority::Medium));
        assert!(sched.core(0).has_higher_priority_than(Priority::Low));
        assert!(sched.core(0).has_higher_priority_than(Priority::Idle));
        assert!(!sched.core(0).has_higher_priority_than(Priority::High));
    }

    #[test]
    fn remove_thread_from_middle_of_queue() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(1), Priority::Medium);
        sched.enqueue(0, ThreadId(2), Priority::Medium);
        sched.enqueue(0, ThreadId(3), Priority::Medium);

        sched.core_mut(0).dequeue(ThreadId(2), Priority::Medium);

        assert_eq!(sched.pick_next(0), Some(ThreadId(1)));
        assert_eq!(sched.pick_next(0), Some(ThreadId(3)));
        assert!(sched.pick_next(0).is_none());
    }

    #[test]
    fn remove_nonexistent_thread_is_noop() {
        let mut sched = Scheduler::new(1);

        sched.enqueue(0, ThreadId(1), Priority::Medium);

        assert!(!sched.core_mut(0).dequeue(ThreadId(99), Priority::Medium));
        assert_eq!(sched.core(0).total_ready(), 1);
    }

    #[test]
    fn remove_from_global_scheduler_finds_correct_core() {
        let mut sched = Scheduler::new(3);

        sched.enqueue(0, ThreadId(1), Priority::Medium);
        sched.enqueue(1, ThreadId(2), Priority::Medium);
        sched.enqueue(2, ThreadId(3), Priority::Medium);

        sched.remove(ThreadId(2));

        assert_eq!(sched.core(0).total_ready(), 1);
        assert_eq!(sched.core(1).total_ready(), 0);
        assert_eq!(sched.core(2).total_ready(), 1);
    }

    #[test]
    fn empty_queue_pick_returns_none() {
        let mut sched = Scheduler::new(1);

        assert!(sched.pick_next(0).is_none());
        assert!(sched.core(0).current().is_none());
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
    #[should_panic(expected = "invalid state transition")]
    fn state_blocked_to_running_panics() {
        let mut t = make_thread(0, Priority::Medium);

        t.set_state(ThreadRunState::Running);
        t.set_state(ThreadRunState::Blocked);
        t.set_state(ThreadRunState::Running);
    }

    // ── FixedRing edge cases ──

    #[test]
    fn fixed_ring_fill_to_capacity() {
        let mut sched = Scheduler::new(1);

        for i in 0..128 {
            sched.enqueue(0, ThreadId(i), Priority::Medium);
        }

        assert_eq!(sched.core(0).total_ready(), 128);

        for i in 0..128 {
            assert_eq!(sched.pick_next(0), Some(ThreadId(i)));
        }

        assert!(sched.pick_next(0).is_none());
    }

    #[test]
    fn fixed_ring_remove_first_middle_last() {
        let mut sched = Scheduler::new(1);

        for i in 0..5 {
            sched.enqueue(0, ThreadId(i), Priority::Medium);
        }

        assert!(sched.core_mut(0).dequeue(ThreadId(0), Priority::Medium));
        assert!(sched.core_mut(0).dequeue(ThreadId(2), Priority::Medium));
        assert!(sched.core_mut(0).dequeue(ThreadId(4), Priority::Medium));
        assert_eq!(sched.pick_next(0), Some(ThreadId(1)));
        assert_eq!(sched.pick_next(0), Some(ThreadId(3)));
        assert!(sched.pick_next(0).is_none());
    }

    #[test]
    fn fixed_ring_wraparound() {
        let mut sched = Scheduler::new(1);

        for i in 0..120 {
            sched.enqueue(0, ThreadId(i), Priority::Medium);
        }

        for _ in 0..120 {
            sched.pick_next(0).unwrap();
        }

        for i in 200..210 {
            sched.enqueue(0, ThreadId(i), Priority::Medium);
        }

        assert_eq!(sched.pick_next(0), Some(ThreadId(200)));
        assert_eq!(sched.core(0).total_ready(), 9);
    }
}
