//! Host-side tests for thread state machine correctness.
//!
//! thread.rs depends on Context (aarch64 inline asm), memory (PA/VA mapping),
//! page_allocator, etc., so we cannot include it via #[path]. Instead, we model
//! the thread state machine and associated logic to test invariants that the
//! real implementation must uphold.
//!
//! Same self-contained model pattern as kernel_scheduler_state.rs.

// --- Minimal thread model (mirrors kernel/thread.rs) ---

const PAGE_SIZE: u64 = 16384;
const KERNEL_STACK_PAGES: u64 = 2; // Matches kernel: KERNEL_STACK_SIZE / PAGE_SIZE
const IDLE_THREAD_ID_MARKER: u64 = 0xFF00;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadState {
    Created,
    Ready,
    Running,
    Blocked,
    Exited,
}

struct Thread {
    id: ThreadId,
    state: ThreadState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

impl Thread {
    /// Create a new user thread in the Ready state (mirrors Thread::new_user).
    fn new_user(id: u64) -> Self {
        Self {
            id: ThreadId(id),
            state: ThreadState::Ready,
        }
    }

    /// Create an idle/boot thread in the Running state (mirrors Thread::new_boot_idle).
    fn new_boot_idle(core_id: u64) -> Self {
        Self {
            id: ThreadId(core_id | IDLE_THREAD_ID_MARKER),
            state: ThreadState::Running,
        }
    }

    /// Create a thread in the Created state (for testing Created transitions).
    fn new_created(id: u64) -> Self {
        Self {
            id: ThreadId(id),
            state: ThreadState::Created,
        }
    }

    // --- State predicates ---

    fn is_ready(&self) -> bool {
        self.state == ThreadState::Ready
    }

    fn is_blocked(&self) -> bool {
        self.state == ThreadState::Blocked
    }

    fn is_exited(&self) -> bool {
        self.state == ThreadState::Exited
    }

    fn is_idle(&self) -> bool {
        self.id.0 & IDLE_THREAD_ID_MARKER == IDLE_THREAD_ID_MARKER
    }

    // --- State transitions (mirrors kernel/thread.rs) ---

    /// Ready -> Running. Returns Err if not Ready.
    fn activate(&mut self) -> Result<(), &'static str> {
        if self.state != ThreadState::Ready {
            return Err("activate requires Ready state");
        }
        self.state = ThreadState::Running;
        Ok(())
    }

    /// Running -> Ready (preempted). No-op if not Running (mirrors kernel behavior).
    fn deschedule(&mut self) {
        if self.state == ThreadState::Running {
            self.state = ThreadState::Ready;
        }
    }

    /// Running -> Blocked. Returns Err if not Running.
    fn block(&mut self) -> Result<(), &'static str> {
        if self.state != ThreadState::Running {
            return Err("block requires Running state");
        }
        self.state = ThreadState::Blocked;
        Ok(())
    }

    /// Blocked -> Ready. Returns true if was Blocked, false otherwise.
    fn wake(&mut self) -> bool {
        if self.state == ThreadState::Blocked {
            self.state = ThreadState::Ready;
            true
        } else {
            false
        }
    }

    /// Any -> Exited (process exit or fault).
    fn mark_exited(&mut self) {
        self.state = ThreadState::Exited;
    }
}

// --- Stack size helpers (mirrors kernel/thread.rs) ---

/// Smallest buddy allocator order that provides at least `pages` contiguous pages.
fn order_for_pages(pages: usize) -> usize {
    pages.next_power_of_two().trailing_zeros() as usize
}

/// Total allocation size for a guarded kernel stack.
/// guard_page (1 page) + stack_pages, rounded up to power-of-two for buddy allocator.
fn guarded_stack_alloc_pages(stack_bytes: usize) -> usize {
    let stack_pages = stack_bytes.div_ceil(PAGE_SIZE as usize);
    let total_pages = stack_pages + 1; // +1 for guard page
    1usize << order_for_pages(total_pages)
}

// ============================================================
// Valid state transitions
// ============================================================

