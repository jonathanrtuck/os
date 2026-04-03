// AUDIT: 2026-03-11 — 4 unsafe blocks verified, 6-category checklist applied.
// Fix: added SAFETY comments to swap_ttbr0 and block_current_unless_woken.
// Idle thread park fix (Fix 1) re-verified sound.
// Fix 17 (2026-03-14): TPIDR_EL1 set in schedule_inner before lock drops.
// 5 unsafe blocks total (swap_ttbr0, block_current_unless_woken, schedule_inner TPIDR,
// init TPIDR, init_secondary TPIDR).
//
// 2026-04-03: Phase 2b — replaced Vec<Box<Thread>> with PoolMeta + ThreadSlots +
// IntrusiveList. Threads stay in pool slots for their entire lifetime. Lists are
// intrusive (linked via list_next/list_prev fields on Thread). All list operations
// are O(1). ThreadLocation enum tracks where each thread lives (Current, Ready,
// Blocked, Suspended, DeferredDrop, DeferredReady). Idle threads remain outside
// the pool in PerCoreState::idle (never enqueued in any list).
//
// ThreadSlots is a separate field from PoolMeta in State so that Rust's borrow
// checker can split borrows: list methods take &mut ThreadSlots while the scheduler
// independently borrows pool metadata, cores, processes, etc.
//! SMP-aware EEVDF scheduler with scheduling contexts.
//!
//! Two-layer design:
//! - **Scheduling contexts** (layer 1): handle-based kernel objects providing
//!   per-workload temporal isolation via budget/period accounting.
//! - **EEVDF** (layer 2): among threads with remaining budget, pick the
//!   eligible thread with the earliest virtual deadline. Shorter requested
//!   time slices yield earlier deadlines (lower latency) without increasing
//!   total CPU share. No heuristics.
//!
//! Per-core ready queues with work stealing, single lock. See DESIGN.md §6.3.
//! Idle threads (one per core) are never enqueued; they run as fallback when
//! no threads are runnable.

use alloc::{boxed::Box, vec::Vec};

use super::{
    handle::HandleObject,
    intrusive_list::{IntrusiveList, PoolMeta, ThreadSlots},
    memory, metrics, paging, per_core,
    process::{Process, ProcessId},
    scheduling_context::{self, SchedulingContext, SchedulingContextId},
    sync::IrqMutex,
    thread::{Thread, ThreadId, ThreadLocation, WaitEntry},
    timer, Context,
};

/// Initialize the scheduler with core 0's boot thread.
/// Default scheduling context: 50ms budget per 50ms period (100% of one core).
/// Effectively unlimited until per-service budgets are implemented. The
/// previous 10ms/50ms (20%) budget caused animation stutter: at 120 Hz,
/// the core service exhausted the shared budget in ~1 frame (8.3ms), then
/// waited ~41ms for replenishment. EEVDF still provides fairness via
/// virtual time even at 100% budget.
/// Values from system_config via paging.
const DEFAULT_BUDGET_NS: u64 = paging::DEFAULT_BUDGET_NS;
const DEFAULT_PERIOD_NS: u64 = paging::DEFAULT_PERIOD_NS;

static STATE: IrqMutex<State> = IrqMutex::new(State {
    slots: ThreadSlots::new(),
    pool: PoolMeta::new(),
    local_queues: {
        const EMPTY: LocalRunQueue = LocalRunQueue::new();
        [EMPTY; per_core::MAX_CORES]
    },
    blocked: IntrusiveList::new(),
    suspended: IntrusiveList::new(),
    deferred_drops: {
        const EMPTY: IntrusiveList = IntrusiveList::new();
        [EMPTY; per_core::MAX_CORES]
    },
    deferred_ready: {
        const EMPTY: IntrusiveList = IntrusiveList::new();
        [EMPTY; per_core::MAX_CORES]
    },
    cores: {
        const INIT: PerCoreState = PerCoreState {
            current_slot: None,
            idle: None,
            is_idle: false,
        };
        [INIT; per_core::MAX_CORES]
    },
    live_thread_count: 0,
    processes: Vec::new(),
    free_process_ids: Vec::new(),
    scheduling_contexts: Vec::new(),
    free_context_ids: Vec::new(),
    default_context_id: None,
});

/// Get a mutable reference to the current thread on `core`.
///
/// For user threads (current_slot is Some), returns from the pool slots.
/// For idle threads (current_slot is None), returns from PerCoreState.idle.
///
/// This is a macro to enable the caller to hold other borrows of State fields
/// simultaneously (structural decomposition).
macro_rules! current_thread {
    ($slots:expr, $cores:expr, $core:expr) => {
        if let Some(slot) = $cores[$core].current_slot {
            $slots.get_mut(slot).expect("current slot empty")
        } else {
            $cores[$core].idle.as_mut().expect("no idle thread")
        }
    };
}
/// Like `current_thread!` but returns a shared reference.
macro_rules! current_thread_ref {
    ($slots:expr, $cores:expr, $core:expr) => {
        if let Some(slot) = $cores[$core].current_slot {
            $slots.get(slot).expect("current slot empty")
        } else {
            $cores[$core].idle.as_ref().expect("no idle thread")
        }
    };
}

/// Information collected under the scheduler lock for thread exit.
#[allow(clippy::large_enum_variant)]
enum ExitInfo {
    /// Last thread in the process — full cleanup required.
    Last {
        thread_id: ThreadId,
        process_id: ProcessId,
        handles: HandleCategories,
        process: Process,
    },
    /// Not the last thread — just clean up this thread.
    NonLast {
        thread_id: ThreadId,
        process_id: ProcessId,
    },
}

/// Result of `block_current_unless_woken`.
///
/// Distinguishes the two return paths so callers know whether post-block
/// cleanup is safe. After the `Blocked` path, code continues on behalf
/// of a *different* thread — any cleanup targeting the blocked thread's
/// registrations would corrupt its state.
pub enum BlockResult {
    /// wake_pending was consumed — same thread, safe to run cleanup.
    WokePending(*const Context),
    /// Thread blocked, `schedule_inner` selected another thread.
    /// Caller must NOT run cleanup (wrong thread identity).
    Blocked(*const Context),
}
/// Per-core ready queue with EEVDF selection and load tracking.
struct LocalRunQueue {
    ready: IntrusiveList,
    /// Number of runnable threads (for work-steal heuristic).
    load: u32,
}
struct PerCoreState {
    /// If Some(slot), a user thread at slots[slot] is running on this core.
    /// If None, the idle thread (stored in `idle`) is running.
    current_slot: Option<u16>,
    /// The idle/boot thread. Always present after init. When a user thread
    /// is current, the idle thread waits here. When idle is current,
    /// it's also here — we just read/write through it, never take it.
    idle: Option<Box<Thread>>,
    /// True when this core is running its idle thread (no runnable work).
    /// Set in `schedule_inner` under the STATE lock. Used by `try_wake_impl`
    /// (Phase 3: IPI) to decide whether to send an inter-processor interrupt.
    is_idle: bool,
}
struct SchedulingContextSlot {
    context: SchedulingContext,
    ref_count: u32,
}
struct State {
    /// Thread slot storage (separate from pool metadata for borrow splitting).
    slots: ThreadSlots,
    /// Pool metadata: free list and generation counters.
    pool: PoolMeta,
    local_queues: [LocalRunQueue; per_core::MAX_CORES],
    blocked: IntrusiveList,
    suspended: IntrusiveList,
    /// Per-core deferred thread drops. When a thread exits, it can't be dropped
    /// immediately because schedule_inner is still executing on its kernel stack.
    /// The thread is pushed to this core's slot and drained at the start of this
    /// core's NEXT schedule_inner — by which time this core has switched to a
    /// different stack via restore_context_and_eret.
    ///
    /// CRITICAL: This MUST be per-core. A global list would allow core A to drain
    /// core B's deferred drops while core B is still on the exited thread's stack
    /// (use-after-free race between lock release and restore_context_and_eret).
    deferred_drops: [IntrusiveList; per_core::MAX_CORES],
    /// Per-core deferred ready threads. When schedule_inner parks a preempted
    /// thread, it can't go directly on the local ready queue because work
    /// stealing would let another core pick it up while this core is still on
    /// its kernel stack (between lock release and restore_context_and_eret).
    ///
    /// Same fix pattern as deferred_drops: push here, drain into local_queues
    /// at the start of THIS CORE's next schedule_inner.
    deferred_ready: [IntrusiveList; per_core::MAX_CORES],
    cores: [PerCoreState; per_core::MAX_CORES],
    /// Live thread count (across all lists + cores). Checked against MAX_THREADS
    /// on spawn. Incremented on spawn, decremented when deferred_drops drains.
    live_thread_count: u32,
    /// All processes. Index = ProcessId.0.
    /// None = freed slot (available via free_process_ids).
    processes: Vec<Option<Process>>,
    /// Freed process IDs available for reuse.
    free_process_ids: Vec<u32>,
    /// All scheduling contexts. Index = SchedulingContextId.0.
    /// None = freed slot (available via free_context_ids).
    scheduling_contexts: Vec<Option<SchedulingContextSlot>>,
    /// Freed scheduling context IDs available for reuse.
    free_context_ids: Vec<u32>,
    /// Default scheduling context bound to all kernel-spawned user threads.
    /// Prevents runaway threads from burning a core indefinitely.
    /// Budget: 50ms per 50ms period (100% of one core).
    default_context_id: Option<SchedulingContextId>,
}

