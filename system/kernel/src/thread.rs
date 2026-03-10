//! Kernel thread representation.

use super::context::Context;
use super::handle::HandleObject;
use super::memory::{self, Pa};
use super::paging::PAGE_SIZE;
use super::process::ProcessId;
use super::scheduling_algorithm::SchedulingState;
use super::scheduling_context::SchedulingContextId;
use alloc::boxed::Box;
use alloc::vec::Vec;

pub const STACK_SIZE: usize = 64 * 1024;
pub const KERNEL_STACK_SIZE: usize = 16 * 1024;
/// Distinguished ID marker for idle threads: `core_id | IDLE_THREAD_ID_MARKER`.
const IDLE_THREAD_ID_MARKER: u64 = 0xFF00;

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
    /// PA of the buddy-allocated stack (including guard page). Zero = no stack.
    stack_alloc_pa: u64,
    /// Buddy allocator order for deallocation.
    stack_alloc_order: usize,
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
/// An entry in a thread's wait set — one handle being waited on.
#[derive(Clone, Copy)]
pub(crate) struct WaitEntry {
    pub(crate) object: HandleObject,
    pub(crate) user_index: u8,
}

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

impl super::waitable::WaitableId for ThreadId {
    fn index(self) -> usize {
        self.0 as usize
    }
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
    /// Idle threads have distinguished IDs: `core_id | IDLE_THREAD_ID_MARKER`.
    pub(crate) fn is_idle(&self) -> bool {
        self.id.0 & IDLE_THREAD_ID_MARKER == IDLE_THREAD_ID_MARKER
    }
    pub(crate) fn is_ready(&self) -> bool {
        self.state == ThreadState::Ready
    }
}
impl Drop for Thread {
    fn drop(&mut self) {
        if self.stack_alloc_pa != 0 {
            // Remap the guard page before freeing — free_frames writes a
            // FreeBlock header at the block's start (the guard page VA).
            let guard_va = memory::phys_to_virt(Pa(self.stack_alloc_pa as usize));

            memory::clear_kernel_guard_page(guard_va);

            super::page_allocator::free_frames(
                Pa(self.stack_alloc_pa as usize),
                self.stack_alloc_order,
            );
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
        let (stack_top, alloc_pa, alloc_order) = alloc_guarded_stack(STACK_SIZE);
        let mut thread = Self::base(ThreadId(id), ThreadState::Ready, TrustLevel::Kernel);

        thread.context.elr = entry as *const () as u64;
        thread.context.sp = stack_top;
        thread.context.spsr = 0b0101; // EL1h, DAIF clear
        thread.context.x[30] = thread_exit as *const () as u64;
        thread.stack_alloc_pa = alloc_pa;
        thread.stack_alloc_order = alloc_order;

        Box::new(thread)
    }

    /// Common field initialization for all thread constructors.
    fn base(id: ThreadId, state: ThreadState, trust_level: TrustLevel) -> Self {
        // SAFETY: Context is #[repr(C)] with integer/float fields; zero is valid.
        let ctx: Context = unsafe { core::mem::zeroed() };

        Thread {
            context: ctx,
            id,
            state,
            trust_level,
            stack_alloc_pa: 0,
            stack_alloc_order: 0,
            process_id: None,
            ttbr0: 0,
            scheduling: Scheduling::new(),
            wake_pending: false,
            wake_result: 0,
            wait_set: Vec::new(),
        }
    }

    /// Boot thread — zeroed context, no stack, no address space.
    ///
    /// The boot thread represents the initial execution context (kernel_main).
    /// Its context is populated by exception.S on the first exception entry.
    pub fn new_boot() -> Box<Self> {
        Box::new(Self::base(
            ThreadId(0),
            ThreadState::Running,
            TrustLevel::Kernel,
        ))
    }
    /// Idle thread — runs at EL1, no stack (uses boot stack), never enqueued.
    ///
    /// One per core. Falls through to WFE when nothing else is runnable.
    /// The idle thread's Context is used as a save area when the core has
    /// no user threads to run.
    pub fn new_idle(core_id: u64) -> Box<Self> {
        Box::new(Self::base(
            ThreadId(core_id | IDLE_THREAD_ID_MARKER),
            ThreadState::Ready,
            TrustLevel::Kernel,
        ))
    }
    /// User thread — runs at EL0 in a process's address space.
    pub fn new_user(
        id: u64,
        process_id: ProcessId,
        ttbr0: u64,
        entry_va: u64,
        user_stack_top: u64,
    ) -> Box<Self> {
        let (kernel_stack_top, alloc_pa, alloc_order) = alloc_guarded_stack(KERNEL_STACK_SIZE);
        let mut thread = Self::base(ThreadId(id), ThreadState::Ready, TrustLevel::Untrusted);

        thread.context.elr = entry_va;
        thread.context.sp = kernel_stack_top;
        thread.context.sp_el0 = user_stack_top;
        thread.context.spsr = 0b0000; // EL0t, DAIF clear
        thread.stack_alloc_pa = alloc_pa;
        thread.stack_alloc_order = alloc_order;
        thread.process_id = Some(process_id);
        thread.ttbr0 = ttbr0;

        Box::new(thread)
    }
}

/// Allocate a stack from the page allocator with a guard page at the bottom.
///
/// The guard page is the lowest page of the allocation. It is unmapped from
/// the kernel's TTBR1 page tables so any access faults immediately.
///
/// Returns `(stack_top_va, allocation_pa, allocation_order)`.
fn alloc_guarded_stack(min_stack_bytes: usize) -> (u64, u64, usize) {
    let stack_pages = (min_stack_bytes + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
    let total_pages = stack_pages + 1; // +1 for guard page
    let order = order_for_pages(total_pages);
    let pa =
        super::page_allocator::alloc_frames(order).expect("alloc_guarded_stack: out of memory");
    let base_va = memory::phys_to_virt(pa);
    let alloc_pages = 1usize << order;
    let stack_top = (base_va + alloc_pages * PAGE_SIZE as usize) as u64;

    // Guard page = bottom page of allocation (lowest VA).
    memory::set_kernel_guard_page(base_va);

    (stack_top, pa.as_u64(), order)
}
/// Smallest buddy allocator order that provides at least `pages` contiguous pages.
fn order_for_pages(pages: usize) -> usize {
    pages.next_power_of_two().trailing_zeros() as usize
}
fn thread_exit() -> ! {
    super::scheduler::exit_current();
}
