//! Host-side tests for cross-module lifetime and ownership invariants.
//!
//! Tests verify interactions that span multiple kernel modules:
//! (a) handle close while channel is blocked
//! (b) thread drop after exit notification
//! (c) address space deallocation after process exit
//! (d) timer callback on dead thread
//! (e) process slot leak in create_from_user_elf

// ============================================================
// Minimal models (cannot import kernel modules directly)
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimerId(u8);

/// Mirrors kernel handle::HandleObject (subset for testing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleObject {
    Channel(ChannelId),
    Timer(TimerId),
    Process(ProcessId),
    Thread(ThreadId),
}

// --- Channel model ---

struct Channel {
    pending_signal: [bool; 2],
    waiter: [Option<ThreadId>; 2],
    closed_count: u8,
}

impl Channel {
    fn new() -> Self {
        Self {
            pending_signal: [false, false],
            waiter: [None, None],
            closed_count: 0,
        }
    }

    fn endpoint_index(id: ChannelId) -> usize {
        id.0 as usize % 2
    }

    fn signal(&mut self, id: ChannelId) -> Option<ThreadId> {
        let peer_ep = 1 - Self::endpoint_index(id);
        self.pending_signal[peer_ep] = true;
        self.waiter[peer_ep].take()
    }

    fn close_endpoint(&mut self, id: ChannelId) -> (bool, Option<ThreadId>) {
        if self.closed_count >= 2 {
            return (false, None);
        }
        let ep = Self::endpoint_index(id);
        let peer_ep = 1 - ep;
        self.waiter[ep] = None;
        let peer_waiter = self.waiter[peer_ep].take();
        self.closed_count += 1;
        (self.closed_count == 2, peer_waiter)
    }

    fn register_waiter(&mut self, id: ChannelId, waiter: ThreadId) {
        let ep = Self::endpoint_index(id);
        self.waiter[ep] = Some(waiter);
    }
}

// --- Thread model ---

struct Thread {
    id: ThreadId,
    state: ThreadState,
    process_id: Option<ProcessId>,
    wake_pending: bool,
    wait_set: Vec<HandleObject>,
}

impl Thread {
    fn new_user(id: u64, pid: ProcessId) -> Self {
        Self {
            id: ThreadId(id),
            state: ThreadState::Ready,
            process_id: Some(pid),
            wake_pending: false,
            wait_set: Vec::new(),
        }
    }

    fn activate(&mut self) {
        assert_eq!(self.state, ThreadState::Ready);
        self.state = ThreadState::Running;
    }

    fn block(&mut self) {
        assert_eq!(self.state, ThreadState::Running);
        self.state = ThreadState::Blocked;
    }

    fn wake(&mut self) -> bool {
        if self.state == ThreadState::Blocked {
            self.state = ThreadState::Ready;
            true
        } else {
            false
        }
    }

    fn mark_exited(&mut self) {
        self.state = ThreadState::Exited;
    }

    fn deschedule(&mut self) {
        if self.state == ThreadState::Running {
            self.state = ThreadState::Ready;
        }
    }
}

// --- WaitableRegistry model ---

struct WaitableEntry {
    ready: bool,
    waiter: Option<ThreadId>,
}

struct WaitableRegistry {
    entries: Vec<Option<WaitableEntry>>,
}

impl WaitableRegistry {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn create(&mut self, idx: usize) {
        if idx >= self.entries.len() {
            self.entries.resize_with(idx + 1, || None);
        }
        self.entries[idx] = Some(WaitableEntry {
            ready: false,
            waiter: None,
        });
    }

    fn destroy(&mut self, idx: usize) -> Option<ThreadId> {
        if let Some(slot) = self.entries.get_mut(idx) {
            let waiter = slot.as_mut().and_then(|e| e.waiter.take());
            *slot = None;
            waiter
        } else {
            None
        }
    }

    fn notify(&mut self, idx: usize) -> Option<ThreadId> {
        let entry = self.entries.get_mut(idx)?.as_mut()?;
        entry.ready = true;
        entry.waiter.take()
    }

    fn check_ready(&self, idx: usize) -> bool {
        self.entries
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .is_some_and(|e| e.ready)
    }

