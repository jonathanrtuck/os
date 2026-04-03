//! Host-side tests for process exit FLOW LOGIC.
//!
//! Tests the branching and sequencing in `exit_current_from_syscall`:
//! non-last vs last thread exit, handle drain and categorization,
//! init process shutdown detection, and the ExitInfo enum behavior.
//!
//! Complements `kernel_process_exit_cleanup.rs` which tests the detailed
//! cleanup model (timer leaks, stale waiters, DMA, page tables). This
//! file focuses on the decision logic that determines WHICH cleanup path
//! runs and WHAT resources are collected for it.

// ============================================================
// Minimal models of the exit flow types
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimerId(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterruptId(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SchedulingContextId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VmoId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EventId(u32);

/// Mirrors kernel's HandleObject enum (all 8 variants).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleObject {
    Channel(ChannelId),
    Event(EventId),
    Interrupt(InterruptId),
    Process(ProcessId),
    SchedulingContext(SchedulingContextId),
    Thread(ThreadId),
    Timer(TimerId),
    Vmo(VmoId),
}

/// Mirrors kernel's HandleCategories — resources sorted for phase 3 cleanup.
#[derive(Debug, Default)]
struct HandleCategories {
    channels: Vec<ChannelId>,
    interrupts: Vec<InterruptId>,
    timers: Vec<TimerId>,
    thread_handles: Vec<ThreadId>,
    process_handles: Vec<ProcessId>,
    vmos: Vec<VmoId>,
}

/// Mirrors kernel's ExitInfo enum — the exit path decision.
enum ExitInfo {
    Last {
        thread_id: ThreadId,
        process_id: ProcessId,
        handles: HandleCategories,
    },
    NonLast {
        thread_id: ThreadId,
        process_id: ProcessId,
    },
}

impl ExitInfo {
    fn process_id(&self) -> ProcessId {
        match self {
            ExitInfo::Last { process_id, .. } | ExitInfo::NonLast { process_id, .. } => *process_id,
        }
    }
    fn thread_id(&self) -> ThreadId {
        match self {
            ExitInfo::Last { thread_id, .. } | ExitInfo::NonLast { thread_id, .. } => *thread_id,
        }
    }
    fn is_last(&self) -> bool {
        matches!(self, ExitInfo::Last { .. })
    }
}

// --- Handle table model ---

struct HandleTable {
    entries: Vec<Option<(HandleObject, u32, u64)>>, // (object, rights, badge)
}

impl HandleTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn insert(&mut self, obj: HandleObject) -> u32 {
        let idx = self.entries.len() as u32;
        self.entries.push(Some((obj, 0xFFFF, 0)));
        idx
    }

    fn drain(&mut self) -> Vec<(HandleObject, u32, u64)> {
        let mut result = Vec::new();
        for slot in &mut self.entries {
            if let Some(entry) = slot.take() {
                result.push(entry);
            }
        }
        result
    }

    fn len(&self) -> usize {
        self.entries.iter().filter(|s| s.is_some()).count()
    }
}

// --- Process model ---

struct Process {
    thread_count: u32,
    handles: HandleTable,
    killed: bool,
}

impl Process {
    fn new() -> Self {
        Self {
            thread_count: 0,
            handles: HandleTable::new(),
            killed: false,
        }
    }
}

// --- Categorization (mirrors kernel's categorize_handles) ---

/// Events and SchedulingContexts are handled inline; everything else
/// is deferred to phase 3 outside the scheduler lock.
fn categorize_handles(
    objects: Vec<HandleObject>,
    events_destroyed: &mut Vec<EventId>,
    sched_contexts_released: &mut Vec<SchedulingContextId>,
) -> HandleCategories {
    let mut categories = HandleCategories::default();

    for obj in objects {
        match obj {
            HandleObject::Channel(id) => categories.channels.push(id),
            HandleObject::Event(id) => events_destroyed.push(id),
            HandleObject::Interrupt(id) => categories.interrupts.push(id),
            HandleObject::Process(id) => categories.process_handles.push(id),
            HandleObject::SchedulingContext(id) => sched_contexts_released.push(id),
            HandleObject::Thread(id) => categories.thread_handles.push(id),
            HandleObject::Timer(id) => categories.timers.push(id),
            HandleObject::Vmo(id) => categories.vmos.push(id),
        }
    }

    categories
}