/// Handles sorted by type for cleanup outside the scheduler lock.
pub struct HandleCategories {
    pub channels: Vec<super::handle::ChannelId>,
    pub interrupts: Vec<super::interrupt::InterruptId>,
    pub timers: Vec<super::timer::TimerId>,
    pub thread_handles: Vec<ThreadId>,
    pub process_handles: Vec<ProcessId>,
    pub vmos: Vec<super::vmo::VmoId>,
}
/// Information collected by `kill_process` for cleanup outside the scheduler lock.
pub struct KillInfo {
    pub thread_ids: Vec<ThreadId>,
    pub handles: HandleCategories,
    /// Address space for immediate cleanup (None if deferred due to running threads).
    pub address_space: Option<Box<super::address_space::AddressSpace>>,
    /// Internal timeout timers from threads that were blocked in `wait` with a
    /// finite timeout. These are NOT tracked in the handle table and must be
    /// explicitly destroyed to avoid leaking timer table slots.
    pub timeout_timers: Vec<timer::TimerId>,
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
}

impl LocalRunQueue {
    const fn new() -> Self {
        Self {
            ready: IntrusiveList::new(),
            load: 0,
        }
    }

    /// Average vruntime across threads in this queue (for vlag computation).
    fn avg_vruntime(&self, slots: &ThreadSlots) -> u64 {
        if self.ready.is_empty() {
            return 0;
        }

        let mut sum: u128 = 0;
        let mut count: u128 = 0;

        for slot in self.ready.iter(slots) {
            let t = slots.get(slot).expect("avg_vruntime: empty slot");

            sum += t.scheduling.eevdf.vruntime as u128;
            count += 1;
        }

        if count == 0 {
            return 0;
        }

        (sum / count) as u64
    }
    /// Recompute load from ready list length. Call after mutations.
    fn update_load(&mut self) {
        self.load = self.ready.len();
    }
}

