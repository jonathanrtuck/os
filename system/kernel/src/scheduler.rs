//! SMP-aware preemptive scheduler with priority levels.
//!
//! Global run queue with three priority levels (Idle < Normal < High).
//! Each core dequeues from the highest non-empty level. Per-core current
//! thread tracked via `PerCpu`. Single global lock — fine for ≤8 cores.
//!
//! Idle threads (one per core) are never enqueued. They run when all
//! queues are empty, as fallback targets stored in PerCpu.

use super::addr_space::AddressSpace;
use super::handle::HandleObject;
use super::memory;
use super::percpu;
use super::sync::IrqMutex;
use super::thread::{Priority, Thread, ThreadId};
use super::Context;
use alloc::{boxed::Box, collections::VecDeque, vec::Vec};

struct PerCoreState {
    current: Option<Box<Thread>>,
    idle: Option<Box<Thread>>,
}
/// Per-priority run queue.
struct RunQueue {
    high: VecDeque<Box<Thread>>,
    normal: VecDeque<Box<Thread>>,
}
struct State {
    queue: RunQueue,
    /// Threads waiting on a resource (Blocked state). Moved here from
    /// cores[].current when a thread blocks; moved back to queue by wake().
    blocked: Vec<Box<Thread>>,
    cores: [PerCoreState; percpu::MAX_CORES],
    next_id: u64,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    queue: RunQueue {
        high: VecDeque::new(),
        normal: VecDeque::new(),
    },
    blocked: Vec::new(),
    cores: {
        const INIT: PerCoreState = PerCoreState {
            current: None,
            idle: None,
        };
        [INIT; percpu::MAX_CORES]
    },
    next_id: 1,
});

/// Dequeue the highest-priority ready thread, or None.
fn dequeue(queue: &mut RunQueue) -> Option<Box<Thread>> {
    if let Some(t) = queue.high.pop_front() {
        return Some(t);
    }

    queue.normal.pop_front()
}
/// Enqueue a thread that's transitioning from Running → Ready (preempted).
fn enqueue(queue: &mut RunQueue, thread: Box<Thread>) {
    match thread.priority() {
        Priority::High => queue.high.push_back(thread),
        Priority::Normal => queue.normal.push_back(thread),
        Priority::Idle => {} // Idle threads are never enqueued.
    }
}
/// Reap exited threads from the run queues and blocked list.
fn reap_exited(queue: &mut RunQueue, blocked: &mut Vec<Box<Thread>>) {
    queue.high.retain(|t| !t.is_exited());
    queue.normal.retain(|t| !t.is_exited());
    blocked.retain(|t| !t.is_exited());
}
fn schedule_inner(s: &mut State, _ctx: *mut Context, core: usize) -> *const Context {
    reap_exited(&mut s.queue, &mut s.blocked);

    let mut old_thread = s.cores[core].current.take().expect("no current thread");

    old_thread.deschedule();

    // Park the old thread in its appropriate location.
    // Must happen before dequeue so we don't lose the thread.
    fn park_old(s: &mut State, old_thread: Box<Thread>) {
        if old_thread.is_ready() {
            enqueue(&mut s.queue, old_thread);
        } else if old_thread.is_exited() {
            drop(old_thread); // Free kernel stack etc.
        } else {
            // Blocked — park until wake() re-enqueues it.
            s.blocked.push(old_thread);
        }
    }

    // Try to dequeue a runnable thread.
    if let Some(mut new_thread) = dequeue(&mut s.queue) {
        new_thread.activate();

        swap_ttbr0(&old_thread, &new_thread);

        let new_ctx = new_thread.context_ptr();

        park_old(s, old_thread);

        s.cores[core].current = Some(new_thread);

        new_ctx
    } else if old_thread.is_ready() {
        // No other runnable threads — continue with the old one.
        old_thread.activate();

        let old_ctx = old_thread.context_ptr();

        s.cores[core].current = Some(old_thread);

        old_ctx
    } else {
        // Old thread exited or blocked, nothing in queue. Run idle thread.
        let idle = s.cores[core].idle.take().expect("no idle thread");
        let idle_ctx = idle.context_ptr();

        swap_ttbr0(&old_thread, &idle);
        park_old(s, old_thread);

        s.cores[core].current = Some(idle);

        idle_ctx
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
fn ttbr0_for(thread: &Thread) -> u64 {
    match &thread.address_space {
        Some(addr_space) => addr_space.ttbr0_value(),
        None => memory::empty_ttbr0(),
    }
}

/// Block the current thread and reschedule. Used by syscalls that need to
/// release other locks before blocking (e.g., channel wait releases the
/// channel lock, then calls this).
pub fn block_current_and_schedule(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();
    let core = percpu::core_id() as usize;
    let thread = s.cores[core].current.as_mut().expect("no current thread");

    thread.block();

    schedule_inner(&mut s, ctx, core)
}
/// Access the current thread via closure. Acquires the scheduler lock for the
/// duration of the closure. Do not call scheduler functions from within `f`.
pub fn current_thread_do<R>(f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();
    let core = percpu::core_id() as usize;
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
        let core = percpu::core_id() as usize;
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

    let core = percpu::core_id() as usize;
    // Phase 1: collect resources to free (under scheduler lock).
    let (channels_to_close, addr_space) = {
        let mut s = STATE.lock();
        let thread = s.cores[core].current.as_mut().expect("no current thread");
        let channels: Vec<ChannelId> = thread
            .handles
            .drain()
            .filter_map(|(_, obj)| match obj {
                HandleObject::Channel(id) => Some(id),
            })
            .collect();
        let addr_space = thread.address_space.take();

        (channels, addr_space)
    };

    // Phase 2: close channel endpoints (acquires channel lock, not scheduler).
    for id in channels_to_close {
        super::channel::close_endpoint(id);
    }

    // Phase 3: free address space (acquires page_alloc and asid locks).
    if let Some(mut addr_space) = addr_space {
        addr_space.invalidate_tlb();
        addr_space.free_all();
        super::asid::free(super::asid::Asid(addr_space.asid()));
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
pub fn schedule(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();
    let core = percpu::core_id() as usize;

    schedule_inner(&mut s, ctx, core)
}
pub fn spawn(entry: fn() -> !) {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new(id, entry);

    enqueue(&mut s.queue, thread);
}
pub fn spawn_user(addr_space: Box<AddressSpace>, entry_va: u64, user_stack_top: u64) -> ThreadId {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new_user(id, addr_space, entry_va, user_stack_top);

    enqueue(&mut s.queue, thread);

    ThreadId(id)
}
/// Wake a blocked thread (Blocked → Ready). Returns true if it was blocked.
pub fn try_wake(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    // Check blocked list — most common case for wake.
    if let Some(pos) = s.blocked.iter().position(|t| t.id() == id) {
        let mut thread = s.blocked.swap_remove(pos);

        if thread.wake() {
            enqueue(&mut s.queue, thread);

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
    // Check run queues (unlikely — blocked threads shouldn't be here).
    for t in s.queue.high.iter_mut() {
        if t.id() == id {
            return t.wake();
        }
    }
    for t in s.queue.normal.iter_mut() {
        if t.id() == id {
            return t.wake();
        }
    }

    false
}
/// Access a thread by ID. Closure receives exclusive access to the thread.
pub fn with_thread_mut<R>(id: ThreadId, f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();

    // Search run queues.
    for t in s.queue.high.iter_mut() {
        if t.id() == id {
            return f(t);
        }
    }
    for t in s.queue.normal.iter_mut() {
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