/// Model the exit decision logic from exit_current_from_syscall phase 1.
fn decide_exit(
    process: &mut Process,
    thread_id: ThreadId,
    process_id: ProcessId,
    events_destroyed: &mut Vec<EventId>,
    sched_contexts_released: &mut Vec<SchedulingContextId>,
) -> ExitInfo {
    process.thread_count = process.thread_count.saturating_sub(1);
    let is_last = process.thread_count == 0;

    if is_last {
        let handle_objects: Vec<HandleObject> = process
            .handles
            .drain()
            .into_iter()
            .map(|(obj, _, _)| obj)
            .collect();
        let handles = categorize_handles(handle_objects, events_destroyed, sched_contexts_released);

        ExitInfo::Last {
            thread_id,
            process_id,
            handles,
        }
    } else {
        ExitInfo::NonLast {
            thread_id,
            process_id,
        }
    }
}

// ============================================================
// (1) Non-last thread exit: thread_count decrements, process stays alive
// ============================================================

#[test]
fn non_last_thread_exit_decrements_count() {
    let mut process = Process::new();
    process.thread_count = 3;

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    assert!(!exit_info.is_last(), "2 threads remain — not last");
    assert_eq!(process.thread_count, 2);
    assert_eq!(exit_info.thread_id(), ThreadId(1));
    assert_eq!(exit_info.process_id(), ProcessId(0));
}

#[test]
fn non_last_thread_exit_does_not_drain_handles() {
    let mut process = Process::new();
    process.thread_count = 2;
    process.handles.insert(HandleObject::Channel(ChannelId(0)));
    process.handles.insert(HandleObject::Timer(TimerId(1)));

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    assert!(!exit_info.is_last());
    // Handles remain in the table — not drained for non-last exit.
    assert_eq!(
        process.handles.len(),
        2,
        "handles must survive for remaining threads"
    );
}

#[test]
fn non_last_thread_exit_sequential_decrement() {
    // Three threads exit one by one. First two are non-last.
    let mut process = Process::new();
    process.thread_count = 3;
    process.handles.insert(HandleObject::Channel(ChannelId(0)));

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();

    // Thread 1 exits (non-last).
    let info1 = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );
    assert!(!info1.is_last());
    assert_eq!(process.thread_count, 2);

    // Thread 2 exits (non-last).
    let info2 = decide_exit(
        &mut process,
        ThreadId(2),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );
    assert!(!info2.is_last());
    assert_eq!(process.thread_count, 1);

    // Thread 3 exits (last).
    let info3 = decide_exit(
        &mut process,
        ThreadId(3),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );
    assert!(info3.is_last());
    assert_eq!(process.thread_count, 0);
}

// ============================================================
// (2) Last thread exit: process is destroyed
// ============================================================

#[test]
fn last_thread_exit_triggers_destroy() {
    let mut process = Process::new();
    process.thread_count = 1;
    process.handles.insert(HandleObject::Channel(ChannelId(0)));

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    assert!(exit_info.is_last(), "sole thread exit destroys process");
    assert_eq!(process.thread_count, 0);
    // Handle table was drained.
    assert_eq!(process.handles.len(), 0, "handle table drained");
}

#[test]
fn last_thread_exit_preserves_ids() {
    let mut process = Process::new();
    process.thread_count = 1;

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(42),
        ProcessId(7),
        &mut events,
        &mut sched_ctxs,
    );

    assert!(exit_info.is_last());
    assert_eq!(exit_info.thread_id(), ThreadId(42));
    assert_eq!(exit_info.process_id(), ProcessId(7));
}

#[test]
fn last_thread_exit_with_empty_handle_table() {
    // Edge case: process with no handles at all.
    let mut process = Process::new();
    process.thread_count = 1;

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    assert!(exit_info.is_last());
    if let ExitInfo::Last { handles, .. } = exit_info {
        assert!(handles.channels.is_empty());
        assert!(handles.timers.is_empty());
        assert!(handles.interrupts.is_empty());
        assert!(handles.thread_handles.is_empty());
        assert!(handles.process_handles.is_empty());
        assert!(handles.vmos.is_empty());
    }
}

