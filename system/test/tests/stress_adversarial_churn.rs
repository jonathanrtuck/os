//! Adversarial tests for rapid create/destroy cycles.
//!
//! Exercises 100+ cycles each of: channel create/close, timer create/close,
//! process create/kill, thread create/exit, scheduling context create/close.
//! Verifies no leaks, panics, or corruption after each cycle batch.
//!
//! These tests model kernel resource management logic on the host. The kernel
//! targets aarch64-unknown-none so we cannot import it directly — instead we
//! faithfully replicate the resource lifecycle logic and verify invariants:
//! resource counts return to baseline after each batch, slot reuse works
//! correctly, and no state corruption occurs.
//!
//! Fulfills: VAL-FUZZ-004 (rapid create/destroy cycles)
//!
//! Run with: cargo test --test adversarial_churn -- --test-threads=1

// --- Stubs for kernel types ---

#[path = "../../kernel/paging.rs"]
mod paging;

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}

#[path = "../../kernel/handle.rs"]
mod handle;

#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;

mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

use handle::*;
use scheduling_context::*;

// ==========================================================================
// Host-side models of kernel resource managers
// ==========================================================================
// These models replicate the kernel's resource allocation/deallocation logic
// so we can verify invariants on the host.

// ---------------------------------------------------------------------------
// Channel model (mirrors kernel/channel.rs)
// ---------------------------------------------------------------------------

/// Physical address stub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pa(u64);

/// Models one channel (two endpoints, two shared pages).
struct ModelChannel {
    pages: [Pa; 2],
    closed_count: u8,
}

/// Models the global channel table (mirrors channel::STATE).
struct ChannelTable {
    channels: Vec<ModelChannel>,
    /// Track allocated pages to verify no leaks.
    page_counter: u64,
    /// Number of pages currently allocated (not freed).
    live_pages: usize,
}

impl ChannelTable {
    fn new() -> Self {
        Self {
            channels: Vec::new(),
            page_counter: 0,
            live_pages: 0,
        }
    }

    /// Allocate a physical page (models page_allocator::alloc_frame).
    fn alloc_page(&mut self) -> Pa {
        self.page_counter += 1;
        self.live_pages += 1;
        Pa(self.page_counter)
    }

    /// Free a physical page (models page_allocator::free_frame).
    fn free_page(&mut self, _pa: Pa) {
        assert!(self.live_pages > 0, "double free detected");
        self.live_pages -= 1;
    }

    /// Create a channel. Returns (ep0_id, ep1_id).
    fn create(&mut self) -> (ChannelId, ChannelId) {
        let page0 = self.alloc_page();
        let page1 = self.alloc_page();
        let idx = self.channels.len() as u32;
        self.channels.push(ModelChannel {
            pages: [page0, page1],
            closed_count: 0,
        });
        (ChannelId(idx * 2), ChannelId(idx * 2 + 1))
    }

    /// Close one endpoint. Frees pages when both endpoints are closed.
    fn close_endpoint(&mut self, id: ChannelId) {
        let ch_idx = id.0 as usize / 2;
        let ch = &mut self.channels[ch_idx];

        // Already fully closed — prevent double-free (matches kernel guard).
        if ch.closed_count >= 2 {
            return;
        }

        ch.closed_count += 1;

        if ch.closed_count == 2 {
            let pages = ch.pages;
            ch.pages = [Pa(0), Pa(0)];
            self.free_page(pages[0]);
            self.free_page(pages[1]);
        }
    }
}

// ---------------------------------------------------------------------------
// Timer model (mirrors kernel/timer.rs)
// ---------------------------------------------------------------------------

const MAX_TIMERS: usize = 32;

/// Models the global timer table.
struct TimerTable {
    /// Slot occupied = Some(deadline_ticks). None = free.
    slots: [Option<u64>; MAX_TIMERS],
}

impl TimerTable {
    fn new() -> Self {
        Self {
            slots: [None; MAX_TIMERS],
        }
    }

    /// Create a timer. Returns TimerId or None if table is full.
    fn create(&mut self, deadline: u64) -> Option<timer::TimerId> {
        for i in 0..MAX_TIMERS {
            if self.slots[i].is_none() {
                self.slots[i] = Some(deadline);
                return Some(timer::TimerId(i as u8));
            }
        }
        None
    }

    /// Destroy a timer.
    fn destroy(&mut self, id: timer::TimerId) {
        self.slots[id.0 as usize] = None;
    }

    /// Count of active (occupied) timer slots.
    fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

// ---------------------------------------------------------------------------
// Process model (mirrors kernel/process.rs lifecycle)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum ProcessState {
    Created,
    Running,
    Exited,
}

struct ModelProcess {
    id: process::ProcessId,
    state: ProcessState,
    /// Track associated resources (handles, threads) for cleanup verification.
    handle_count: u32,
    thread_count: u32,
}

/// Models the global process table.
struct ProcessTable {
    processes: Vec<ModelProcess>,
    /// Track how many processes are alive (not exited).
    live_count: usize,
}

