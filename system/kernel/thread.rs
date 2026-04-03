// AUDIT: 2026-03-11 — 3 unsafe sites verified (2 unsafe impl + 1 unsafe block),
// 6-category checklist applied. No bugs found.
//
// unsafe impl Send: sound — Thread owns all its data (embedded Context, tracked
//   stack PA) and is transferred between cores only via IrqMutex<State>.
// unsafe impl Sync: sound — Thread is never accessed concurrently; all access
//   serialized by IrqMutex<State> in scheduler.rs.
// core::mem::zeroed() for Context: sound — Context is #[repr(C)] with only
//   integer/float fields; zero is a valid bit pattern for all of them.
//
// Deferred thread drop (Fix 4) re-verified sound: exited threads are pushed to
//   deferred_drops by park_old, then dropped at the start of the NEXT
//   schedule_inner when we're safely on a different thread's stack. Between
//   push and drop, the thread is unreachable (not in ready/blocked/current).
//
// Drop ordering verified correct: guard page is remapped before free_frames
//   (required because free_frames writes a FreeBlock header at block start,
//   which is the guard page VA — writing to an unmapped page would fault).
//
// State machine transitions verified: Ready→Running, Running→Ready,
//   Running→Blocked, Running→Exited, Blocked→Ready. deschedule is intentionally
//   a no-op for non-Running states (handles kill_process marking threads Exited
//   on other cores).
//
// 2026-04-03: Added intrusive list links (pool_slot, list_next, list_prev),
// ThreadLocation enum, and generational ThreadId for O(1) scheduler operations.

//! Kernel thread representation.

use alloc::{boxed::Box, vec::Vec};

use super::{
    context::Context,
    handle::HandleObject,
    memory::{self, Pa},
    paging::PAGE_SIZE,
    process::ProcessId,
    scheduling_algorithm::SchedulingState,
    scheduling_context::SchedulingContextId,
};

pub const KERNEL_STACK_SIZE: usize = super::paging::KERNEL_STACK_SIZE as usize;
/// Sentinel user_index for internal timeout timer entries in the wait set.
/// Not a valid user handle index (max handles = 16, index fits in 0..15).
pub(crate) const TIMEOUT_SENTINEL: u8 = 0xFF;

const _: () = assert!(core::mem::offset_of!(Thread, context) == 0);

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
    Untrusted,
}

/// Where a thread lives in the scheduler's data structures.
///
/// Used for O(1) thread lookup: instead of scanning all lists to find a thread,
/// check its location and go directly to the right list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ThreadLocation {
    /// Running on core N. Stored in pool, referenced by cores[N].current_slot.
    Current(u8),
    /// In local_queues[N].ready intrusive list.
    Ready(u8),
    /// In the blocked intrusive list.
    Blocked,
    /// In the suspended intrusive list.
    Suspended,
    /// In deferred_drops[N]. Awaiting Drop on next schedule_inner.
    DeferredDrop(u8),
    /// In deferred_ready[N]. Will be moved to ready on next schedule_inner.
    DeferredReady(u8),
}

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
    /// Core this thread last ran on. Used for cache-affine placement
    /// when the thread is woken from blocked state. Set in schedule_inner
    /// when the thread is activated. Default 0 for newly created threads.
    pub(crate) last_core: u32,
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
    /// Stale waiter registrations from a previous `wait` that took the
    /// BlockResult::Blocked path. The Blocked path can't unregister waiters
    /// (it's already running as a different thread). These are cleaned up
    /// at the start of the next `sys_wait` call.
    pub(crate) stale_waiters: Vec<WaitEntry>,
    /// Internal timeout timer from a `wait` with finite timeout.
    /// Cleaned up on the next `wait` call (deferred cleanup for the
    /// Blocked path, where sys_wait can't run cleanup code).
    pub(crate) timeout_timer: Option<super::timer::TimerId>,
    /// If Some, this thread is blocked waiting for a pager to supply a page.
    /// Contains (VmoId, page_offset). Set by `block_current_for_pager`,
    /// cleared by `wake_pager_waiters` when the page is supplied.
    pub(crate) pager_wait: Option<(super::vmo::VmoId, u64)>,
    /// True if this is a boot/idle thread (one per core, never enqueued).
    pub(crate) is_idle_thread: bool,
    /// Index of this thread's slot in the ThreadPool. Only valid for
    /// non-idle threads (idle threads are not in the pool).
    pub(crate) pool_slot: u16,
    /// Intrusive list link: next thread in the same list (pool slot index).
    pub(crate) list_next: Option<u16>,
    /// Intrusive list link: previous thread in the same list (pool slot index).
    pub(crate) list_prev: Option<u16>,
    /// Which scheduler list this thread is currently in.
    /// Only valid for non-idle threads.
    pub(crate) location: ThreadLocation,
}