// ============================================================
// (3) Handle drain returns all handle objects for categorized cleanup
// ============================================================

#[test]
fn handle_drain_returns_all_object_types() {
    let mut process = Process::new();
    process.thread_count = 1;

    // Insert one of each type.
    process.handles.insert(HandleObject::Channel(ChannelId(10)));
    process.handles.insert(HandleObject::Event(EventId(20)));
    process
        .handles
        .insert(HandleObject::Interrupt(InterruptId(30)));
    process.handles.insert(HandleObject::Process(ProcessId(40)));
    process
        .handles
        .insert(HandleObject::SchedulingContext(SchedulingContextId(50)));
    process.handles.insert(HandleObject::Thread(ThreadId(60)));
    process.handles.insert(HandleObject::Timer(TimerId(70)));
    process.handles.insert(HandleObject::Vmo(VmoId(80)));

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    assert!(exit_info.is_last());
    if let ExitInfo::Last { handles, .. } = exit_info {
        // Deferred categories (phase 3 outside scheduler lock).
        assert_eq!(handles.channels, vec![ChannelId(10)]);
        assert_eq!(handles.interrupts, vec![InterruptId(30)]);
        assert_eq!(handles.process_handles, vec![ProcessId(40)]);
        assert_eq!(handles.thread_handles, vec![ThreadId(60)]);
        assert_eq!(handles.timers, vec![TimerId(70)]);
        assert_eq!(handles.vmos, vec![VmoId(80)]);

        // Inline-handled categories (handled during categorization).
        assert_eq!(events, vec![EventId(20)], "events destroyed inline");
        assert_eq!(
            sched_ctxs,
            vec![SchedulingContextId(50)],
            "sched contexts released inline"
        );
    } else {
        panic!("expected ExitInfo::Last");
    }
}

#[test]
fn handle_drain_multiple_of_same_type() {
    let mut process = Process::new();
    process.thread_count = 1;

    // Multiple channels.
    process.handles.insert(HandleObject::Channel(ChannelId(0)));
    process.handles.insert(HandleObject::Channel(ChannelId(1)));
    process.handles.insert(HandleObject::Channel(ChannelId(2)));
    // Multiple timers.
    process.handles.insert(HandleObject::Timer(TimerId(0)));
    process.handles.insert(HandleObject::Timer(TimerId(1)));

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    if let ExitInfo::Last { handles, .. } = exit_info {
        assert_eq!(handles.channels.len(), 3, "all 3 channels collected");
        assert_eq!(handles.timers.len(), 2, "all 2 timers collected");
    } else {
        panic!("expected ExitInfo::Last");
    }
}

#[test]
fn handle_drain_leaves_table_empty() {
    let mut table = HandleTable::new();
    table.insert(HandleObject::Channel(ChannelId(0)));
    table.insert(HandleObject::Timer(TimerId(1)));
    table.insert(HandleObject::Vmo(VmoId(2)));

    assert_eq!(table.len(), 3);

    let drained = table.drain();
    assert_eq!(drained.len(), 3, "all entries returned");
    assert_eq!(table.len(), 0, "table is empty after drain");
}

#[test]
fn handle_drain_is_idempotent() {
    let mut table = HandleTable::new();
    table.insert(HandleObject::Channel(ChannelId(0)));

    let first = table.drain();
    assert_eq!(first.len(), 1);

    let second = table.drain();
    assert_eq!(second.len(), 0, "second drain yields nothing");
}

// ============================================================
// (4) Event and SchedulingContext handles are handled inline
// ============================================================

#[test]
fn events_destroyed_during_categorization() {
    // Events are destroyed inline during categorize_handles, not deferred.
    let objects = vec![
        HandleObject::Event(EventId(1)),
        HandleObject::Event(EventId(2)),
        HandleObject::Channel(ChannelId(3)),
    ];

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let categories = categorize_handles(objects, &mut events, &mut sched_ctxs);

    assert_eq!(events, vec![EventId(1), EventId(2)]);
    assert_eq!(categories.channels, vec![ChannelId(3)]);
}

