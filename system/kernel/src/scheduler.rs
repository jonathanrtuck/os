//! Round-robin preemptive scheduler.

use super::addr_space::AddressSpace;
use super::handle::{HandleObject, HandleTable};
use super::memory;
use super::sync::IrqMutex;
use super::thread::{Thread, ThreadId, ThreadState};
use super::Context;
use alloc::{boxed::Box, vec::Vec};

struct State {
    #[allow(clippy::vec_box)]
    threads: Vec<Box<Thread>>,
    current: usize,
    next_id: u64,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    threads: Vec::new(),
    current: 0,
    next_id: 1,
});

/// Swap TTBR0 when the address space changes between old and new threads.
///
/// No per-switch TLB invalidation needed on single-core: ASIDs are recycled
/// (asid::free / asid::alloc), but the exit path invalidates TLB entries for
/// the ASID *before* returning it to the pool (see exit_current_from_syscall).
/// On single-core, no new entries can be created between that TLBI and reuse.
/// Multi-core would require TLBI on ASID reuse (stale entries on other cores).
fn swap_ttbr0(old_idx: usize, new_idx: usize, s: &State) {
    let old_ttbr0 = ttbr0_for(&s.threads[old_idx]);
    let new_ttbr0 = ttbr0_for(&s.threads[new_idx]);

    if old_ttbr0 != new_ttbr0 {
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

/// Access the current thread via closure. Acquires the scheduler lock for the
/// duration of the closure. Do not call scheduler functions from within `f`.
pub fn current_thread_do<R>(f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();
    let idx = s.current;

    f(&mut s.threads[idx])
}
/// Exit the current kernel thread (no context pointer available).
///
/// Only safe for kernel threads that have no resources (no address space,
/// no handles). User threads must exit via `exit_current_from_syscall` which
/// performs full cleanup. The thread spins until the next timer tick reaps it.
pub fn exit_current() -> ! {
    {
        let mut s = STATE.lock();
        let idx = s.current;
        let thread = &s.threads[idx];

        debug_assert!(
            thread.address_space.is_none(),
            "exit_current called on thread with address space — use exit_current_from_syscall"
        );

        s.threads[idx].state = ThreadState::Exited;
    }

    loop {
        core::hint::spin_loop();
    }
}
pub fn exit_current_from_syscall(ctx: *mut Context) -> *const Context {
    use super::handle::ChannelId;

    // Phase 1: collect resources to free (under scheduler lock).
    let (channels_to_close, addr_space) = {
        let mut s = STATE.lock();
        let idx = s.current;
        let thread = &mut s.threads[idx];
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
    let idx = s.current;

    s.threads[idx].state = ThreadState::Exited;

    schedule_inner(&mut s, ctx)
}
pub fn init() {
    let mut s = STATE.lock();
    let mut boot_thread = Box::new(Thread {
        context: unsafe { core::mem::zeroed() },
        id: ThreadId(0),
        state: ThreadState::Running,
        stack_bottom: core::ptr::null_mut(),
        stack_size: 0,
        address_space: None,
        handles: HandleTable::new(),
    });
    let ctx_ptr = &mut boot_thread.context as *mut Context;

    s.threads.push(boot_thread);

    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {0}",
            in(reg) ctx_ptr as usize,
            options(nostack, nomem)
        );
    }
}
/// Remove exited threads and free their resources (Box<Thread> drop frees
/// the kernel stack via Thread::drop). Safe because the current thread is
/// never reaped — it's either Running (timer path) or Exited but still on
/// its kernel stack (exit path). Exited-but-current threads get reaped on
/// the *next* schedule() call, when a different thread is current.
fn reap_exited(s: &mut State) {
    let mut i = s.threads.len();

    while i > 0 {
        i -= 1;

        if i != s.current && s.threads[i].state == ThreadState::Exited {
            s.threads.swap_remove(i);

            // swap_remove moves the last element to position i.
            // If current was the last element, it's now at i.
            if s.current == s.threads.len() {
                s.current = i;
            }
        }
    }
}
fn schedule_inner(s: &mut State, ctx: *mut Context) -> *const Context {
    reap_exited(s);

    let n = s.threads.len();
    let cur = &mut s.threads[s.current];

    if cur.state == ThreadState::Running {
        cur.state = ThreadState::Ready;
    }

    let old_idx = s.current;

    for offset in 1..=n {
        let idx = (s.current + offset) % n;

        if s.threads[idx].state == ThreadState::Ready {
            s.threads[idx].state = ThreadState::Running;
            s.current = idx;

            // Swap TTBR0 if switching between different address spaces.
            swap_ttbr0(old_idx, idx, s);

            return &s.threads[idx].context as *const Context;
        }
    }

    ctx as *const Context
}

/// Block the current thread and reschedule. Used by syscalls that need to
/// release other locks before blocking (e.g., channel wait releases the
/// channel lock, then calls this).
pub fn block_current_and_schedule(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();
    let idx = s.current;

    s.threads[idx].state = ThreadState::Blocked;

    schedule_inner(&mut s, ctx)
}
pub fn schedule(ctx: *mut Context) -> *const Context {
    let mut s = STATE.lock();

    schedule_inner(&mut s, ctx)
}
pub fn spawn(entry: fn() -> !) {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new(id, entry);

    s.threads.push(thread);
}
pub fn spawn_user(addr_space: Box<AddressSpace>, entry_va: u64, user_stack_top: u64) -> ThreadId {
    let mut s = STATE.lock();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new_user(id, addr_space, entry_va, user_stack_top);

    s.threads.push(thread);

    ThreadId(id)
}
/// Wake a blocked thread (set Blocked -> Ready). Returns true if it was blocked.
pub fn try_wake(id: ThreadId) -> bool {
    let mut s = STATE.lock();

    for thread in &mut s.threads {
        if thread.id == id && thread.state == ThreadState::Blocked {
            thread.state = ThreadState::Ready;

            return true;
        }
    }

    false
}
/// Access a thread by ID. Closure receives exclusive access to the thread.
pub fn with_thread_mut<R>(id: ThreadId, f: impl FnOnce(&mut Thread) -> R) -> R {
    let mut s = STATE.lock();
    let thread = s
        .threads
        .iter_mut()
        .find(|t| t.id == id)
        .expect("thread not found");

    f(thread)
}
