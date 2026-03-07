//! Round-robin preemptive scheduler.

use super::addr_space::AddressSpace;
use super::handle::{HandleObject, HandleTable};
use super::memory;
use super::thread::{Thread, ThreadId, ThreadState};
use super::Context;
use alloc::{boxed::Box, vec::Vec};
use core::cell::SyncUnsafeCell;

struct State {
    #[allow(clippy::vec_box)]
    threads: Vec<Box<Thread>>,
    current: usize,
    next_id: u64,
}

static STATE: SyncUnsafeCell<State> = SyncUnsafeCell::new(State {
    threads: Vec::new(),
    current: 0,
    next_id: 1,
});

fn state() -> &'static mut State {
    unsafe { &mut *STATE.get() }
}
/// Swap TTBR0 when the address space changes between old and new threads.
///
/// No TLB invalidation needed: ASIDs are never recycled, so stale entries
/// from a previous ASID can't alias the new one. Add TLBI if ASID recycling
/// is ever introduced.
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

/// Access the current thread mutably (for handle table operations in syscalls).
/// Must be called with IRQs masked or from syscall context (which has IRQs masked).
pub fn current_thread() -> &'static mut Thread {
    let s = state();

    &mut s.threads[s.current]
}
pub fn exit_current() -> ! {
    unsafe { core::arch::asm!("msr daifset, #2", options(nostack, nomem)) };

    let s = state();

    s.threads[s.current].state = ThreadState::Exited;

    unsafe { core::arch::asm!("msr daifclr, #2", options(nostack, nomem)) };

    loop {
        core::hint::spin_loop();
    }
}
pub fn exit_current_from_syscall(ctx: *mut Context) -> *const Context {
    let s = state();
    let thread = &mut s.threads[s.current];

    // Close all handles and notify referenced objects.
    for (_handle, object) in thread.handles.drain() {
        match object {
            HandleObject::Channel(id) => {
                super::channel::close_endpoint(id);
            }
        }
    }

    // Free address space: TLB invalidation, page tables, user pages, ASID.
    if let Some(mut addr_space) = thread.address_space.take() {
        addr_space.invalidate_tlb();
        addr_space.free_all();
        super::asid::free(super::asid::Asid(addr_space.asid()));
    }

    thread.state = ThreadState::Exited;

    schedule(ctx)
}
pub fn init() {
    let s = state();
    let mut boot_thread = Box::new(Thread {
        context: unsafe { core::mem::zeroed() },
        id: ThreadId(0),
        state: ThreadState::Running,
        stack_bottom: core::ptr::null_mut(),
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
pub fn schedule(ctx: *mut Context) -> *const Context {
    let s = state();
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
pub fn spawn(entry: fn() -> !) {
    let s = state();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new(id, entry);

    s.threads.push(thread);
}
pub fn spawn_user(addr_space: Box<AddressSpace>, entry_va: u64, user_stack_top: u64) -> ThreadId {
    let s = state();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new_user(id, addr_space, entry_va, user_stack_top);

    s.threads.push(thread);
    ThreadId(id)
}
/// Wake a blocked thread (set Blocked → Ready). Returns true if it was blocked.
pub fn try_wake(id: ThreadId) -> bool {
    let s = state();

    for thread in &mut s.threads {
        if thread.id == id && thread.state == ThreadState::Blocked {
            thread.state = ThreadState::Ready;

            return true;
        }
    }

    false
}
/// Access a thread by ID. Closure receives exclusive access to the thread.
/// Must be called with no preemption (before timer starts, or from syscall context).
pub fn with_thread_mut<R>(id: ThreadId, f: impl FnOnce(&mut Thread) -> R) -> R {
    let s = state();
    let thread = s
        .threads
        .iter_mut()
        .find(|t| t.id == id)
        .expect("thread not found");

    f(thread)
}