impl ProcessTable {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            live_count: 0,
        }
    }

    /// Create a new process. Returns ProcessId.
    fn create(&mut self) -> process::ProcessId {
        let id = process::ProcessId(self.processes.len() as u32);
        self.processes.push(ModelProcess {
            id,
            state: ProcessState::Created,
            handle_count: 0,
            thread_count: 1, // Initial thread.
        });
        self.live_count += 1;
        id
    }

    /// Start a process.
    fn start(&mut self, id: process::ProcessId) -> Result<(), &'static str> {
        let p = &mut self.processes[id.0 as usize];
        match p.state {
            ProcessState::Created => {
                p.state = ProcessState::Running;
                Ok(())
            }
            ProcessState::Running => Err("already running"),
            ProcessState::Exited => Err("already exited"),
        }
    }

    /// Kill a process. Simulates resource cleanup.
    fn kill(&mut self, id: process::ProcessId) -> Result<(), &'static str> {
        let p = &mut self.processes[id.0 as usize];
        match p.state {
            ProcessState::Exited => Err("already exited"),
            _ => {
                p.state = ProcessState::Exited;
                p.handle_count = 0;
                p.thread_count = 0;
                self.live_count -= 1;
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Thread model (mirrors kernel/thread.rs lifecycle)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Exited,
}

struct ModelThread {
    id: thread::ThreadId,
    state: ThreadState,
}

/// Models the global thread table.
struct ThreadTable {
    threads: Vec<ModelThread>,
    next_id: u64,
    /// Count of threads that haven't exited.
    live_count: usize,
}

impl ThreadTable {
    fn new() -> Self {
        Self {
            threads: Vec::new(),
            next_id: 0,
            live_count: 0,
        }
    }

    /// Create a new thread. Returns ThreadId.
    fn create(&mut self) -> thread::ThreadId {
        let id = thread::ThreadId(self.next_id);
        self.next_id += 1;
        self.threads.push(ModelThread {
            id,
            state: ThreadState::Ready,
        });
        self.live_count += 1;
        id
    }

    /// Exit a thread. Simulates thread cleanup.
    fn exit(&mut self, id: thread::ThreadId) -> Result<(), &'static str> {
        let idx = self
            .threads
            .iter()
            .position(|t| t.id == id)
            .ok_or("thread not found")?;
        let t = &mut self.threads[idx];
        match t.state {
            ThreadState::Exited => Err("already exited"),
            _ => {
                t.state = ThreadState::Exited;
                self.live_count -= 1;
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduling context model (mirrors kernel/scheduling_context.rs + scheduler.rs)
// ---------------------------------------------------------------------------

const MAX_SCHED_CONTEXTS: usize = 32;

struct SchedContextEntry {
    ctx: SchedulingContext,
    ref_count: u32,
}

/// Models the scheduler's scheduling context table.
struct SchedContextTable {
    slots: Vec<Option<SchedContextEntry>>,
    /// Count of live (non-freed) contexts.
    live_count: usize,
}

impl SchedContextTable {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            live_count: 0,
        }
    }

    /// Create a scheduling context. Returns id or None if full.
    fn create(&mut self, budget: u64, period: u64) -> Option<SchedulingContextId> {
        if !validate_params(budget, period) {
            return None;
        }

        // Find a free slot or append.
        for i in 0..self.slots.len() {
            if self.slots[i].is_none() {
                self.slots[i] = Some(SchedContextEntry {
                    ctx: SchedulingContext::new(budget, period, 0),
                    ref_count: 1,
                });
                self.live_count += 1;
                return Some(SchedulingContextId(i as u32));
            }
        }

        if self.slots.len() < MAX_SCHED_CONTEXTS {
            let id = SchedulingContextId(self.slots.len() as u32);
            self.slots.push(Some(SchedContextEntry {
                ctx: SchedulingContext::new(budget, period, 0),
                ref_count: 1,
            }));
            self.live_count += 1;
            return Some(id);
        }

        None
    }

    /// Release a scheduling context (decrement ref_count, free when 0).
    fn release(&mut self, id: SchedulingContextId) {
        if let Some(Some(entry)) = self.slots.get_mut(id.0 as usize) {
            entry.ref_count = entry.ref_count.saturating_sub(1);
            if entry.ref_count == 0 {
                self.slots[id.0 as usize] = None;
                self.live_count -= 1;
            }
        }
    }
}

// ==========================================================================
// SECTION 1: Channel create/close churn (100+ cycles)
// ==========================================================================

/// Rapid channel create/close cycle. Each iteration creates a channel (two
/// endpoints, two shared pages) and immediately closes both endpoints.
/// Verifies page count returns to baseline after each cycle.
#[test]
fn churn_channel_create_close_100_cycles() {
    let mut ct = ChannelTable::new();

    for cycle in 0..150u32 {
        let baseline_pages = ct.live_pages;

        // Create a channel (allocates 2 pages).
        let (ep0, ep1) = ct.create();

        assert_eq!(
            ct.live_pages,
            baseline_pages + 2,
            "cycle {cycle}: channel create should allocate 2 pages"
        );

        // Close both endpoints.
        ct.close_endpoint(ep0);
        ct.close_endpoint(ep1);

        assert_eq!(
            ct.live_pages, baseline_pages,
            "cycle {cycle}: after closing both endpoints, pages should be freed"
        );
    }

    // Final invariant: all pages freed.
    assert_eq!(
        ct.live_pages, 0,
        "all pages should be freed after all cycles"
    );
}

/// Channel churn with close in reverse order (endpoint 1 first, then 0).
#[test]
fn churn_channel_close_reverse_order_100_cycles() {
    let mut ct = ChannelTable::new();

    for cycle in 0..150u32 {
        let (ep0, ep1) = ct.create();

        // Close in reverse order.
        ct.close_endpoint(ep1);
        ct.close_endpoint(ep0);

        assert_eq!(
            ct.live_pages, 0,
            "cycle {cycle}: pages should be freed regardless of close order"
        );
    }
}

/// Channel churn: create many, then close all. Verifies batch cleanup.
#[test]
fn churn_channel_batch_create_then_close() {
    let mut ct = ChannelTable::new();
    let mut endpoints = Vec::new();

    // Create 100 channels at once.
    for _ in 0..100 {
        let (ep0, ep1) = ct.create();
        endpoints.push((ep0, ep1));
    }

    assert_eq!(ct.live_pages, 200, "100 channels = 200 pages");

    // Close all.
    for (ep0, ep1) in endpoints {
        ct.close_endpoint(ep0);
        ct.close_endpoint(ep1);
    }

    assert_eq!(ct.live_pages, 0, "all pages freed after batch close");
}

/// Channel churn: double-close of endpoints is harmless.
#[test]
fn churn_channel_double_close_150_cycles() {
    let mut ct = ChannelTable::new();

    for cycle in 0..150u32 {
        let (ep0, ep1) = ct.create();

        ct.close_endpoint(ep0);
        ct.close_endpoint(ep1);

        // Double-close should be harmless (guard in close_endpoint).
        ct.close_endpoint(ep0);
        ct.close_endpoint(ep1);

        assert_eq!(
            ct.live_pages, 0,
            "cycle {cycle}: double-close should not corrupt state"
        );
    }
}

/// Channel churn: interleaved create and close.
#[test]
fn churn_channel_interleaved_create_close() {
    let mut ct = ChannelTable::new();
    let mut pending: Vec<(ChannelId, ChannelId)> = Vec::new();

    for cycle in 0..200u32 {
        // Create a new channel.
        let eps = ct.create();
        pending.push(eps);

        // Every 3rd cycle, close the oldest pending channel.
        if cycle % 3 == 0 && !pending.is_empty() {
            let (ep0, ep1) = pending.remove(0);
            ct.close_endpoint(ep0);
            ct.close_endpoint(ep1);
        }
    }

    // Close all remaining.
    for (ep0, ep1) in pending {
        ct.close_endpoint(ep0);
        ct.close_endpoint(ep1);
    }

    assert_eq!(ct.live_pages, 0, "all pages freed after interleaved churn");
}

// ==========================================================================
// SECTION 2: Timer create/close churn (100+ cycles)
// ==========================================================================

/// Rapid timer create/destroy cycle. Each iteration creates a timer and
/// immediately destroys it. Verifies active count returns to baseline.
#[test]
fn churn_timer_create_destroy_150_cycles() {
    let mut tt = TimerTable::new();

    for cycle in 0..150u64 {
        let baseline = tt.active_count();

        let id = tt
            .create(cycle * 1000)
            .expect("timer table should not be full");

        assert_eq!(
            tt.active_count(),
            baseline + 1,
            "cycle {cycle}: timer create should occupy one slot"
        );

        tt.destroy(id);

        assert_eq!(
            tt.active_count(),
            baseline,
            "cycle {cycle}: timer destroy should free the slot"
        );
    }

    assert_eq!(tt.active_count(), 0, "all timer slots freed");
}

/// Timer churn: fill table to capacity, destroy all, refill.
#[test]
fn churn_timer_fill_destroy_refill() {
    let mut tt = TimerTable::new();

    for round in 0..5u64 {
        // Fill all 32 slots.
        let mut ids = Vec::new();
        for i in 0..MAX_TIMERS as u64 {
            let id = tt.create(round * 1000 + i).expect("should allocate timer");
            ids.push(id);
        }

        assert_eq!(tt.active_count(), MAX_TIMERS);

        // Table is full — next create fails.
        assert!(tt.create(9999).is_none(), "table full should return None");

        // Destroy all.
        for id in ids {
            tt.destroy(id);
        }

        assert_eq!(tt.active_count(), 0, "round {round}: all timers freed");
    }
}

/// Timer churn: interleaved create and destroy exercises slot reuse.
#[test]
fn churn_timer_interleaved_create_destroy() {
    let mut tt = TimerTable::new();
    let mut active: Vec<timer::TimerId> = Vec::new();

    for cycle in 0..200u64 {
        // Create a timer (if space available).
        if tt.active_count() < MAX_TIMERS {
            let id = tt.create(cycle * 100).unwrap();
            active.push(id);
        }

        // Destroy the oldest timer every other cycle.
        if cycle % 2 == 0 && !active.is_empty() {
            let id = active.remove(0);
            tt.destroy(id);
        }
    }

    // Cleanup remaining.
    for id in active {
        tt.destroy(id);
    }

    assert_eq!(
        tt.active_count(),
        0,
        "all timers freed after interleaved churn"
    );
}

/// Timer churn: create and destroy same slot repeatedly to test reuse.
#[test]
fn churn_timer_same_slot_reuse_150_cycles() {
    let mut tt = TimerTable::new();

    for cycle in 0..150u64 {
        let id = tt.create(cycle).unwrap();
        // Should always get slot 0 since we destroy immediately.
        assert_eq!(id.0, 0, "cycle {cycle}: should reuse slot 0");
        tt.destroy(id);
    }

    assert_eq!(tt.active_count(), 0);
}

// ==========================================================================
// SECTION 3: Process create/kill churn (100+ cycles)
// ==========================================================================

/// Rapid process create/kill cycle. Each iteration creates a process and
/// immediately kills it. Verifies live count returns to baseline.
#[test]
fn churn_process_create_kill_150_cycles() {
    let mut pt = ProcessTable::new();

    for cycle in 0..150u32 {
        let baseline = pt.live_count;

        let pid = pt.create();

        assert_eq!(
            pt.live_count,
            baseline + 1,
            "cycle {cycle}: process create should increment live count"
        );

        pt.kill(pid).expect("kill should succeed");

        assert_eq!(
            pt.live_count, baseline,
            "cycle {cycle}: process kill should decrement live count"
        );
    }

    assert_eq!(pt.live_count, 0, "all processes cleaned up");
}

/// Process churn: create, start, then kill. Tests the full lifecycle.
#[test]
fn churn_process_create_start_kill_150_cycles() {
    let mut pt = ProcessTable::new();

    for cycle in 0..150u32 {
        let pid = pt.create();

        pt.start(pid).expect("start should succeed");
        assert_eq!(pt.processes[pid.0 as usize].state, ProcessState::Running);

        pt.kill(pid).expect("kill should succeed");
        assert_eq!(pt.processes[pid.0 as usize].state, ProcessState::Exited);

        assert_eq!(
            pt.live_count, 0,
            "cycle {cycle}: killed process should be cleaned up"
        );
    }
}

/// Process churn: kill without start (process never ran). Tests cleanup
/// of unstarted processes.
#[test]
fn churn_process_create_kill_without_start_100_cycles() {
    let mut pt = ProcessTable::new();

    for cycle in 0..100u32 {
        let pid = pt.create();

        // Kill without starting.
        pt.kill(pid)
            .expect("kill of unstarted process should succeed");

        assert_eq!(
            pt.live_count, 0,
            "cycle {cycle}: killed unstarted process should be cleaned up"
        );
    }
}

/// Process churn: batch create, then batch kill.
#[test]
fn churn_process_batch_create_then_kill() {
    let mut pt = ProcessTable::new();
    let mut pids = Vec::new();

    // Create 100 processes.
    for _ in 0..100 {
        let pid = pt.create();
        pt.start(pid).unwrap();
        pids.push(pid);
    }

    assert_eq!(pt.live_count, 100);

    // Kill all.
    for pid in pids {
        pt.kill(pid).unwrap();
    }

    assert_eq!(
        pt.live_count, 0,
        "all processes cleaned up after batch kill"
    );
}

/// Process churn: double-kill returns error, doesn't corrupt state.
#[test]
fn churn_process_double_kill_150_cycles() {
    let mut pt = ProcessTable::new();

    for cycle in 0..150u32 {
        let pid = pt.create();
        pt.kill(pid).expect("first kill succeeds");

        // Double kill returns error.
        assert_eq!(
            pt.kill(pid),
            Err("already exited"),
            "cycle {cycle}: double kill should fail"
        );

        assert_eq!(pt.live_count, 0, "cycle {cycle}: no leak from double kill");
    }
}

/// Process churn: interleaved create and kill.
#[test]
fn churn_process_interleaved_create_kill() {
    let mut pt = ProcessTable::new();
    let mut live_pids: Vec<process::ProcessId> = Vec::new();

    for cycle in 0..200u32 {
        let pid = pt.create();
        pt.start(pid).unwrap();
        live_pids.push(pid);

        // Kill the oldest process every 3rd cycle.
        if cycle % 3 == 0 && !live_pids.is_empty() {
            let old = live_pids.remove(0);
            pt.kill(old).unwrap();
        }
    }

    // Cleanup remaining.
    for pid in live_pids {
        pt.kill(pid).unwrap();
    }

    assert_eq!(
        pt.live_count, 0,
        "all processes cleaned up after interleaved churn"
    );
}

// ==========================================================================
// SECTION 4: Thread create/exit churn (100+ cycles)
// ==========================================================================

/// Rapid thread create/exit cycle. Each iteration creates a thread and
/// immediately exits it. Verifies live count returns to baseline.
#[test]
fn churn_thread_create_exit_150_cycles() {
    let mut tt = ThreadTable::new();

    for cycle in 0..150u64 {
        let baseline = tt.live_count;

        let tid = tt.create();

        assert_eq!(
            tt.live_count,
            baseline + 1,
            "cycle {cycle}: thread create should increment live count"
        );

        tt.exit(tid).expect("exit should succeed");

        assert_eq!(
            tt.live_count, baseline,
            "cycle {cycle}: thread exit should decrement live count"
        );
    }

    assert_eq!(tt.live_count, 0, "all threads cleaned up");
}

/// Thread churn: batch create, then batch exit.
#[test]
fn churn_thread_batch_create_then_exit() {
    let mut tt = ThreadTable::new();
    let mut tids = Vec::new();

    // Create 100 threads.
    for _ in 0..100 {
        let tid = tt.create();
        tids.push(tid);
    }

    assert_eq!(tt.live_count, 100);

    // Exit all.
    for tid in tids {
        tt.exit(tid).unwrap();
    }

    assert_eq!(tt.live_count, 0, "all threads cleaned up after batch exit");
}

/// Thread churn: double-exit returns error, doesn't corrupt state.
#[test]
fn churn_thread_double_exit_150_cycles() {
    let mut tt = ThreadTable::new();

    for cycle in 0..150u64 {
        let tid = tt.create();
        tt.exit(tid).expect("first exit succeeds");

        // Double exit returns error.
        assert_eq!(
            tt.exit(tid),
            Err("already exited"),
            "cycle {cycle}: double exit should fail"
        );

        assert_eq!(tt.live_count, 0, "cycle {cycle}: no leak from double exit");
    }
}

/// Thread churn: interleaved create and exit.
#[test]
fn churn_thread_interleaved_create_exit() {
    let mut tt = ThreadTable::new();
    let mut live_tids: Vec<thread::ThreadId> = Vec::new();

    for cycle in 0..200u64 {
        let tid = tt.create();
        live_tids.push(tid);

        // Exit the oldest thread every other cycle.
        if cycle % 2 == 0 && !live_tids.is_empty() {
            let old = live_tids.remove(0);
            tt.exit(old).unwrap();
        }
    }

    // Cleanup remaining.
    for tid in live_tids {
        tt.exit(tid).unwrap();
    }

    assert_eq!(
        tt.live_count, 0,
        "all threads cleaned up after interleaved churn"
    );
}

/// Thread churn: many threads created simultaneously, exit in reverse order.
#[test]
fn churn_thread_reverse_exit_order() {
    let mut tt = ThreadTable::new();
    let mut tids = Vec::new();

    for _ in 0..100 {
        let tid = tt.create();
        tids.push(tid);
    }

    assert_eq!(tt.live_count, 100);

    // Exit in reverse order.
    for tid in tids.into_iter().rev() {
        tt.exit(tid).unwrap();
    }

    assert_eq!(
        tt.live_count, 0,
        "reverse-order exit should clean up all threads"
    );
}

// ==========================================================================
// SECTION 5: Scheduling context create/close churn (100+ cycles)
// ==========================================================================

/// Rapid scheduling context create/release cycle. Each iteration creates
/// a context and immediately releases it (ref_count drops to 0).
#[test]
fn churn_sched_ctx_create_release_150_cycles() {
    let mut sct = SchedContextTable::new();

    for cycle in 0..150u32 {
        let baseline = sct.live_count;

        let id = sct
            .create(MIN_BUDGET_NS, MIN_PERIOD_NS)
            .expect("should create scheduling context");

        assert_eq!(
            sct.live_count,
            baseline + 1,
            "cycle {cycle}: create should increment live count"
        );

        sct.release(id);

        assert_eq!(
            sct.live_count, baseline,
            "cycle {cycle}: release should decrement live count"
        );
    }

    assert_eq!(sct.live_count, 0, "all scheduling contexts freed");
}

/// Scheduling context churn: fill to capacity, release all, refill.
#[test]
fn churn_sched_ctx_fill_release_refill() {
    let mut sct = SchedContextTable::new();

    for round in 0..5u32 {
        let mut ids = Vec::new();

        // Fill all slots.
        for _ in 0..MAX_SCHED_CONTEXTS {
            let id = sct.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();
            ids.push(id);
        }

        assert_eq!(sct.live_count, MAX_SCHED_CONTEXTS);

        // Table is full — next create fails.
        assert!(
            sct.create(MIN_BUDGET_NS, MIN_PERIOD_NS).is_none(),
            "should fail when full"
        );

        // Release all.
        for id in ids {
            sct.release(id);
        }

        assert_eq!(sct.live_count, 0, "round {round}: all contexts freed");
    }
}

/// Scheduling context churn: interleaved create and release.
#[test]
fn churn_sched_ctx_interleaved_create_release() {
    let mut sct = SchedContextTable::new();
    let mut active: Vec<SchedulingContextId> = Vec::new();

    for cycle in 0..200u32 {
        // Create if space available.
        if sct.live_count < MAX_SCHED_CONTEXTS {
            let id = sct.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();
            active.push(id);
        }

        // Release oldest every 3rd cycle.
        if cycle % 3 == 0 && !active.is_empty() {
            let id = active.remove(0);
            sct.release(id);
        }
    }

    // Cleanup remaining.
    for id in active {
        sct.release(id);
    }

    assert_eq!(
        sct.live_count, 0,
        "all contexts freed after interleaved churn"
    );
}

/// Scheduling context churn: slot reuse after release.
#[test]
fn churn_sched_ctx_slot_reuse_150_cycles() {
    let mut sct = SchedContextTable::new();

    for cycle in 0..150u32 {
        let id = sct.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();
        // Should reuse slot 0 since we release immediately.
        assert_eq!(id.0, 0, "cycle {cycle}: should reuse slot 0");
        sct.release(id);
    }

    assert_eq!(sct.live_count, 0);
}

/// Scheduling context churn: create with varying valid parameters.
#[test]
fn churn_sched_ctx_varying_params_100_cycles() {
    let mut sct = SchedContextTable::new();

    let params = [
        (MIN_BUDGET_NS, MIN_PERIOD_NS),
        (MIN_BUDGET_NS, MAX_PERIOD_NS),
        (MAX_PERIOD_NS, MAX_PERIOD_NS),
        (500_000, 1_000_000),    // 500µs / 1ms
        (1_000_000, 10_000_000), // 1ms / 10ms
    ];

    for (cycle, &(budget, period)) in params.iter().cycle().take(100).enumerate() {
        let id = sct.create(budget, period).unwrap();

        // Verify the context was created with correct parameters.
        let entry = sct.slots[id.0 as usize].as_ref().unwrap();
        assert_eq!(entry.ctx.budget, budget, "cycle {cycle}: budget mismatch");
        assert_eq!(entry.ctx.period, period, "cycle {cycle}: period mismatch");
        assert_eq!(
            entry.ctx.remaining, budget,
            "cycle {cycle}: remaining should equal budget"
        );

        sct.release(id);
    }

    assert_eq!(sct.live_count, 0);
}

/// Scheduling context churn: double-release is harmless (saturating_sub).
#[test]
fn churn_sched_ctx_double_release_100_cycles() {
    let mut sct = SchedContextTable::new();

    for _cycle in 0..100u32 {
        let id = sct.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();
        sct.release(id);

        // Double release: slot is already None, release is a no-op.
        sct.release(id);
    }

    assert_eq!(sct.live_count, 0, "double-release should not corrupt state");
}

// ==========================================================================
// SECTION 6: Handle table churn (cross-cutting resource management)
// ==========================================================================

/// Handle table churn: rapid insert/close cycle for channel handles.
/// Verifies slot reuse and no corruption after 100+ cycles.
#[test]
fn churn_handle_channel_insert_close_200_cycles() {
    let mut t = HandleTable::new();

    for cycle in 0..200u32 {
        let h = t
            .insert(HandleObject::Channel(ChannelId(cycle)), Rights::READ_WRITE)
            .expect("insert should succeed");

        // Verify we can access the handle.
        assert!(matches!(
            t.get(h, Rights::READ),
            Ok(HandleObject::Channel(_))
        ));

        // Close it.
        let (obj, _rights, _) = t.close(h).expect("close should succeed");
        assert!(
            matches!(obj, HandleObject::Channel(_)),
            "cycle {cycle}: closed object should be Channel"
        );

        // Verify it's gone.
        assert!(matches!(
            t.get(h, Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }
}

/// Handle table churn: rapid insert/close for all handle types.
#[test]
fn churn_handle_all_types_insert_close_100_cycles() {
    let mut t = HandleTable::new();

    for cycle in 0..100u32 {
        let objs: [HandleObject; 6] = [
            HandleObject::Channel(ChannelId(cycle)),
            HandleObject::Timer(timer::TimerId(cycle as u8)),
            HandleObject::Interrupt(interrupt::InterruptId(cycle as u8)),
            HandleObject::SchedulingContext(SchedulingContextId(cycle)),
            HandleObject::Process(process::ProcessId(cycle)),
            HandleObject::Thread(thread::ThreadId(cycle as u64)),
        ];

        let mut handles = Vec::new();
        for obj in &objs {
            let h = t.insert(*obj, Rights::READ_WRITE).unwrap();
            handles.push(h);
        }

        // Verify all accessible.
        for &h in &handles {
            assert!(t.get(h, Rights::READ).is_ok());
        }

        // Close all.
        for h in handles {
            t.close(h).unwrap();
        }

        // Verify all gone.
        for i in 0..6u16 {
            assert!(matches!(
                t.get(Handle(i), Rights::READ),
                Err(HandleError::InvalidHandle)
            ));
        }
    }
}

/// Handle table churn: fill all slots, close all, refill — 5 rounds.
#[test]
fn churn_handle_fill_drain_refill_5_rounds() {
    let mut t = HandleTable::new();
    let capacity = handle::MAX_HANDLES;

    for round in 0..5u32 {
        // Fill all slots.
        for i in 0..capacity as u32 {
            t.insert(
                HandleObject::Channel(ChannelId(round * 10000 + i)),
                Rights::READ_WRITE,
            )
            .expect("insert should succeed");
        }

        // Table full.
        assert!(matches!(
            t.insert(HandleObject::Channel(ChannelId(9_999_999)), Rights::READ),
            Err(HandleError::TableFull)
        ));

        // Drain all.
        let drained: Vec<_> = t.drain().collect();
        assert_eq!(
            drained.len(),
            capacity,
            "round {round}: drain should yield {capacity} entries"
        );

        // Verify first 256 slots empty (spot check).
        for i in 0..=255u16 {
            assert!(matches!(
                t.get(Handle(i), Rights::READ),
                Err(HandleError::InvalidHandle)
            ));
        }
    }
}

/// Handle table churn: interleaved insert and close exercises free-list behavior.
#[test]
fn churn_handle_interleaved_insert_close() {
    let mut t = HandleTable::new();
    let mut live_handles: Vec<Handle> = Vec::new();

    for cycle in 0..300u32 {
        // Insert.
        if let Ok(h) = t.insert(HandleObject::Channel(ChannelId(cycle)), Rights::READ_WRITE) {
            live_handles.push(h);
        }

        // Close the oldest every other cycle.
        if cycle % 2 == 0 && !live_handles.is_empty() {
            let h = live_handles.remove(0);
            t.close(h).unwrap();
        }
    }

    // Close all remaining.
    for h in live_handles {
        t.close(h).unwrap();
    }

    // Verify completely empty.
    for i in 0..=255u16 {
        assert!(matches!(
            t.get(Handle(i), Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }
}

// ==========================================================================
// SECTION 7: Combined resource churn (multi-resource lifecycle)
// ==========================================================================

/// Combined churn: create and destroy channels + timers + scheduling contexts
/// together, modeling a realistic workload where all resource types are
/// churning simultaneously.
#[test]
fn churn_combined_all_resources_100_cycles() {
    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();
    let mut processes = ProcessTable::new();
    let mut threads = ThreadTable::new();
    let mut sched_ctxs = SchedContextTable::new();

    for cycle in 0..100u32 {
        // Create one of each.
        let (ep0, ep1) = channels.create();
        let timer_id = timers.create(cycle as u64 * 1000).unwrap();
        let pid = processes.create();
        processes.start(pid).unwrap();
        let tid = threads.create();
        let sc_id = sched_ctxs.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();

        // Verify all alive.
        assert!(channels.live_pages > 0);
        assert!(timers.active_count() > 0);
        assert!(processes.live_count > 0);
        assert!(threads.live_count > 0);
        assert!(sched_ctxs.live_count > 0);

        // Destroy all.
        channels.close_endpoint(ep0);
        channels.close_endpoint(ep1);
        timers.destroy(timer_id);
        processes.kill(pid).unwrap();
        threads.exit(tid).unwrap();
        sched_ctxs.release(sc_id);

        // All back to baseline.
        assert_eq!(
            channels.live_pages, 0,
            "cycle {cycle}: channel pages leaked"
        );
        assert_eq!(timers.active_count(), 0, "cycle {cycle}: timers leaked");
        assert_eq!(processes.live_count, 0, "cycle {cycle}: processes leaked");
        assert_eq!(threads.live_count, 0, "cycle {cycle}: threads leaked");
        assert_eq!(
            sched_ctxs.live_count, 0,
            "cycle {cycle}: sched contexts leaked"
        );
    }
}

/// Combined churn: batch create then batch destroy (stress overlapping lifetimes).
#[test]
fn churn_combined_batch_create_destroy() {
    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();
    let mut processes = ProcessTable::new();
    let mut threads = ThreadTable::new();
    let mut sched_ctxs = SchedContextTable::new();

    let mut ch_eps = Vec::new();
    let mut timer_ids = Vec::new();
    let mut pids = Vec::new();
    let mut tids = Vec::new();
    let mut sc_ids = Vec::new();

    let batch_size = 30; // Limited by MAX_TIMERS and MAX_SCHED_CONTEXTS (32).

    // Create batch_size of each resource.
    for i in 0..batch_size {
        ch_eps.push(channels.create());
        timer_ids.push(timers.create(i as u64 * 100).unwrap());
        let pid = processes.create();
        processes.start(pid).unwrap();
        pids.push(pid);
        tids.push(threads.create());
        sc_ids.push(sched_ctxs.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap());
    }

    assert_eq!(channels.live_pages, batch_size * 2);
    assert_eq!(timers.active_count(), batch_size);
    assert_eq!(processes.live_count, batch_size);
    assert_eq!(threads.live_count, batch_size);
    assert_eq!(sched_ctxs.live_count, batch_size);

    // Destroy all.
    for (ep0, ep1) in ch_eps {
        channels.close_endpoint(ep0);
        channels.close_endpoint(ep1);
    }
    for id in timer_ids {
        timers.destroy(id);
    }
    for pid in pids {
        processes.kill(pid).unwrap();
    }
    for tid in tids {
        threads.exit(tid).unwrap();
    }
    for id in sc_ids {
        sched_ctxs.release(id);
    }

    assert_eq!(channels.live_pages, 0, "all channel pages freed");
    assert_eq!(timers.active_count(), 0, "all timers freed");
    assert_eq!(processes.live_count, 0, "all processes freed");
    assert_eq!(threads.live_count, 0, "all threads freed");
    assert_eq!(sched_ctxs.live_count, 0, "all sched contexts freed");
}

// ==========================================================================
// SECTION 8: Handle table + channel model combined churn
// ==========================================================================

/// Models the full channel lifecycle through the handle table:
/// create channel → insert handles → close handles → free pages.
#[test]
fn churn_handle_channel_full_lifecycle_100_cycles() {
    let mut ct = ChannelTable::new();
    let mut ht = HandleTable::new();

    for cycle in 0..100u32 {
        // Create a channel.
        let (ep0, ep1) = ct.create();

        // Insert handles.
        let h0 = ht
            .insert(HandleObject::Channel(ep0), Rights::READ_WRITE)
            .unwrap();
        let h1 = ht
            .insert(HandleObject::Channel(ep1), Rights::READ_WRITE)
            .unwrap();

        // Verify handles are valid.
        assert!(matches!(
            ht.get(h0, Rights::READ),
            Ok(HandleObject::Channel(_))
        ));
        assert!(matches!(
            ht.get(h1, Rights::READ),
            Ok(HandleObject::Channel(_))
        ));

        // Close handles (returns the ChannelId for cleanup).
        let (obj0, _, _) = ht.close(h0).unwrap();
        let id0 = match obj0 {
            HandleObject::Channel(id) => id,
            _ => panic!("expected Channel"),
        };
        ct.close_endpoint(id0);

        let (obj1, _, _) = ht.close(h1).unwrap();
        let id1 = match obj1 {
            HandleObject::Channel(id) => id,
            _ => panic!("expected Channel"),
        };
        ct.close_endpoint(id1);

        // Verify resources returned to baseline.
        assert_eq!(
            ct.live_pages, 0,
            "cycle {cycle}: channel pages should be freed"
        );
        assert!(matches!(
            ht.get(h0, Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
        assert!(matches!(
            ht.get(h1, Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }
}

/// Models the full timer lifecycle through the handle table.
#[test]
fn churn_handle_timer_full_lifecycle_100_cycles() {
    let mut tt = TimerTable::new();
    let mut ht = HandleTable::new();

    for cycle in 0..100u64 {
        // Create a timer.
        let timer_id = tt.create(cycle * 1000).unwrap();

        // Insert handle.
        let h = ht
            .insert(HandleObject::Timer(timer_id), Rights::READ_WRITE)
            .unwrap();

        // Verify handle.
        assert!(matches!(
            ht.get(h, Rights::READ),
            Ok(HandleObject::Timer(_))
        ));

        // Close handle → destroy timer.
        let (obj, _, _) = ht.close(h).unwrap();
        let id = match obj {
            HandleObject::Timer(id) => id,
            _ => panic!("expected Timer"),
        };
        tt.destroy(id);

        // Verify baseline.
        assert_eq!(tt.active_count(), 0, "cycle {cycle}: timer should be freed");
    }
}

/// Models the full scheduling context lifecycle through the handle table.
#[test]
fn churn_handle_sched_ctx_full_lifecycle_100_cycles() {
    let mut sct = SchedContextTable::new();
    let mut ht = HandleTable::new();

    for cycle in 0..100u32 {
        // Create scheduling context.
        let sc_id = sct.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();

        // Insert handle.
        let h = ht
            .insert(HandleObject::SchedulingContext(sc_id), Rights::READ_WRITE)
            .unwrap();

        // Verify handle.
        assert!(matches!(
            ht.get(h, Rights::READ),
            Ok(HandleObject::SchedulingContext(_))
        ));

        // Close handle → release context.
        let (obj, _, _) = ht.close(h).unwrap();
        let id = match obj {
            HandleObject::SchedulingContext(id) => id,
            _ => panic!("expected SchedulingContext"),
        };
        sct.release(id);

        // Verify baseline.
        assert_eq!(
            sct.live_count, 0,
            "cycle {cycle}: sched context should be freed"
        );
    }
}
