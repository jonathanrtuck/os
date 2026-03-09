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
use super::scheduling_context::{self, SchedulingContext, SchedulingContextId};
use super::sync::IrqMutex;
use super::thread::{Thread, ThreadId, WaitEntry};
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
/// Scheduling context with handle-based ownership tracking.
struct SchedulingContextSlot {
    context: SchedulingContext,
    ref_count: u32,
}
struct State {
    queue: RunQueue,
    /// Threads waiting on a resource (Blocked state). Moved here from
    /// cores[].current when a thread blocks; moved back to queue by wake().
    blocked: Vec<Box<Thread>>,
    cores: [PerCoreState; per_core::MAX_CORES],
    next_id: u64,
    /// All scheduling contexts. Index = SchedulingContextId.0.
    /// None = freed slot (available via free_context_ids).
    scheduling_contexts: Vec<Option<SchedulingContextSlot>>,
    /// Freed scheduling context IDs available for reuse.
    free_context_ids: Vec<u32>,
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
    free_context_ids: Vec::new(),
});

/// Charge elapsed time to the old thread's EEVDF vruntime and scheduling context.
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
/// Check if a thread has budget (unlimited if no scheduling context).
fn has_budget(thread: &Thread, contexts: &[Option<SchedulingContextSlot>]) -> bool {
    match thread.scheduling.context_id {
        None => true, // Kernel/idle threads: unlimited
        Some(id) => contexts
            .get(id.0 as usize)
            .and_then(|slot| slot.as_ref())
            .map_or(true, |slot| slot.context.has_budget()),
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
/// Replenish all scheduling contexts that are due.
fn replenish_contexts(contexts: &mut [Option<SchedulingContextSlot>], now: u64) {
    for slot in contexts.iter_mut() {
        if let Some(entry) = slot {
            entry.context = entry.context.maybe_replenish(now);
        }
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
            old_thread.scheduling.eevdf = old_thread.scheduling.eevdf.mark_eligible();

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
        new_thread.scheduling.last_started = now;

        swap_ttbr0(&old_thread, &new_thread);

        let new_ctx = new_thread.context_ptr();

        park_old(s, old_thread);

        s.cores[core].current = Some(new_thread);

        new_ctx
    } else if old_thread.is_ready() {
        // No other runnable threads — continue with the old one.
        old_thread.activate();
        old_thread.scheduling.last_started = now;

        let old_ctx = old_thread.context_ptr();

        s.cores[core].current = Some(old_thread);

        old_ctx
    } else {
        // Old thread exited or blocked, nothing in queue. Run idle thread.
        let mut idle = s.cores[core].idle.take().expect("no idle thread");

        idle.activate();
        idle.scheduling.last_started = now;

        let idle_ctx = idle.context_ptr();

        swap_ttbr0(&old_thread, &idle);
        park_old(s, old_thread);

        s.cores[core].current = Some(idle);

        idle_ctx
    }
}
/// Select the best thread from the ready queue using EEVDF.
///
/// Zero-allocation: computes avg vruntime (over threads with budget only)
/// and selects in two passes over the ready queue. Returns the index into
/// `queue.ready`, or None if no thread has budget.
fn select_best(queue: &RunQueue, contexts: &[Option<SchedulingContextSlot>]) -> Option<usize> {
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

            if best.map_or(true, |(_, d)| deadline < d) {
                best = Some((i, deadline));
            }
        }

        if fallback.map_or(true, |(_, v)| eevdf.vruntime < v) {
            fallback = Some((i, eevdf.vruntime));
        }
    }

    best.or(fallback).map(|(idx, _)| idx)
}
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

    for t in s.queue.ready.iter_mut() {
        if t.id() == id {
            t.wake_pending = true;
            t.wake_result = 0;

            return;
        }
    }
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
/// Shared wake implementation. If `reason` is Some, the thread's wait set
/// is consulted to compute the return index and patch `context.x[0]`.
fn try_wake_impl(s: &mut State, id: ThreadId, reason: Option<&HandleObject>) -> bool {
    // Check blocked list — most common case for wake.
    if let Some(pos) = s.blocked.iter().position(|t| t.id() == id) {
        let mut thread = s.blocked.swap_remove(pos);

        if thread.wake() {
            if let Some(obj) = reason {
                let result = thread.complete_wait_for(obj);

                thread.context.x[0] = result;
            }

            thread.scheduling.eevdf = thread.scheduling.eevdf.mark_eligible();

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
fn ttbr0_for(thread: &Thread) -> u64 {
    match &thread.address_space {
        Some(addr_space) => addr_space.ttbr0_value(),
        None => memory::empty_ttbr0(),
    }
}

/// Bind a scheduling context to the current thread.
///
/// The caller (syscall layer) validates that `ctx_id` refers to a valid,
/// live context via handle lookup. Returns false if the thread already has
/// a context bound.
pub fn bind_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;

    // Verify context exists before borrowing thread (disjoint field access).
    match s.scheduling_contexts.get(ctx_id.0 as usize) {
        Some(Some(_)) => {}
        _ => return false,
    }

    let thread = s.cores[core].current.as_mut().expect("no current thread");

    if thread.scheduling.context_id.is_some() {
        return false;
    }

    thread.scheduling.context_id = Some(ctx_id);

    true
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
pub fn block_current_unless_woken(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    if thread.wake_pending {
        // Waker already ran — consume the flag, patch return value, don't block.
        thread.wake_pending = false;

        let c = unsafe { &mut *ctx };

        c.x[0] = thread.wake_result;

        thread.wait_set.clear();

        return ctx as *const Context;
    }

    thread.block();

    schedule_inner(&mut s, ctx, core)
}
/// Borrow another thread's scheduling context (context donation).
///
/// Saves the current context and switches to the borrowed one. The caller
/// (syscall layer) validates that `ctx_id` refers to a valid, live context
/// via handle lookup.
pub fn borrow_scheduling_context(ctx_id: SchedulingContextId) -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;

    // Verify context exists before borrowing thread (disjoint field access).
    match s.scheduling_contexts.get(ctx_id.0 as usize) {
        Some(Some(_)) => {}
        _ => return false,
    }

    let thread = s.cores[core].current.as_mut().expect("no current thread");

    // Can't borrow if already borrowing.
    if thread.scheduling.saved_context_id.is_some() {
        return false;
    }

    thread.scheduling.saved_context_id = thread.scheduling.context_id;
    thread.scheduling.context_id = Some(ctx_id);

    true
}
/// Clear the wait set and any pending wake on the current thread.
/// Called when a handle is found ready during the initial scan (no need
/// to block) or when returning early (poll mode, error).
pub fn clear_wait_state() {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.wait_set.clear();
    thread.wake_pending = false;
}
/// Create a new scheduling context. Returns the SchedulingContextId.
///
/// The context starts with ref_count=1 (the handle inserted by the caller).
/// Does not bind the context to any thread — use `bind_scheduling_context`
/// separately.
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
    let core = per_core::core_id() as usize;
    // Phase 1: collect resources to free (under scheduler lock).
    let (channels_to_close, interrupts_to_close, timers_to_close, addr_space, thread_id) = {
        let mut s = STATE.lock();
        // Collect handle objects into an owned Vec so we can release the
        // thread borrow and then access s.scheduling_contexts freely.
        let (handle_objects, tid) = {
            let thread = s.cores[core].current.as_mut().expect("no current thread");

            // Auto-return borrowed scheduling context on exit.
            if let Some(saved) = thread.scheduling.saved_context_id.take() {
                thread.scheduling.context_id = Some(saved);
            }

            let objects: Vec<HandleObject> = thread.handles.drain().map(|(_, obj)| obj).collect();

            (objects, thread.id())
        };
        let mut channels = Vec::new();
        let mut timers = Vec::new();
        let mut interrupts = Vec::new();

        for obj in handle_objects {
            match obj {
                HandleObject::Channel(id) => channels.push(id),
                HandleObject::Interrupt(id) => interrupts.push(id),
                HandleObject::SchedulingContext(id) => {
                    release_context_inner(&mut s, id);
                }
                HandleObject::Timer(id) => timers.push(id),
            }
        }

        let addr_space = s.cores[core]
            .current
            .as_mut()
            .expect("no current thread")
            .address_space
            .take();

        (channels, interrupts, timers, addr_space, tid)
    };

    // Phase 2: close channel endpoints (acquires channel lock, not scheduler).
    for id in channels_to_close {
        super::channel::close_endpoint(id);
    }
    // Phase 2a: destroy interrupt registrations (acquires interrupt lock, not scheduler).
    for id in interrupts_to_close {
        super::interrupt::destroy(id);
    }
    // Phase 2c: destroy timer objects (acquires timer lock, not scheduler).
    for id in timers_to_close {
        super::timer::destroy(id);
    }

    // Phase 2d: remove from futex wait queues (acquires futex lock, not scheduler).
    super::futex::remove_thread(thread_id);

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
/// Release a scheduling context handle (decrement ref count, free if zero).
pub fn release_scheduling_context(ctx_id: SchedulingContextId) {
    let mut s = STATE.lock();

    release_context_inner(&mut s, ctx_id);
}
/// Return a borrowed scheduling context, restoring the saved one.
pub fn return_scheduling_context() -> bool {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    match thread.scheduling.saved_context_id.take() {
        Some(saved) => {
            thread.scheduling.context_id = Some(saved);
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
/// Set the wake-pending flag on a thread that is not yet blocked (futex path).
///
/// Called by `futex::wake` when `try_wake` returns false (thread is still
/// Running, hasn't entered the scheduler yet). Sets `wake_result = 0`.
/// The flag is consumed by `block_current_unless_woken`.
pub fn set_wake_pending(id: ThreadId) {
    let mut s = STATE.lock();

    set_wake_pending_inner(&mut s, id);
}
/// Set the wake-pending flag for a handle-based event (channel signal, etc.).
///
/// Only sets the flag if the thread has an active wait set (is inside a `wait`
/// syscall). Computes `wake_result` from the matching entry in the wait set.
/// If `wake_pending` is already set, does nothing (first signal wins).
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

    for t in s.queue.ready.iter_mut() {
        if t.id() == id {
            apply(t, &reason);

            return;
        }
    }
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
/// Store a wait set on the current thread. Must be called BEFORE checking
/// handle readiness, so that signals arriving during the gap can find the
/// wait set and set `wake_pending`.
pub fn store_wait_set(entries: Vec<WaitEntry>) {
    let mut s = STATE.lock();
    let core = per_core::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.wait_set = entries;
}
/// Wake a blocked thread (Blocked → Ready). Returns true if it was blocked.
/// Does not interact with the thread's wait set — used by futex.
pub fn try_wake(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    try_wake_impl(&mut s, id, None)
}
/// Wake a blocked thread and resolve its wait set against `reason`.
///
/// If the thread has an active wait set (from a `wait` syscall), computes the
/// return index from the matching entry and patches `context.x[0]`. Used by
/// channel signal (and future timer/device notification).
pub fn try_wake_for_handle(id: ThreadId, reason: HandleObject) -> bool {
    let mut s = STATE.lock();

    try_wake_impl(&mut s, id, Some(&reason))
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