#[test]
fn scheduling_contexts_released_during_categorization() {
    // SchedulingContexts release their ref_count inline.
    let objects = vec![
        HandleObject::SchedulingContext(SchedulingContextId(0)),
        HandleObject::SchedulingContext(SchedulingContextId(1)),
        HandleObject::Timer(TimerId(5)),
    ];

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let categories = categorize_handles(objects, &mut events, &mut sched_ctxs);

    assert_eq!(
        sched_ctxs,
        vec![SchedulingContextId(0), SchedulingContextId(1)]
    );
    assert_eq!(categories.timers, vec![TimerId(5)]);
}

// ============================================================
// (5) Init process exit triggers shutdown check
// ============================================================

/// Model the init detection logic from exit_current_from_syscall.
struct InitTracker {
    init_pid: u32,
    shutdown_triggered: bool,
}

impl InitTracker {
    fn new(init_pid: u32) -> Self {
        Self {
            init_pid,
            shutdown_triggered: false,
        }
    }

    fn is_init(&self, pid: ProcessId) -> bool {
        self.init_pid == pid.0
    }

    /// Mirrors the post-notify phase in exit_current_from_syscall.
    fn on_last_thread_exit(&mut self, process_id: ProcessId) {
        if self.is_init(process_id) {
            self.shutdown_triggered = true;
        }
    }
}

#[test]
fn init_exit_triggers_shutdown() {
    let mut tracker = InitTracker::new(1);

    // Process 1 (init) exits last thread.
    tracker.on_last_thread_exit(ProcessId(1));

    assert!(
        tracker.shutdown_triggered,
        "init exit must trigger system shutdown"
    );
}

#[test]
fn non_init_exit_does_not_trigger_shutdown() {
    let mut tracker = InitTracker::new(1);

    // Process 2 (not init) exits last thread.
    tracker.on_last_thread_exit(ProcessId(2));

    assert!(
        !tracker.shutdown_triggered,
        "non-init exit must not trigger shutdown"
    );
}

#[test]
fn init_non_last_thread_exit_does_not_trigger_shutdown() {
    // The shutdown check only runs on last-thread-exit.
    let mut tracker = InitTracker::new(1);
    let mut process = Process::new();
    process.thread_count = 2;

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(1), // This IS init
        &mut events,
        &mut sched_ctxs,
    );

    // Non-last exit: shutdown check does NOT run.
    if exit_info.is_last() {
        tracker.on_last_thread_exit(exit_info.process_id());
    }

    assert!(
        !tracker.shutdown_triggered,
        "non-last thread exit in init does not shut down"
    );
}

#[test]
fn init_last_thread_exit_full_flow() {
    // Full flow: init has 1 thread, it exits, shutdown fires.
    let mut tracker = InitTracker::new(1);
    let mut process = Process::new();
    process.thread_count = 1;
    process.handles.insert(HandleObject::Channel(ChannelId(0)));

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(1), // init
        &mut events,
        &mut sched_ctxs,
    );

    assert!(exit_info.is_last());

    if exit_info.is_last() {
        tracker.on_last_thread_exit(exit_info.process_id());
    }

    assert!(
        tracker.shutdown_triggered,
        "init last-thread-exit shuts down"
    );
    assert_eq!(process.handles.len(), 0, "handles drained before shutdown");
}

// ============================================================
// (6) saturating_sub prevents underflow on thread_count
// ============================================================

#[test]
fn thread_count_zero_saturates_to_zero() {
    // Edge case: what if thread_count is already 0? (Should not happen in
    // practice, but saturating_sub ensures no underflow.)
    let mut process = Process::new();
    process.thread_count = 0;

    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let exit_info = decide_exit(
        &mut process,
        ThreadId(1),
        ProcessId(0),
        &mut events,
        &mut sched_ctxs,
    );

    assert_eq!(process.thread_count, 0);
    // thread_count was already 0, saturating_sub keeps it at 0 → is_last = true.
    assert!(exit_info.is_last());
}

// ============================================================
// (7) ExitInfo accessors return correct values for both variants
// ============================================================