#[test]
fn ready_to_running() {
    let mut t = Thread::new_user(1);
    assert!(t.is_ready());

    assert!(t.activate().is_ok());
    assert_eq!(t.state, ThreadState::Running);
}

#[test]
fn running_to_ready_via_deschedule() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();
    assert_eq!(t.state, ThreadState::Running);

    t.deschedule();
    assert!(t.is_ready());
}

#[test]
fn running_to_blocked() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();

    assert!(t.block().is_ok());
    assert!(t.is_blocked());
}

#[test]
fn blocked_to_ready_via_wake() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();
    t.block().unwrap();
    assert!(t.is_blocked());

    assert!(t.wake(), "wake must return true for Blocked thread");
    assert!(t.is_ready());
}

#[test]
fn running_to_exited() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();

    t.mark_exited();
    assert!(t.is_exited());
}

#[test]
fn full_lifecycle_ready_running_blocked_ready_running_exited() {
    let mut t = Thread::new_user(1);

    // Ready -> Running
    t.activate().unwrap();
    assert_eq!(t.state, ThreadState::Running);

    // Running -> Blocked
    t.block().unwrap();
    assert_eq!(t.state, ThreadState::Blocked);

    // Blocked -> Ready (wake)
    assert!(t.wake());
    assert_eq!(t.state, ThreadState::Ready);

    // Ready -> Running (again)
    t.activate().unwrap();
    assert_eq!(t.state, ThreadState::Running);

    // Running -> Exited
    t.mark_exited();
    assert_eq!(t.state, ThreadState::Exited);
}

// ============================================================
// Invalid state transitions
// ============================================================

#[test]
fn activate_from_running_is_err() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();

    assert!(t.activate().is_err(), "activate from Running must fail");
    // State unchanged.
    assert_eq!(t.state, ThreadState::Running);
}

#[test]
fn activate_from_blocked_is_err() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();
    t.block().unwrap();

    assert!(
        t.activate().is_err(),
        "activate from Blocked must fail (must wake first)"
    );
    assert!(t.is_blocked());
}

#[test]
fn activate_from_exited_is_err() {
    let mut t = Thread::new_user(1);
    t.mark_exited();

    assert!(
        t.activate().is_err(),
        "activate from Exited must fail (terminal state)"
    );
    assert!(t.is_exited());
}

#[test]
fn block_from_ready_is_err() {
    let mut t = Thread::new_user(1);

    assert!(
        t.block().is_err(),
        "block from Ready must fail (only Running can block)"
    );
    assert!(t.is_ready());
}

#[test]
fn block_from_blocked_is_err() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();
    t.block().unwrap();

    assert!(
        t.block().is_err(),
        "block from Blocked must fail (already blocked)"
    );
    assert!(t.is_blocked());
}

#[test]
fn block_from_exited_is_err() {
    let mut t = Thread::new_user(1);
    t.mark_exited();

    assert!(t.block().is_err(), "block from Exited must fail");
    assert!(t.is_exited());
}

#[test]
fn wake_from_ready_returns_false() {
    let mut t = Thread::new_user(1);

    assert!(!t.wake(), "wake from Ready must return false");
    assert!(t.is_ready(), "state must not change");
}

#[test]
fn wake_from_running_returns_false() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();

    assert!(!t.wake(), "wake from Running must return false");
    assert_eq!(t.state, ThreadState::Running, "state must not change");
}

#[test]
fn wake_from_exited_returns_false() {
    let mut t = Thread::new_user(1);
    t.mark_exited();

    assert!(
        !t.wake(),
        "wake from Exited must return false (terminal state)"
    );
    assert!(t.is_exited());
}

#[test]
fn deschedule_from_ready_is_noop() {
    let mut t = Thread::new_user(1);
    assert!(t.is_ready());

    t.deschedule();
    assert!(t.is_ready(), "deschedule from Ready must be a no-op");
}

#[test]
fn deschedule_from_blocked_is_noop() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();
    t.block().unwrap();

    t.deschedule();
    assert!(
        t.is_blocked(),
        "deschedule from Blocked must be a no-op (handles kill_process on other cores)"
    );
}