/// Bind the default scheduling context to a kernel-spawned user thread.
/// Increments the context's ref_count so it survives handle closes.
fn bind_default_context(s: &mut State, thread: &mut Thread) {
    if let Some(ctx_id) = s.default_context_id {
        if let Some(Some(slot)) = s.scheduling_contexts.get_mut(ctx_id.0 as usize) {
            slot.ref_count += 1;
            thread.scheduling.context_id = Some(ctx_id);
        }
    }
}
/// Sort handle objects into typed buckets for cleanup outside the lock.
///
/// SchedulingContext handles are released immediately (they only need `s`).
fn categorize_handles(objects: Vec<HandleObject>, s: &mut State) -> HandleCategories {
    let mut categories = HandleCategories {
        channels: Vec::new(),
        interrupts: Vec::new(),
        timers: Vec::new(),
        thread_handles: Vec::new(),
        process_handles: Vec::new(),
        vmos: Vec::new(),
    };

    for obj in objects {
        match obj {
            HandleObject::Channel(id) => categories.channels.push(id),
            HandleObject::Event(id) => super::event::destroy(id),
            HandleObject::Interrupt(id) => categories.interrupts.push(id),
            HandleObject::Process(id) => categories.process_handles.push(id),
            HandleObject::SchedulingContext(id) => release_context_inner(s, id),
            HandleObject::Thread(id) => categories.thread_handles.push(id),
            HandleObject::Timer(id) => categories.timers.push(id),
            HandleObject::Vmo(id) => categories.vmos.push(id),
        }
    }

    categories
}
/// Charge elapsed time to the old thread's EEVDF vruntime and scheduling context.
#[inline(never)]
fn charge_thread(thread: &mut Thread, contexts: &mut [Option<SchedulingContextSlot>], now: u64) {
    if thread.is_idle() || thread.scheduling.last_started == 0 {
        return;
    }

    let elapsed = now.saturating_sub(thread.scheduling.last_started);

    if elapsed == 0 {
        return;
    }

    // Charge EEVDF vruntime.
    thread.scheduling.eevdf = thread.scheduling.eevdf.charge(elapsed);

    // Charge scheduling context budget.
    if let Some(id) = thread.scheduling.context_id {
        if let Some(Some(slot)) = contexts.get_mut(id.0 as usize) {
            slot.context = slot.context.charge(elapsed);
        }
    }
}
/// Find the busiest remote core (highest load > 0). Returns None if no core
/// has any runnable threads (nothing worth stealing).
fn find_busiest_core(local_queues: &[LocalRunQueue], my_core: usize) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;

    for i in 0..per_core::MAX_CORES {
        if i == my_core {
            continue;
        }

        let load = local_queues[i].load;

        // Steal from any core with ready threads. The previous threshold
        // of `load > 1` prevented stealing a single queued thread, causing
        // starvation when only one thread existed (e.g., standalone init).
        if load > 0 {
            if best.is_none_or(|(_, l)| load > l) {
                best = Some((i, load));
            }
        }
    }

    best.map(|(i, _)| i)
}
/// Check if a thread has budget (unlimited if no scheduling context).
fn has_budget(thread: &Thread, contexts: &[Option<SchedulingContextSlot>]) -> bool {
    match thread.scheduling.context_id {
        None => true, // Kernel/idle threads: unlimited
        Some(id) => contexts
            .get(id.0 as usize)
            .and_then(|slot| slot.as_ref())
            .is_none_or(|slot| slot.context.has_budget()),
    }
}
/// Send an IPI to a single idle core (if any), skipping `current_core`.
///
/// Called under the STATE lock after adding a thread to the ready queue.
/// The `send_ipi` call is a raw `msr ICC_SGI1R_EL1` — it acquires no lock,
/// so it is safe to call while holding the scheduler lock. The IPI handler
/// on the target core will acquire the scheduler lock independently (after
/// the sender releases it, since IRQs are masked while the lock is held
/// and IPI delivery is asynchronous).
fn ipi_kick_idle_core(cores: &[PerCoreState], current_core: usize) {
    use super::interrupt_controller::{InterruptController, GIC};

    for (i, core) in cores.iter().enumerate() {
        if i == current_core {
            continue; // No self-IPI
        }

        if core.is_idle {
            GIC.send_ipi(i as u32);

            return; // One IPI is enough — the woken core will pick up the thread.
        }
    }
}
/// Deferred address space cleanup for killed processes.
///
/// When `kill_process` finds threads still running on other cores, it marks
/// them Exited and sets `process.killed = true` with `thread_count` reflecting
/// the number of still-running threads. Each time one of those threads is
/// parked (reaped), `thread_count` is decremented. When it reaches zero, the
/// address space is freed inline (under the scheduler lock — acceptable since
/// this is a rare path and the process is typically small).
fn maybe_cleanup_killed_process(s: &mut State, pid: Option<ProcessId>, was_exited: bool) {
    if !was_exited {
        return;
    }

    let pid = match pid {
        Some(p) => p,
        None => return,
    };

    if let Some(Some(process)) = s.processes.get_mut(pid.0 as usize) {
        if process.killed {
            process.thread_count = process.thread_count.saturating_sub(1);

            if process.thread_count == 0 {
                // Unwrap justified: we just accessed this slot via get_mut() above.
                let process = s.processes[pid.0 as usize].take().unwrap();
                let mut addr_space = process.address_space;

                addr_space.invalidate_tlb();
                addr_space.free_all();

                super::address_space_id::free(super::address_space_id::Asid(addr_space.asid()));

                s.free_process_ids.push(pid.0);
            }
        }
    }
}
/// Read hardware counter and convert to nanoseconds.
fn now_ns() -> u64 {
    timer::counter_to_ns(timer::counter())
}
/// Choose which core to place a woken/spawned thread on.
/// 1. Prefer `preferred_core` if that core is idle (cache affinity).
/// 2. Otherwise, pick the least-loaded core.
fn pick_target_core(
    cores: &[PerCoreState],
    local_queues: &[LocalRunQueue],
    preferred_core: usize,
) -> usize {
    if preferred_core < per_core::MAX_CORES && cores[preferred_core].is_idle {
        return preferred_core;
    }

    let mut best_core = preferred_core.min(per_core::MAX_CORES - 1);
    let mut best_load = u32::MAX;

    for i in 0..per_core::MAX_CORES {
        if local_queues[i].load < best_load {
            best_load = local_queues[i].load;
            best_core = i;
        }
    }

    best_core
}
/// Reap exited threads from a local queue and the blocked list.
///
/// With intrusive lists, iterate and collect slots of exited threads, then
/// remove them and free from the pool.
#[inline(never)]
fn reap_exited(s: &mut State, core: usize) {
    // Collect exited slots from this core's local ready queue.
    let mut to_reap: [u16; 128] = [0; 128];
    let mut reap_count = 0;

    for slot in s.local_queues[core].ready.iter(&s.slots) {
        let t = s.slots.get(slot).expect("reap: empty slot");

        if t.is_exited() && reap_count < to_reap.len() {
            to_reap[reap_count] = slot;
            reap_count += 1;
        }
    }

    for i in 0..reap_count {
        let slot = to_reap[i];

        s.local_queues[core].ready.remove(slot, &mut s.slots);
        s.pool.free(&mut s.slots, slot);
    }

    s.local_queues[core].update_load();

    // Collect exited slots from blocked list.
    let mut reap_count = 0;

    for slot in s.blocked.iter(&s.slots) {
        let t = s.slots.get(slot).expect("reap: empty slot");

        if t.is_exited() && reap_count < to_reap.len() {
            to_reap[reap_count] = slot;
            reap_count += 1;
        }
    }

    for i in 0..reap_count {
        let slot = to_reap[i];

        s.blocked.remove(slot, &mut s.slots);
        s.pool.free(&mut s.slots, slot);
    }
}
/// Decrement ref count on a scheduling context. Frees it if count reaches zero.
fn release_context_inner(s: &mut State, ctx_id: SchedulingContextId) {
    if let Some(slot) = s.scheduling_contexts.get_mut(ctx_id.0 as usize) {
        if let Some(entry) = slot {
            entry.ref_count = entry.ref_count.saturating_sub(1);

            if entry.ref_count == 0 {
                *slot = None;

                s.free_context_ids.push(ctx_id.0);
            }
        }
    }
}
/// Decrement ref count on a scheduling context (standalone version that takes
/// the individual fields rather than &mut State, for use in borrow-split code).
fn release_context_slot(
    scheduling_contexts: &mut Vec<Option<SchedulingContextSlot>>,
    free_context_ids: &mut Vec<u32>,
    ctx_id: SchedulingContextId,
) {
    if let Some(slot) = scheduling_contexts.get_mut(ctx_id.0 as usize) {
        if let Some(entry) = slot {
            entry.ref_count = entry.ref_count.saturating_sub(1);

            if entry.ref_count == 0 {
                *slot = None;

                free_context_ids.push(ctx_id.0);
            }
        }
    }
}
/// Release a thread's bound and borrowed scheduling context refs.
///
/// Takes both `context_id` and `saved_context_id` from the thread and
/// decrements the corresponding ref counts. Used by both thread exit and
/// process kill paths.
fn release_thread_context_ids(
    scheduling_contexts: &mut Vec<Option<SchedulingContextSlot>>,
    free_context_ids: &mut Vec<u32>,
    thread: &mut Thread,
) {
    if let Some(id) = thread.scheduling.context_id.take() {
        release_context_slot(scheduling_contexts, free_context_ids, id);
    }
    if let Some(id) = thread.scheduling.saved_context_id.take() {
        release_context_slot(scheduling_contexts, free_context_ids, id);
    }
}
/// Replenish all scheduling contexts that are due.
#[inline(never)]
fn replenish_contexts(contexts: &mut [Option<SchedulingContextSlot>], now: u64) {
    for entry in contexts.iter_mut().flatten() {
        entry.context = entry.context.maybe_replenish(now);
    }
}
/// Compute the scheduler's next deadline in counter ticks.
///
/// Considers:
/// 1. The current thread's remaining scheduling context budget (quantum expiry).
/// 2. The earliest `replenish_at` across all active scheduling contexts.
///
/// Returns `None` if there are no scheduler-driven deadlines (e.g., idle thread
/// with no scheduling contexts, or thread with unlimited budget).
///
/// `now_ns_val`: current time in nanoseconds (already computed by caller).
fn scheduler_deadline_ticks(
    thread: &Thread,
    contexts: &[Option<SchedulingContextSlot>],
    now_ns_val: u64,
) -> Option<u64> {
    let freq = timer::counter_freq();

    if freq == 0 {
        return None;
    }

    let now_ticks = timer::counter();
    let mut earliest_ns: Option<u64> = None;

    // Source 1: current thread's remaining budget → quantum expiry deadline.
    if !thread.is_idle() {
        if let Some(ctx_id) = thread.scheduling.context_id {
            if let Some(Some(slot)) = contexts.get(ctx_id.0 as usize) {
                if slot.context.has_budget() && slot.context.remaining > 0 {
                    // Quantum expires at now + remaining budget.
                    let deadline_ns = now_ns_val.saturating_add(slot.context.remaining);

                    earliest_ns = Some(deadline_ns);
                }
            }
        }
    }

    // Source 2: replenishment deadlines for exhausted contexts (any time)
    // or non-exhausted contexts with future replenishment. The latter
    // causes harmless spurious timer wakeups — the scheduler will find
    // no work and return to idle.
    for slot in contexts.iter().flatten() {
        if !slot.context.has_budget() || slot.context.replenish_at > now_ns_val {
            let replenish = slot.context.replenish_at;

            earliest_ns = Some(earliest_ns.map_or(replenish, |e| e.min(replenish)));
        }
    }

    // Convert from nanoseconds to counter ticks.
    earliest_ns.map(|deadline_ns| {
        if deadline_ns <= now_ns_val {
            // Already passed — return current tick count (will fire immediately).
            now_ticks
        } else {
            let delta_ns = deadline_ns - now_ns_val;
            let delta_ticks = (delta_ns as u128 * freq as u128 / 1_000_000_000) as u64;

            now_ticks.saturating_add(delta_ticks)
        }
    })
}
#[inline(never)]
fn schedule_inner(s: &mut State, _ctx: *mut Context, core: usize) -> *const Context {
    // Drop threads deferred from THIS CORE's previous schedule_inner. Safe now
    // because we're on the current (live) thread's kernel stack, not the exited
    // one's. Only this core's slot is drained — other cores may still be on
    // their exited threads' stacks between their lock release and their
    // restore_context_and_eret SP switch.
    let mut drop_count: u32 = 0;

    while let Some(slot) = s.deferred_drops[core].pop_front(&mut s.slots) {
        s.pool.free(&mut s.slots, slot);
        drop_count += 1;
    }

    s.live_thread_count = s.live_thread_count.saturating_sub(drop_count);

    // Drain deferred ready threads into this core's local queue. These were
    // parked by the PREVIOUS schedule_inner on this core — safe to enqueue
    // now because we're on a different thread's stack.
    while let Some(slot) = s.deferred_ready[core].pop_front(&mut s.slots) {
        let thread = s.slots.get_mut(slot).expect("deferred_ready: empty slot");

        thread.location = ThreadLocation::Ready(core as u8);
        s.local_queues[core].ready.push_back(slot, &mut s.slots);
    }

    s.local_queues[core].update_load();

    reap_exited(s, core);

    let now = now_ns();

    // Replenish any due scheduling contexts.
    replenish_contexts(&mut s.scheduling_contexts, now);

    // Charge the old thread for elapsed time.
    {
        let old_thread = current_thread!(s.slots, s.cores, core);

        charge_thread(old_thread, &mut s.scheduling_contexts, now);
        old_thread.deschedule();
    }

    // Capture for deferred kill cleanup (before park_old consumes the thread).
    let old_pid;
    let old_exited;
    let old_is_idle;
    let old_is_ready;
    let old_slot = s.cores[core].current_slot;

    {
        let old_thread = current_thread_ref!(s.slots, s.cores, core);

        old_pid = old_thread.process_id;
        old_exited = old_thread.is_exited();
        old_is_idle = old_thread.is_idle();
        old_is_ready = old_thread.is_ready();
    }

    // Helper: activate a user thread from a pool slot.
    #[inline(always)]
    fn activate_slot(
        s: &mut State,
        slot: u16,
        core: usize,
        now: u64,
        old_slot: Option<u16>,
    ) -> *const Context {
        // Swap TTBR0 if the address space changes.
        {
            let old_thread: &Thread = if let Some(os) = old_slot {
                s.slots.get(os).expect("activate: old slot empty")
            } else {
                s.cores[core].idle.as_ref().expect("no idle thread")
            };
            let new_thread = s.slots.get(slot).expect("activate: new slot empty");

            swap_ttbr0(old_thread, new_thread, &s.processes);
        }

        metrics::inc_context_switches();

        let thread = s.slots.get_mut(slot).expect("activate: slot empty");

        thread.activate();
        thread.scheduling.last_started = now;
        thread.scheduling.last_core = core as u32;
        thread.location = ThreadLocation::Current(core as u8);

        let new_ctx = thread.context_ptr();

        park_old(s, old_slot, core);

        s.cores[core].current_slot = Some(slot);
        s.cores[core].is_idle = false;

        new_ctx
    }
    // Helper: continue running the old thread (re-activate without context switch).
    #[inline(always)]
    fn continue_old(s: &mut State, core: usize, now: u64, is_idle: bool) -> *const Context {
        let old_thread = current_thread!(s.slots, s.cores, core);

        old_thread.activate();
        old_thread.scheduling.last_started = now;
        old_thread.scheduling.last_core = core as u32;

        if !is_idle {
            old_thread.location = ThreadLocation::Current(core as u8);
        }

        let ctx = old_thread.context_ptr();

        s.cores[core].is_idle = is_idle;

        ctx
    }
    // Park the old thread in its appropriate location.
    // `park_old` handles the three cases: Ready (deferred_ready or idle restore),
    // Exited (deferred_drops), Blocked (blocked list).
    #[inline(never)]
    fn park_old(s: &mut State, old_slot: Option<u16>, core: usize) {
        if let Some(slot) = old_slot {
            let thread = s.slots.get_mut(slot).expect("park_old: empty slot");

            if thread.is_ready() {
                // Update eligible_at before re-enqueuing.
                thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

                // Defer ready — we're still on this thread's kernel stack
                // until restore_context_and_eret switches SP.
                thread.location = ThreadLocation::DeferredReady(core as u8);

                s.deferred_ready[core].push_back(slot, &mut s.slots);
            } else if thread.is_exited() {
                // Defer drop — we're still running on this thread's kernel stack.
                thread.location = ThreadLocation::DeferredDrop(core as u8);

                s.deferred_drops[core].push_back(slot, &mut s.slots);
            } else {
                // Blocked — park until wake() re-enqueues it.
                thread.location = ThreadLocation::Blocked;

                s.blocked.push_back(slot, &mut s.slots);
            }
        }
        // If old_slot is None, the idle thread was current — no parking needed.
    }
    // Helper: switch to the idle thread.
    #[inline(always)]
    fn switch_to_idle(
        s: &mut State,
        core: usize,
        now: u64,
        old_slot: Option<u16>,
    ) -> *const Context {
        // Swap TTBR0 between old thread and idle.
        {
            let old_thread: &Thread = if let Some(os) = old_slot {
                s.slots.get(os).expect("switch_to_idle: old slot empty")
            } else {
                s.cores[core].idle.as_ref().expect("no idle thread")
            };
            let idle = s.cores[core].idle.as_ref().expect("no idle thread");

            swap_ttbr0(old_thread, idle, &s.processes);
        }

        metrics::inc_context_switches();

        let idle = s.cores[core].idle.as_mut().expect("no idle thread");

        idle.activate();
        idle.scheduling.last_started = now;

        let idle_ctx = idle.context_ptr();

        park_old(s, old_slot, core);

        s.cores[core].current_slot = None;
        s.cores[core].is_idle = true;

        idle_ctx
    }

    // Try to select a runnable thread via EEVDF from this core's local queue.
    let result = if let Some(slot) =
        select_best(&s.local_queues[core], &s.slots, &s.scheduling_contexts)
    {
        s.local_queues[core].ready.remove(slot, &mut s.slots);
        s.local_queues[core].update_load();

        activate_slot(s, slot, core, now, old_slot)
    } else if let Some(victim) = find_busiest_core(&s.local_queues, core) {
        // Work stealing: local queue empty, steal from busiest remote core.
        if steal_from(s, core, victim) {
            if let Some(slot) = select_best(&s.local_queues[core], &s.slots, &s.scheduling_contexts)
            {
                s.local_queues[core].ready.remove(slot, &mut s.slots);
                s.local_queues[core].update_load();

                activate_slot(s, slot, core, now, old_slot)
            } else if old_is_ready
                && !old_is_idle
                && has_budget(
                    current_thread_ref!(s.slots, s.cores, core),
                    &s.scheduling_contexts,
                )
            {
                continue_old(s, core, now, false)
            } else if old_is_ready && old_is_idle {
                continue_old(s, core, now, true)
            } else {
                switch_to_idle(s, core, now, old_slot)
            }
        } else if old_is_ready
            && has_budget(
                current_thread_ref!(s.slots, s.cores, core),
                &s.scheduling_contexts,
            )
        {
            continue_old(s, core, now, old_is_idle)
        } else {
            switch_to_idle(s, core, now, old_slot)
        }
    } else if old_is_ready
        && has_budget(
            current_thread_ref!(s.slots, s.cores, core),
            &s.scheduling_contexts,
        )
    {
        // No other runnable threads and old thread has budget — continue with it.
        continue_old(s, core, now, old_is_idle)
    } else {
        // Old thread exited or blocked, nothing in queue. Run idle thread.
        switch_to_idle(s, core, now, old_slot)
    };

    // Deferred cleanup: if the old thread was from a killed process, decrement
    // the running count and free the address space when it reaches zero.
    maybe_cleanup_killed_process(s, old_pid, old_exited);

    // Tickless timer: reprogram the hardware timer for the next deadline.
    let sched_deadline = {
        let current = current_thread_ref!(s.slots, s.cores, core);

        scheduler_deadline_ticks(current, &s.scheduling_contexts, now)
    };

    timer::reprogram_next_deadline(sched_deadline);

    debug_assert!(
        !result.is_null(),
        "schedule_inner returned null context pointer"
    );

    // Validate the Context we're about to restore has a sane kernel SP.
    // SAFETY: result is a valid Context pointer (just checked non-null).
    let result_sp = unsafe { core::ptr::addr_of!((*result).sp).read() };
    let result_spsr = unsafe { core::ptr::addr_of!((*result).spsr).read() };
    let result_mode = result_spsr & 0xF;

    // Only check SP for EL1 returns — EL0 threads use SP_EL0 (not Context.sp).
    if (result_mode == 4 || result_mode == 5)
        && (result_sp < (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64 || result_sp == 0)
    {
        panic!(
            "schedule_inner: EL1 context has non-kernel SP={result_sp:#x} \
             (mode={result_mode}, core={core})"
        );
    }

    // Update TPIDR_EL1 to point at the new thread's Context while the
    // scheduler lock is held and IRQs are masked. This is critical.
    // SAFETY: `result` is a valid Context pointer from context_ptr() (stable
    // heap address).
    unsafe {
        super::arch::scheduler::set_current_thread(result as usize);
    }

    result
}
/// Select the best thread from the ready queue using EEVDF.
///
/// Returns the pool slot of the chosen thread, or None if no thread has budget.
#[inline(never)]
fn select_best(
    queue: &LocalRunQueue,
    slots: &ThreadSlots,
    contexts: &[Option<SchedulingContextSlot>],
) -> Option<u16> {
    if queue.ready.is_empty() {
        return None;
    }

    // Pass 1: avg vruntime of threads with budget only.
    let mut sum: u128 = 0;
    let mut count: u64 = 0;

    for slot in queue.ready.iter(slots) {
        let t = slots.get(slot).expect("select_best: empty slot");

        if has_budget(t, contexts) {
            sum += t.scheduling.eevdf.vruntime as u128;
            count += 1;
        }
    }

    if count == 0 {
        return None;
    }

    let avg = (sum / count as u128) as u64;
    // Pass 2: eligible + budget → earliest deadline; also track fallback.
    let mut best: Option<(u16, u64)> = None;
    let mut fallback: Option<(u16, u64)> = None;

    for slot in queue.ready.iter(slots) {
        let t = slots.get(slot).expect("select_best: empty slot");

        if !has_budget(t, contexts) {
            continue;
        }

        let eevdf = &t.scheduling.eevdf;

        if eevdf.is_eligible(avg) {
            let deadline = eevdf.virtual_deadline();

            if best.is_none_or(|(_, d)| deadline < d) {
                best = Some((slot, deadline));
            }
        }

        if fallback.is_none_or(|(_, v)| eevdf.vruntime < v) {
            fallback = Some((slot, eevdf.vruntime));
        }
    }

    best.or(fallback).map(|(slot, _)| slot)
}
/// Select the best steal victim from a remote queue.
fn select_steal_victim(
    queue: &LocalRunQueue,
    slots: &ThreadSlots,
    contexts: &[Option<SchedulingContextSlot>],
) -> Option<u16> {
    if queue.ready.is_empty() {
        return None;
    }

    let avg = queue.avg_vruntime(slots);
    let mut best: Option<(u16, i64)> = None;

    for slot in queue.ready.iter(slots) {
        let t = slots.get(slot).expect("select_steal_victim: empty slot");

        if !has_budget(t, contexts) {
            continue;
        }

        let vlag = t.scheduling.eevdf.compute_vlag(avg);

        if best.is_none_or(|(_, v)| vlag > v) {
            best = Some((slot, vlag));
        }
    }

    best.map(|(slot, _)| slot)
}
/// Set wake_pending on a thread that is not yet blocked (Running or Ready).
#[inline(never)]
fn set_wake_pending_inner(s: &mut State, id: ThreadId) {
    // O(1) via pool lookup + location check.
    if let Some(thread) = s.pool.get_mut(&mut s.slots, id) {
        match thread.location {
            ThreadLocation::Current(_)
            | ThreadLocation::Ready(_)
            | ThreadLocation::DeferredReady(_) => {
                thread.wake_pending = true;
                thread.wake_result = 0;
            }
            _ => {}
        }
        return;
    }

    // Also check idle threads (they can be "current" without a pool slot).
    for core_state in s.cores.iter_mut() {
        if let Some(idle) = &mut core_state.idle {
            if idle.id() == id {
                idle.wake_pending = true;
                idle.wake_result = 0;

                return;
            }
        }
    }
}
/// Steal threads from a remote core using workload-granularity migration.
fn steal_from(s: &mut State, my_core: usize, victim_core: usize) -> bool {
    let victim_avg = s.local_queues[victim_core].avg_vruntime(&s.slots);
    let primary_slot = match select_steal_victim(
        &s.local_queues[victim_core],
        &s.slots,
        &s.scheduling_contexts,
    ) {
        Some(slot) => slot,
        None => return false,
    };
    let context_id = s
        .slots
        .get(primary_slot)
        .expect("steal: empty primary slot")
        .scheduling
        .context_id;
    let max_steal = (s.local_queues[victim_core].load / 2).max(1) as usize;
    // Collect slot indices of threads to steal (same context, up to max_steal).
    let mut steal_buf = [0u16; 128];
    let mut steal_count = 0;

    steal_buf[0] = primary_slot;
    steal_count += 1;

    if let Some(ctx_id) = context_id {
        for slot in s.local_queues[victim_core].ready.iter(&s.slots) {
            if slot == primary_slot {
                continue;
            }

            let t = s.slots.get(slot).expect("steal: empty slot");

            if t.scheduling.context_id == Some(ctx_id) && has_budget(t, &s.scheduling_contexts) {
                steal_buf[steal_count] = slot;
                steal_count += 1;

                if steal_count >= max_steal || steal_count >= steal_buf.len() {
                    break;
                }
            }
        }
    }

    let dest_avg = s.local_queues[my_core].avg_vruntime(&s.slots);

    // Remove stolen threads from victim and add to my_core.
    for i in 0..steal_count {
        let slot = steal_buf[i];

        s.local_queues[victim_core].ready.remove(slot, &mut s.slots);

        let thread = s.slots.get_mut(slot).expect("steal: empty slot");
        let vlag = thread.scheduling.eevdf.compute_vlag(victim_avg);

        thread.scheduling.eevdf = thread.scheduling.eevdf.apply_vlag(vlag, dest_avg);
        thread.scheduling.last_core = my_core as u32;
        thread.location = ThreadLocation::Ready(my_core as u8);

        s.local_queues[my_core].ready.push_back(slot, &mut s.slots);
    }

    s.local_queues[victim_core].update_load();
    s.local_queues[my_core].update_load();

    true
}
/// Swap TTBR0 when the address space changes between old and new threads.
#[inline(never)]
fn swap_ttbr0(old: &Thread, new: &Thread, processes: &[Option<Process>]) {
    let old_ttbr0 = ttbr0_for(old);
    let new_ttbr0 = ttbr0_for(new);

    if old_ttbr0 != new_ttbr0 {
        let old_asid = old_ttbr0 >> 48;

        // SAFETY: old_asid and new_ttbr0 are valid values from thread
        // address spaces. The arch implementation handles TLB invalidation
        // and page table root switch.
        unsafe {
            super::arch::scheduler::switch_address_space(old_asid, new_ttbr0);
        }

        // Load per-process PAC keys for the new address space.
        if let Some(new_pid) = new.process_id {
            if let Some(Some(process)) = processes.get(new_pid.0 as usize) {
                super::arch::security::set_pac_keys(&process.pac_keys);
            }
        }
    }
}
/// Shared wake implementation.
#[inline(never)]
fn try_wake_impl(s: &mut State, id: ThreadId, reason: Option<&HandleObject>) -> bool {
    // O(1) lookup via pool + location check.
    let location = match s.pool.get(&s.slots, id) {
        Some(t) => t.location,
        None => return false,
    };

    if location != ThreadLocation::Blocked {
        return false;
    }

    let slot = id.slot();

    // Remove from blocked list (O(1) with intrusive list).
    s.blocked.remove(slot, &mut s.slots);

    let thread = s
        .slots
        .get_mut(slot)
        .expect("wake: slot empty after remove");

    if thread.wake() {
        if let Some(obj) = reason {
            let result = thread.complete_wait_for(obj);

            thread.context.set_arg(0, result);
        }

        thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

        let preferred = thread.scheduling.last_core as usize;
        let target = pick_target_core(&s.cores, &s.local_queues, preferred);

        // Must re-borrow thread after pick_target_core (borrows released).
        s.slots.get_mut(slot).expect("wake: slot empty").location =
            ThreadLocation::Ready(target as u8);
        s.local_queues[target].ready.push_back(slot, &mut s.slots);
        s.local_queues[target].update_load();

        // IPI an idle core so it picks up the newly-ready thread.
        ipi_kick_idle_core(&s.cores, per_core::core_id() as usize);

        return true;
    }

    // Not actually blocked — put it back.
    thread.location = ThreadLocation::Blocked;

    s.blocked.push_back(slot, &mut s.slots);

    false
}
fn ttbr0_for(thread: &Thread) -> u64 {
    match thread.process_id {
        Some(_) => thread.ttbr0,
        None => memory::empty_ttbr0(),
    }
}

/// Bind a scheduling context to the current thread.
#[inline(never)]
pub fn bind_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;

    // Check preconditions before mutating ref_count.
    {
        let thread = current_thread_ref!(s.slots, s.cores, core);

        if thread.scheduling.context_id.is_some() {
            return false;
        }
    }

    // Verify context exists and increment ref_count.
    match s.scheduling_contexts.get_mut(ctx_id.0 as usize) {
        Some(Some(entry)) => entry.ref_count += 1,
        _ => return false,
    }

    let thread = current_thread!(s.slots, s.cores, core);

    thread.scheduling.context_id = Some(ctx_id);

    true
}
/// Block the current thread waiting for a userspace pager to supply a page.
pub fn block_current_for_pager(
    ctx: *mut Context,
    vmo_id: super::vmo::VmoId,
    page_offset: u64,
) -> *const Context {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    thread.pager_wait = Some((vmo_id, page_offset));

    thread.block();

    schedule_inner(&mut s, ctx, core)
}
/// Block the current thread and reschedule, unless a wake is already pending.
#[inline(never)]
pub fn block_current_unless_woken(ctx: *mut Context) -> BlockResult {
    let mut canary: u64 = 0;

    // SAFETY: Writing to a local variable via write_volatile. The address (&mut canary)
    // is valid stack memory. write_volatile ensures the compiler doesn't optimize away
    // this sentinel write, which we check after context switch to detect stack corruption.
    unsafe {
        core::ptr::write_volatile(&mut canary, 0xDEAD_BEEF_CAFE_BABE_u64);
    }

    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    if thread.wake_pending {
        // Waker already ran — consume the flag, patch return value, don't block.
        thread.wake_pending = false;

        let wake_result = thread.wake_result;

        thread.wait_set.clear();

        // SAFETY: Write return value via raw pointer to avoid aliasing UB.
        unsafe {
            let x0_ptr = core::ptr::addr_of_mut!((*ctx).x) as *mut u64;

            x0_ptr.write(wake_result);
        }

        return BlockResult::WokePending(ctx as *const Context);
    }

    thread.block();

    let result = schedule_inner(&mut s, ctx, core);
    // SAFETY: Reading from a local variable via read_volatile.
    let check = unsafe { core::ptr::read_volatile(&canary) };

    if check != 0xDEAD_BEEF_CAFE_BABE {
        panic!("block_current: stack canary corrupt (got {check:#018x})");
    }

    BlockResult::Blocked(result)
}
/// Borrow another thread's scheduling context (context donation).
#[inline(never)]
pub fn borrow_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;

    // Check preconditions before mutating ref_count.
    {
        let thread = current_thread_ref!(s.slots, s.cores, core);

        if thread.scheduling.saved_context_id.is_some() {
            return false;
        }
    }

    let saved = current_thread_ref!(s.slots, s.cores, core)
        .scheduling
        .context_id;

    // Verify context exists and increment ref_count.
    match s.scheduling_contexts.get_mut(ctx_id.0 as usize) {
        Some(Some(entry)) => entry.ref_count += 1,
        _ => return false,
    }

    let thread = current_thread!(s.slots, s.cores, core);

    thread.scheduling.saved_context_id = saved;
    thread.scheduling.context_id = Some(ctx_id);

    true
}
/// Clear the wait set and any pending wake on the current thread.
#[inline(never)]
pub fn clear_wait_state() {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    thread.wait_set.clear();

    thread.wake_pending = false;
}
/// Close all resources in a `HandleCategories` set. Must be called outside the
/// scheduler lock — the individual destroy functions acquire their own locks.
#[inline(never)]
pub fn close_handle_categories(h: HandleCategories) {
    for id in h.channels {
        super::channel::close_endpoint(id);
    }
    for id in h.interrupts {
        super::interrupt::destroy(id);
    }
    for id in h.timers {
        super::timer::destroy(id);
    }
    for id in h.thread_handles {
        super::thread_exit::destroy(id);
    }
    for id in h.process_handles {
        super::process_exit::destroy(id);
    }
    for id in h.vmos {
        let freed_pages = super::vmo::destroy(id);

        for pa in freed_pages {
            super::page_allocator::free_frame(pa);
        }
    }
}
/// Create a new process with the given address space.
#[inline(never)]
pub fn create_process(
    addr_space: Box<super::address_space::AddressSpace>,
    pac_keys: super::arch::security::PacKeys,
) -> Option<ProcessId> {
    let mut s = STATE.lock();
    let process = Process::new(addr_space, pac_keys);

    // Reuse a freed slot if available.
    if let Some(free_id) = s.free_process_ids.pop() {
        s.processes[free_id as usize] = Some(process);

        return Some(ProcessId(free_id));
    }

    // Otherwise append — but enforce the cap.
    if s.processes.len() >= paging::MAX_PROCESSES as usize {
        return None;
    }

    let id = s.processes.len() as u32;

    s.processes.push(Some(process));

    Some(ProcessId(id))
}
/// Create a new scheduling context. Returns the SchedulingContextId.
#[inline(never)]
pub fn create_scheduling_context(budget: u64, period: u64) -> Option<SchedulingContextId> {
    if !scheduling_context::validate_params(budget, period) {
        return None;
    }

    let mut s = STATE.lock();
    let now = now_ns();
    let context = SchedulingContext::new(budget, period, now);
    let slot = SchedulingContextSlot {
        context,
        ref_count: 1,
    };
    let id = if let Some(free_id) = s.free_context_ids.pop() {
        s.scheduling_contexts[free_id as usize] = Some(slot);

        SchedulingContextId(free_id)
    } else {
        let len = s.scheduling_contexts.len();

        if len >= paging::MAX_SCHEDULING_CONTEXTS as usize {
            return None;
        }

        s.scheduling_contexts.push(Some(slot));

        SchedulingContextId(len as u32)
    };

    Some(id)
}
/// Access the current thread's process via closure.
#[inline(never)]
pub fn current_process_do<R>(f: impl FnOnce(&mut Process) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let pid = current_thread_ref!(s.slots, s.cores, core)
        .process_id
        .expect("kernel thread has no process")
        .0 as usize;
    let process = s.processes[pid].as_mut().expect("process not found");

    f(process)
}
/// Access the current thread's process AND its ProcessId via closure.
#[inline(never)]
pub fn current_process_with_pid_do<R>(f: impl FnOnce(ProcessId, &mut Process) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let pid = current_thread_ref!(s.slots, s.cores, core)
        .process_id
        .expect("kernel thread has no process");
    let process = s.processes[pid.0 as usize]
        .as_mut()
        .expect("process not found");

    f(pid, process)
}
/// Access both the current thread and its process via closure.
#[inline(never)]
pub fn current_thread_and_process_do<R>(f: impl FnOnce(&mut Thread, &mut Process) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    // Destructure State for disjoint field borrows (slots/cores vs processes).
    let State {
        ref mut slots,
        ref mut cores,
        ref mut processes,
        ..
    } = *s;
    let thread = if let Some(slot) = cores[core].current_slot {
        slots.get_mut(slot).expect("current slot empty")
    } else {
        cores[core].idle.as_mut().expect("no idle thread")
    };
    let pid = thread.process_id.expect("kernel thread has no process").0 as usize;
    let process = processes[pid].as_mut().expect("process not found");

    f(thread, process)
}
/// Access the current thread via closure.
#[inline(never)]
pub fn current_thread_do<R>(f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    f(thread)
}
#[inline(never)]
pub fn exit_current_from_syscall(ctx: *mut Context) -> *const Context {
    let core = per_core::core_id() as usize;
    // Phase 1: determine if this is the last thread, collect resources.
    let exit_info = {
        let mut s = STATE.lock();
        // Release all scheduling context bind/borrow refs on exit.
        let (ctx_id, saved_ctx_id) = {
            let thread = current_thread!(s.slots, s.cores, core);

            (
                thread.scheduling.context_id.take(),
                thread.scheduling.saved_context_id.take(),
            )
        };

        if let Some(id) = ctx_id {
            release_context_inner(&mut s, id);
        }
        if let Some(id) = saved_ctx_id {
            release_context_inner(&mut s, id);
        }

        let tid = current_thread_ref!(s.slots, s.cores, core).id();
        let pid = current_thread_ref!(s.slots, s.cores, core)
            .process_id
            .expect("not a user thread");
        let process = s.processes[pid.0 as usize]
            .as_mut()
            .expect("process not found");

        process.thread_count = process.thread_count.saturating_sub(1);

        let is_last = process.thread_count == 0;

        if is_last {
            let mut process = s.processes[pid.0 as usize]
                .take()
                .expect("process not found");
            let handle_objects: Vec<HandleObject> =
                process.handles.drain().map(|(obj, _, _)| obj).collect();
            let handles = categorize_handles(handle_objects, &mut s);

            ExitInfo::Last {
                thread_id: tid,
                process_id: pid,
                handles,
                process,
            }
        } else {
            ExitInfo::NonLast {
                thread_id: tid,
                process_id: pid,
            }
        }
    };

    // Phase 1b: collect internal timeout timer from the exiting thread.
    let timeout_timer = {
        let mut s = STATE.lock();
        let thread = current_thread!(s.slots, s.cores, core);

        thread.timeout_timer.take()
    };

    if let Some(timer_id) = timeout_timer {
        timer::destroy(timer_id);
    }

    // Phase 2: notify thread exit.
    let thread_id = exit_info.thread_id();
    let process_id = exit_info.process_id();
    let is_last = matches!(exit_info, ExitInfo::Last { .. });

    super::thread_exit::notify_exit(thread_id);

    if is_last {
        super::process_exit::notify_exit(process_id);

        if super::process::is_init(process_id) {
            super::serial::puts("init exited — shutting down\n");
            super::power::system_off();
        }
    }

    // Phase 2a: remove from futex wait queues.
    super::futex::remove_thread(thread_id);

    match exit_info {
        ExitInfo::Last {
            handles, process, ..
        } => {
            // Phase 3: close resources outside scheduler lock.
            close_handle_categories(handles);

            // Phase 4: free address space.
            let mut addr_space = process.address_space;

            addr_space.invalidate_tlb();
            addr_space.free_all();

            super::address_space_id::free(super::address_space_id::Asid(addr_space.asid()));

            // Phase 5: mark exited and schedule.
            let mut s = STATE.lock();

            current_thread!(s.slots, s.cores, core).mark_exited();

            schedule_inner(&mut s, ctx, core)
        }
        ExitInfo::NonLast { .. } => {
            let mut s = STATE.lock();

            current_thread!(s.slots, s.cores, core).mark_exited();

            schedule_inner(&mut s, ctx, core)
        }
    }
}
#[inline(never)]
pub fn init() {
    let mut s = STATE.lock();

    // Pre-allocate the thread pool to full capacity.
    {
        let State {
            ref mut pool,
            ref mut slots,
            ..
        } = *s;

        pool.init(slots, paging::MAX_THREADS as usize);
    }

    let boot_thread = Thread::new_boot_idle(0);
    let ctx_ptr = boot_thread.context_ptr();

    // The boot thread IS the idle thread.
    s.cores[0].idle = Some(boot_thread);
    s.cores[0].current_slot = None;
    s.cores[0].is_idle = true;

    // Create the default scheduling context for kernel-spawned user threads.
    let now = now_ns();
    let context = SchedulingContext::new(DEFAULT_BUDGET_NS, DEFAULT_PERIOD_NS, now);
    let slot = SchedulingContextSlot {
        context,
        ref_count: 1,
    };

    s.scheduling_contexts.push(Some(slot));

    s.default_context_id = Some(SchedulingContextId(0));

    // Pre-allocate all capped data structures to full capacity.
    s.processes.reserve(paging::MAX_PROCESSES as usize);
    s.free_process_ids.reserve(paging::MAX_PROCESSES as usize);
    s.scheduling_contexts
        .reserve(paging::MAX_SCHEDULING_CONTEXTS as usize);
    s.free_context_ids
        .reserve(paging::MAX_SCHEDULING_CONTEXTS as usize);

    // SAFETY: ctx_ptr points to the Context at offset 0 of the boot thread,
    // which lives in a Box (stable address) stored in the scheduler state.
    unsafe {
        super::arch::scheduler::set_current_thread(ctx_ptr as usize);
    }
}
/// Initialize a secondary core's scheduler state with a boot/idle thread.
#[inline(never)]
pub fn init_secondary(core_id: u32) {
    let mut s = STATE.lock();
    let idx = core_id as usize;

    let boot_thread = Thread::new_boot_idle(core_id as u64);
    let boot_ctx_ptr = boot_thread.context_ptr();

    s.cores[idx].idle = Some(boot_thread);
    s.cores[idx].current_slot = None;
    s.cores[idx].is_idle = true;

    // SAFETY: boot_ctx_ptr points to a stable Context in a Box.
    unsafe {
        super::arch::scheduler::set_current_thread(boot_ctx_ptr as usize);
    }
}
/// Kill all threads of a process and collect resources for cleanup.
#[inline(never)]
pub fn kill_process(target_pid: ProcessId) -> Option<KillInfo> {
    let mut s = STATE.lock();

    // Validate process exists and is alive.
    {
        let process = s.processes.get(target_pid.0 as usize)?.as_ref()?;

        if process.killed || process.thread_count == 0 {
            return None;
        }
    }

    let mut killed_threads = Vec::new();
    let mut timeout_timers = Vec::new();
    let mut running_count: u32 = 0;
    let mut removed_count: u32 = 0;

    // Helper: collect slots from a list matching target_pid.
    fn collect_matching(
        list: &IntrusiveList,
        slots: &ThreadSlots,
        target_pid: ProcessId,
        buf: &mut [u16; 128],
    ) -> usize {
        let mut count = 0;

        for slot in list.iter(slots) {
            let t = slots.get(slot).expect("kill: empty slot");

            if t.process_id == Some(target_pid) && count < buf.len() {
                buf[count] = slot;
                count += 1;
            }
        }

        count
    }
    // Helper: remove collected slots from a list, release context refs, free.
    fn remove_and_free(
        list: &mut IntrusiveList,
        slots: &mut ThreadSlots,
        pool: &mut PoolMeta,
        scheduling_contexts: &mut Vec<Option<SchedulingContextSlot>>,
        free_context_ids: &mut Vec<u32>,
        buf: &[u16],
        count: usize,
        killed_threads: &mut Vec<ThreadId>,
        timeout_timers: &mut Vec<timer::TimerId>,
    ) -> u32 {
        let mut removed: u32 = 0;

        for i in 0..count {
            let slot = buf[i];

            list.remove(slot, slots);

            let thread = slots.get_mut(slot).expect("kill: empty slot after remove");

            release_thread_context_ids(scheduling_contexts, free_context_ids, thread);

            if let Some(timer_id) = thread.timeout_timer.take() {
                timeout_timers.push(timer_id);
            }

            killed_threads.push(thread.id());
            pool.free(slots, slot);

            removed += 1;
        }

        removed
    }

    // Remove target threads from all local ready queues.
    for q in 0..per_core::MAX_CORES {
        let mut buf: [u16; 128] = [0; 128];
        let count = collect_matching(&s.local_queues[q].ready, &s.slots, target_pid, &mut buf);
        let State {
            ref mut local_queues,
            ref mut slots,
            ref mut pool,
            ref mut scheduling_contexts,
            ref mut free_context_ids,
            ..
        } = *s;

        removed_count += remove_and_free(
            &mut local_queues[q].ready,
            slots,
            pool,
            scheduling_contexts,
            free_context_ids,
            &buf,
            count,
            &mut killed_threads,
            &mut timeout_timers,
        );

        s.local_queues[q].update_load();
    }

    // Remove from blocked list.
    {
        let mut buf: [u16; 128] = [0; 128];
        let count = collect_matching(&s.blocked, &s.slots, target_pid, &mut buf);
        let State {
            ref mut blocked,
            ref mut slots,
            ref mut pool,
            ref mut scheduling_contexts,
            ref mut free_context_ids,
            ..
        } = *s;

        removed_count += remove_and_free(
            blocked,
            slots,
            pool,
            scheduling_contexts,
            free_context_ids,
            &buf,
            count,
            &mut killed_threads,
            &mut timeout_timers,
        );
    }
    // Remove from suspended list.
    {
        let mut buf: [u16; 128] = [0; 128];
        let count = collect_matching(&s.suspended, &s.slots, target_pid, &mut buf);
        let State {
            ref mut suspended,
            ref mut slots,
            ref mut pool,
            ref mut scheduling_contexts,
            ref mut free_context_ids,
            ..
        } = *s;

        removed_count += remove_and_free(
            suspended,
            slots,
            pool,
            scheduling_contexts,
            free_context_ids,
            &buf,
            count,
            &mut killed_threads,
            &mut timeout_timers,
        );
    }

    // Decrement live count for immediately-removed threads.
    s.live_thread_count = s.live_thread_count.saturating_sub(removed_count);

    // Mark running threads on other cores as Exited.
    let mut deferred_context_releases: Vec<SchedulingContextId> = Vec::new();

    for core_idx in 0..per_core::MAX_CORES {
        if let Some(slot) = s.cores[core_idx].current_slot {
            let t = s.slots.get_mut(slot).expect("kill: empty current slot");

            if t.process_id == Some(target_pid) {
                killed_threads.push(t.id());

                if let Some(id) = t.scheduling.context_id.take() {
                    deferred_context_releases.push(id);
                }
                if let Some(id) = t.scheduling.saved_context_id.take() {
                    deferred_context_releases.push(id);
                }

                if let Some(timer_id) = t.timeout_timer.take() {
                    timeout_timers.push(timer_id);
                }

                t.mark_exited();

                running_count += 1;
            }
        }
    }

    for id in deferred_context_releases {
        release_context_inner(&mut s, id);
    }

    // Drain handle table and categorize resources for cleanup outside the lock.
    let handle_objects: Vec<HandleObject> = {
        let process = s.processes[target_pid.0 as usize].as_mut().unwrap();

        process.handles.drain().map(|(obj, _, _)| obj).collect()
    };
    let handles = categorize_handles(handle_objects, &mut s);
    let address_space = if running_count == 0 {
        let process = s.processes[target_pid.0 as usize].take().unwrap();

        s.free_process_ids.push(target_pid.0);

        Some(process.address_space)
    } else {
        let process = s.processes[target_pid.0 as usize].as_mut().unwrap();

        process.thread_count = running_count;
        process.killed = true;

        None
    };

    Some(KillInfo {
        thread_ids: killed_threads,
        handles,
        address_space,
        timeout_timers,
    })
}
/// Push a single entry to the current thread's wait set.
#[inline(never)]
pub fn push_wait_entry(entry: WaitEntry) {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    thread.wait_set.push(entry);
}
/// Read the saved register state of a suspended thread.
///
/// # Safety
///
/// `dst` must point to a valid, writable buffer of at least
/// `size_of::<Context>()` bytes.
pub unsafe fn read_thread_state(id: ThreadId, dst: *mut u8) -> bool {
    let s = STATE.lock();
    let thread = match s.pool.get(&s.slots, id) {
        Some(t) if t.location == ThreadLocation::Suspended => t,
        _ => return false,
    };
    let src = &thread.context as *const super::Context as *const u8;
    let size = core::mem::size_of::<super::Context>();

    // SAFETY: src is the thread's Context (valid, aligned). dst validated by caller.
    core::ptr::copy_nonoverlapping(src, dst, size);

    true
}
/// Release a scheduling context handle (decrement ref count, free if zero).
#[inline(never)]
pub fn release_scheduling_context(ctx_id: SchedulingContextId) {
    let mut s = STATE.lock();

    release_context_inner(&mut s, ctx_id);
}
/// Remove an orphaned process that has no threads.
#[inline(never)]
pub fn remove_empty_process(pid: ProcessId) {
    let mut s = STATE.lock();

    if let Some(slot) = s.processes.get_mut(pid.0 as usize) {
        if let Some(process) = slot.as_ref() {
            debug_assert_eq!(
                process.thread_count, 0,
                "remove_empty_process: process has live threads"
            );
        }

        if slot.take().is_some() {
            s.free_process_ids.push(pid.0);
        }
    }
}
/// Resume a suspended thread.
pub fn resume_thread(id: ThreadId) -> bool {
    let mut s = STATE.lock();
    let location = match s.pool.get(&s.slots, id) {
        Some(t) => t.location,
        None => return false,
    };

    if location != ThreadLocation::Suspended {
        return false;
    }

    let slot = id.slot();

    {
        let State {
            ref mut suspended,
            ref mut slots,
            ..
        } = *s;

        suspended.remove(slot, slots);
    }

    let thread = s.slots.get_mut(slot).expect("resume: slot empty");

    thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

    let preferred = thread.scheduling.last_core as usize;
    let target = pick_target_core(&s.cores, &s.local_queues, preferred);

    s.slots.get_mut(slot).expect("resume: slot empty").location =
        ThreadLocation::Ready(target as u8);

    {
        let State {
            ref mut local_queues,
            ref mut slots,
            ..
        } = *s;

        local_queues[target].ready.push_back(slot, slots);
    }

    s.local_queues[target].update_load();

    ipi_kick_idle_core(&s.cores, per_core::core_id() as usize);

    true
}
/// Return a borrowed scheduling context, restoring the saved one.
#[inline(never)]
pub fn return_scheduling_context() -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    match thread.scheduling.saved_context_id.take() {
        Some(saved) => {
            let borrowed = thread.scheduling.context_id;

            thread.scheduling.context_id = Some(saved);

            if let Some(id) = borrowed {
                release_context_inner(&mut s, id);
            }

            true
        }
        None => false,
    }
}
#[inline(never)]
pub fn schedule(ctx: *mut Context) -> *const Context {
    let mut canary: u64 = 0;

    // SAFETY: volatile write to a stack-local.
    unsafe {
        core::ptr::write_volatile(&mut canary, 0xDEAD_BEEF_CAFE_BABE_u64);
    }

    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let result = schedule_inner(&mut s, ctx, core);
    // SAFETY: volatile read of our stack-local canary.
    let check = unsafe { core::ptr::read_volatile(&canary) };

    if check != 0xDEAD_BEEF_CAFE_BABE {
        panic!("schedule: stack canary corrupt (got {check:#018x})");
    }

    result
}
/// Store a timeout timer ID on the current thread for deferred cleanup.
#[inline(never)]
pub fn set_timeout_timer(timer_id: timer::TimerId) {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    thread.timeout_timer = Some(timer_id);
}
/// Clear the timeout timer field on the current thread (timer already destroyed).
#[inline(never)]
pub fn set_timeout_timer_none() {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    thread.timeout_timer = None;
}
/// Set the wake-pending flag on a thread that is not yet blocked (futex path).
#[inline(never)]
pub fn set_wake_pending(id: ThreadId) {
    let mut s = STATE.lock();

    set_wake_pending_inner(&mut s, id);
}
/// Set the wake-pending flag for a handle-based event (channel signal, etc.).
#[inline(never)]
pub fn set_wake_pending_for_handle(id: ThreadId, reason: HandleObject) {
    let mut s = STATE.lock();

    fn apply(t: &mut Thread, reason: &HandleObject) {
        if !t.wake_pending && !t.wait_set.is_empty() {
            t.wake_result = t.complete_wait_for(reason);
            t.wake_pending = true;
        }
    }

    // O(1) via pool lookup + location check.
    {
        let State {
            ref mut pool,
            ref mut slots,
            ..
        } = *s;

        if let Some(thread) = pool.get_mut(slots, id) {
            match thread.location {
                ThreadLocation::Current(_)
                | ThreadLocation::Ready(_)
                | ThreadLocation::DeferredReady(_) => {
                    apply(thread, &reason);
                }
                _ => {}
            }

            return;
        }
    }

    // Also check idle threads.
    for core_state in s.cores.iter_mut() {
        if let Some(idle) = &mut core_state.idle {
            if idle.id() == id {
                apply(idle, &reason);

                return;
            }
        }
    }
}
#[inline(never)]
pub fn spawn_user(process_id: ProcessId, entry_va: u64, user_stack_top: u64) -> Option<ThreadId> {
    let mut s = STATE.lock();

    if s.live_thread_count >= paging::MAX_THREADS as u32 {
        return None;
    }

    let process = s.processes[process_id.0 as usize]
        .as_mut()
        .expect("process not found");
    let ttbr0 = process.address_space.ttbr0_value();
    let mut thread = Thread::new_user(process_id, ttbr0, entry_va, user_stack_top)?;

    bind_default_context(&mut s, &mut thread);

    let (slot, tid) = {
        let State {
            ref mut pool,
            ref mut slots,
            ..
        } = *s;

        pool.alloc(slots, thread)?
    };
    let process = s.processes[process_id.0 as usize]
        .as_mut()
        .expect("process not found");

    process.thread_count += 1;
    s.live_thread_count += 1;

    let target = pick_target_core(&s.cores, &s.local_queues, per_core::core_id() as usize);

    s.slots
        .get_mut(slot)
        .expect("just-allocated slot empty")
        .location = ThreadLocation::Ready(target as u8);

    {
        let State {
            ref mut local_queues,
            ref mut slots,
            ..
        } = *s;

        local_queues[target].ready.push_back(slot, slots);
    }

    s.local_queues[target].update_load();

    ipi_kick_idle_core(&s.cores, per_core::core_id() as usize);

    Some(tid)
}
/// Like `spawn_user`, but the thread is placed in the suspended list.
#[inline(never)]
pub fn spawn_user_suspended(
    process_id: ProcessId,
    entry_va: u64,
    user_stack_top: u64,
) -> Option<ThreadId> {
    let mut s = STATE.lock();

    if s.live_thread_count >= paging::MAX_THREADS as u32 {
        return None;
    }

    let process = s.processes[process_id.0 as usize]
        .as_mut()
        .expect("process not found");
    let ttbr0 = process.address_space.ttbr0_value();
    let mut thread = Thread::new_user(process_id, ttbr0, entry_va, user_stack_top)?;

    bind_default_context(&mut s, &mut thread);

    let (slot, tid) = {
        let State {
            ref mut pool,
            ref mut slots,
            ..
        } = *s;

        pool.alloc(slots, thread)?
    };
    let process = s.processes[process_id.0 as usize]
        .as_mut()
        .expect("process not found");

    process.thread_count += 1;
    s.live_thread_count += 1;

    s.slots
        .get_mut(slot)
        .expect("just-allocated slot empty")
        .location = ThreadLocation::Suspended;

    {
        let State {
            ref mut suspended,
            ref mut slots,
            ..
        } = *s;

        suspended.push_back(slot, slots);
    }

    Some(tid)
}
/// Move all suspended threads belonging to `process_id` into the ready queue.
#[inline(never)]
pub fn start_suspended_threads(process_id: ProcessId) -> bool {
    let mut s = STATE.lock();
    let mut started = false;
    let mut to_start: [u16; 128] = [0; 128];
    let mut start_count = 0;

    for slot in s.suspended.iter(&s.slots) {
        let t = s.slots.get(slot).expect("start_suspended: empty slot");

        if t.process_id == Some(process_id) && start_count < to_start.len() {
            to_start[start_count] = slot;
            start_count += 1;
        }
    }

    for i in 0..start_count {
        let slot = to_start[i];

        {
            let State {
                ref mut suspended,
                ref mut slots,
                ..
            } = *s;

            suspended.remove(slot, slots);
        }

        let preferred = s
            .slots
            .get(slot)
            .expect("start: empty slot")
            .scheduling
            .last_core as usize;
        let target = pick_target_core(&s.cores, &s.local_queues, preferred);

        s.slots.get_mut(slot).expect("start: empty slot").location =
            ThreadLocation::Ready(target as u8);

        {
            let State {
                ref mut local_queues,
                ref mut slots,
                ..
            } = *s;

            local_queues[target].ready.push_back(slot, slots);
        }

        s.local_queues[target].update_load();

        started = true;
    }

    if started {
        if let Some(Some(process)) = s.processes.get_mut(process_id.0 as usize) {
            process.started = true;
        }

        ipi_kick_idle_core(&s.cores, per_core::core_id() as usize);
    }

    started
}
/// Suspend a thread by ThreadId.
pub fn suspend_thread(id: ThreadId) -> bool {
    let mut s = STATE.lock();
    let location = match s.pool.get(&s.slots, id) {
        Some(t) => t.location,
        None => return false,
    };
    let slot = id.slot();

    match location {
        ThreadLocation::Ready(core) => {
            {
                let State {
                    ref mut local_queues,
                    ref mut slots,
                    ..
                } = *s;

                local_queues[core as usize].ready.remove(slot, slots);
            }

            s.local_queues[core as usize].update_load();

            s.slots.get_mut(slot).expect("suspend: slot empty").location =
                ThreadLocation::Suspended;

            {
                let State {
                    ref mut suspended,
                    ref mut slots,
                    ..
                } = *s;
                suspended.push_back(slot, slots);
            }

            true
        }
        ThreadLocation::Blocked => {
            {
                let State {
                    ref mut blocked,
                    ref mut slots,
                    ..
                } = *s;
                blocked.remove(slot, slots);
            }

            s.slots.get_mut(slot).expect("suspend: slot empty").location =
                ThreadLocation::Suspended;

            {
                let State {
                    ref mut suspended,
                    ref mut slots,
                    ..
                } = *s;

                suspended.push_back(slot, slots);
            }

            true
        }
        _ => false,
    }
}
/// Take and return stale waiter entries from the current thread.
#[inline(never)]
pub fn take_stale_waiters() -> Vec<WaitEntry> {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    core::mem::take(&mut thread.stale_waiters)
}
/// Take and return any stale timeout timer from the current thread.
#[inline(never)]
pub fn take_timeout_timer() -> Option<timer::TimerId> {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = current_thread!(s.slots, s.cores, core);

    thread.timeout_timer.take()
}
/// Wake a blocked thread (Blocked -> Ready). Returns true if it was blocked.
#[inline(never)]
pub fn try_wake(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    try_wake_impl(&mut s, id, None)
}
/// Wake a blocked thread and resolve its wait set against `reason`.
#[inline(never)]
pub fn try_wake_for_handle(id: ThreadId, reason: HandleObject) -> bool {
    let mut s = STATE.lock();

    try_wake_impl(&mut s, id, Some(&reason))
}
/// Two-phase wake: attempt to wake, fall back to setting wake-pending flag.
pub fn wake_for_handle(id: ThreadId, reason: HandleObject) {
    if !try_wake_for_handle(id, reason) {
        set_wake_pending_for_handle(id, reason);
    }
}
/// Wake all threads blocked on pager faults for the given VMO page range.
pub fn wake_pager_waiters(vmo_id: super::vmo::VmoId, offset: u64, count: u64) {
    let mut s = STATE.lock();
    let range_end = offset + count;
    let mut to_wake: [u16; 64] = [0; 64];
    let mut wake_count = 0;

    for slot in s.blocked.iter(&s.slots) {
        let thread = s.slots.get(slot).expect("wake_pager: empty slot");

        if let Some((wait_vmo, wait_page)) = thread.pager_wait {
            if wait_vmo == vmo_id && wait_page >= offset && wait_page < range_end {
                if wake_count < to_wake.len() {
                    to_wake[wake_count] = slot;
                    wake_count += 1;
                }
            }
        }
    }

    for i in 0..wake_count {
        let slot = to_wake[i];

        {
            let State {
                ref mut blocked,
                ref mut slots,
                ..
            } = *s;

            blocked.remove(slot, slots);
        }

        let (did_wake, preferred) = {
            let thread = s.slots.get_mut(slot).expect("wake_pager: empty slot");

            thread.pager_wait = None;

            let did_wake = thread.wake();

            if did_wake {
                thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();
            }

            (did_wake, thread.scheduling.last_core as usize)
        };

        if did_wake {
            let target = pick_target_core(&s.cores, &s.local_queues, preferred);

            s.slots
                .get_mut(slot)
                .expect("wake_pager: slot empty")
                .location = ThreadLocation::Ready(target as u8);

            {
                let State {
                    ref mut local_queues,
                    ref mut slots,
                    ..
                } = *s;

                local_queues[target].ready.push_back(slot, slots);
            }

            s.local_queues[target].update_load();
        } else {
            s.slots
                .get_mut(slot)
                .expect("wake_pager: slot empty")
                .location = ThreadLocation::Blocked;
            {
                let State {
                    ref mut blocked,
                    ref mut slots,
                    ..
                } = *s;

                blocked.push_back(slot, slots);
            }
        }
    }

    if wake_count > 0 {
        ipi_kick_idle_core(&s.cores, per_core::core_id() as usize);
    }
}
/// Access a process by ProcessId. Acquires the scheduler lock.
#[inline(never)]
pub fn with_process<R>(pid: ProcessId, f: impl FnOnce(&mut Process) -> R) -> Option<R> {
    let mut s = STATE.lock();
    let process = s.processes.get_mut(pid.0 as usize)?.as_mut()?;

    Some(f(process))
}