#[test]
fn exit_info_accessors_last() {
    let info = ExitInfo::Last {
        thread_id: ThreadId(100),
        process_id: ProcessId(200),
        handles: HandleCategories::default(),
    };

    assert_eq!(info.thread_id(), ThreadId(100));
    assert_eq!(info.process_id(), ProcessId(200));
    assert!(info.is_last());
}

#[test]
fn exit_info_accessors_non_last() {
    let info = ExitInfo::NonLast {
        thread_id: ThreadId(300),
        process_id: ProcessId(400),
    };

    assert_eq!(info.thread_id(), ThreadId(300));
    assert_eq!(info.process_id(), ProcessId(400));
    assert!(!info.is_last());
}

// ============================================================
// (8) Phase ordering: notifications only fire on last-thread-exit
// ============================================================

#[test]
fn notification_ordering_multi_thread_process() {
    // Model the notification sequence for a 3-thread process.
    let mut thread_exit_notifications = Vec::new();
    let mut process_exit_notifications = Vec::new();
    let mut tracker = InitTracker::new(99); // Not init.

    let mut process = Process::new();
    process.thread_count = 3;
    process.handles.insert(HandleObject::Channel(ChannelId(0)));
    process.handles.insert(HandleObject::Timer(TimerId(1)));

    for tid in 1..=3u64 {
        let mut events = Vec::new();
        let mut sched_ctxs = Vec::new();
        let exit_info = decide_exit(
            &mut process,
            ThreadId(tid),
            ProcessId(5),
            &mut events,
            &mut sched_ctxs,
        );

        // Phase 2: thread_exit always fires.
        thread_exit_notifications.push(exit_info.thread_id());

        // Phase 2: process_exit only fires on last thread.
        if exit_info.is_last() {
            process_exit_notifications.push(exit_info.process_id());
            tracker.on_last_thread_exit(exit_info.process_id());
        }
    }

    assert_eq!(thread_exit_notifications.len(), 3, "all 3 threads notified");
    assert_eq!(
        process_exit_notifications.len(),
        1,
        "process exit notified exactly once"
    );
    assert_eq!(process_exit_notifications[0], ProcessId(5));
    assert!(!tracker.shutdown_triggered, "non-init, no shutdown");
}

// ============================================================
// (9) Kill path: immediate vs deferred address space cleanup
// ============================================================

/// Model the kill_process decision about immediate vs deferred cleanup.
struct KillResult {
    thread_ids: Vec<ThreadId>,
    handles: HandleCategories,
    /// Some if no threads running (immediate cleanup), None if deferred.
    address_space_taken: bool,
    timeout_timers_collected: Vec<TimerId>,
}

fn model_kill_process(
    process: &mut Process,
    running_on_other_cores: u32,
    non_running_threads: Vec<(ThreadId, Option<TimerId>)>,
) -> KillResult {
    let mut killed_threads = Vec::new();
    let mut timeout_timers = Vec::new();

    // Phase: collect non-running threads (ready + blocked + suspended).
    for (tid, timer) in non_running_threads {
        killed_threads.push(tid);
        if let Some(timer_id) = timer {
            timeout_timers.push(timer_id);
        }
    }

    // Phase: drain handles.
    let handle_objects: Vec<HandleObject> = process
        .handles
        .drain()
        .into_iter()
        .map(|(obj, _, _)| obj)
        .collect();
    let mut events = Vec::new();
    let mut sched_ctxs = Vec::new();
    let handles = categorize_handles(handle_objects, &mut events, &mut sched_ctxs);

    // Phase: decide immediate vs deferred.
    let address_space_taken = if running_on_other_cores == 0 {
        true // Take process for immediate cleanup.
    } else {
        process.thread_count = running_on_other_cores;
        process.killed = true;
        false // Deferred.
    };

    KillResult {
        thread_ids: killed_threads,
        handles,
        address_space_taken,
        timeout_timers_collected: timeout_timers,
    }
}