#[test]
fn deschedule_from_exited_is_noop() {
    let mut t = Thread::new_user(1);
    t.mark_exited();

    t.deschedule();
    assert!(t.is_exited(), "deschedule from Exited must be a no-op");
}

// ============================================================
// mark_exited from any state
// ============================================================

#[test]
fn mark_exited_from_created() {
    let mut t = Thread::new_created(1);
    assert_eq!(t.state, ThreadState::Created);

    t.mark_exited();
    assert!(t.is_exited());
}

#[test]
fn mark_exited_from_ready() {
    let mut t = Thread::new_user(1);
    assert!(t.is_ready());

    t.mark_exited();
    assert!(t.is_exited());
}

#[test]
fn mark_exited_from_running() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();

    t.mark_exited();
    assert!(t.is_exited());
}

#[test]
fn mark_exited_from_blocked() {
    let mut t = Thread::new_user(1);
    t.activate().unwrap();
    t.block().unwrap();

    t.mark_exited();
    assert!(t.is_exited());
}

#[test]
fn mark_exited_from_exited_is_idempotent() {
    let mut t = Thread::new_user(1);
    t.mark_exited();
    assert!(t.is_exited());

    // Calling again should not panic — it's a no-op.
    t.mark_exited();
    assert!(t.is_exited());
}

// ============================================================
// Thread ID uniqueness and monotonicity
// ============================================================

#[test]
fn thread_ids_are_unique() {
    let threads: Vec<Thread> = (0..100).map(|i| Thread::new_user(i)).collect();

    // Collect all IDs into a set and verify no duplicates.
    let mut seen = std::collections::HashSet::new();
    for t in &threads {
        assert!(seen.insert(t.id.0), "thread ID {} must be unique", t.id.0);
    }
    assert_eq!(seen.len(), 100);
}

#[test]
fn thread_ids_are_monotonic() {
    let threads: Vec<Thread> = (0..100).map(|i| Thread::new_user(i)).collect();

    for i in 1..threads.len() {
        assert!(
            threads[i].id.0 > threads[i - 1].id.0,
            "thread IDs must be monotonically increasing: {} > {}",
            threads[i].id.0,
            threads[i - 1].id.0
        );
    }
}

#[test]
fn idle_thread_id_distinct_from_user_threads() {
    let user = Thread::new_user(0);
    let idle = Thread::new_boot_idle(0);

    assert_ne!(
        user.id, idle.id,
        "idle thread ID must differ from user thread ID for same core"
    );
    assert!(!user.is_idle());
    assert!(idle.is_idle());
}

#[test]
fn idle_thread_ids_encode_core() {
    for core_id in 0..4u64 {
        let idle = Thread::new_boot_idle(core_id);
        assert!(idle.is_idle());
        assert_eq!(
            idle.id.0 & !IDLE_THREAD_ID_MARKER,
            core_id,
            "idle thread ID must encode core_id in low bits"
        );
    }
}

// ============================================================
// Stack size calculations
// ============================================================

#[test]
fn stack_alloc_is_page_aligned() {
    // Kernel stack = 2 pages + 1 guard = 3 pages -> buddy rounds to 4.
    let alloc_pages = guarded_stack_alloc_pages(KERNEL_STACK_PAGES as usize * PAGE_SIZE as usize);
    let alloc_bytes = alloc_pages * PAGE_SIZE as usize;

    assert_eq!(
        alloc_bytes % PAGE_SIZE as usize,
        0,
        "stack allocation must be page-aligned"
    );
}

#[test]
fn stack_alloc_includes_guard_page() {
    let stack_bytes = KERNEL_STACK_PAGES as usize * PAGE_SIZE as usize;
    let stack_pages = stack_bytes / PAGE_SIZE as usize;
    let alloc_pages = guarded_stack_alloc_pages(stack_bytes);

    assert!(
        alloc_pages > stack_pages,
        "allocation must include at least one guard page: alloc={alloc_pages} > stack={stack_pages}"
    );
}