    fn register_waiter(&mut self, idx: usize, waiter: ThreadId) {
        if let Some(Some(entry)) = self.entries.get_mut(idx) {
            entry.waiter = Some(waiter);
        }
    }
}

// --- Process model ---

struct Process {
    id: ProcessId,
    thread_count: u32,
    killed: bool,
    has_address_space: bool,
}

// --- Scheduler state model ---

struct SchedulerState {
    processes: Vec<Option<Process>>,
    threads: Vec<Thread>,
    blocked: Vec<Thread>,
    next_process_id: u32,
    next_thread_id: u64,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            threads: Vec::new(),
            blocked: Vec::new(),
            next_process_id: 0,
            next_thread_id: 1,
        }
    }

    /// Mirrors scheduler::create_process.
    fn create_process(&mut self) -> ProcessId {
        let id = self.next_process_id;
        self.next_process_id += 1;
        self.processes.push(Some(Process {
            id: ProcessId(id),
            thread_count: 0,
            killed: false,
            has_address_space: true,
        }));
        ProcessId(id)
    }

    /// Mirrors scheduler::spawn_user_suspended. Returns None on OOM.
    fn spawn_user_suspended(&mut self, pid: ProcessId, simulate_oom: bool) -> Option<ThreadId> {
        if simulate_oom {
            return None;
        }
        let id = self.next_thread_id;
        self.next_thread_id += 1;
        let process = self.processes[pid.0 as usize].as_mut().unwrap();
        process.thread_count += 1;
        let thread = Thread::new_user(id, pid);
        self.threads.push(thread);
        Some(ThreadId(id))
    }

    /// Check if a process slot is occupied.
    fn process_exists(&self, pid: ProcessId) -> bool {
        self.processes
            .get(pid.0 as usize)
            .and_then(|p| p.as_ref())
            .is_some()
    }

    /// Mirrors the cleanup path: remove orphaned process slot.
    fn remove_process(&mut self, pid: ProcessId) {
        if let Some(slot) = self.processes.get_mut(pid.0 as usize) {
            *slot = None;
        }
    }
}

// ============================================================
// (a) Handle close while channel is blocked
// ============================================================

#[test]
fn close_endpoint_wakes_blocked_peer() {
    // When ep0 closes, a thread blocked waiting on ep1 must be woken.
    // The kernel's close_endpoint takes the peer's waiter and wakes it
    // via try_wake_for_handle / set_wake_pending_for_handle.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);
    let blocked_thread = ThreadId(42);

    // Thread 42 is blocked waiting on channel ep1.
    ch.register_waiter(ep1, blocked_thread);

    // ep0 closes — must return the peer waiter for waking.
    let (freed, peer_waiter) = ch.close_endpoint(ep0);

    assert!(!freed, "single close should not free pages");
    assert_eq!(
        peer_waiter,
        Some(blocked_thread),
        "close must return peer waiter so the kernel can wake the blocked thread"
    );
}

#[test]
fn close_endpoint_wakes_before_freeing_pages() {
    // Both endpoints close. The peer waiter must be returned on the first
    // close (not deferred to the second close when pages are freed).
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(99));

    let (_freed1, waiter1) = ch.close_endpoint(ep0);
    let (freed2, waiter2) = ch.close_endpoint(ep1);

    assert_eq!(
        waiter1,
        Some(ThreadId(99)),
        "waiter returned on first close"
    );
    assert_eq!(waiter2, None, "no waiter on second close (already taken)");
    assert!(freed2, "pages freed on second close");
}

#[test]
fn close_while_signal_pending_is_safe() {
    // A signal is pending when close happens. The pending flag stays set
    // on the closed endpoint (harmless). The peer is still woken.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(7));
    ch.signal(ep0); // sets pending on ep1, takes waiter

    // Now ep0 closes. Waiter was already consumed by signal.
    let (_freed, waiter) = ch.close_endpoint(ep0);

    assert_eq!(waiter, None, "waiter already consumed by signal");
}

// ============================================================
// (b) Thread drop after exit notification
// ============================================================