#[test]
fn kill_no_running_threads_immediate_cleanup() {
    let mut process = Process::new();
    process.thread_count = 2;
    process.handles.insert(HandleObject::Channel(ChannelId(0)));

    let result = model_kill_process(
        &mut process,
        0, // No threads running on other cores.
        vec![(ThreadId(1), None), (ThreadId(2), Some(TimerId(5)))],
    );

    assert!(
        result.address_space_taken,
        "no running threads → immediate cleanup"
    );
    assert_eq!(result.thread_ids.len(), 2);
    assert_eq!(result.timeout_timers_collected, vec![TimerId(5)]);
    assert_eq!(result.handles.channels, vec![ChannelId(0)]);
}

#[test]
fn kill_with_running_threads_deferred_cleanup() {
    let mut process = Process::new();
    process.thread_count = 3;
    process.handles.insert(HandleObject::Timer(TimerId(1)));

    let result = model_kill_process(
        &mut process,
        2,                         // 2 threads still running on other cores.
        vec![(ThreadId(1), None)], // 1 non-running thread removed.
    );

    assert!(
        !result.address_space_taken,
        "running threads → deferred cleanup"
    );
    assert!(process.killed, "process marked as killed");
    assert_eq!(
        process.thread_count, 2,
        "thread_count set to running count for deferred tracking"
    );
}

#[test]
fn kill_collects_timeout_timers_from_all_queues() {
    let mut process = Process::new();
    process.thread_count = 4;

    let result = model_kill_process(
        &mut process,
        0,
        vec![
            (ThreadId(1), Some(TimerId(0))), // From ready queue.
            (ThreadId(2), Some(TimerId(1))), // From blocked list.
            (ThreadId(3), None),             // From suspended (no timer).
            (ThreadId(4), Some(TimerId(2))), // Another blocked.
        ],
    );

    assert_eq!(
        result.timeout_timers_collected,
        vec![TimerId(0), TimerId(1), TimerId(2)],
        "all timeout timers collected for destruction"
    );
}

#[test]
fn kill_already_killed_process_returns_none() {
    // Model: kill_process returns None if process.killed is true.
    let mut process = Process::new();
    process.thread_count = 1;
    process.killed = true;

    // In the real kernel, kill_process checks `process.killed` early and returns None.
    let should_kill = !process.killed && process.thread_count > 0;
    assert!(!should_kill, "already-killed process rejected");
}

#[test]
fn kill_process_with_zero_threads_returns_none() {
    // Model: kill_process returns None if thread_count is 0.
    let mut process = Process::new();
    process.thread_count = 0;
    process.killed = false;

    let should_kill = !process.killed && process.thread_count > 0;
    assert!(!should_kill, "zero-thread process rejected");
}

// ============================================================
// (10) Deferred cleanup: maybe_cleanup_killed_process
// ============================================================

/// Model the deferred reap logic in schedule_inner.
/// Each time a killed process's thread is reaped, thread_count decrements.
/// When it hits 0, the address space can be freed.
struct DeferredCleanup {
    thread_count: u32,
    killed: bool,
    address_space_freed: bool,
}

impl DeferredCleanup {
    fn new(running_count: u32) -> Self {
        Self {
            thread_count: running_count,
            killed: true,
            address_space_freed: false,
        }
    }

    /// Called when schedule_inner reaps an exited thread from a killed process.
    fn reap_thread(&mut self) -> bool {
        self.thread_count = self.thread_count.saturating_sub(1);
        if self.thread_count == 0 && self.killed {
            self.address_space_freed = true;
            true // Process fully cleaned up.
        } else {
            false
        }
    }
}

#[test]
fn deferred_cleanup_last_reap_frees_address_space() {
    let mut cleanup = DeferredCleanup::new(3);

    assert!(!cleanup.reap_thread()); // 2 remaining.
    assert!(!cleanup.address_space_freed);
    assert!(!cleanup.reap_thread()); // 1 remaining.
    assert!(!cleanup.address_space_freed);
    assert!(cleanup.reap_thread()); // 0 remaining → freed.
    assert!(cleanup.address_space_freed);
}

#[test]
fn deferred_cleanup_single_thread_immediate() {
    let mut cleanup = DeferredCleanup::new(1);

    assert!(cleanup.reap_thread());
    assert!(cleanup.address_space_freed);
}
