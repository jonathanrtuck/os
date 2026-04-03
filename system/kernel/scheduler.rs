// AUDIT: 2026-03-11 — 4 unsafe blocks verified, 6-category checklist applied.
// Fix: added SAFETY comments to swap_ttbr0 and block_current_unless_woken.
// Idle thread park fix (Fix 1) re-verified sound.
// Fix 17 (2026-03-14): TPIDR_EL1 set in schedule_inner before lock drops.
// 5 unsafe blocks total (swap_ttbr0, block_current_unless_woken, schedule_inner TPIDR,
// init TPIDR, init_secondary TPIDR).
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
    memory, metrics, paging, per_core,
    process::{Process, ProcessId},
    scheduling_context::{self, SchedulingContext, SchedulingContextId},
    sync::IrqMutex,
    thread::{Thread, ThreadId, WaitEntry},
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
    local_queues: {
        const EMPTY: LocalRunQueue = LocalRunQueue::new();
        [EMPTY; per_core::MAX_CORES]
    },
    blocked: Vec::new(),
    suspended: Vec::new(),
    deferred_drops: {
        const EMPTY: Vec<Box<Thread>> = Vec::new();
        [EMPTY; per_core::MAX_CORES]
    },
    deferred_ready: {
        const EMPTY: Vec<Box<Thread>> = Vec::new();
        [EMPTY; per_core::MAX_CORES]
    },
    cores: {
        const INIT: PerCoreState = PerCoreState {
            current: None,
            idle: None,
            is_idle: false,
        };
        [INIT; per_core::MAX_CORES]
    },
    next_id: 1,
    processes: Vec::new(),
    next_process_id: 0,
    scheduling_contexts: Vec::new(),
    free_context_ids: Vec::new(),
    default_context_id: None,
});

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

