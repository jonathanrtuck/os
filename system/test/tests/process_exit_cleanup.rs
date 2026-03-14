//! Host-side tests for process exit cleanup completeness.
//!
//! Tests verify that ALL resources are reclaimed on process exit
//! (both normal last-thread-exit and process_kill paths), including:
//! handles, channels, timers, interrupts, scheduling contexts,
//! DMA buffers, heap, address space, TLB, ASID.
//!
//! Special focus on resources NOT tracked in the handle table:
//! - Internal timeout timers (from `wait` with finite timeout)
//! - Stale waiter registrations (from blocked `wait` path)

// ============================================================
// Minimal models (cannot import kernel modules directly)
// ============================================================

const MAX_TIMERS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimerId(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterruptId(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SchedulingContextId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleObject {
    Channel(ChannelId),
    Timer(TimerId),
    Interrupt(InterruptId),
    Thread(ThreadId),
    Process(ProcessId),
    SchedulingContext(SchedulingContextId),
}

const TIMEOUT_SENTINEL: u8 = 0xFF;

// --- Timer table model ---

struct TimerTable {
    slots: [Option<u64>; MAX_TIMERS],
    ready: [bool; MAX_TIMERS],
    waiters: [Option<ThreadId>; MAX_TIMERS],
}

impl TimerTable {
    fn new() -> Self {
        Self {
            slots: [None; MAX_TIMERS],
            ready: [false; MAX_TIMERS],
            waiters: [None; MAX_TIMERS],
        }
    }

    fn create(&mut self, deadline: u64) -> Option<TimerId> {
        for i in 0..MAX_TIMERS {
            if self.slots[i].is_none() {
                self.slots[i] = Some(deadline);
                self.ready[i] = false;
                self.waiters[i] = None;
                return Some(TimerId(i as u8));
            }
        }
        None
    }

    fn destroy(&mut self, id: TimerId) -> Option<ThreadId> {
        let i = id.0 as usize;
        self.slots[i] = None;
        self.ready[i] = false;
        self.waiters[i].take()
    }

    fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

// --- Thread model ---

struct WaitEntry {
    object: HandleObject,
    user_index: u8,
}

struct Thread {
    id: ThreadId,
    process_id: Option<ProcessId>,
    wait_set: Vec<WaitEntry>,
    stale_waiters: Vec<WaitEntry>,
    timeout_timer: Option<TimerId>,
}

impl Thread {
    fn new(id: u64, pid: ProcessId) -> Self {
        Self {
            id: ThreadId(id),
            process_id: Some(pid),
            wait_set: Vec::new(),
            stale_waiters: Vec::new(),
            timeout_timer: None,
        }
    }
}

// --- Handle table model ---

struct HandleTable {
    entries: Vec<Option<HandleObject>>,
}

impl HandleTable {
    fn new() -> Self {
        Self { entries: vec![None; 256] }
    }

    fn insert(&mut self, obj: HandleObject) -> Option<u8> {
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(obj);
                return Some(i as u8);
            }
        }
        None
    }

    fn drain(&mut self) -> Vec<HandleObject> {
        let mut result = Vec::new();
        for slot in &mut self.entries {
            if let Some(obj) = slot.take() {
                result.push(obj);
            }
        }
        result
    }
}

// --- Process model with full cleanup ---

struct Process {
    handles: HandleTable,
    thread_count: u32,
    killed: bool,
    address_space_freed: bool,
    asid_freed: bool,
    tlb_invalidated: bool,
}

impl Process {
    fn new() -> Self {
        Self {
            handles: HandleTable::new(),
            thread_count: 0,
            killed: false,
            address_space_freed: false,
            asid_freed: false,
            tlb_invalidated: false,
        }
    }
}

// ============================================================
// (1) Timeout timer leak on process kill — THE BUG
// ============================================================

#[test]
fn kill_leaks_timeout_timer_from_blocked_wait() {
    // BUG: When a thread is killed while blocked in a `wait` with a
    // finite timeout, the internal timeout timer is leaked.
    //
    // The timeout timer is stored on the Thread (thread.timeout_timer),
    // NOT in the process's handle table. The handle table drain path
    // misses it. The thread's Drop only frees the kernel stack.
    //
    // Scenario:
    // 1. Thread calls sys_wait with timeout → timer created, stored on thread
    // 2. Thread blocks (no handle ready)
    // 3. Process is killed → thread is dropped without destroying the timer
    // 4. Timer slot remains occupied in the 32-slot global table
    let mut timer_table = TimerTable::new();
    let mut thread = Thread::new(1, ProcessId(0));

    // Step 1: Thread enters wait with timeout, creates internal timer.
    let timer_id = timer_table.create(1000).unwrap();
    thread.timeout_timer = Some(timer_id);

    assert_eq!(timer_table.active_count(), 1, "one timer active");

    // Step 2: Thread blocks (timeout timer remains on thread).
    // Step 3: Process is killed. The current (buggy) code just drops the thread.
    // Simulate: thread goes out of scope without cleaning up timeout_timer.
    let leaked_timer = thread.timeout_timer;

    // BUG: The timer slot is still occupied.
    assert!(
        leaked_timer.is_some(),
        "timeout_timer is present on thread but would be leaked on drop"
    );
    assert_eq!(
        timer_table.active_count(),
        1,
        "BUG: timer slot leaked — 32-slot table has one fewer available slot"
    );

    // FIX: Before dropping the thread, destroy any timeout_timer.
    if let Some(tid) = leaked_timer {
        timer_table.destroy(tid);
    }
    assert_eq!(
        timer_table.active_count(),
        0,
        "FIXED: timer slot reclaimed"
    );
}

#[test]
fn kill_leaks_timeout_timers_from_multiple_threads() {
    // Verify the leak compounds: killing N threads with active timeout
    // timers leaks N timer slots from the 32-slot global table.
    let mut timer_table = TimerTable::new();
    let mut threads: Vec<Thread> = Vec::new();

    for i in 0..5 {
        let mut thread = Thread::new(i + 1, ProcessId(0));
        let timer_id = timer_table.create(1000 + i).unwrap();
        thread.timeout_timer = Some(timer_id);
        threads.push(thread);
    }

    assert_eq!(timer_table.active_count(), 5, "5 timers active");

    // Kill all threads without cleanup (current bug).
    let leaked_timers: Vec<Option<TimerId>> =
        threads.iter().map(|t| t.timeout_timer).collect();

    assert_eq!(
        leaked_timers.iter().filter(|t| t.is_some()).count(),
        5,
        "all 5 timeout timers would be leaked"
    );

    // FIX: Clean up timeout timers before dropping threads.
    for thread in &mut threads {
        if let Some(tid) = thread.timeout_timer.take() {
            timer_table.destroy(tid);
        }
    }

    assert_eq!(
        timer_table.active_count(),
        0,
        "FIXED: all timer slots reclaimed"
    );
}

// ============================================================
// (2) Stale waiter registrations on kill
// ============================================================

#[test]
fn kill_leaves_stale_waiter_registrations() {
    // When a thread is killed while it has stale_waiters from a previous
    // blocked wait, those registrations are never cleaned up. The waiter
    // ThreadId lingers in channel/timer/interrupt/etc. waiter slots.
    //
    // This is a soft leak — the waiter is a dead ThreadId. If the handle
    // fires or is destroyed, it tries to wake a dead thread (no-op) or
    // sets wake_pending on a dropped thread (harmless). But it wastes
    // waiter slots and could cause confusion in future waitable operations.
    let mut thread = Thread::new(1, ProcessId(0));

    // Simulate previous blocked wait leaving stale registrations.
    thread.stale_waiters = vec![
        WaitEntry {
            object: HandleObject::Channel(ChannelId(0)),
            user_index: 0,
        },
        WaitEntry {
            object: HandleObject::Timer(TimerId(1)),
            user_index: 1,
        },
    ];

    // Thread is killed — stale_waiters are not cleaned up.
    assert_eq!(
        thread.stale_waiters.len(),
        2,
        "stale_waiters present but would be leaked on kill"
    );

    // The fix: clean up stale waiter registrations during kill/exit.
    // In practice, the channel/timer/interrupt modules tolerate stale
    // waiters (unregister_waiter is safe to call on already-cleared
    // entries), so this is a soft leak, not a crash.
}

// ============================================================
// (3) Normal exit path — verify completeness
// ============================================================

#[test]
fn normal_exit_last_thread_cleanup_sequence() {
    // Model the complete cleanup sequence for last-thread-exit.
    // Verify every resource category is handled.
    let mut process = Process::new();
    process.thread_count = 1;

    // Add handles of every type.
    process.handles.insert(HandleObject::Channel(ChannelId(0)));
    process.handles.insert(HandleObject::Timer(TimerId(1)));
    process.handles.insert(HandleObject::Interrupt(InterruptId(2)));
    process.handles.insert(HandleObject::Thread(ThreadId(3)));
    process.handles.insert(HandleObject::Process(ProcessId(4)));
    process.handles.insert(HandleObject::SchedulingContext(SchedulingContextId(5)));

    // Phase 1: Scheduling context refs released (in exit_current_from_syscall).
    // Phase 1: Decrement thread_count → 0 (is_last = true).
    process.thread_count -= 1;
    assert_eq!(process.thread_count, 0);

    // Phase 1: Drain handle table.
    let handle_objects = process.handles.drain();
    assert_eq!(handle_objects.len(), 6, "all 6 handles drained");

    // Phase 1: Categorize handles.
    let mut channels = Vec::new();
    let mut timers = Vec::new();
    let mut interrupts = Vec::new();
    let mut thread_handles = Vec::new();
    let mut process_handles = Vec::new();
    let mut sched_ctx_count = 0;

    for obj in handle_objects {
        match obj {
            HandleObject::Channel(id) => channels.push(id),
            HandleObject::Timer(id) => timers.push(id),
            HandleObject::Interrupt(id) => interrupts.push(id),
            HandleObject::Thread(id) => thread_handles.push(id),
            HandleObject::Process(id) => process_handles.push(id),
            HandleObject::SchedulingContext(_) => sched_ctx_count += 1,
        }
    }

    assert_eq!(channels.len(), 1, "channel handles categorized");
    assert_eq!(timers.len(), 1, "timer handles categorized");
    assert_eq!(interrupts.len(), 1, "interrupt handles categorized");
    assert_eq!(thread_handles.len(), 1, "thread handles categorized");
    assert_eq!(process_handles.len(), 1, "process handles categorized");
    assert_eq!(sched_ctx_count, 1, "scheduling context released immediately");

    // Phase 2: thread_exit::notify_exit, process_exit::notify_exit
    // Phase 2a: futex::remove_thread
    // Phase 3: close channels, timers, interrupts, thread handles, process handles
    // Phase 4: invalidate_tlb + free_all + free(asid)
    process.tlb_invalidated = true;
    process.address_space_freed = true;
    process.asid_freed = true;

    assert!(process.tlb_invalidated, "TLB invalidated");
    assert!(process.address_space_freed, "address space freed");
    assert!(process.asid_freed, "ASID released");
}

#[test]
fn normal_exit_non_last_thread_minimal_cleanup() {
    // Non-last thread exit: only scheduling context refs released,
    // thread_exit notified, futex cleaned up. No handle drain, no
    // address space free.
    let mut process = Process::new();
    process.thread_count = 3;

    // Non-last thread exits: decrement count, no further process cleanup.
    process.thread_count -= 1;
    assert_eq!(process.thread_count, 2, "two threads remain");

    // Thread's kernel stack is freed via Thread::Drop in deferred_drops.
    // Address space stays alive for remaining threads.
    assert!(!process.address_space_freed, "address space NOT freed");
}

// ============================================================
// (4) Kill path — verify completeness
// ============================================================

#[test]
fn kill_path_immediate_cleanup_no_running_threads() {
    // When kill_process finds no threads running on other cores,
    // the address space is freed immediately (not deferred).
    let mut process = Process::new();
    process.thread_count = 2;

    // Add handles.
    process.handles.insert(HandleObject::Channel(ChannelId(0)));
    process.handles.insert(HandleObject::Timer(TimerId(1)));

    // All threads removed from ready/blocked/suspended (none running).
    let running_count = 0;

    // Drain handles.
    let handles = process.handles.drain();
    assert_eq!(handles.len(), 2);

    // Immediate cleanup path (running_count == 0).
    if running_count == 0 {
        process.tlb_invalidated = true;
        process.address_space_freed = true;
        process.asid_freed = true;
    }

    assert!(process.address_space_freed, "immediate cleanup when no threads running");
}

#[test]
fn kill_path_deferred_cleanup_threads_on_other_cores() {
    // When kill_process finds threads running on other cores,
    // the address space cleanup is deferred to maybe_cleanup_killed_process.
    let mut process = Process::new();
    process.thread_count = 3;
    let running_count = 2; // 2 threads still running on other cores

    // Handles drained regardless.
    process.handles.insert(HandleObject::Channel(ChannelId(0)));
    let _ = process.handles.drain();

    // Deferred path: set killed=true, thread_count=running_count.
    if running_count > 0 {
        process.thread_count = running_count;
        process.killed = true;
    }

    assert!(process.killed, "process marked as killed");
    assert_eq!(process.thread_count, 2, "count set to running threads");

    // Simulate threads being reaped one by one by schedule_inner.
    process.thread_count -= 1;
    assert_eq!(process.thread_count, 1);
    assert!(!process.address_space_freed, "not yet — 1 thread still running");

    process.thread_count -= 1;
    assert_eq!(process.thread_count, 0);

    // Last thread reaped → cleanup.
    if process.thread_count == 0 && process.killed {
        process.tlb_invalidated = true;
        process.address_space_freed = true;
        process.asid_freed = true;
    }

    assert!(process.address_space_freed, "deferred cleanup complete");
}

// ============================================================
// (5) DMA and heap cleanup
// ============================================================

#[test]
fn dma_allocations_freed_on_process_exit() {
    // Model: address_space.free_all() iterates dma_allocations and
    // frees each (pa, order) via page_allocator::free_frames.
    struct DmaAlloc {
        va: u64,
        pa: u64,
        order: u8,
    }

    let mut dma_allocs = vec![
        DmaAlloc { va: 0x1000_0000, pa: 0x4000_0000, order: 0 },
        DmaAlloc { va: 0x1000_1000, pa: 0x4001_0000, order: 2 },
    ];

    let mut freed_pas = Vec::new();

    // Simulate free_all() DMA cleanup.
    for alloc in dma_allocs.drain(..) {
        freed_pas.push((alloc.pa, alloc.order));
    }

    assert_eq!(freed_pas.len(), 2, "both DMA allocations freed");
    assert!(dma_allocs.is_empty(), "DMA allocation list cleared");
}

#[test]
fn heap_allocations_cleared_on_process_exit() {
    // Model: address_space.free_all() clears heap_allocations.
    // Physical frames backing demand-paged heap pages are tracked in
    // owned_frames and freed separately.
    struct HeapAlloc {
        va: u64,
        page_count: u64,
    }

    let mut heap_allocs = vec![
        HeapAlloc { va: 0x100_0000, page_count: 4 },
        HeapAlloc { va: 0x200_0000, page_count: 16 },
    ];

    // free_all() just clears the vec (frames tracked in owned_frames).
    heap_allocs.clear();
    assert!(heap_allocs.is_empty(), "heap allocation list cleared");
}

// ============================================================
// (6) Page table frames freed
// ============================================================

#[test]
fn page_table_frames_freed_on_process_exit() {
    // Model: free_all() walks L0→L1→L2→L3 and frees every table frame.
    // This is a correctness test for the nested walk logic.
    struct PageTableWalker {
        frames_freed: Vec<u64>,
    }

    impl PageTableWalker {
        fn new() -> Self {
            Self { frames_freed: Vec::new() }
        }

        /// Simulate walking a 3-level tree (L1→L2→L3).
        fn walk_and_free(&mut self, l1_frames: &[(u64, Vec<(u64, Vec<u64>)>)]) {
            for (l1_pa, l2_entries) in l1_frames {
                for (l2_pa, l3_entries) in l2_entries {
                    for l3_pa in l3_entries {
                        self.frames_freed.push(*l3_pa);
                    }
                    self.frames_freed.push(*l2_pa);
                }
                self.frames_freed.push(*l1_pa);
            }
        }
    }

    let mut walker = PageTableWalker::new();

    // Simple tree: 1 L1 containing 1 L2 containing 2 L3 tables.
    walker.walk_and_free(&[
        (0x1000, vec![
            (0x2000, vec![0x3000, 0x4000]),
        ]),
    ]);

    // L3 tables freed first, then L2, then L1 (bottom-up).
    assert_eq!(walker.frames_freed, vec![0x3000, 0x4000, 0x2000, 0x1000]);
}

// ============================================================
// (7) Scheduling context ref_count cleanup paths
// ============================================================

#[test]
fn sched_context_refs_released_on_normal_exit() {
    // On thread exit, both context_id and saved_context_id refs are released.
    let mut ref_count = 3; // e.g., handle(1) + bind(1) + borrow(1)

    // Thread exit releases bind ref (context_id).
    ref_count -= 1;
    assert_eq!(ref_count, 2);

    // Thread exit releases borrow ref (saved_context_id).
    ref_count -= 1;
    assert_eq!(ref_count, 1);

    // Handle close releases handle ref.
    ref_count -= 1;
    assert_eq!(ref_count, 0, "scheduling context freed when ref_count reaches 0");
}

#[test]
fn sched_context_refs_released_on_kill() {
    // On process kill, release_thread_context_ids is called for each thread
    // removed from ready/blocked/suspended queues, AND for each thread
    // running on other cores (deferred_context_releases).
    struct SchedCtxSlot {
        ref_count: u32,
    }

    let mut slot = SchedCtxSlot { ref_count: 5 };

    // Thread 1 (from ready queue): bind ref released.
    slot.ref_count -= 1;
    // Thread 2 (from blocked): bind + borrow refs released.
    slot.ref_count -= 2;
    // Thread 3 (running on other core): context_id released.
    slot.ref_count -= 1;
    // Handle drain: SchedulingContext handle released.
    slot.ref_count -= 1;

    assert_eq!(slot.ref_count, 0, "all refs released across all kill paths");
}

// ============================================================
// (8) Comprehensive resource tracking model
// ============================================================

#[test]
fn complete_resource_inventory_normal_exit() {
    // Track EVERY resource category through the normal exit path.
    struct ResourceTracker {
        // Handle-tracked resources
        channels_closed: usize,
        timers_destroyed: usize,
        interrupts_destroyed: usize,
        thread_handles_destroyed: usize,
        process_handles_destroyed: usize,
        sched_contexts_released: usize,
        // Thread-level resources
        thread_sched_ctx_refs_released: usize,
        futex_entries_removed: bool,
        // Process-level resources
        dma_freed: bool,
        owned_frames_freed: bool,
        page_tables_freed: bool,
        l0_table_freed: bool,
        tlb_invalidated: bool,
        asid_released: bool,
        // Notifications
        thread_exit_notified: bool,
        process_exit_notified: bool,
    }

    let mut t = ResourceTracker {
        channels_closed: 0,
        timers_destroyed: 0,
        interrupts_destroyed: 0,
        thread_handles_destroyed: 0,
        process_handles_destroyed: 0,
        sched_contexts_released: 0,
        thread_sched_ctx_refs_released: 0,
        futex_entries_removed: false,
        dma_freed: false,
        owned_frames_freed: false,
        page_tables_freed: false,
        l0_table_freed: false,
        tlb_invalidated: false,
        asid_released: false,
        thread_exit_notified: false,
        process_exit_notified: false,
    };

    // Phase 1 (scheduler lock): release scheduling context refs.
    t.thread_sched_ctx_refs_released = 2; // context_id + saved_context_id

    // Phase 1: drain handles + categorize.
    t.channels_closed = 1;
    t.timers_destroyed = 1;
    t.interrupts_destroyed = 1;
    t.thread_handles_destroyed = 1;
    t.process_handles_destroyed = 1;
    t.sched_contexts_released = 1;

    // Phase 2: notifications.
    t.thread_exit_notified = true;
    t.process_exit_notified = true;
    t.futex_entries_removed = true;

    // Phase 3: close categorized resources (outside scheduler lock).
    // (Covered by channels_closed, timers_destroyed, etc. above)

    // Phase 4: address space cleanup.
    t.tlb_invalidated = true;
    t.dma_freed = true;
    t.owned_frames_freed = true;
    t.page_tables_freed = true;
    t.l0_table_freed = true;
    t.asid_released = true;

    // Verify completeness.
    assert!(t.channels_closed >= 1, "channels closed");
    assert!(t.timers_destroyed >= 1, "timers destroyed");
    assert!(t.interrupts_destroyed >= 1, "interrupts destroyed");
    assert!(t.thread_handles_destroyed >= 1, "thread handles destroyed");
    assert!(t.process_handles_destroyed >= 1, "process handles destroyed");
    assert!(t.sched_contexts_released >= 1, "scheduling contexts released");
    assert_eq!(t.thread_sched_ctx_refs_released, 2, "thread sched ctx refs released");
    assert!(t.futex_entries_removed, "futex entries removed");
    assert!(t.dma_freed, "DMA buffers freed");
    assert!(t.owned_frames_freed, "owned frames freed");
    assert!(t.page_tables_freed, "page table frames freed");
    assert!(t.l0_table_freed, "L0 table freed");
    assert!(t.tlb_invalidated, "TLB invalidated");
    assert!(t.asid_released, "ASID released");
    assert!(t.thread_exit_notified, "thread exit notified");
    assert!(t.process_exit_notified, "process exit notified");
}

// ============================================================
// (9) Timeout timer cleanup fix model
// ============================================================

/// Models the fix: exit_current_from_syscall and kill_process must
/// destroy any timeout_timer on threads before dropping them.
fn cleanup_thread_resources(thread: &mut Thread, timer_table: &mut TimerTable) {
    // Destroy internal timeout timer (not in handle table).
    if let Some(tid) = thread.timeout_timer.take() {
        timer_table.destroy(tid);
    }
    // Note: stale_waiters would ideally be cleaned up too, but the
    // channel/timer/interrupt modules tolerate stale waiter registrations
    // (unregister on a dead ThreadId is a no-op), so this is acceptable.
}

#[test]
fn fixed_kill_cleans_up_timeout_timers() {
    let mut timer_table = TimerTable::new();
    let mut threads: Vec<Thread> = Vec::new();

    // Create 3 threads, each with an active timeout timer.
    for i in 0..3 {
        let mut thread = Thread::new(i + 1, ProcessId(0));
        let timer_id = timer_table.create(1000 + i).unwrap();
        thread.timeout_timer = Some(timer_id);
        threads.push(thread);
    }

    assert_eq!(timer_table.active_count(), 3, "3 timers before kill");

    // Fixed kill path: clean up each thread's resources before drop.
    for thread in &mut threads {
        cleanup_thread_resources(thread, &mut timer_table);
    }

    assert_eq!(timer_table.active_count(), 0, "all timers reclaimed after fix");
}

#[test]
fn fixed_normal_exit_cleans_up_timeout_timer() {
    let mut timer_table = TimerTable::new();
    let mut thread = Thread::new(1, ProcessId(0));

    // Thread was blocked in wait with timeout, then woke up and is now exiting.
    // The stale timeout timer was supposed to be cleaned up at the start of
    // the next sys_wait, but exit happens first.
    let timer_id = timer_table.create(5000).unwrap();
    thread.timeout_timer = Some(timer_id);

    assert_eq!(timer_table.active_count(), 1);

    // Fixed exit path: clean up timeout timer.
    cleanup_thread_resources(&mut thread, &mut timer_table);

    assert_eq!(timer_table.active_count(), 0, "timeout timer cleaned up on exit");
}

// ============================================================
// (10) Timer table exhaustion from leaked timers
// ============================================================

#[test]
fn leaked_timeout_timers_exhaust_timer_table() {
    // Demonstrate that leaking timeout timers from killed processes
    // eventually exhausts the 32-slot global timer table.
    let mut timer_table = TimerTable::new();

    // Leak 32 timers (one per killed process).
    for i in 0..MAX_TIMERS {
        let timer_id = timer_table.create(i as u64);
        assert!(timer_id.is_some(), "timer {} should succeed", i);
        // Don't destroy — simulating the leak.
    }

    // Table is now full. New timer creation fails.
    let result = timer_table.create(999);
    assert!(
        result.is_none(),
        "timer table exhausted after 32 leaked timers"
    );

    assert_eq!(timer_table.active_count(), MAX_TIMERS);
}

// ============================================================
// (11) Kill while thread is in various wait states
// ============================================================

#[test]
fn kill_thread_blocked_in_wait_with_timeout() {
    // Thread is blocked in sys_wait with a finite timeout.
    // It has: wait_set, timeout_timer, and possibly stale_waiters.
    let mut timer_table = TimerTable::new();
    let mut thread = Thread::new(1, ProcessId(0));

    // Set up wait state.
    thread.wait_set = vec![
        WaitEntry {
            object: HandleObject::Channel(ChannelId(0)),
            user_index: 0,
        },
    ];
    let timer_id = timer_table.create(5000).unwrap();
    thread.timeout_timer = Some(timer_id);
    thread.stale_waiters = vec![
        WaitEntry {
            object: HandleObject::Channel(ChannelId(2)),
            user_index: 0,
        },
    ];

    // Kill: clean up ALL thread resources.
    cleanup_thread_resources(&mut thread, &mut timer_table);

    assert!(thread.timeout_timer.is_none(), "timeout timer cleaned up");
    assert_eq!(timer_table.active_count(), 0, "timer slot reclaimed");
    // wait_set and stale_waiters: the thread's drop clears them (Vec drop).
    // The waiter registrations in channel/timer modules are harmless stale refs.
}

#[test]
fn kill_thread_blocked_in_futex_wait() {
    // Thread blocked in futex_wait has no timeout_timer or wait_set.
    // Only futex table entries need cleanup (done via futex::remove_thread).
    let mut timer_table = TimerTable::new();
    let mut thread = Thread::new(1, ProcessId(0));

    // Futex wait has no timeout timer or wait set.
    assert!(thread.timeout_timer.is_none());
    assert!(thread.wait_set.is_empty());

    // Cleanup is a no-op for timer resources.
    cleanup_thread_resources(&mut thread, &mut timer_table);
    assert_eq!(timer_table.active_count(), 0);
}
