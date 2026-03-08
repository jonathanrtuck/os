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
//! Global run queue with a single lock — fine for ≤8 cores. Idle threads
//! (one per core) are never enqueued; they run as fallback when no threads
//! are runnable.

use super::address_space::AddressSpace;
use super::handle::HandleObject;
use super::memory;
use super::per_core;
use super::scheduling_algorithm;
use super::scheduling_context::{self, SchedulingContext, SchedulingContextId};
use super::sync::IrqMutex;
use super::thread::{Thread, ThreadId};
use super::timer;
use super::Context;
use alloc::{boxed::Box, vec::Vec};

struct PerCoreState {
    current: Option<Box<Thread>>,
    idle: Option<Box<Thread>>,
}
/// Single run queue — linear scan for EEVDF selection.
struct RunQueue {
    ready: Vec<Box<Thread>>,
}
struct State {
    queue: RunQueue,
    /// Threads waiting on a resource (Blocked state). Moved here from
    /// cores[].current when a thread blocks; moved back to queue by wake().
    blocked: Vec<Box<Thread>>,
    cores: [PerCoreState; per_core::MAX_CORES],
    next_id: u64,
    /// All scheduling contexts. Index = SchedulingContextId.0.
    scheduling_contexts: Vec<SchedulingContext>,
    next_scheduling_context_id: u32,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    queue: RunQueue { ready: Vec::new() },
    blocked: Vec::new(),
    cores: {
        const INIT: PerCoreState = PerCoreState {
            current: None,
            idle: None,
        };
        [INIT; per_core::MAX_CORES]
    },
    next_id: 1,
    scheduling_contexts: Vec::new(),
    next_scheduling_context_id: 0,
});

/// Charge elapsed time to the old thread's EEVDF vruntime and scheduling context.
fn charge_thread(thread: &mut Thread, contexts: &mut [SchedulingContext], now: u64) {
    if thread.last_started == 0 {
        return; // Never ran (boot thread before first tick).
    }

    let elapsed = now.saturating_sub(thread.last_started);

    if elapsed == 0 {
        return;
    }

    // Charge EEVDF vruntime.
    thread.scheduling_algorithm = thread.scheduling_algorithm.charge(elapsed);

    // Charge scheduling context budget.
    if let Some(id) = thread.scheduling_context_id {
        if let Some(ctx) = contexts.get_mut(id.0 as usize) {
            *ctx = ctx.charge(elapsed);
        }
    }
}
/// Check if a thread has budget (unlimited if no scheduling context).
fn has_budget(thread: &Thread, contexts: &[SchedulingContext]) -> bool {
    match thread.scheduling_context_id {
        None => true, // Kernel/idle threads: unlimited
        Some(id) => contexts
            .get(id.0 as usize)
            .map_or(true, |ctx| ctx.has_budget()),
    }
}
/// Read hardware counter and convert to nanoseconds.
fn now_ns() -> u64 {
    timer::counter_to_ns(timer::counter())
}
/// Reap exited threads from the run queue and blocked list.
fn reap_exited(queue: &mut RunQueue, blocked: &mut Vec<Box<Thread>>) {
    queue.ready.retain(|t| !t.is_exited());
    blocked.retain(|t| !t.is_exited());
}
/// Replenish all scheduling contexts that are due.
fn replenish_contexts(contexts: &mut [SchedulingContext], now: u64) {
    for ctx in contexts.iter_mut() {
        *ctx = ctx.maybe_replenish(now);
    }
}
fn schedule_inner(s: &mut State, _ctx: *mut Context, core: usize) -> *const Context {
    reap_exited(&mut s.queue, &mut s.blocked);

    let now = now_ns();

    // Replenish any due scheduling contexts.
    replenish_contexts(&mut s.scheduling_contexts, now);

    let mut old_thread = s.cores[core].current.take().expect("no current thread");

    // Charge the old thread for elapsed time.
    charge_thread(&mut old_thread, &mut s.scheduling_contexts, now);

    old_thread.deschedule();

    // Park the old thread in its appropriate location.
    fn park_old(s: &mut State, mut old_thread: Box<Thread>) {
        if old_thread.is_ready() {
            // Update eligible_at before re-enqueuing.
            old_thread.scheduling_algorithm = old_thread.scheduling_algorithm.mark_eligible();

            if !old_thread.is_idle() {
                s.queue.ready.push(old_thread);
            }
            // Idle threads are never enqueued — they go back to cores[].idle.
        } else if old_thread.is_exited() {
            drop(old_thread);
        } else {
            // Blocked — park until wake() re-enqueues it.
            s.blocked.push(old_thread);
        }
    }

    // Try to select a runnable thread via EEVDF.
    if let Some(idx) = select_best(&s.queue, &s.scheduling_contexts) {
        let mut new_thread = s.queue.ready.swap_remove(idx);

        new_thread.activate();
        new_thread.last_started = now;

        swap_ttbr0(&old_thread, &new_thread);

        let new_ctx = new_thread.context_ptr();

        park_old(s, old_thread);

        s.cores[core].current = Some(new_thread);

        new_ctx
    } else if old_thread.is_ready() {
        // No other runnable threads — continue with the old one.
        old_thread.activate();
        old_thread.last_started = now;

        let old_ctx = old_thread.context_ptr();

        s.cores[core].current = Some(old_thread);

        old_ctx
    } else {
        // Old thread exited or blocked, nothing in queue. Run idle thread.
        let mut idle = s.cores[core].idle.take().expect("no idle thread");

        idle.activate();
        idle.last_started = now;

        let idle_ctx = idle.context_ptr();

        swap_ttbr0(&old_thread, &idle);
        park_old(s, old_thread);

        s.cores[core].current = Some(idle);

        idle_ctx
    }
}
/// Select the best thread from the ready queue using EEVDF.
/// Returns the index into `queue.ready`, or None.
fn select_best(queue: &RunQueue, contexts: &[SchedulingContext]) -> Option<usize> {
    if queue.ready.is_empty() {
        return None;
    }

    // Build (State, has_budget) pairs for selection.
    let candidates: Vec<(scheduling_algorithm::State, bool)> = queue
        .ready
        .iter()
        .map(|t| (t.scheduling_algorithm, has_budget(t, contexts)))
        .collect();

    let states: Vec<scheduling_algorithm::State> = candidates.iter().map(|(s, _)| *s).collect();
    let avg = scheduling_algorithm::avg_vruntime(&states);

    scheduling_algorithm::select_next(&candidates, avg)
}
/// Swap TTBR0 when the address space changes between old and new threads.
fn swap_ttbr0(old: &Thread, new: &Thread) {
    let old_ttbr0 = ttbr0_for(old);
    let new_ttbr0 = ttbr0_for(new);

    if old_ttbr0 != new_ttbr0 {
        // SAFETY: new_ttbr0 is a valid TTBR0 value — physical address of an
        // L0 table OR'd with a valid ASID. The barriers ensure ordering.
        unsafe {
            core::arch::asm!(
                "dsb ish",
                "msr ttbr0_el1, {v}",
                "isb",
                v = in(reg) new_ttbr0,
                options(nostack)
            );
        }
    }
}
fn ttbr0_for(thread: &Thread) -> u64 {
    match &thread.address_space {
        Some(addr_space) => addr_space.ttbr0_value(),
        None => memory::empty_ttbr0(),
    }
}