struct PerCoreState {
    current: Option<Box<Thread>>,
    idle: Option<Box<Thread>>,
    /// True when this core is running its idle thread (no runnable work).
    /// Set in `schedule_inner` under the STATE lock. Used by `try_wake_impl`
    /// (Phase 3: IPI) to decide whether to send an inter-processor interrupt.
    is_idle: bool,
}
/// Per-core ready queue with EEVDF selection and load tracking.
#[allow(clippy::vec_box)]
struct LocalRunQueue {
    ready: Vec<Box<Thread>>,
    /// Number of runnable threads (for work-steal heuristic).
    load: u32,
}
struct SchedulingContextSlot {
    context: SchedulingContext,
    ref_count: u32,
}
// Box<Thread> is intentional: threads must have stable addresses because
// TPIDR_EL1 holds a raw pointer to the current thread's Context.
#[allow(clippy::vec_box)]
struct State {
    local_queues: [LocalRunQueue; per_core::MAX_CORES],
    blocked: Vec<Box<Thread>>,
    suspended: Vec<Box<Thread>>,
    /// Per-core deferred thread drops. When a thread exits, it can't be dropped
    /// immediately because schedule_inner is still executing on its kernel stack.
    /// The thread is pushed to this core's slot and drained at the start of this
    /// core's NEXT schedule_inner — by which time this core has switched to a
    /// different stack via restore_context_and_eret.
    ///
    /// CRITICAL: This MUST be per-core. A global Vec would allow core A to drain
    /// core B's deferred drops while core B is still on the exited thread's stack
    /// (use-after-free race between lock release and restore_context_and_eret).
    deferred_drops: [Vec<Box<Thread>>; per_core::MAX_CORES],
    /// Per-core deferred ready threads. When schedule_inner parks a preempted
    /// thread, it can't go directly on the local ready queue because work
    /// stealing would let another core pick it up while this core is still on
    /// its kernel stack (between lock release and restore_context_and_eret).
    ///
    /// Same fix pattern as deferred_drops: push here, drain into local_queues
    /// at the start of THIS CORE's next schedule_inner.
    deferred_ready: [Vec<Box<Thread>>; per_core::MAX_CORES],
    cores: [PerCoreState; per_core::MAX_CORES],
    next_id: u64,
    /// All processes. Index = ProcessId.0.
    /// None = freed slot (available via free_process_ids).
    processes: Vec<Option<Process>>,
    next_process_id: u32,
    /// All scheduling contexts. Index = SchedulingContextId.0.
    /// None = freed slot (available via free_context_ids).
    scheduling_contexts: Vec<Option<SchedulingContextSlot>>,
    /// Freed scheduling context IDs available for reuse.
    free_context_ids: Vec<u32>,
    /// Default scheduling context bound to all kernel-spawned user threads.
    /// Prevents runaway threads from burning a core indefinitely.
    /// Budget: 10ms per 50ms period (20% of one core).
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
            ready: Vec::new(),
            load: 0,
        }
    }

    /// Average vruntime across threads in this queue (for vlag computation).
    fn avg_vruntime(&self) -> u64 {
        if self.ready.is_empty() {
            return 0;
        }

        let sum: u128 = self
            .ready
            .iter()
            .map(|t| t.scheduling.eevdf.vruntime as u128)
            .sum();

        (sum / self.ready.len() as u128) as u64
    }
    /// Recompute load from ready vec length. Call after mutations.
    fn update_load(&mut self) {
        self.load = self.ready.len() as u32;
    }
}
/// Scheduling context with handle-based ownership tracking.

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
/// Find the busiest remote core (highest load > 1). Returns None if no core
/// has more than 1 runnable thread (nothing worth stealing).
fn find_busiest_core(s: &State, my_core: usize) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;

    for i in 0..per_core::MAX_CORES {
        if i == my_core {
            continue;
        }

        let load = s.local_queues[i].load;

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
fn ipi_kick_idle_core(s: &State, current_core: usize) {
    use super::interrupt_controller::{InterruptController, GIC};

    for (i, core) in s.cores.iter().enumerate() {
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
fn pick_target_core(s: &State, preferred_core: usize) -> usize {
    if preferred_core < per_core::MAX_CORES && s.cores[preferred_core].is_idle {
        return preferred_core;
    }

    let mut best_core = preferred_core.min(per_core::MAX_CORES - 1);
    let mut best_load = u32::MAX;

    for i in 0..per_core::MAX_CORES {
        if s.local_queues[i].load < best_load {
            best_load = s.local_queues[i].load;
            best_core = i;
        }
    }

    best_core
}
/// Reap exited threads from a local queue and the blocked list.
#[inline(never)]
#[allow(clippy::vec_box)]
fn reap_exited(local_queue: &mut LocalRunQueue, blocked: &mut Vec<Box<Thread>>) {
    local_queue.ready.retain(|t| !t.is_exited());
    local_queue.update_load();
    blocked.retain(|t| !t.is_exited());
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
/// Release a thread's bound and borrowed scheduling context refs.
///
/// Takes both `context_id` and `saved_context_id` from the thread and
/// decrements the corresponding ref counts. Used by both thread exit and
/// process kill paths.
fn release_thread_context_ids(s: &mut State, thread: &mut Thread) {
    if let Some(id) = thread.scheduling.context_id.take() {
        release_context_inner(s, id);
    }
    if let Some(id) = thread.scheduling.saved_context_id.take() {
        release_context_inner(s, id);
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
    s.deferred_drops[core].clear();

    // Drain deferred ready threads into this core's local queue. These were
    // parked by the PREVIOUS schedule_inner on this core — safe to enqueue
    // now because we're on a different thread's stack.
    for thread in s.deferred_ready[core].drain(..) {
        s.local_queues[core].ready.push(thread);
    }

    s.local_queues[core].update_load();

    reap_exited(&mut s.local_queues[core], &mut s.blocked);

    let now = now_ns();

    // Replenish any due scheduling contexts.
    replenish_contexts(&mut s.scheduling_contexts, now);

    let mut old_thread = s.cores[core].current.take().expect("no current thread");

    // Charge the old thread for elapsed time.
    charge_thread(&mut old_thread, &mut s.scheduling_contexts, now);

    old_thread.deschedule();

    // Capture for deferred kill cleanup (before park_old consumes the thread).
    let old_pid = old_thread.process_id;
    let old_exited = old_thread.is_exited();

    // Park the old thread in its appropriate location.
    #[inline(never)]
    fn park_old(s: &mut State, mut old_thread: Box<Thread>, core: usize) {
        if old_thread.is_ready() {
            // Update eligible_at before re-enqueuing.
            old_thread.scheduling.eevdf = old_thread.scheduling.eevdf.mark_eligible();

            if old_thread.is_idle() {
                // Idle threads are never enqueued — restore to this core's idle slot.
                s.cores[core].idle = Some(old_thread);
            } else {
                // Defer ready — we're still on this thread's kernel stack
                // until restore_context_and_eret switches SP. If we put it
                // directly on local_queues, work stealing would let another
                // core pick it up and restore it while we're still on the
                // stack (cross-core stack reuse → canary corruption).
                //
                // Same pattern as deferred_drops: pushed here, drained into
                // local_queues at the start of this core's NEXT schedule_inner.
                s.deferred_ready[core].push(old_thread);
            }
        } else if old_thread.is_exited() {
            // Defer drop — we're still running on this thread's kernel stack.
            // Dropping now would free the stack pages while schedule_inner is
            // still executing on them (use-after-free). The per-core deferred_drops
            // slot is drained at the start of THIS CORE's next schedule_inner,
            // when we're safely on a different thread's stack.
            s.deferred_drops[core].push(old_thread);
        } else {
            // Blocked — park until wake() re-enqueues it.
            s.blocked.push(old_thread);
        }
    }

    // Try to select a runnable thread via EEVDF from this core's local queue.
    let result = if let Some(idx) = select_best(&s.local_queues[core], &s.scheduling_contexts) {
        let mut new_thread = s.local_queues[core].ready.swap_remove(idx);

        s.local_queues[core].update_load();

        new_thread.activate();
        new_thread.scheduling.last_started = now;
        new_thread.scheduling.last_core = core as u32;

        swap_ttbr0(&old_thread, &new_thread, &s.processes);

        metrics::inc_context_switches();

        let new_ctx = new_thread.context_ptr();

        park_old(s, old_thread, core);

        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;

        new_ctx
    } else if let Some(victim) = find_busiest_core(&s, core) {
        // Work stealing: local queue empty, steal from busiest remote core.
        if steal_from(s, core, victim) {
            if let Some(idx) = select_best(&s.local_queues[core], &s.scheduling_contexts) {
                let mut new_thread = s.local_queues[core].ready.swap_remove(idx);

                s.local_queues[core].update_load();

                new_thread.activate();
                new_thread.scheduling.last_started = now;
                new_thread.scheduling.last_core = core as u32;

                swap_ttbr0(&old_thread, &new_thread, &s.processes);

                metrics::inc_context_switches();

                let new_ctx = new_thread.context_ptr();

                park_old(s, old_thread, core);

                s.cores[core].current = Some(new_thread);
                s.cores[core].is_idle = false;

                new_ctx
            } else if old_thread.is_ready() && has_budget(&old_thread, &s.scheduling_contexts) {
                // Steal succeeded but nothing schedulable — continue with old thread.
                let is_idle_thread = old_thread.is_idle();

                old_thread.activate();
                old_thread.scheduling.last_started = now;
                old_thread.scheduling.last_core = core as u32;

                let old_ctx = old_thread.context_ptr();

                s.cores[core].current = Some(old_thread);
                s.cores[core].is_idle = is_idle_thread;

                old_ctx
            } else {
                // Steal succeeded but nothing schedulable and old thread can't continue.
                let mut idle = s.cores[core].idle.take().expect("no idle thread");

                idle.activate();
                idle.scheduling.last_started = now;

                let idle_ctx = idle.context_ptr();

                swap_ttbr0(&old_thread, &idle, &s.processes);

                metrics::inc_context_switches();

                park_old(s, old_thread, core);

                s.cores[core].current = Some(idle);
                s.cores[core].is_idle = true;

                idle_ctx
            }
        } else if old_thread.is_ready() && has_budget(&old_thread, &s.scheduling_contexts) {
            // Steal failed — continue with old thread.
            let is_idle_thread = old_thread.is_idle();

            old_thread.activate();
            old_thread.scheduling.last_started = now;
            old_thread.scheduling.last_core = core as u32;

            let old_ctx = old_thread.context_ptr();

            s.cores[core].current = Some(old_thread);
            s.cores[core].is_idle = is_idle_thread;

            old_ctx
        } else {
            // Steal failed and old thread can't continue — run idle.
            let mut idle = s.cores[core].idle.take().expect("no idle thread");

            idle.activate();
            idle.scheduling.last_started = now;

            let idle_ctx = idle.context_ptr();

            swap_ttbr0(&old_thread, &idle, &s.processes);

            metrics::inc_context_switches();

            park_old(s, old_thread, core);

            s.cores[core].current = Some(idle);
            s.cores[core].is_idle = true;

            idle_ctx
        }
    } else if old_thread.is_ready() && has_budget(&old_thread, &s.scheduling_contexts) {
        // No other runnable threads and old thread has budget — continue with it.
        // If the old thread is the idle thread (no work exists), preserve is_idle
        // so that ipi_kick_idle_core correctly identifies this core as available.
        let is_idle_thread = old_thread.is_idle();

        old_thread.activate();
        old_thread.scheduling.last_started = now;
        old_thread.scheduling.last_core = core as u32;

        let old_ctx = old_thread.context_ptr();

        s.cores[core].current = Some(old_thread);
        s.cores[core].is_idle = is_idle_thread;

        old_ctx
    } else {
        // Old thread exited or blocked, nothing in queue. Run idle thread.
        let mut idle = s.cores[core].idle.take().expect("no idle thread");

        idle.activate();
        idle.scheduling.last_started = now;

        let idle_ctx = idle.context_ptr();

        swap_ttbr0(&old_thread, &idle, &s.processes);

        metrics::inc_context_switches();

        park_old(s, old_thread, core);

        s.cores[core].current = Some(idle);
        s.cores[core].is_idle = true;

        idle_ctx
    };

    // Deferred cleanup: if the old thread was from a killed process, decrement
    // the running count and free the address space when it reaches zero.
    maybe_cleanup_killed_process(s, old_pid, old_exited);

    // Tickless timer: reprogram the hardware timer for the next deadline.
    // Compute the scheduler's next deadline from the current thread's quantum
    // and context replenishment, then delegate to timer::reprogram_next_deadline
    // which also considers timer objects.
    let sched_deadline = s.cores[core]
        .current
        .as_ref()
        .and_then(|t| scheduler_deadline_ticks(t, &s.scheduling_contexts, now));

    timer::reprogram_next_deadline(sched_deadline);

    debug_assert!(
        !result.is_null(),
        "schedule_inner returned null context pointer"
    );

    // Validate the Context we're about to restore has a sane kernel SP.
    // A zeroed Context (sp=0) or a user-range SP would cause a cascade of
    // faults when the exception handler tries to use the stack.
    // SAFETY: result is a valid Context pointer (just checked non-null).
    let result_sp = unsafe { core::ptr::addr_of!((*result).sp).read() };
    let result_spsr = unsafe { core::ptr::addr_of!((*result).spsr).read() };
    let result_mode = result_spsr & 0xF;

    // Only check SP for EL1 returns — EL0 threads use SP_EL0 (not Context.sp).
    if (result_mode == 4 || result_mode == 5)
        && (result_sp < (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64 || result_sp == 0)
    {
        // The idle thread on its first activation would hit this — but with
        // the merged boot/idle thread, it should never have a zeroed Context.
        panic!(
            "schedule_inner: EL1 context has non-kernel SP={result_sp:#x} \
             (mode={result_mode}, core={core})"
        );
    }

    // Update TPIDR_EL1 to point at the new thread's Context while the
    // scheduler lock is held and IRQs are masked. This is critical: when the
    // lock drops, IRQs are re-enabled. If a timer IRQ fires before the caller
    // (exception.S) updates TPIDR, save_context would write to the OLD
    // thread's Context — which has been parked in the ready queue. That
    // corrupts the old thread's saved state with kernel-mode registers
    // (SPSR=EL1h, ELR=kernel addr, SP=wrong stack), causing an EC=0x21
    // instruction abort when the old thread is later restored.
    //
    // Set TPIDR_EL1 (or equivalent) to the new thread's Context. Must happen
    // before lock release (which re-enables IRQs — exception.S reads TPIDR).
    // SAFETY: `result` is a valid Context pointer from context_ptr() (stable
    // heap address).
    unsafe {
        super::arch::scheduler::set_current_thread(result as usize);
    }

    result
}
/// Select the best thread from the ready queue using EEVDF.
///
/// Zero-allocation: computes avg vruntime (over threads with budget only)
/// and selects in two passes over the ready queue. Returns the index into
/// `queue.ready`, or None if no thread has budget.
#[inline(never)]
fn select_best(queue: &LocalRunQueue, contexts: &[Option<SchedulingContextSlot>]) -> Option<usize> {
    if queue.ready.is_empty() {
        return None;
    }

    // Pass 1: avg vruntime of threads with budget only.
    let mut sum: u128 = 0;
    let mut count: u64 = 0;

    for t in &queue.ready {
        if has_budget(t, contexts) {
            sum += t.scheduling.eevdf.vruntime as u128;
            count += 1;
        }
    }

    if count == 0 {
        return None; // All threads exhausted — wait for replenishment.
    }

    let avg = (sum / count as u128) as u64;
    // Pass 2: eligible + budget → earliest deadline; also track fallback.
    let mut best: Option<(usize, u64)> = None;
    let mut fallback: Option<(usize, u64)> = None;

    for (i, t) in queue.ready.iter().enumerate() {
        if !has_budget(t, contexts) {
            continue;
        }

        let eevdf = &t.scheduling.eevdf;

        if eevdf.is_eligible(avg) {
            let deadline = eevdf.virtual_deadline();

            if best.is_none_or(|(_, d)| deadline < d) {
                best = Some((i, deadline));
            }
        }

        if fallback.is_none_or(|(_, v)| eevdf.vruntime < v) {
            fallback = Some((i, eevdf.vruntime));
        }
    }

    best.or(fallback).map(|(idx, _)| idx)
}
/// Blocked list is intentionally not searched: this is only called when
/// `try_wake` already returned false, meaning the thread is Running or Ready
/// (not yet blocked). A blocked thread would have been woken by `try_wake`.
#[inline(never)]
fn set_wake_pending_inner(s: &mut State, id: ThreadId) {
    for core_state in s.cores.iter_mut() {
        if let Some(t) = &mut core_state.current {
            if t.id() == id {
                t.wake_pending = true;
                t.wake_result = 0;

                return;
            }
        }
    }
    for local_queue in s.local_queues.iter_mut() {
        for t in local_queue.ready.iter_mut() {
            if t.id() == id {
                t.wake_pending = true;
                t.wake_result = 0;

                return;
            }
        }
    }
}
/// Select the best steal victim from a remote queue: highest vlag (most
/// underserved) thread that has budget. Returns index into the remote queue.
fn select_steal_victim(
    queue: &LocalRunQueue,
    contexts: &[Option<SchedulingContextSlot>],
) -> Option<usize> {
    if queue.ready.is_empty() {
        return None;
    }

    let avg = queue.avg_vruntime();
    let mut best: Option<(usize, i64)> = None;

    for (i, t) in queue.ready.iter().enumerate() {
        if !has_budget(t, contexts) {
            continue;
        }

        let vlag = t.scheduling.eevdf.compute_vlag(avg);

        if best.is_none_or(|(_, v)| vlag > v) {
            best = Some((i, vlag));
        }
    }

    best.map(|(i, _)| i)
}
/// Steal threads from a remote core using workload-granularity migration.
/// Prefers to steal all threads sharing the same scheduling context as the
/// primary victim, up to half the victim's load (never drain a core).
/// Normalizes vruntimes via vlag preservation.
fn steal_from(s: &mut State, my_core: usize, victim_core: usize) -> bool {
    let victim_avg = s.local_queues[victim_core].avg_vruntime();
    let primary_idx =
        match select_steal_victim(&s.local_queues[victim_core], &s.scheduling_contexts) {
            Some(i) => i,
            None => return false,
        };
    let context_id = s.local_queues[victim_core].ready[primary_idx]
        .scheduling
        .context_id;
    let max_steal = (s.local_queues[victim_core].load / 2).max(1) as usize;
    // Collect indices of threads to steal (same context, up to max_steal).
    let mut steal_indices: Vec<usize> = Vec::new();

    steal_indices.push(primary_idx);

    if let Some(ctx_id) = context_id {
        for (i, t) in s.local_queues[victim_core].ready.iter().enumerate() {
            if i == primary_idx {
                continue;
            }

            if t.scheduling.context_id == Some(ctx_id) && has_budget(t, &s.scheduling_contexts) {
                steal_indices.push(i);

                if steal_indices.len() >= max_steal {
                    break;
                }
            }
        }
    }

    // Steal in reverse index order (swap_remove from back preserves earlier indices).
    steal_indices.sort_unstable();

    let dest_avg = s.local_queues[my_core].avg_vruntime();

    for &idx in steal_indices.iter().rev() {
        let mut thread = s.local_queues[victim_core].ready.swap_remove(idx);
        let vlag = thread.scheduling.eevdf.compute_vlag(victim_avg);

        thread.scheduling.eevdf = thread.scheduling.eevdf.apply_vlag(vlag, dest_avg);
        thread.scheduling.last_core = my_core as u32;

        s.local_queues[my_core].ready.push(thread);
    }

    s.local_queues[victim_core].update_load();
    s.local_queues[my_core].update_load();

    true
}
/// Swap TTBR0 when the address space changes between old and new threads.
///
/// Invalidates the old ASID's TLB entries after switching. Without this,
/// stale TLB entries from the old process remain cached — if a physical
/// page is freed and reused (e.g., as a kernel stack), the stale entry
/// creates a memory alias that corrupts the new allocation.
///
/// Uses per-ASID invalidation (TLBI ASIDE1IS) instead of full flush
/// (TLBI VMALLE1IS) to avoid unnecessarily flushing kernel I-TLB entries
/// on all cores, which could cause transient performance issues and
/// interact badly with speculative execution.
#[inline(never)]
fn swap_ttbr0(old: &Thread, new: &Thread, processes: &[Option<Process>]) {
    let old_ttbr0 = ttbr0_for(old);
    let new_ttbr0 = ttbr0_for(new);

    if old_ttbr0 != new_ttbr0 {
        // Extract old ASID from TTBR0 bits [63:48].
        let old_asid = old_ttbr0 >> 48;

        // SAFETY: old_asid and new_ttbr0 are valid values from thread
        // address spaces. The arch implementation handles TLB invalidation
        // and page table root switch.
        unsafe {
            super::arch::scheduler::switch_address_space(old_asid, new_ttbr0);
        }

        // Load per-process PAC keys for the new address space.
        // Only on address space change — threads within the same process
        // share keys, so intra-process context switches skip this.
        if let Some(new_pid) = new.process_id {
            if let Some(Some(process)) = processes.get(new_pid.0 as usize) {
                super::arch::security::set_pac_keys(&process.pac_keys);
            }
        }
    }
}
/// Shared wake implementation. If `reason` is Some, the thread's wait set
/// is consulted to compute the return index and patch `context.x[0]`.
#[inline(never)]
fn try_wake_impl(s: &mut State, id: ThreadId, reason: Option<&HandleObject>) -> bool {
    // Check blocked list — most common case for wake.
    if let Some(pos) = s.blocked.iter().position(|t| t.id() == id) {
        let mut thread = s.blocked.swap_remove(pos);

        if thread.wake() {
            if let Some(obj) = reason {
                let result = thread.complete_wait_for(obj);

                thread.context.set_arg(0, result);
            }

            thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

            let target = pick_target_core(s, thread.scheduling.last_core as usize);
            s.local_queues[target].ready.push(thread);
            s.local_queues[target].update_load();

            // IPI an idle core so it picks up the newly-ready thread.
            ipi_kick_idle_core(s, per_core::core_id() as usize);

            return true;
        }

        // Not actually blocked — put it back.
        s.blocked.push(thread);

        return false;
    }

    // Check current threads on all cores (thread might be Running).
    for core_state in s.cores.iter_mut() {
        if let Some(t) = &mut core_state.current {
            if t.id() == id {
                return t.wake();
            }
        }
    }
    // Check all local queues (unlikely — blocked threads shouldn't be here).
    for local_queue in s.local_queues.iter_mut() {
        for t in local_queue.ready.iter_mut() {
            if t.id() == id {
                return t.wake();
            }
        }
    }

    false
}
fn ttbr0_for(thread: &Thread) -> u64 {
    match thread.process_id {
        Some(_) => thread.ttbr0,
        None => memory::empty_ttbr0(),
    }
}

/// Bind a scheduling context to the current thread.
///
/// The caller (syscall layer) validates that `ctx_id` refers to a valid,
/// live context via handle lookup. Returns false if the thread already has
/// a context bound.
///
/// Increments the context's ref_count so the slot stays alive as long as
/// the thread holds a bind reference. Decremented on thread exit or
/// process kill.
#[inline(never)]
pub fn bind_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    // Check preconditions before mutating ref_count.
    if thread.scheduling.context_id.is_some() {
        return false;
    }

    // Verify context exists and increment ref_count.
    match s.scheduling_contexts.get_mut(ctx_id.0 as usize) {
        Some(Some(entry)) => entry.ref_count += 1,
        _ => return false,
    }

    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.scheduling.context_id = Some(ctx_id);

    true
}
/// Block the current thread waiting for a userspace pager to supply a page.
///
/// The thread transitions to Blocked with `pager_wait` set to
/// `(vmo_id, page_offset)`. When `pager_supply` commits that page,
/// `wake_pager_waiters` scans the blocked list and wakes matching threads.
///
/// Called from `user_fault_handler` (not a syscall path), so the Context
/// pointer comes from the exception frame.
pub fn block_current_for_pager(
    ctx: *mut Context,
    vmo_id: super::vmo::VmoId,
    page_offset: u64,
) -> *const Context {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.pager_wait = Some((vmo_id, page_offset));

    thread.block();

    schedule_inner(&mut s, ctx, core)
}
/// Block the current thread and reschedule, unless a wake is already pending.
///
/// Used by both `futex_wait` and `wait` syscalls. The sequence is:
/// 1. Caller records intent (futex table entry or wait set on thread).
/// 2. Caller releases its lock.
/// 3. This function checks `wake_pending` — if set, clears it, patches
///    x0 with `wake_result`, and returns immediately (the waker already
///    ran during step 2). Otherwise, blocks and reschedules.
///
/// This prevents the lost-wakeup race: a wake that arrives between
/// steps 1 and 3 sets the pending flag instead of trying to unblock
/// a thread that isn't blocked yet.
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
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    if thread.wake_pending {
        // Waker already ran — consume the flag, patch return value, don't block.
        thread.wake_pending = false;

        let wake_result = thread.wake_result;

        thread.wait_set.clear();

        // SAFETY: Write return value via raw pointer to avoid aliasing UB.
        // `ctx` is a `*mut Context` pointing to the current thread's context
        // (offset 0 of the Thread). We cannot create `&mut *ctx` because
        // `s: &mut State` already borrows the State that contains this Context
        // (via `cores[core].current`). Using `addr_of_mut!` on the raw pointer
        // avoids creating a second `&mut` reference. The pointer is valid and
        // properly aligned because it comes from a live Box<Thread>.
        unsafe {
            let x0_ptr = core::ptr::addr_of_mut!((*ctx).x) as *mut u64;

            x0_ptr.write(wake_result);
        }

        return BlockResult::WokePending(ctx as *const Context);
    }

    thread.block();

    let result = schedule_inner(&mut s, ctx, core);
    // SAFETY: Reading from a local variable via read_volatile. The address (&canary) is
    // valid stack memory that persists across the context switch (this thread's stack is
    // preserved). read_volatile ensures the compiler reads the actual memory value, not
    // a cached register copy, which is essential for detecting stack corruption.
    let check = unsafe { core::ptr::read_volatile(&canary) };

    if check != 0xDEAD_BEEF_CAFE_BABE {
        panic!("block_current: stack canary corrupt (got {check:#018x})");
    }

    BlockResult::Blocked(result)
}
/// Borrow another thread's scheduling context (context donation).
///
/// Saves the current context and switches to the borrowed one. The caller
/// (syscall layer) validates that `ctx_id` refers to a valid, live context
/// via handle lookup.
///
/// Increments the borrowed context's ref_count. Decremented on return
/// (`return_scheduling_context`) or thread exit.
#[inline(never)]
pub fn borrow_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    // Check preconditions before mutating ref_count.
    if thread.scheduling.saved_context_id.is_some() {
        return false;
    }

    let saved = thread.scheduling.context_id;

    // Verify context exists and increment ref_count.
    match s.scheduling_contexts.get_mut(ctx_id.0 as usize) {
        Some(Some(entry)) => entry.ref_count += 1,
        _ => return false,
    }

    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.scheduling.saved_context_id = saved;
    thread.scheduling.context_id = Some(ctx_id);

    true
}
/// Clear the wait set and any pending wake on the current thread.
/// Called when a handle is found ready during the initial scan (no need
/// to block) or when returning early (poll mode, error).
#[inline(never)]
pub fn clear_wait_state() {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

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
/// Create a new process with the given address space. Returns the ProcessId.
///
/// The process starts with an empty handle table. No threads yet — call
/// `spawn_user` to add the initial thread.
#[inline(never)]
pub fn create_process(
    addr_space: Box<super::address_space::AddressSpace>,
    pac_keys: super::arch::security::PacKeys,
) -> ProcessId {
    let mut s = STATE.lock();
    let id = s.next_process_id;

    s.next_process_id += 1;

    let process = Process::new(addr_space, pac_keys);

    s.processes.push(Some(process));

    ProcessId(id)
}
/// Create a new scheduling context. Returns the SchedulingContextId.
///
/// The context starts with ref_count=1 (the handle inserted by the caller).
/// Does not bind the context to any thread — use `bind_scheduling_context`
/// separately.
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
    // Reuse freed ID or allocate new one.
    let id = if let Some(free_id) = s.free_context_ids.pop() {
        s.scheduling_contexts[free_id as usize] = Some(slot);

        SchedulingContextId(free_id)
    } else {
        let len = s.scheduling_contexts.len();

        if len > u32::MAX as usize {
            return None; // ID space exhausted
        }

        s.scheduling_contexts.push(Some(slot));

        SchedulingContextId(len as u32)
    };

    Some(id)
}
/// Access the current thread's process via closure. Acquires the scheduler
/// lock for the duration. Panics if the current thread has no process
/// (kernel threads).
#[inline(never)]
pub fn current_process_do<R>(f: impl FnOnce(&mut Process) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let pid = s.cores[core]
        .current
        .as_ref()
        .expect("no current thread")
        .process_id
        .expect("kernel thread has no process")
        .0 as usize;
    let process = s.processes[pid].as_mut().expect("process not found");

    f(process)
}
/// Access the current thread's process AND its ProcessId via closure.
/// Single lock acquisition for operations that need both (e.g., VMO mapping
/// which must record the ProcessId in the VMO's mapping tracker).
#[inline(never)]
pub fn current_process_with_pid_do<R>(f: impl FnOnce(ProcessId, &mut Process) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let pid = s.cores[core]
        .current
        .as_ref()
        .expect("no current thread")
        .process_id
        .expect("kernel thread has no process");
    let process = s.processes[pid.0 as usize]
        .as_mut()
        .expect("process not found");

    f(pid, process)
}
/// Access both the current thread and its process via closure.
///
/// Uses struct destructuring to obtain disjoint mutable borrows of
/// `State.cores` and `State.processes` simultaneously.
#[inline(never)]
pub fn current_thread_and_process_do<R>(f: impl FnOnce(&mut Thread, &mut Process) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let pid = s.cores[core]
        .current
        .as_ref()
        .expect("no current thread")
        .process_id
        .expect("kernel thread has no process")
        .0 as usize;
    // Destructure State for disjoint field borrows (cores vs processes).
    let State {
        ref mut cores,
        ref mut processes,
        ..
    } = *s;
    let thread = cores[core].current.as_mut().expect("no current thread");
    let process = processes[pid].as_mut().expect("process not found");

    f(thread, process)
}
/// Access the current thread via closure. Acquires the scheduler lock for the
/// duration of the closure. Do not call scheduler functions from within `f`.
#[inline(never)]
pub fn current_thread_do<R>(f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    f(thread)
}
#[inline(never)]
pub fn exit_current_from_syscall(ctx: *mut Context) -> *const Context {
    let core = per_core::core_id() as usize;
    // Phase 1: determine if this is the last thread, collect resources.
    let exit_info = {
        let mut s = STATE.lock();
        // Release all scheduling context bind/borrow refs on exit.
        // Take IDs first to avoid overlapping borrows on `s`.
        let (ctx_id, saved_ctx_id) = {
            let thread = s.cores[core].current.as_mut().expect("no current thread");
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

        // Unwrap justified: cores[core].current always set after scheduler::init.
        let tid = s.cores[core].current.as_ref().unwrap().id();
        let pid = s.cores[core]
            .current
            .as_ref()
            .unwrap()
            .process_id
            // Expect justified: only user threads call exit_current_from_syscall.
            .expect("not a user thread");
        let process = s.processes[pid.0 as usize]
            .as_mut()
            // Expect justified: process slot confirmed via thread's process_id.
            .expect("process not found");

        process.thread_count = process.thread_count.saturating_sub(1);

        let is_last = process.thread_count == 0;

        if is_last {
            // Take the entire process — we're destroying it.
            // Expect justified: process slot confirmed present (is_last path).
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
    // This timer is NOT tracked in the handle table — it's an internal
    // resource from `wait` with a finite timeout. Must be destroyed
    // explicitly or the 32-slot timer table leaks a slot.
    let timeout_timer = {
        let mut s = STATE.lock();
        let thread = s.cores[core].current.as_mut().expect("no current thread");

        thread.timeout_timer.take()
    };

    if let Some(timer_id) = timeout_timer {
        timer::destroy(timer_id);
    }

    // Phase 2: notify thread exit (acquires thread_exit lock, then scheduler lock).
    let thread_id = exit_info.thread_id();
    let process_id = exit_info.process_id();
    let is_last = matches!(exit_info, ExitInfo::Last { .. });

    super::thread_exit::notify_exit(thread_id);

    // Notify process exit if this was the last thread.
    if is_last {
        super::process_exit::notify_exit(process_id);

        // Init exiting means the system has no more work to do. Shut down
        // cleanly via PSCI rather than leaving cores in idle loops forever.
        if super::process::is_init(process_id) {
            super::serial::puts("init exited — shutting down\n");
            super::power::system_off();
        }
    }

    // Phase 2a: remove from futex wait queues (acquires futex lock, not scheduler).
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
            let thread = s.cores[core].current.as_mut().expect("no current thread");

            thread.mark_exited();

            schedule_inner(&mut s, ctx, core)
        }
        ExitInfo::NonLast { .. } => {
            // Non-last thread: just mark exited and schedule. The thread's
            // kernel stack is reclaimed when the Thread is reaped/dropped.
            let mut s = STATE.lock();
            let thread = s.cores[core].current.as_mut().expect("no current thread");

            thread.mark_exited();

            schedule_inner(&mut s, ctx, core)
        }
    }
}
#[inline(never)]
pub fn init() {
    let mut s = STATE.lock();
    let boot_thread = Thread::new_boot_idle(0);
    let ctx_ptr = boot_thread.context_ptr();

    // The boot thread IS the idle thread. It starts as `current` and moves to
    // the `idle` slot on the first schedule_inner that picks a user thread.
    // No separate idle thread needed — the zeroed Context is populated by
    // save_context on the first exception (timer IRQ during WFI loop).
    s.cores[0].current = Some(boot_thread);
    s.cores[0].is_idle = true;

    // Create the default scheduling context for kernel-spawned user threads.
    // ref_count starts at 1 (the State itself holds a logical reference).
    let now = now_ns();
    let context = SchedulingContext::new(DEFAULT_BUDGET_NS, DEFAULT_PERIOD_NS, now);
    let slot = SchedulingContextSlot {
        context,
        ref_count: 1,
    };

    s.scheduling_contexts.push(Some(slot));

    s.default_context_id = Some(SchedulingContextId(0));

    // Set current-thread pointer so exception.S can locate the save area.
    // SAFETY: ctx_ptr points to the Context at offset 0 of the boot thread,
    // which lives in a Box (stable address) stored in the scheduler state.
    unsafe {
        super::arch::scheduler::set_current_thread(ctx_ptr as usize);
    }
}
/// Initialize a secondary core's scheduler state with a boot/idle thread.
///
/// Called from `secondary_main` on each secondary core. Creates a single
/// boot/idle thread (same as core 0's pattern) and sets TPIDR_EL1.
#[inline(never)]
pub fn init_secondary(core_id: u32) {
    let mut s = STATE.lock();
    let idx = core_id as usize;
    // Single boot/idle thread per core — no separate idle thread.
    let boot_thread = Thread::new_boot_idle(core_id as u64);
    let boot_ctx_ptr = boot_thread.context_ptr();

    s.cores[idx].current = Some(boot_thread);
    // Mark idle so ipi_kick_idle_core can target this core. Without this,
    // is_idle stays false (default) until schedule_inner runs — but
    // schedule_inner only runs in response to IRQs, and no IRQ arrives
    // unless someone sends an IPI, creating a deadlock.
    s.cores[idx].is_idle = true;

    // SAFETY: boot_ctx_ptr points to a stable Context in a Box.
    unsafe {
        super::arch::scheduler::set_current_thread(boot_ctx_ptr as usize);
    }
}
/// Kill all threads of a process and collect resources for cleanup.
///
/// Threads in the ready queue, blocked list, and suspended list are removed
/// and dropped immediately. Threads running on other cores are marked Exited
/// and will be reaped on their next schedule (deferred cleanup via
/// `maybe_cleanup_killed_process` in `schedule_inner`).
///
/// Returns `None` if the process doesn't exist, is already killed, or has no
/// threads. The caller must perform Phase 2 cleanup (notify exits, close
/// resources, free address space if returned).
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
    // Remove target threads from all local ready queues.
    for q in 0..per_core::MAX_CORES {
        let mut i = 0;

        while i < s.local_queues[q].ready.len() {
            if s.local_queues[q].ready[i].process_id == Some(target_pid) {
                let mut thread = s.local_queues[q].ready.swap_remove(i);

                release_thread_context_ids(&mut s, &mut thread);

                // Collect internal timeout timer (not tracked in handle table).
                if let Some(timer_id) = thread.timeout_timer.take() {
                    timeout_timers.push(timer_id);
                }

                killed_threads.push(thread.id());
            } else {
                i += 1;
            }
        }

        s.local_queues[q].update_load();
    }

    // Remove from blocked list.
    let mut i = 0;

    while i < s.blocked.len() {
        if s.blocked[i].process_id == Some(target_pid) {
            let mut thread = s.blocked.swap_remove(i);

            release_thread_context_ids(&mut s, &mut thread);

            // Collect internal timeout timer (not tracked in handle table).
            if let Some(timer_id) = thread.timeout_timer.take() {
                timeout_timers.push(timer_id);
            }

            killed_threads.push(thread.id());
        } else {
            i += 1;
        }
    }

    // Remove from suspended list.
    let mut i = 0;

    while i < s.suspended.len() {
        if s.suspended[i].process_id == Some(target_pid) {
            let mut thread = s.suspended.swap_remove(i);

            release_thread_context_ids(&mut s, &mut thread);

            // Suspended threads should never have timeout timers (they haven't
            // started running yet), but take it defensively.
            if let Some(timer_id) = thread.timeout_timer.take() {
                timeout_timers.push(timer_id);
            }

            killed_threads.push(thread.id());
        } else {
            i += 1;
        }
    }

    // Mark running threads on other cores as Exited. Collect context IDs and
    // timeout timers to release after the loop (can't call release_context_inner
    // while iterating s.cores due to borrow conflict).
    let mut deferred_context_releases: Vec<SchedulingContextId> = Vec::new();

    for core_state in s.cores.iter_mut() {
        if let Some(t) = &mut core_state.current {
            if t.process_id == Some(target_pid) {
                killed_threads.push(t.id());

                if let Some(id) = t.scheduling.context_id.take() {
                    deferred_context_releases.push(id);
                }
                if let Some(id) = t.scheduling.saved_context_id.take() {
                    deferred_context_releases.push(id);
                }

                // Collect internal timeout timer from running thread.
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
    // Unwrap justified: process validated at top of kill_process (early-return on None).
    let handle_objects: Vec<HandleObject> = {
        let process = s.processes[target_pid.0 as usize].as_mut().unwrap();

        process.handles.drain().map(|(obj, _, _)| obj).collect()
    };
    let handles = categorize_handles(handle_objects, &mut s);
    // Take or defer the process based on whether threads are still running.
    let address_space = if running_count == 0 {
        // No threads running on any core — take the process for immediate cleanup.
        // Unwrap justified: process validated at top of kill_process.
        let process = s.processes[target_pid.0 as usize].take().unwrap();

        Some(process.address_space)
    } else {
        // Threads still running on other cores — defer address space cleanup.
        // Set thread_count to the number of still-running threads so
        // maybe_cleanup_killed_process can track when they're all reaped.
        // Unwrap justified: process validated at top of kill_process.
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
/// Release a scheduling context handle (decrement ref count, free if zero).
#[inline(never)]
pub fn release_scheduling_context(ctx_id: SchedulingContextId) {
    let mut s = STATE.lock();

    release_context_inner(&mut s, ctx_id);
}
/// Remove an orphaned process that has no threads.
///
/// Called on error paths where `create_process` succeeded but thread
/// creation failed. The process's `Box<AddressSpace>` is dropped, which
/// triggers its `Drop` impl (invalidate TLB + free all frames + free ASID).
///
/// # Panics
///
/// Debug-asserts that the process has zero threads. Removing a process
/// with live threads would leak those threads.
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

        // Take and drop the process — AddressSpace::drop handles cleanup.
        *slot = None;
    }
}
/// Return a borrowed scheduling context, restoring the saved one.
///
/// Decrements the borrowed context's ref_count (balances the increment
/// in `borrow_scheduling_context`).
#[inline(never)]
pub fn return_scheduling_context() -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    match thread.scheduling.saved_context_id.take() {
        Some(saved) => {
            let borrowed = thread.scheduling.context_id;

            thread.scheduling.context_id = Some(saved);

            // Release the borrow ref.
            if let Some(id) = borrowed {
                release_context_inner(&mut s, id);
            }

            true
        }
        None => false, // Not borrowing.
    }
}
#[inline(never)]
pub fn schedule(ctx: *mut Context) -> *const Context {
    // Stack canary: detect corruption of this function's stack frame during
    // schedule_inner. Write a known value to a stack slot BEFORE schedule_inner,
    // verify it AFTER. The volatile writes/reads prevent the compiler from
    // reordering them past schedule_inner.
    let mut canary: u64 = 0;

    // SAFETY: volatile write to a stack-local. Ensures the canary is physically
    // written to the stack before schedule_inner runs. No aliasing concern —
    // canary is a local variable owned by this function.
    unsafe {
        core::ptr::write_volatile(&mut canary, 0xDEAD_BEEF_CAFE_BABE_u64);
    }

    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let result = schedule_inner(&mut s, ctx, core);
    // SAFETY: volatile read of our stack-local canary. If schedule_inner
    // (or anything it calls) corrupted our stack frame, this value will differ.
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
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.timeout_timer = Some(timer_id);
}
/// Clear the timeout timer field on the current thread (timer already destroyed).
#[inline(never)]
pub fn set_timeout_timer_none() {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.timeout_timer = None;
}
/// Set the wake-pending flag on a thread that is not yet blocked (futex path).
///
/// Called by `futex::wake` when `try_wake` returns false (thread is still
/// Running, hasn't entered the scheduler yet). Sets `wake_result = 0`.
/// The flag is consumed by `block_current_unless_woken`.
#[inline(never)]
pub fn set_wake_pending(id: ThreadId) {
    let mut s = STATE.lock();

    set_wake_pending_inner(&mut s, id);
}
/// Set the wake-pending flag for a handle-based event (channel signal, etc.).
///
/// Only sets the flag if the thread has an active wait set (is inside a `wait`
/// syscall). Computes `wake_result` from the matching entry in the wait set.
/// If `wake_pending` is already set, does nothing (first signal wins).
#[inline(never)]
pub fn set_wake_pending_for_handle(id: ThreadId, reason: HandleObject) {
    let mut s = STATE.lock();

    // Helper: attempt to set wake pending on a thread reference.
    fn apply(t: &mut Thread, reason: &HandleObject) {
        if !t.wake_pending && !t.wait_set.is_empty() {
            t.wake_result = t.complete_wait_for(reason);
            t.wake_pending = true;
        }
    }

    for core_state in s.cores.iter_mut() {
        if let Some(t) = &mut core_state.current {
            if t.id() == id {
                apply(t, &reason);

                return;
            }
        }
    }

    for local_queue in s.local_queues.iter_mut() {
        for t in local_queue.ready.iter_mut() {
            if t.id() == id {
                apply(t, &reason);

                return;
            }
        }
    }
}
#[inline(never)]
pub fn spawn_user(process_id: ProcessId, entry_va: u64, user_stack_top: u64) -> Option<ThreadId> {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let process = s.processes[process_id.0 as usize]
        .as_mut()
        .expect("process not found");
    let ttbr0 = process.address_space.ttbr0_value();
    let mut thread = Thread::new_user(id, process_id, ttbr0, entry_va, user_stack_top)?;

    process.thread_count += 1;

    bind_default_context(&mut s, &mut thread);

    let target = pick_target_core(&s, per_core::core_id() as usize);

    s.local_queues[target].ready.push(thread);
    s.local_queues[target].update_load();

    // IPI an idle core so it picks up the newly-spawned thread.
    ipi_kick_idle_core(&s, per_core::core_id() as usize);

    Some(ThreadId(id))
}
/// Like `spawn_user`, but the thread is placed in the suspended list instead
/// of the ready queue. Call `start_suspended_threads` to make it runnable.
///
/// Used by `process_create` (two-phase creation: create suspended, then start).
#[inline(never)]
pub fn spawn_user_suspended(
    process_id: ProcessId,
    entry_va: u64,
    user_stack_top: u64,
) -> Option<ThreadId> {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let process = s.processes[process_id.0 as usize]
        .as_mut()
        .expect("process not found");
    let ttbr0 = process.address_space.ttbr0_value();
    let mut thread = Thread::new_user(id, process_id, ttbr0, entry_va, user_stack_top)?;

    process.thread_count += 1;

    bind_default_context(&mut s, &mut thread);

    s.suspended.push(thread);

    Some(ThreadId(id))
}
/// Move all suspended threads belonging to `process_id` into the ready queue.
///
/// Returns true if any threads were started. Used by `process_start` syscall.
#[inline(never)]
pub fn start_suspended_threads(process_id: ProcessId) -> bool {
    let mut s = STATE.lock();
    let mut started = false;
    let mut i = 0;

    while i < s.suspended.len() {
        if s.suspended[i].process_id == Some(process_id) {
            let thread = s.suspended.swap_remove(i);
            let target = pick_target_core(&s, thread.scheduling.last_core as usize);

            s.local_queues[target].ready.push(thread);
            s.local_queues[target].update_load();

            started = true;
            // Don't increment i — swap_remove moved the last element here.
        } else {
            i += 1;
        }
    }

    if started {
        if let Some(Some(process)) = s.processes.get_mut(process_id.0 as usize) {
            process.started = true;
        }

        // IPI an idle core so it picks up the newly-ready threads.
        ipi_kick_idle_core(&s, per_core::core_id() as usize);
    }

    started
}
/// Suspend a thread by ThreadId. Removes it from the ready queue or blocked
/// list and moves it to the suspended list. Returns true on success, false
/// if the thread is currently running (caller must retry after preemption)
/// or not found.
pub fn suspend_thread(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    // Check all local ready queues.
    for local_queue in s.local_queues.iter_mut() {
        if let Some(pos) = local_queue.ready.iter().position(|t| t.id() == id) {
            let thread = local_queue.ready.swap_remove(pos);

            local_queue.update_load();
            s.suspended.push(thread);

            return true;
        }
    }

    // Check blocked list.
    if let Some(pos) = s.blocked.iter().position(|t| t.id() == id) {
        let thread = s.blocked.swap_remove(pos);

        s.suspended.push(thread);

        return true;
    }

    // Thread is running or not found — can't suspend.
    false
}
/// Resume a suspended thread. Moves it from the suspended list to the ready
/// queue. Returns true on success, false if not found in suspended list.
pub fn resume_thread(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    if let Some(pos) = s.suspended.iter().position(|t| t.id() == id) {
        let mut thread = s.suspended.swap_remove(pos);

        thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

        let target = pick_target_core(&s, thread.scheduling.last_core as usize);

        s.local_queues[target].ready.push(thread);
        s.local_queues[target].update_load();

        ipi_kick_idle_core(&s, per_core::core_id() as usize);

        return true;
    }

    false
}
/// Read the saved register state of a suspended thread.
///
/// Copies the thread's Context to a caller-provided buffer. The thread
/// must be in the suspended list (not running). Returns the Context size
/// in bytes on success, or 0 if the thread is not suspended.
/// # Safety
///
/// `dst` must point to a valid, writable buffer of at least
/// `size_of::<Context>()` bytes.
pub unsafe fn read_thread_state(id: ThreadId, dst: *mut u8) -> bool {
    let s = STATE.lock();

    if let Some(thread) = s.suspended.iter().find(|t| t.id() == id) {
        let src = &thread.context as *const super::Context as *const u8;
        let size = core::mem::size_of::<super::Context>();

        // SAFETY: src is the thread's Context (valid, aligned). dst validated by caller.
        core::ptr::copy_nonoverlapping(src, dst, size);

        true
    } else {
        false
    }
}
/// Push a single entry to the current thread's wait set.
///
/// Used by `sys_wait` to add the internal timeout timer entry after the
/// main wait set is populated in-place.
#[inline(never)]
pub fn push_wait_entry(entry: WaitEntry) {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.wait_set.push(entry);
}
/// Take and return stale waiter entries from the current thread.
/// Called at the start of `sys_wait` to clean up registrations from a
/// previous wait that took the BlockResult::Blocked path.
#[inline(never)]
pub fn take_stale_waiters() -> Vec<WaitEntry> {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    core::mem::take(&mut thread.stale_waiters)
}
/// Take and return any stale timeout timer from the current thread.
/// Called at the start of `sys_wait` to clean up from a previous blocked wait.
#[inline(never)]
pub fn take_timeout_timer() -> Option<timer::TimerId> {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.timeout_timer.take()
}
/// Wake a blocked thread (Blocked → Ready). Returns true if it was blocked.
/// Does not interact with the thread's wait set — used by futex.
#[inline(never)]
pub fn try_wake(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    try_wake_impl(&mut s, id, None)
}
/// Wake a blocked thread and resolve its wait set against `reason`.
///
/// If the thread has an active wait set (from a `wait` syscall), computes the
/// return index from the matching entry and patches `context.x[0]`. Used by
/// channel signal (and future timer/device notification).
#[inline(never)]
pub fn try_wake_for_handle(id: ThreadId, reason: HandleObject) -> bool {
    let mut s = STATE.lock();

    try_wake_impl(&mut s, id, Some(&reason))
}
/// Two-phase wake: attempt to wake a blocked thread, falling back to
/// setting the wake-pending flag if the thread isn't blocked yet.
///
/// This is the standard pattern used after releasing a subsystem lock
/// (channel, timer, event, interrupt, etc.) to maintain lock ordering:
/// subsystem lock → scheduler lock. The two phases handle the race where
/// the target thread may not have blocked yet when the signal arrives.
pub fn wake_for_handle(id: ThreadId, reason: HandleObject) {
    if !try_wake_for_handle(id, reason) {
        set_wake_pending_for_handle(id, reason);
    }
}
/// Wake all threads blocked on pager faults for the given VMO page range.
///
/// Called by `pager_supply` after pages have been committed. Threads are
/// woken and will re-enter the fault handler, finding committed pages.
pub fn wake_pager_waiters(vmo_id: super::vmo::VmoId, offset: u64, count: u64) {
    let mut s = STATE.lock();
    let range_end = offset + count;
    // Collect indices of blocked threads that match.
    let mut to_wake = Vec::new();

    for (i, thread) in s.blocked.iter().enumerate() {
        if let Some((wait_vmo, wait_page)) = thread.pager_wait {
            if wait_vmo == vmo_id && wait_page >= offset && wait_page < range_end {
                to_wake.push(i);
            }
        }
    }

    // Wake in reverse order (swap_remove from back to front preserves indices).
    for &idx in to_wake.iter().rev() {
        let mut thread = s.blocked.swap_remove(idx);

        thread.pager_wait = None;

        if thread.wake() {
            // Mark eligible and enqueue — same pattern as try_wake_impl.
            thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

            let target = pick_target_core(&s, thread.scheduling.last_core as usize);

            s.local_queues[target].ready.push(thread);
            s.local_queues[target].update_load();
        } else {
            // Thread state wasn't Blocked (shouldn't happen, but defensive).
            s.blocked.push(thread);
        }
    }

    // IPI an idle core so it picks up newly-ready threads.
    if !to_wake.is_empty() {
        ipi_kick_idle_core(&mut s, per_core::core_id() as usize);
    }
}
/// Access a process by ProcessId. Acquires the scheduler lock.
///
/// Returns `None` if the process doesn't exist (e.g., already killed).
/// Used by `channel::setup_endpoint` (boot), `handle_send` (syscall).
#[inline(never)]
pub fn with_process<R>(pid: ProcessId, f: impl FnOnce(&mut Process) -> R) -> Option<R> {
    let mut s = STATE.lock();
    let process = s.processes.get_mut(pid.0 as usize)?.as_mut()?;

    Some(f(process))
}
