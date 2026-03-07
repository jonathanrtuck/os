//! Kernel thread representation.

use super::Context;
use alloc::boxed::Box;
use core::alloc::Layout;

pub const STACK_SIZE: usize = 64 * 1024;
pub const KERNEL_STACK_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ThreadId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Exited,
}

/// A kernel thread.
///
/// `context` MUST be the first field — `TPIDR_EL1` points at the start of
/// the Thread, and `boot.S` expects the Context at offset 0.
#[repr(C)]
pub struct Thread {
    pub context: Context,
    pub id: ThreadId,
    pub state: ThreadState,
    pub stack_bottom: *mut u8,
}

// Verify Context is at offset 0 — boot.S relies on TPIDR_EL1 pointing at
// the start of Thread and treating it as a Context pointer.
const _: () = assert!(core::mem::offset_of!(Thread, context) == 0);

// Safety: Thread is only accessed from a single core, either during init
// (single-threaded) or inside the IRQ handler (interrupts disabled).
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    /// Create a new thread that will begin executing at `entry`.
    pub fn new(id: u64, entry: fn() -> !) -> Box<Self> {
        let stack_layout = Layout::from_size_align(STACK_SIZE, 16).unwrap();
        let stack_bottom = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };

        assert!(!stack_bottom.is_null());

        let stack_top = unsafe { stack_bottom.add(STACK_SIZE) } as u64;

        let mut ctx: Context = unsafe { core::mem::zeroed() };

        ctx.elr = entry as *const () as u64;
        ctx.sp = stack_top;
        // EL1h with DAIF clear — IRQs unmasked so the thread is preemptible.
        ctx.spsr = 0b0101;
        // If the entry fn somehow returns, x30 catches it.
        ctx.x[30] = thread_exit as *const () as u64;

        Box::new(Thread {
            context: ctx,
            id: ThreadId(id),
            state: ThreadState::Ready,
            stack_bottom,
        })
    }

    /// Create a new EL0 (user) thread.
    ///
    /// The kernel stack is allocated here (for syscall/IRQ handling).
    /// The user stack is provided by the caller.
    pub fn new_user(id: u64, entry: usize, user_stack_top: u64) -> Box<Self> {
        let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        let stack_bottom = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };

        assert!(!stack_bottom.is_null());

        let kernel_stack_top = unsafe { stack_bottom.add(KERNEL_STACK_SIZE) } as u64;
        let mut ctx: Context = unsafe { core::mem::zeroed() };

        ctx.elr = entry as u64;
        ctx.sp = kernel_stack_top; // SP_EL1 — used during exception handling
        ctx.sp_el0 = user_stack_top; // SP_EL0 — user stack
        ctx.spsr = 0b0000; // EL0t, DAIF clear

        Box::new(Thread {
            context: ctx,
            id: ThreadId(id),
            state: ThreadState::Ready,
            stack_bottom,
        })
    }
}

/// Safety net: if a `-> !` entry function somehow returns, we land here.
fn thread_exit() -> ! {
    super::scheduler::exit_current();
}