/// Generational thread identifier.
///
/// Packs a slot index and a generation counter into a single u64:
/// - bits [15:0] = pool slot index (max 65535)
/// - bits [63:16] = generation (increments on slot reuse, prevents stale aliasing)
///
/// ThreadId is kernel-internal — userspace only sees opaque handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId(pub u64);

impl ThreadId {
    /// Create a ThreadId from a pool slot and generation.
    pub fn new(slot: u16, generation: u64) -> Self {
        Self((generation << 16) | slot as u64)
    }
    /// Extract the pool slot index (bits [15:0]).
    pub fn slot(self) -> u16 {
        self.0 as u16
    }
    /// Extract the generation counter (bits [63:16]).
    #[allow(dead_code)] // API for tests and future use (stale-ID diagnostics).
    pub fn generation(self) -> u64 {
        self.0 >> 16
    }
}

/// An entry in a thread's wait set — one handle being waited on.
#[derive(Clone, Copy)]
pub(crate) struct WaitEntry {
    pub(crate) object: HandleObject,
    pub(crate) user_index: u8,
}

impl Scheduling {
    pub(crate) const fn new() -> Self {
        Self {
            eevdf: SchedulingState::new(),
            context_id: None,
            saved_context_id: None,
            last_started: 0,
            last_core: 0,
        }
    }
}