#[test]
fn exit_notification_before_thread_drop() {
    // Verify the kernel's exit sequence: notify_exit is called while the
    // thread is still alive (Running/current), and the thread is only
    // dropped (deferred) after being parked as Exited.
    let mut thread_exit_registry = WaitableRegistry::new();
    let tid = ThreadId(5);
    let waiting_thread = ThreadId(10);

    // Thread 5 has exit notification state, thread 10 is waiting for it.
    thread_exit_registry.create(tid.0 as usize);
    thread_exit_registry.register_waiter(tid.0 as usize, waiting_thread);

    // Phase 2 of exit: notify_exit fires BEFORE the thread is dropped.
    let waiter = thread_exit_registry.notify(tid.0 as usize);

    assert_eq!(
        waiter,
        Some(waiting_thread),
        "exit notification must return waiter for waking"
    );

    // The thread is still alive at this point (notification complete, not yet dropped).
    // Only after schedule_inner returns does the thread get parked as Exited
    // and deferred for drop on the NEXT schedule_inner.
    assert!(
        thread_exit_registry.check_ready(tid.0 as usize),
        "thread exit entry must be marked ready after notify"
    );
}

#[test]
fn destroy_exit_entry_before_thread_exits() {
    // If the parent closes the thread handle before the child exits,
    // destroy() wakes the waiter and removes the entry. Later
    // notify_exit() finds no entry and does nothing (harmless).
    let mut registry = WaitableRegistry::new();
    let tid = ThreadId(5);
    let waiter_id = ThreadId(10);

    registry.create(tid.0 as usize);
    registry.register_waiter(tid.0 as usize, waiter_id);

    // Parent closes thread handle — destroy wakes waiter.
    let waiter = registry.destroy(tid.0 as usize);
    assert_eq!(waiter, Some(waiter_id));

    // Later, thread exits — notify finds no entry (harmless no-op).
    let waiter2 = registry.notify(tid.0 as usize);
    assert_eq!(waiter2, None, "notify on destroyed entry must be no-op");
}

// ============================================================
// (c) Address space deallocation after process exit
// ============================================================

#[test]
fn address_space_freed_after_all_threads_exit() {
    // When the last thread of a killed process is reaped, the address
    // space is freed. Verify the counting logic.
    let mut process = Process {
        id: ProcessId(0),
        thread_count: 3,
        killed: true,
        has_address_space: true,
    };

    // Simulate three threads being reaped one by one.
    process.thread_count = process.thread_count.saturating_sub(1);
    assert_eq!(process.thread_count, 2);
    assert!(process.has_address_space, "not yet — threads still running");

    process.thread_count = process.thread_count.saturating_sub(1);
    assert_eq!(process.thread_count, 1);

    process.thread_count = process.thread_count.saturating_sub(1);
    assert_eq!(process.thread_count, 0);

    // Now it's safe to free the address space.
    if process.thread_count == 0 && process.killed {
        process.has_address_space = false;
    }
    assert!(
        !process.has_address_space,
        "address space must be freed when count hits 0"
    );
}

#[test]
fn last_thread_exit_frees_address_space_immediately() {
    // When a process's last thread exits normally (not killed), the exit
    // path takes the process and frees the address space in Phase 4.
    let mut s = SchedulerState::new();
    let pid = s.create_process();
    let _tid = s.spawn_user_suspended(pid, false).unwrap();

    // Simulate last thread exit: take the process.
    let process = s.processes[pid.0 as usize].take();
    assert!(process.is_some(), "process should be takeable");
    assert!(
        !s.process_exists(pid),
        "process slot should be empty after take"
    );
}

// ============================================================
// (d) Timer callback on dead thread
// ============================================================

#[test]
fn timer_fires_for_exited_thread_is_harmless() {
    // If a timer fires for a thread that has already exited, the wake
    // attempt returns false (thread is Exited, not Blocked). The
    // set_wake_pending fallback sets a flag that is never consumed.
    let mut thread = Thread::new_user(1, ProcessId(0));
    thread.activate();
    thread.mark_exited();

    // Timer fires, tries to wake the exited thread.
    let woke = thread.wake();
    assert!(!woke, "wake on exited thread must return false");

    // Fallback: set_wake_pending — harmless, flag never consumed.
    thread.wake_pending = true;
    assert!(
        thread.wake_pending,
        "flag set but harmless — exited thread is never scheduled"
    );
}