#[test]
fn stack_alloc_is_power_of_two() {
    // Buddy allocator requires power-of-two page counts.
    for stack_pages in 1..=8usize {
        let alloc_pages = guarded_stack_alloc_pages(stack_pages * PAGE_SIZE as usize);
        assert!(
            alloc_pages.is_power_of_two(),
            "alloc_pages={alloc_pages} must be power of two for {stack_pages} stack pages"
        );
    }
}

#[test]
fn order_for_pages_specific_values() {
    assert_eq!(order_for_pages(1), 0, "1 page = order 0 (2^0 = 1)");
    assert_eq!(order_for_pages(2), 1, "2 pages = order 1 (2^1 = 2)");
    assert_eq!(order_for_pages(3), 2, "3 pages = order 2 (2^2 = 4)");
    assert_eq!(order_for_pages(4), 2, "4 pages = order 2 (2^2 = 4)");
    assert_eq!(order_for_pages(5), 3, "5 pages = order 3 (2^3 = 8)");
    assert_eq!(order_for_pages(8), 3, "8 pages = order 3 (2^3 = 8)");
    assert_eq!(order_for_pages(9), 4, "9 pages = order 4 (2^4 = 16)");
}

#[test]
fn stack_16_byte_alignment() {
    // ARM64 ABI requires 16-byte stack alignment. The stack top is computed as
    // base_va + alloc_pages * PAGE_SIZE. Since PAGE_SIZE (16384) is already
    // 16-byte aligned and alloc_pages is integral, the result is always aligned.
    for stack_pages in 1..=8usize {
        let alloc_pages = guarded_stack_alloc_pages(stack_pages * PAGE_SIZE as usize);
        let simulated_stack_top = alloc_pages as u64 * PAGE_SIZE;

        assert_eq!(
            simulated_stack_top % 16,
            0,
            "stack top must be 16-byte aligned for {stack_pages} stack pages"
        );
    }
}

#[test]
fn kernel_stack_size_matches_expected() {
    // The kernel uses KERNEL_STACK_SIZE = 2 * PAGE_SIZE = 32768 bytes.
    // With guard page: 3 pages needed, buddy rounds to 4 (order 2).
    let alloc_pages = guarded_stack_alloc_pages(KERNEL_STACK_PAGES as usize * PAGE_SIZE as usize);

    assert_eq!(alloc_pages, 4, "2 stack pages + 1 guard = 3, rounds to 4");
    assert_eq!(order_for_pages(3), 2, "3 pages needs order 2");
}

// ============================================================
// Boot/idle thread properties
// ============================================================

#[test]
fn boot_idle_starts_running() {
    let idle = Thread::new_boot_idle(0);
    assert_eq!(
        idle.state,
        ThreadState::Running,
        "boot/idle thread must start in Running state"
    );
}

#[test]
fn user_thread_starts_ready() {
    let t = Thread::new_user(1);
    assert_eq!(
        t.state,
        ThreadState::Ready,
        "user thread must start in Ready state"
    );
}

// ============================================================
// Predicate consistency
// ============================================================

#[test]
fn predicates_are_mutually_exclusive() {
    let states = [
        ThreadState::Ready,
        ThreadState::Blocked,
        ThreadState::Exited,
    ];

    for &state in &states {
        let t = Thread {
            id: ThreadId(1),
            state,
        };

        let ready = t.is_ready() as u8;
        let blocked = t.is_blocked() as u8;
        let exited = t.is_exited() as u8;

        assert_eq!(
            ready + blocked + exited,
            1,
            "exactly one predicate must be true for state {state:?}: \
             ready={ready}, blocked={blocked}, exited={exited}"
        );
    }
}

#[test]
fn running_state_all_predicates_false() {
    // Running has no predicate (is_running doesn't exist in the kernel API).
    // All three predicates must be false.
    let t = Thread {
        id: ThreadId(1),
        state: ThreadState::Running,
    };

    assert!(!t.is_ready());
    assert!(!t.is_blocked());
    assert!(!t.is_exited());
}