impl Thread {
    /// Common field initialization for all thread constructors.
    fn base(id: ThreadId, state: ThreadState, trust_level: TrustLevel) -> Self {
        // SAFETY: Context is #[repr(C)] with only integer (u64) and float (u128)
        // fields — see context.rs. Zero is a valid bit pattern for all of them.
        // The resulting Context represents "no saved state" (all registers zero),
        // which is the correct initial state for a new thread.
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
            stale_waiters: Vec::new(),
            timeout_timer: None,
            pager_wait: None,
            is_idle_thread: false,
            pool_slot: 0,
            list_next: None,
            list_prev: None,
            location: ThreadLocation::Suspended,
        }
    }

    /// Boot/idle thread — one per core, zeroed context, uses the core's boot stack.
    ///
    /// Serves dual purpose: represents the initial execution context (kernel_main
    /// on core 0, secondary_main on cores 1+), and acts as the idle fallback when
    /// no user threads are runnable. Context is populated by exception.S save_context
    /// on the first exception entry.
    ///
    /// Idle threads live outside the thread pool (in PerCoreState::idle/current).
    /// They are never enqueued in any scheduler list.
    pub fn new_boot_idle(_core_id: u64) -> Box<Self> {
        let mut thread = Self::base(
            ThreadId::new(0, 0), // Idle threads don't need a valid pool ID.
            ThreadState::Running,
            TrustLevel::Kernel,
        );

        thread.is_idle_thread = true;

        Box::new(thread)
    }
    /// User thread — runs at EL0 in a process's address space.
    ///
    /// Returns `None` if the kernel stack cannot be allocated (OOM).
    /// The caller (ThreadPool::alloc) assigns the pool_slot and ThreadId.
    pub fn new_user(
        process_id: ProcessId,
        ttbr0: u64,
        entry_va: u64,
        user_stack_top: u64,
    ) -> Option<Box<Self>> {
        let (kernel_stack_top, alloc_pa, alloc_order) = alloc_guarded_stack(KERNEL_STACK_SIZE)?;
        let mut thread = Self::base(
            ThreadId::new(0, 0), // Placeholder — set by ThreadPool::alloc.
            ThreadState::Ready,
            TrustLevel::Untrusted,
        );

        thread.context.set_pc(entry_va);
        thread.context.set_sp(kernel_stack_top);
        thread.context.set_user_sp(user_stack_top);
        thread.context.set_user_mode();

        thread.stack_alloc_pa = alloc_pa;
        thread.stack_alloc_order = alloc_order;
        thread.process_id = Some(process_id);
        thread.ttbr0 = ttbr0;

        Some(Box::new(thread))
    }
}
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
    /// Resolve a handle-based wake against this thread's wait set.
    ///
    /// Finds the matching entry, clears the wait set, and returns the
    /// user_index to place in x0. Returns 0 if the wait set is empty
    /// (thread was not in a `wait` syscall).
    ///
    /// If the matching entry has `user_index == TIMEOUT_SENTINEL`, this is
    /// an internal timeout timer from a `wait` with finite timeout. Returns
    /// the `WouldBlock` error code instead of a handle index.
    pub(crate) fn complete_wait_for(&mut self, reason: &HandleObject) -> u64 {
        if self.wait_set.is_empty() {
            return 0;
        }

        let result = self
            .wait_set
            .iter()
            .find(|e| e.object == *reason)
            .map(|e| {
                if e.user_index == TIMEOUT_SENTINEL {
                    // Internal timeout fired → return WouldBlock error code.
                    super::syscall::WOULD_BLOCK_RAW
                } else {
                    e.user_index as u64
                }
            })
            .unwrap_or(0);

        // Move unfired entries to stale_waiters for deferred cleanup.
        // The Blocked path in sys_wait can't unregister waiters (it's
        // running as a different thread). The next sys_wait call will
        // clean these up.
        self.stale_waiters.clear();

        for entry in self.wait_set.iter() {
            if entry.object != *reason {
                self.stale_waiters.push(*entry);
            }
        }

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
    pub(crate) fn is_idle(&self) -> bool {
        self.is_idle_thread
    }
    pub(crate) fn is_ready(&self) -> bool {
        self.state == ThreadState::Ready
    }
    /// Set the thread's ID. Called by ThreadPool::alloc after slot assignment.
    pub(crate) fn set_id(&mut self, id: ThreadId) {
        self.id = id;
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
// SAFETY: Thread owns all its data (Context is embedded at offset 0, stack is
// tracked by physical address). Threads are transferred between cores only
// through the IrqMutex<State> in scheduler.rs, which serializes all access.
// No raw pointers to external mutable state are stored — context_ptr() returns
// a derived pointer on demand, not a stored one.
unsafe impl Send for Thread {}
// SAFETY: Thread is never accessed concurrently. All access goes through
// IrqMutex<State> in scheduler.rs, which provides exclusive (&mut) access.
// Sync is required because IrqMutex<State> (which is Sync) contains
// Vec<Box<Thread>>, and Box<T>: Send requires T: Send (satisfied above).
// In practice, &Thread is never shared across threads — but the trait bound
// is needed for the static IrqMutex.
unsafe impl Sync for Thread {}

impl super::waitable::WaitableId for ThreadId {
    fn index(self) -> usize {
        self.slot() as usize
    }
}

/// Allocate a stack from the page allocator with a guard page at the bottom.
///
/// The guard page is the lowest page of the allocation. It is unmapped from
/// the kernel's TTBR1 page tables so any access faults immediately.
///
/// Returns `None` on OOM (cannot allocate frames or guard page table).
fn alloc_guarded_stack(min_stack_bytes: usize) -> Option<(u64, u64, usize)> {
    let stack_pages = min_stack_bytes.div_ceil(PAGE_SIZE as usize);
    let total_pages = stack_pages + 1; // +1 for guard page
    let order = order_for_pages(total_pages);
    let pa = super::page_allocator::alloc_frames(order)?;
    let base_va = memory::phys_to_virt(pa);
    let alloc_pages = 1usize << order;
    let stack_top = (base_va + alloc_pages * PAGE_SIZE as usize) as u64;

    if !memory::try_set_kernel_guard_page(base_va) {
        super::page_allocator::free_frames(pa, order);

        return None;
    }

    Some((stack_top, pa.as_u64(), order))
}
/// Smallest buddy allocator order that provides at least `pages` contiguous pages.
fn order_for_pages(pages: usize) -> usize {
    pages.next_power_of_two().trailing_zeros() as usize
}
