//! Round-robin preemptive scheduler.

use super::thread::{Thread, ThreadId, ThreadState};
use super::Context;
use alloc::{boxed::Box, vec::Vec};
use core::cell::SyncUnsafeCell;

struct State {
    #[allow(clippy::vec_box)]
    // Box required: TPIDR_EL1 holds raw pointers into Thread contexts, so Threads must not move on Vec realloc.
    threads: Vec<Box<Thread>>,
    current: usize,
    next_id: u64,
}

static STATE: SyncUnsafeCell<State> = SyncUnsafeCell::new(State {
    threads: Vec::new(),
    current: 0,
    next_id: 1,
});

/// Safety: STATE is only accessed during init (single-threaded, IRQs masked)
/// or inside the IRQ handler (interrupts disabled, single core).
fn state() -> &'static mut State {
    unsafe { &mut *STATE.get() }
}

/// Mark the current thread as exited and spin until the next timer tick
/// reschedules away from it.
pub fn exit_current() -> ! {
    let s = state();

    s.threads[s.current].state = ThreadState::Exited;

    loop {
        core::hint::spin_loop();
    }
}

/// Initialize the scheduler. Call after `heap::init()`, before `timer::init()`.
///
/// Creates a Thread for the boot thread (already running) and updates
/// TPIDR_EL1 to point at its embedded Context.
pub fn init() {
    let s = state();
    let mut boot_thread = Box::new(Thread {
        context: unsafe { core::mem::zeroed() },
        id: ThreadId(0),
        state: ThreadState::Running,
        stack_bottom: core::ptr::null_mut(), // boot stack is from the linker script
    });
    let ctx_ptr = &mut boot_thread.context as *mut Context;

    s.threads.push(boot_thread);

    // Point TPIDR_EL1 at the boot thread's Context so the IRQ handler
    // saves into the right place from now on.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {0}",
            in(reg) ctx_ptr as usize,
            options(nostack, nomem)
        );
    }
}

/// Pick the next thread to run. Called from `irq_handler` on each timer tick.
///
/// Returns a pointer to the next thread's Context. If no other thread is
/// ready, returns the current thread's context (no switch).
pub fn schedule(current_ctx: *mut Context) -> *const Context {
    let s = state();
    let n = s.threads.len();
    // Mark the current thread as Ready (unless it exited).
    let cur = &mut s.threads[s.current];

    if cur.state == ThreadState::Running {
        cur.state = ThreadState::Ready;
    }

    // Scan forward (wrapping) for the next Ready thread.
    for offset in 1..=n {
        let idx = (s.current + offset) % n;

        if s.threads[idx].state == ThreadState::Ready {
            s.threads[idx].state = ThreadState::Running;
            s.current = idx;

            return &s.threads[idx].context as *const Context;
        }
    }

    // No other thread ready — continue running the current one.
    let cur = &mut s.threads[s.current];

    if cur.state == ThreadState::Ready {
        cur.state = ThreadState::Running;
    }

    current_ctx as *const Context
}

/// Spawn a new kernel thread that begins executing at `entry`.
pub fn spawn(entry: fn() -> !) {
    let s = state();
    let id = s.next_id;

    s.next_id += 1;

    let thread = Thread::new(id, entry);

    s.threads.push(thread);
}