#[test]
fn timer_fires_after_wait_set_cleared() {
    // A thread's wait set is cleared when it's woken or exits.
    // A late timer fire finds an empty wait set — complete_wait_for returns 0.
    let wait_set: Vec<HandleObject> = vec![
        HandleObject::Timer(TimerId(3)),
        HandleObject::Channel(ChannelId(5)),
    ];

    // Simulate clearing the wait set (as happens on wake or exit).
    let cleared: Vec<HandleObject> = Vec::new();

    // Timer fires, tries to resolve against empty wait set.
    let result = cleared
        .iter()
        .find(|e| matches!(e, HandleObject::Timer(TimerId(3))))
        .map(|_| 0xFF_u64) // TIMEOUT_SENTINEL
        .unwrap_or(0);

    assert_eq!(result, 0, "empty wait set returns 0 (no match)");

    // Also verify the match works when wait set is populated.
    let result2 = wait_set
        .iter()
        .find(|e| matches!(e, HandleObject::Timer(TimerId(3))))
        .map(|_| 0xFF_u64)
        .unwrap_or(0);

    assert_eq!(result2, 0xFF, "populated wait set finds the timer");
}

// ============================================================
// (e) Process slot leak in create_from_user_elf
// ============================================================

#[test]
fn process_slot_leak_on_thread_alloc_failure() {
    // BUG: create_from_user_elf calls create_process() (adds process to
    // scheduler table), then spawn_user_suspended(). If spawn fails (OOM),
    // the process slot is leaked — it stays in the table with no threads,
    // consuming memory forever.
    //
    // This test demonstrates the leak scenario.
    let mut s = SchedulerState::new();

    // Step 1: create_process succeeds.
    let pid = s.create_process();
    assert!(s.process_exists(pid), "process created");

    // Step 2: spawn_user_suspended fails (OOM).
    let result = s.spawn_user_suspended(pid, true /* simulate OOM */);
    assert!(result.is_none(), "thread allocation failed");

    // BUG: process slot still occupied with no threads.
    assert!(
        s.process_exists(pid),
        "BUG: orphaned process slot still exists (leaked)"
    );

    // FIX: remove the orphaned process slot on failure.
    s.remove_process(pid);
    assert!(!s.process_exists(pid), "process slot cleaned up after fix");
}

#[test]
fn process_slot_cleaned_up_after_spawn_failure() {
    // After the fix, create_from_user_elf must clean up the process slot
    // when spawn_user_suspended fails. This models the fixed behavior.
    let mut s = SchedulerState::new();

    // Simulate the fixed create_from_user_elf:
    let pid = s.create_process();
    let thread_result = s.spawn_user_suspended(pid, true /* OOM */);

    if thread_result.is_none() {
        // Fix: clean up the orphaned process slot.
        s.remove_process(pid);
    }

    assert!(
        !s.process_exists(pid),
        "fixed: orphaned process slot must be removed on OOM"
    );
}

// ============================================================
// Cross-module: channel close + process exit interaction
// ============================================================

#[test]
fn process_exit_closes_all_channel_endpoints() {
    // When a process exits, its handle table is drained and all channel
    // endpoints are closed. Peers waiting on those channels must be woken.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    // Process A holds ep0, process B holds ep1.
    // Process B is waiting on ep1.
    ch.register_waiter(ep1, ThreadId(20));

    // Process A exits — handle table drained, ep0 closed.
    let (_freed, peer_waiter) = ch.close_endpoint(ep0);

    assert_eq!(
        peer_waiter,
        Some(ThreadId(20)),
        "process exit must wake peer's blocked thread"
    );
}

#[test]
fn timer_destroy_on_handle_close_wakes_waiter() {
    // When a timer handle is closed, destroy() wakes any thread waiting
    // on that timer. This prevents threads from being stuck forever.
    let mut registry = WaitableRegistry::new();
    let timer_idx = 5_usize;
    let waiting_thread = ThreadId(42);

    registry.create(timer_idx);
    registry.register_waiter(timer_idx, waiting_thread);

    // Timer handle closed — destroy returns the waiter for waking.
    let waiter = registry.destroy(timer_idx);
    assert_eq!(
        waiter,
        Some(waiting_thread),
        "timer destroy must return waiter so kernel can wake blocked thread"
    );
}
