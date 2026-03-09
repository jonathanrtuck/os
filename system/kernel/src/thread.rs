//! Kernel thread representation.

use super::context::Context;
use super::handle::HandleObject;
use super::process::ProcessId;
use super::scheduling_algorithm::SchedulingState;
use super::scheduling_context::SchedulingContextId;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;

/// An entry in a thread's wait set — one handle being waited on.
#[derive(Clone, Copy)]
pub(crate) struct WaitEntry {
    pub(crate) object: HandleObject,
    pub(crate) user_index: u8,
}

pub const STACK_SIZE: usize = 64 * 1024;
pub const KERNEL_STACK_SIZE: usize = 16 * 1024;

/// Scheduling-related fields grouped together.
pub(crate) struct Scheduling {
    /// Per-thread EEVDF state (vruntime, weight, slice, eligible_at).
    pub(crate) eevdf: SchedulingState,
    /// Scheduling context this thread is currently charged against.
    /// None = unlimited budget (kernel/idle threads).
    pub(crate) context_id: Option<SchedulingContextId>,
    /// Saved scheduling context during donation (borrow/return).
    pub(crate) saved_context_id: Option<SchedulingContextId>,
    /// Hardware counter timestamp when this thread last started running.
    pub(crate) last_started: u64,
}

impl Scheduling {
    pub(crate) const fn new() -> Self {
        Self {
            eevdf: SchedulingState::new(),
            context_id: None,
            saved_context_id: None,
            last_started: 0,
        }
    }
}

/// A kernel thread.
///
/// `context` MUST be the first field — `TPIDR_EL1` points at the start of
/// the Thread, and exception.S expects the Context at offset 0.
#[repr(C)]
pub struct Thread {
    pub(crate) context: Context,
    id: ThreadId,
    state: ThreadState,
    trust_level: TrustLevel,
    stack_bottom: *mut u8,
    stack_size: usize,
    pub(crate) process_id: Option<ProcessId>,
    /// Cached TTBR0 value for context switch. Set from the process's address
    /// space at thread creation. Zero for kernel threads (scheduler uses
    /// empty_ttbr0 fallback).
    pub(crate) ttbr0: u64,
    pub(crate) scheduling: Scheduling,
    /// Set when a wake arrives before the thread has blocked (lost-wakeup
    /// prevention). Consumed by `block_current_unless_woken`.
    pub(crate) wake_pending: bool,
    /// Return value to place in x0 when `wake_pending` is consumed.
    /// Futex sets this to 0; wait sets this to the ready handle's index.
    pub(crate) wake_result: u64,
    /// Handles this thread is waiting on via the `wait` syscall.
    /// Empty when not in a wait. Cleared on wake or early return.
    pub(crate) wait_set: Vec<WaitEntry>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}
/// Process privilege / trust classification.
///
/// Maps to the three-layer architecture: kernel (EL1), OS service (EL0
/// trusted), and editors (EL0 untrusted). Not enforced yet — records intent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    Kernel,
    Trusted,
    Untrusted,
}

const _: () = assert!(core::mem::offset_of!(Thread, context) == 0);

// --- State queries ---

impl Thread {
    /// Resolve a handle-based wake against this thread's wait set.
    ///
    /// Finds the matching entry, clears the wait set, and returns the
    /// user_index to place in x0. Returns 0 if the wait set is empty
    /// (thread was not in a `wait` syscall).
    pub(crate) fn complete_wait_for(&mut self, reason: &HandleObject) -> u64 {
        if self.wait_set.is_empty() {
            return 0;
        }

        let result = self
            .wait_set
            .iter()
            .find(|e| e.object == *reason)
            .map(|e| e.user_index as u64)
            .unwrap_or(0);

        self.wait_set.clear();

        result
    }
    /// Return a raw pointer to this thread's Context (at offset 0).
    /// Used by the scheduler to set TPIDR_EL1 and for context switch.
    pub(crate) fn context_ptr(&self) -> *const Context {
        &self.context as *const Context
    }
    pub(crate) fn id(&self) -> ThreadId {
        self.id
    }
    pub(crate) fn is_exited(&self) -> bool {
        self.state == ThreadState::Exited
    }
    /// Idle threads have distinguished IDs: 0xFF00 | core_id.
    pub(crate) fn is_idle(&self) -> bool {
        self.id.0 & 0xFF00 == 0xFF00
    }
    pub(crate) fn is_ready(&self) -> bool {
        self.state == ThreadState::Ready
    }
}
impl Drop for Thread {
    fn drop(&mut self) {
        if self.stack_size > 0 && !self.stack_bottom.is_null() {
            // SAFETY: Layout matches what was passed to alloc_zeroed during
            // construction. stack_bottom was returned by that allocation.
            unsafe {
                alloc::alloc::dealloc(
                    self.stack_bottom,
                    Layout::from_size_align_unchecked(self.stack_size, 16),
                );
            }
        }
    }
}
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

// --- State transitions ---
//
// Valid transitions:
//   Ready   → Running  (activate)
//   Running → Ready    (deschedule)
//   Running → Blocked  (block)
//   Running → Exited   (mark_exited)
//   Blocked → Ready    (wake)