/// Bind a scheduling context to the current thread. The thread must not
/// already have a context bound.
pub fn bind_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let num_contexts = s.scheduling_contexts.len();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    if thread.scheduling_context_id.is_some() {
        return false;
    }
    if ctx_id.0 as usize >= num_contexts {
        return false;
    }

    thread.scheduling_context_id = Some(ctx_id);

    true
}
/// Block the current thread and reschedule. Used by syscalls that need to
/// release other locks before blocking (e.g., channel wait releases the
/// channel lock, then calls this).
pub fn block_current_and_schedule(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.block();

    schedule_inner(&mut s, ctx, core)
}
/// Borrow another thread's scheduling context (context donation).
/// Saves the current context and switches to the borrowed one.
pub fn borrow_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let num_contexts = s.scheduling_contexts.len();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    // Can't borrow if already borrowing.
    if thread.saved_context_id.is_some() {
        return false;
    }
    if ctx_id.0 as usize >= num_contexts {
        return false;
    }

    thread.saved_context_id = thread.scheduling_context_id;
    thread.scheduling_context_id = Some(ctx_id);

    true
}
/// Create a new scheduling context. Returns the SchedulingContextId.
pub fn create_scheduling_context(budget: u64, period: u64) -> Option<SchedulingContextId> {
    if !scheduling_context::validate_params(budget, period) {
        return None;
    }

    let mut s = STATE.lock();
    let id = SchedulingContextId(s.next_scheduling_context_id);
    let now = now_ns();

    s.scheduling_contexts
        .push(SchedulingContext::new(budget, period, now));
    s.next_scheduling_context_id += 1;

    Some(id)
}
/// Access the current thread via closure. Acquires the scheduler lock for the
/// duration of the closure. Do not call scheduler functions from within `f`.
pub fn current_thread_do<R>(f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    f(thread)
}
/// Exit the current kernel thread (no context pointer available).
///
/// Only safe for kernel threads that have no resources (no address space,
/// no handles). User threads must exit via `exit_current_from_syscall` which
/// performs full cleanup. The thread spins until the next timer tick reaps it.
pub fn exit_current() -> ! {
    {
        let mut s = STATE.lock();
        let core = per_core::core_id() as usize;
        let thread = s.cores[core].current.as_mut().expect("no current thread");

        debug_assert!(
            thread.address_space.is_none(),
            "exit_current called on thread with address space — use exit_current_from_syscall"
        );

        thread.mark_exited();
    }

    loop {
        core::hint::spin_loop();
    }
}
pub fn exit_current_from_syscall(ctx: *mut Context) -> *const Context {
    use super::handle::ChannelId;

    let core = per_core::core_id() as usize;
    // Phase 1: collect resources to free (under scheduler lock).
    let (channels_to_close, addr_space) = {
        let mut s = STATE.lock();
        let thread = s.cores[core].current.as_mut().expect("no current thread");

        // Auto-return borrowed scheduling context on exit.
        if let Some(saved) = thread.saved_context_id.take() {
            thread.scheduling_context_id = Some(saved);
        }

        let channels: Vec<ChannelId> = thread
            .handles
            .drain()
            .filter_map(|(_, obj)| match obj {
                HandleObject::Channel(id) => Some(id),
                HandleObject::SchedulingContext(_) => None,
            })
            .collect();
        let addr_space = thread.address_space.take();

        (channels, addr_space)
    };

    // Phase 2: close channel endpoints (acquires channel lock, not scheduler).
    for id in channels_to_close {
        super::channel::close_endpoint(id);
    }

    // Phase 3: free address space (acquires page_allocator and address_space_id locks).
    if let Some(mut addr_space) = addr_space {
        addr_space.invalidate_tlb();
        addr_space.free_all();
        super::address_space_id::free(super::address_space_id::Asid(addr_space.asid()));
    }

    // Phase 4: mark exited and schedule (under scheduler lock).
    let mut s = STATE.lock();
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.mark_exited();

    schedule_inner(&mut s, ctx, core)
}
/// Initialize the scheduler with core 0's boot thread.
pub fn init() {
    let mut s = STATE.lock();
    let boot_thread = Thread::new_boot();
    let ctx_ptr = boot_thread.context_ptr();

    s.cores[0].current = Some(boot_thread);
    // Create idle thread for core 0 (used when no runnable threads exist).
    s.cores[0].idle = Some(Thread::new_idle(0));

    // SAFETY: ctx_ptr points to the Context at offset 0 of the boot thread,
    // which lives in a Box (stable address) stored in the scheduler state.
    // TPIDR_EL1 is read by exception.S to locate the save area.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {0}",
            in(reg) ctx_ptr as usize,
            options(nostack, nomem)
        );
    }
}
/// Initialize a secondary core's scheduler state with an idle thread.
///
/// Called from `secondary_main` on each secondary core. Creates the idle
/// thread, sets TPIDR_EL1, and makes the idle thread the current thread.
pub fn init_secondary(core_id: u32) {
    let mut s = STATE.lock();
    let idx = core_id as usize;
    let idle = Thread::new_idle(core_id as u64);
    let ctx_ptr = idle.context_ptr();

    s.cores[idx].idle = Some(idle);

    // Create a boot thread for this core as its current thread.
    let boot_thread = Thread::new_boot();
    let boot_ctx_ptr = boot_thread.context_ptr();

    s.cores[idx].current = Some(boot_thread);

    // SAFETY: boot_ctx_ptr points to a stable Context in a Box.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {0}",
            in(reg) boot_ctx_ptr as usize,
            options(nostack, nomem)
        );
    }

    // Keep ctx_ptr used so idle isn't optimized away.
    let _ = ctx_ptr;
}
/// Return a borrowed scheduling context, restoring the saved one.
pub fn return_scheduling_context() -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    match thread.saved_context_id.take() {
        Some(saved) => {
            thread.scheduling_context_id = Some(saved);
            true
        }
        None => false, // Not borrowing.
    }
}
pub fn schedule(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;

    schedule_inner(&mut s, ctx, core)
}
pub fn spawn(entry: fn() -> !) {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new(id, entry);

    s.queue.ready.push(thread);
}
pub fn spawn_user(addr_space: Box<AddressSpace>, entry_va: u64, user_stack_top: u64) -> ThreadId {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new_user(id, addr_space, entry_va, user_stack_top);

    s.queue.ready.push(thread);

    ThreadId(id)
}
/// Wake a blocked thread (Blocked → Ready). Returns true if it was blocked.
pub fn try_wake(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    // Check blocked list — most common case for wake.
    if let Some(pos) = s.blocked.iter().position(|t| t.id() == id) {
        let mut thread = s.blocked.swap_remove(pos);

        if thread.wake() {
            thread.scheduling_algorithm = thread.scheduling_algorithm.mark_eligible();

            s.queue.ready.push(thread);

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
    // Check run queue (unlikely — blocked threads shouldn't be here).
    for t in s.queue.ready.iter_mut() {
        if t.id() == id {
            return t.wake();
        }
    }

    false
}
/// Access a thread by ID. Closure receives exclusive access to the thread.
pub fn with_thread_mut<R>(id: ThreadId, f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();

    // Search run queue.
    for t in s.queue.ready.iter_mut() {
        if t.id() == id {
            return f(t);
        }
    }
    // Search blocked list.
    for t in s.blocked.iter_mut() {
        if t.id() == id {
            return f(t);
        }
    }
    // Search current threads on all cores.
    for core_state in s.cores.iter_mut() {
        if let Some(t) = &mut core_state.current {
            if t.id() == id {
                return f(t);
            }
        }
    }

    panic!("thread not found");
}