impl Thread {
    /// Ready → Running (picked by scheduler).
    pub(crate) fn activate(&mut self) {
        debug_assert_eq!(self.state, ThreadState::Ready);

        self.state = ThreadState::Running;
    }
    /// Running → Blocked (waiting on a resource).
    pub(crate) fn block(&mut self) {
        debug_assert_eq!(self.state, ThreadState::Running);

        self.state = ThreadState::Blocked;
    }
    /// Running → Ready (preempted by scheduler). No-op if not Running.
    pub(crate) fn deschedule(&mut self) {
        if self.state == ThreadState::Running {
            self.state = ThreadState::Ready;
        }
    }
    /// Any → Exited (process exit or fault).
    pub(crate) fn mark_exited(&mut self) {
        self.state = ThreadState::Exited;
    }
    /// Blocked → Ready (resource available). Returns true if was blocked.
    pub(crate) fn wake(&mut self) -> bool {
        if self.state == ThreadState::Blocked {
            self.state = ThreadState::Ready;
            true
        } else {
            false
        }
    }
}
impl Thread {
    /// Kernel thread — runs at EL1, no address space.
    pub fn new(id: u64, entry: fn() -> !) -> Box<Self> {
        let stack_layout = Layout::from_size_align(STACK_SIZE, 16).unwrap();
        // SAFETY: Layout is valid (non-zero size, power-of-two alignment).
        let stack_bottom = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };

        assert!(!stack_bottom.is_null());

        // SAFETY: stack_bottom is non-null, points to STACK_SIZE allocated bytes.
        let stack_top = unsafe { stack_bottom.add(STACK_SIZE) } as u64;
        // SAFETY: Context is #[repr(C)] with integer/float fields; zero is valid.
        let mut ctx: Context = unsafe { core::mem::zeroed() };

        ctx.elr = entry as *const () as u64;
        ctx.sp = stack_top;
        ctx.spsr = 0b0101; // EL1h, DAIF clear
        ctx.x[30] = thread_exit as *const () as u64;

        Box::new(Thread {
            context: ctx,
            id: ThreadId(id),
            state: ThreadState::Ready,
            trust_level: TrustLevel::Kernel,
            stack_bottom,
            stack_size: STACK_SIZE,
            process_id: None,
            ttbr0: 0,
            scheduling: Scheduling::new(),
            wake_pending: false,
            wake_result: 0,
            wait_set: Vec::new(),
        })
    }

    /// Boot thread — zeroed context, no stack, no address space.
    ///
    /// The boot thread represents the initial execution context (kernel_main).
    /// Its context is populated by exception.S on the first exception entry.
    pub fn new_boot() -> Box<Self> {
        // SAFETY: Context is #[repr(C)] with integer and float fields;
        // all-zeros is valid (registers cleared, EL0t mode).
        let ctx: Context = unsafe { core::mem::zeroed() };

        Box::new(Thread {
            context: ctx,
            id: ThreadId(0),
            state: ThreadState::Running,
            trust_level: TrustLevel::Kernel,
            stack_bottom: core::ptr::null_mut(),
            stack_size: 0,
            process_id: None,
            ttbr0: 0,
            scheduling: Scheduling::new(),
            wake_pending: false,
            wake_result: 0,
            wait_set: Vec::new(),
        })
    }
    /// Idle thread — runs at EL1, no stack (uses boot stack), never enqueued.
    ///
    /// One per core. Falls through to WFE when nothing else is runnable.
    /// The idle thread's Context is used as a save area when the core has
    /// no user threads to run.
    pub fn new_idle(core_id: u64) -> Box<Self> {
        // SAFETY: Context is #[repr(C)] with integer and float fields;
        // all-zeros is valid.
        let ctx: Context = unsafe { core::mem::zeroed() };

        Box::new(Thread {
            context: ctx,
            id: ThreadId(core_id | 0xFF00), // Distinguished idle thread IDs.
            state: ThreadState::Ready,
            trust_level: TrustLevel::Kernel,
            stack_bottom: core::ptr::null_mut(),
            stack_size: 0,
            process_id: None,
            ttbr0: 0,
            scheduling: Scheduling::new(),
            wake_pending: false,
            wake_result: 0,
            wait_set: Vec::new(),
        })
    }
    /// User thread — runs at EL0 in a process's address space.
    pub fn new_user(
        id: u64,
        process_id: ProcessId,
        ttbr0: u64,
        entry_va: u64,
        user_stack_top: u64,
    ) -> Box<Self> {
        let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        // SAFETY: Layout is valid (non-zero size, power-of-two alignment).
        let stack_bottom = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };

        assert!(!stack_bottom.is_null());

        // SAFETY: stack_bottom is non-null, points to KERNEL_STACK_SIZE allocated bytes.
        let kernel_stack_top = unsafe { stack_bottom.add(KERNEL_STACK_SIZE) } as u64;
        // SAFETY: Context is #[repr(C)] with integer/float fields; zero is valid.
        let mut ctx: Context = unsafe { core::mem::zeroed() };

        ctx.elr = entry_va;
        ctx.sp = kernel_stack_top;
        ctx.sp_el0 = user_stack_top;
        ctx.spsr = 0b0000; // EL0t, DAIF clear

        Box::new(Thread {
            context: ctx,
            id: ThreadId(id),
            state: ThreadState::Ready,
            trust_level: TrustLevel::Untrusted,
            stack_bottom,
            stack_size: KERNEL_STACK_SIZE,
            process_id: Some(process_id),
            ttbr0,
            scheduling: Scheduling::new(),
            wake_pending: false,
            wake_result: 0,
            wait_set: Vec::new(),
        })
    }
}

fn thread_exit() -> ! {
    super::scheduler::exit_current();
}
