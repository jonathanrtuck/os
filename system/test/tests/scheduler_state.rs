//! Host-side tests for scheduler state machine transitions.
//!
//! scheduler.rs depends on IrqMutex (aarch64 inline asm), per_core, TPIDR_EL1,
//! etc., so we cannot include it via #[path]. Instead, we model the parking and
//! thread lifecycle logic to test invariants that the real scheduler must uphold.
//!
//! These tests exist because the idle thread drop bug (2026-03-11) was caused by
//! a comment describing intended behavior that was never implemented. Each test
//! here documents a state machine invariant that must hold.

// --- Minimal thread model ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

const IDLE_THREAD_ID_MARKER: u64 = 0xFF00;

struct Thread {
    id: u64,
    state: ThreadState,
}

impl Thread {
    fn new(id: u64) -> Self {
        Self {
            id,
            state: ThreadState::Ready,
        }
    }

    fn new_idle(core_id: u64) -> Self {
        Self {
            id: core_id | IDLE_THREAD_ID_MARKER,
            state: ThreadState::Ready,
        }
    }

    fn is_idle(&self) -> bool {
        self.id & IDLE_THREAD_ID_MARKER == IDLE_THREAD_ID_MARKER
    }

    fn is_ready(&self) -> bool {
        self.state == ThreadState::Ready
    }

    fn is_exited(&self) -> bool {
        self.state == ThreadState::Exited
    }

    // --- State transitions (mirrors kernel/thread.rs) ---

    fn activate(&mut self) {
        assert_eq!(self.state, ThreadState::Ready);
        self.state = ThreadState::Running;
    }

    fn deschedule(&mut self) {
        if self.state == ThreadState::Running {
            self.state = ThreadState::Ready;
        }
    }

    fn block(&mut self) {
        assert_eq!(self.state, ThreadState::Running);
        self.state = ThreadState::Blocked;
    }

    fn wake(&mut self) -> bool {
        if self.state == ThreadState::Blocked {
            self.state = ThreadState::Ready;
            true
        } else {
            false
        }
    }

    fn mark_exited(&mut self) {
        self.state = ThreadState::Exited;
    }
}

// --- Minimal scheduler state model ---

struct PerCoreState {
    current: Option<Thread>,
    idle: Option<Thread>,
    is_idle: bool,
}

struct State {
    cores: Vec<PerCoreState>,
    ready: Vec<Thread>,
    blocked: Vec<Thread>,
    /// Exited threads deferred for drop. In the real kernel, dropping an exited
    /// thread frees its kernel stack — but schedule_inner is still running on
    /// that stack. Deferred drops are drained at the start of the NEXT
    /// schedule_inner, when we're on a different thread's stack.
    deferred_drops: Vec<Thread>,
}

impl State {
    fn new(num_cores: usize) -> Self {
        let cores = (0..num_cores)
            .map(|i| PerCoreState {
                current: None,
                idle: Some(Thread::new_idle(i as u64)),
                is_idle: false,
            })
            .collect();

        Self {
            cores,
            ready: Vec::new(),
            blocked: Vec::new(),
            deferred_drops: Vec::new(),
        }
    }
}

/// Mirrors scheduler.rs park_old. Must be kept in sync with the real implementation.
fn park_old(s: &mut State, old_thread: Thread, core: usize) {
    if old_thread.is_ready() {
        if old_thread.is_idle() {
            // Idle threads are never enqueued — restore to this core's idle slot.
            s.cores[core].idle = Some(old_thread);
        } else {
            s.ready.push(old_thread);
        }
    } else if old_thread.is_exited() {
        // Deferred drop — in the real kernel, we're still running on this
        // thread's kernel stack. Dropping would free the stack (use-after-free).
        s.deferred_drops.push(old_thread);
    } else {
        // Blocked.
        s.blocked.push(old_thread);
    }
}

/// Mirrors the try_wake_impl logic: find thread in blocked list, wake it, move to ready.
fn try_wake(s: &mut State, id: u64) -> bool {
    if let Some(pos) = s.blocked.iter().position(|t| t.id == id) {
        let mut thread = s.blocked.swap_remove(pos);

        if thread.wake() {
            s.ready.push(thread);
            return true;
        }

        s.blocked.push(thread);
    }

    false
}

// ============================================================
// Idle thread lifecycle
// ============================================================

#[test]
fn idle_thread_restored_after_park() {
    let mut s = State::new(4);

    // Take idle from core 0.
    let mut idle = s.cores[0].idle.take().unwrap();

    idle.activate();
    idle.deschedule();

    // Park it — should go back to cores[0].idle.
    park_old(&mut s, idle, 0);

    assert!(
        s.cores[0].idle.is_some(),
        "idle thread must be restored to core's idle slot after parking"
    );
    assert!(
        s.cores[0].idle.as_ref().unwrap().is_idle(),
        "restored thread must be an idle thread"
    );
}

#[test]
fn idle_thread_not_in_ready_queue() {
    let mut s = State::new(4);
    let mut idle = s.cores[0].idle.take().unwrap();

    idle.activate();
    idle.deschedule();
    park_old(&mut s, idle, 0);

    assert!(
        s.ready.is_empty(),
        "idle threads must never appear in the ready queue"
    );
}

#[test]
fn idle_survives_multiple_round_trips() {
    let mut s = State::new(4);

    for _ in 0..100 {
        // Take idle, run it, park it back.
        let mut idle = s.cores[0]
            .idle
            .take()
            .expect("idle thread must be available for every round trip");

        idle.activate();
        idle.deschedule();
        park_old(&mut s, idle, 0);
    }

    assert!(s.cores[0].idle.is_some());
    assert!(s.ready.is_empty());
}

#[test]
fn idle_round_trip_on_all_cores() {
    let mut s = State::new(4);

    for core in 0..4 {
        let mut idle = s.cores[core].idle.take().expect("idle should exist");

        idle.activate();
        idle.deschedule();
        park_old(&mut s, idle, core);

        assert!(
            s.cores[core].idle.is_some(),
            "core {core} idle must be restored"
        );
    }
}

#[test]
fn idle_take_then_real_thread_replaces_then_idle_again() {
    // Simulates: core goes idle → real thread becomes available → core goes idle again.
    let mut s = State::new(1);

    // 1. Core 0 has nothing to run — switch to idle.
    let mut idle = s.cores[0].idle.take().unwrap();
    idle.activate();
    s.cores[0].current = Some(idle);

    // 2. A real thread becomes ready.
    s.ready.push(Thread::new(1));

    // 3. Scheduler tick: park idle, switch to real thread.
    let mut old = s.cores[0].current.take().unwrap();
    old.deschedule();
    park_old(&mut s, old, 0);

    let mut new = s.ready.pop().unwrap();
    new.activate();
    s.cores[0].current = Some(new);

    assert!(
        s.cores[0].idle.is_some(),
        "idle must be back in slot after being replaced by real thread"
    );

    // 4. Real thread blocks — core goes idle again.
    let mut current = s.cores[0].current.take().unwrap();
    current.block();
    park_old(&mut s, current, 0);

    let mut idle2 = s.cores[0]
        .idle
        .take()
        .expect("idle must still be available for second use");
    idle2.activate();
    s.cores[0].current = Some(idle2);

    // Should not panic, should not lose the idle thread.
    assert!(s.cores[0].current.as_ref().unwrap().is_idle());
}

// ============================================================
// Thread lifecycle
// ============================================================

#[test]
fn ready_thread_parked_to_ready_queue() {
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    t.activate();
    t.deschedule(); // Running → Ready
    park_old(&mut s, t, 0);

    assert_eq!(s.ready.len(), 1);
    assert_eq!(s.ready[0].id, 1);
    assert!(s.ready[0].is_ready());
}

#[test]
fn blocked_thread_parked_to_blocked_list() {
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    t.activate();
    t.block(); // Running → Blocked
    park_old(&mut s, t, 0);

    assert_eq!(s.blocked.len(), 1);
    assert_eq!(s.blocked[0].id, 1);
    assert_eq!(s.blocked[0].state, ThreadState::Blocked);
}

#[test]
fn exited_thread_deferred_not_dropped_immediately() {
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    t.mark_exited();
    park_old(&mut s, t, 0);

    assert!(
        s.ready.is_empty(),
        "exited thread must not be in ready queue"
    );
    assert!(
        s.blocked.is_empty(),
        "exited thread must not be in blocked list"
    );
    assert_eq!(
        s.deferred_drops.len(),
        1,
        "exited thread must be deferred, not dropped immediately \
         (schedule_inner is still running on its kernel stack)"
    );
}

#[test]
fn deferred_drops_drained_on_next_schedule() {
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    t.mark_exited();
    park_old(&mut s, t, 0);

    assert_eq!(s.deferred_drops.len(), 1);

    // Simulate the start of the next schedule_inner: drain deferred drops.
    // In the real kernel, this runs on a different thread's stack — safe to free.
    s.deferred_drops.clear();

    assert!(s.deferred_drops.is_empty());
}

#[test]
fn blocked_then_woken_moves_to_ready() {
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    t.activate();
    t.block();
    park_old(&mut s, t, 0);

    assert_eq!(s.blocked.len(), 1);
    assert!(s.ready.is_empty());

    // Wake the thread.
    assert!(try_wake(&mut s, 1));

    assert!(s.blocked.is_empty());
    assert_eq!(s.ready.len(), 1);
    assert!(s.ready[0].is_ready());
}

#[test]
fn wake_nonexistent_thread_returns_false() {
    let mut s = State::new(1);

    assert!(!try_wake(&mut s, 999));
}

#[test]
fn wake_ready_thread_returns_false() {
    let mut s = State::new(1);
    let t = Thread::new(1);

    s.ready.push(t);

    // Thread is in ready queue, not blocked — wake should fail.
    assert!(!try_wake(&mut s, 1));
}

#[test]
fn full_thread_lifecycle() {
    // Ready → Running → Blocked → Woken(Ready) → Running → Exited
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    // Ready → Running
    assert!(t.is_ready());
    t.activate();
    assert_eq!(t.state, ThreadState::Running);

    // Running → Blocked
    t.block();
    assert_eq!(t.state, ThreadState::Blocked);
    park_old(&mut s, t, 0);
    assert_eq!(s.blocked.len(), 1);

    // Blocked → Ready (woken)
    assert!(try_wake(&mut s, 1));
    assert_eq!(s.ready.len(), 1);
    let mut t = s.ready.pop().unwrap();

    // Ready → Running
    t.activate();
    assert_eq!(t.state, ThreadState::Running);

    // Running → Exited
    t.mark_exited();
    assert!(t.is_exited());
    park_old(&mut s, t, 0);
    assert!(s.ready.is_empty());
    assert!(s.blocked.is_empty());
    assert_eq!(s.deferred_drops.len(), 1, "exited thread must be deferred");
}

// ============================================================
// Multi-core idle interaction
// ============================================================

#[test]
fn idle_threads_are_per_core() {
    let mut s = State::new(4);

    // Take idle from core 2, park on core 2.
    let mut idle = s.cores[2].idle.take().unwrap();
    idle.activate();
    idle.deschedule();
    park_old(&mut s, idle, 2);

    // Core 2's idle is restored; others untouched.
    assert!(s.cores[0].idle.is_some());
    assert!(s.cores[1].idle.is_some());
    assert!(s.cores[2].idle.is_some());
    assert!(s.cores[3].idle.is_some());
}

#[test]
fn concurrent_idle_on_multiple_cores() {
    // Simulate two cores going idle and back simultaneously.
    let mut s = State::new(4);

    let mut idle0 = s.cores[0].idle.take().unwrap();
    let mut idle1 = s.cores[1].idle.take().unwrap();

    idle0.activate();
    idle1.activate();
    idle0.deschedule();
    idle1.deschedule();

    park_old(&mut s, idle0, 0);
    park_old(&mut s, idle1, 1);

    assert!(s.cores[0].idle.is_some());
    assert!(s.cores[1].idle.is_some());
    assert!(s.ready.is_empty());
}

// ============================================================
// State machine transition validity
// ============================================================

#[test]
fn deschedule_non_running_is_noop() {
    let mut t = Thread::new(1);

    assert_eq!(t.state, ThreadState::Ready);
    t.deschedule(); // Ready → noop
    assert_eq!(t.state, ThreadState::Ready);
}

#[test]
fn wake_non_blocked_returns_false() {
    let mut t = Thread::new(1);

    assert!(!t.wake()); // Ready → can't wake

    t.activate();
    assert!(!t.wake()); // Running → can't wake
}

// ============================================================
// Property-based / adversarial randomized tests
// ============================================================

/// Deterministic PRNG (xorshift64) for reproducible adversarial tests.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, max: usize) -> usize {
        (self.next() % max as u64) as usize
    }
}

/// Invariant checker: validates all structural properties of the scheduler state.
fn check_invariants(s: &State, label: &str) {
    // 1. No thread appears in multiple locations.
    let mut all_ids: Vec<u64> = Vec::new();
    for t in &s.ready {
        all_ids.push(t.id);
    }
    for t in &s.blocked {
        all_ids.push(t.id);
    }
    for t in &s.deferred_drops {
        all_ids.push(t.id);
    }
    for core in &s.cores {
        if let Some(t) = &core.current {
            if !t.is_idle() {
                all_ids.push(t.id);
            }
        }
    }
    let original_len = all_ids.len();
    all_ids.sort();
    all_ids.dedup();
    assert_eq!(
        all_ids.len(),
        original_len,
        "{label}: duplicate thread ID found"
    );

    // 2. Idle threads are in their core's idle slot, never in queues.
    for t in &s.ready {
        assert!(!t.is_idle(), "{label}: idle thread in ready queue");
    }
    for t in &s.blocked {
        assert!(!t.is_idle(), "{label}: idle thread in blocked list");
    }

    // 3. Ready queue contains only Ready threads.
    for t in &s.ready {
        assert!(t.is_ready(), "{label}: non-ready thread in ready queue");
    }

    // 4. Blocked list contains only Blocked threads.
    for t in &s.blocked {
        assert_eq!(
            t.state,
            ThreadState::Blocked,
            "{label}: non-blocked thread in blocked list"
        );
    }

    // 5. Deferred drops contains only Exited threads.
    for t in &s.deferred_drops {
        assert!(
            t.is_exited(),
            "{label}: non-exited thread in deferred drops"
        );
    }

    // 6. is_idle must be consistent with the current thread.
    for (i, core) in s.cores.iter().enumerate() {
        if let Some(t) = &core.current {
            if t.is_idle() {
                assert!(
                    core.is_idle,
                    "{label}: core {i} runs idle thread but is_idle is false"
                );
            } else {
                assert!(
                    !core.is_idle,
                    "{label}: core {i} runs non-idle thread but is_idle is true"
                );
            }
        }
    }
}

/// Actions that can be randomly applied to the scheduler state.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// Spawn a new thread (add to ready queue).
    Spawn,
    /// Pick a ready thread and run it on a core.
    ScheduleOnCore(usize),
    /// Block the current thread on a core.
    BlockOnCore(usize),
    /// Wake a blocked thread by ID.
    Wake(u64),
    /// Mark a running thread as exited.
    ExitOnCore(usize),
    /// Run a full schedule cycle on a core (deschedule old, pick new).
    ScheduleCycle(usize),
    /// Drain deferred drops (simulates start of schedule_inner).
    DrainDeferred,
}

#[test]
fn randomized_scheduler_state_machine() {
    // Run multiple seeds to explore different interleavings.
    // Under Miri, reduce iteration counts for practical runtime (~30s vs ~1400s).
    #[cfg(miri)]
    const SEEDS: u64 = 5;
    #[cfg(not(miri))]
    const SEEDS: u64 = 50;
    #[cfg(miri)]
    const STEPS: usize = 50;
    #[cfg(not(miri))]
    const STEPS: usize = 500;

    for seed in 1..=SEEDS {
        let mut rng = Rng::new(seed);
        let num_cores = 4;
        let mut s = State::new(num_cores);
        let mut next_id: u64 = 1;
        let mut live_ids: Vec<u64> = Vec::new();

        for step in 0..STEPS {
            let label = format!("seed={seed} step={step}");

            // Pick a random action.
            let action = match rng.range(7) {
                0 => Action::Spawn,
                1 => Action::ScheduleOnCore(rng.range(num_cores)),
                2 => Action::BlockOnCore(rng.range(num_cores)),
                3 => {
                    if live_ids.is_empty() {
                        Action::Spawn
                    } else {
                        let idx = rng.range(live_ids.len());
                        Action::Wake(live_ids[idx])
                    }
                }
                4 => Action::ExitOnCore(rng.range(num_cores)),
                5 => Action::ScheduleCycle(rng.range(num_cores)),
                6 => Action::DrainDeferred,
                _ => unreachable!(),
            };

            match action {
                Action::Spawn => {
                    let id = next_id;
                    next_id += 1;
                    s.ready.push(Thread::new(id));
                    live_ids.push(id);
                }
                Action::ScheduleOnCore(core) => {
                    if !s.ready.is_empty() && s.cores[core].current.is_none() {
                        let mut t = s.ready.remove(0);
                        t.activate();
                        s.cores[core].current = Some(t);
                        s.cores[core].is_idle = false;
                    }
                }
                Action::BlockOnCore(core) => {
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.block();
                            let t = s.cores[core].current.take().unwrap();
                            park_old(&mut s, t, core);
                        }
                    }
                }
                Action::Wake(id) => {
                    try_wake(&mut s, id);
                }
                Action::ExitOnCore(core) => {
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() {
                            t.mark_exited();
                            let t = s.cores[core].current.take().unwrap();
                            live_ids.retain(|&x| x != t.id);
                            park_old(&mut s, t, core);
                        }
                    }
                }
                Action::ScheduleCycle(core) => {
                    // Full cycle: deschedule old, park, pick new or idle.
                    if let Some(mut old) = s.cores[core].current.take() {
                        old.deschedule();
                        let was_exited = old.is_exited();
                        if was_exited {
                            live_ids.retain(|&x| x != old.id);
                        }
                        park_old(&mut s, old, core);
                    }
                    // Drain deferred drops (start of schedule_inner).
                    s.deferred_drops.clear();

                    // Pick a new thread or fall back to idle.
                    if !s.ready.is_empty() {
                        let mut t = s.ready.remove(0);
                        t.activate();
                        s.cores[core].current = Some(t);
                        s.cores[core].is_idle = false;
                    } else {
                        let mut idle = s.cores[core]
                            .idle
                            .take()
                            .unwrap_or_else(|| panic!("{label}: idle missing for core {core}"));
                        idle.activate();
                        s.cores[core].current = Some(idle);
                        s.cores[core].is_idle = true;
                    }
                }
                Action::DrainDeferred => {
                    s.deferred_drops.clear();
                }
            }

            check_invariants(&s, &label);
        }
    }
}

#[test]
fn rapid_block_wake_never_duplicates() {
    // Simulates the exact pattern from the keyboard input crash:
    // rapid block/wake cycles on multiple cores.
    let mut s = State::new(4);

    // Spawn 8 threads.
    for i in 1..=8 {
        s.ready.push(Thread::new(i));
    }

    // Put one thread on each core.
    for core in 0..4 {
        let mut t = s.ready.remove(0);
        t.activate();
        s.cores[core].current = Some(t);
        s.cores[core].is_idle = false;
    }

    // Rapid block/wake cycles. Reduced under Miri for practical runtime.
    #[cfg(miri)]
    const ITERS: usize = 100;
    #[cfg(not(miri))]
    const ITERS: usize = 1000;
    let mut rng = Rng::new(42);

    for step in 0..ITERS {
        let label = format!("step={step}");
        let core = rng.range(4);

        // Block current thread on this core.
        if let Some(t) = &s.cores[core].current {
            if !t.is_idle() && t.state == ThreadState::Running {
                let id = t.id;
                let t = s.cores[core].current.as_mut().unwrap();
                t.block();
                let t = s.cores[core].current.take().unwrap();
                park_old(&mut s, t, core);

                // Immediately try to wake it (simulates channel signal arriving
                // right after block — the lost-wakeup scenario).
                try_wake(&mut s, id);
            }
        }

        // Drain deferred and pick a new thread.
        s.deferred_drops.clear();

        if !s.ready.is_empty() {
            let mut t = s.ready.remove(0);
            t.activate();
            s.cores[core].current = Some(t);
            s.cores[core].is_idle = false;
        } else if s.cores[core].current.is_none() {
            let mut idle = s.cores[core].idle.take().expect("idle missing");
            idle.activate();
            s.cores[core].current = Some(idle);
            s.cores[core].is_idle = true;
        }

        check_invariants(&s, &label);
    }
}

#[test]
fn all_threads_eventually_reaped() {
    // Spawn threads, exit them all, verify no leaks.
    let mut s = State::new(2);

    for i in 1..=20 {
        s.ready.push(Thread::new(i));
    }

    // Run each thread briefly, then exit it.
    let mut exited = 0;
    while !s.ready.is_empty()
        || s.cores
            .iter()
            .any(|c| c.current.as_ref().map_or(false, |t| !t.is_idle()))
    {
        for core in 0..2 {
            // Deschedule + park current.
            if let Some(mut old) = s.cores[core].current.take() {
                old.deschedule();
                park_old(&mut s, old, core);
            }
            s.deferred_drops.clear();

            if !s.ready.is_empty() {
                let mut t = s.ready.remove(0);
                t.activate();
                // Exit it immediately.
                t.mark_exited();
                park_old(&mut s, t, core);
                exited += 1;

                // Put idle on core.
                let mut idle = s.cores[core].idle.take().expect("idle missing");
                idle.activate();
                s.cores[core].current = Some(idle);
            } else {
                let mut idle = s.cores[core]
                    .idle
                    .take()
                    .unwrap_or_else(|| Thread::new_idle(core as u64));
                idle.activate();
                s.cores[core].current = Some(idle);
            }
        }
    }

    // Final drain.
    s.deferred_drops.clear();

    assert_eq!(exited, 20, "all 20 threads must have been exited");
    assert!(s.ready.is_empty(), "ready queue must be empty");
    assert!(s.blocked.is_empty(), "blocked list must be empty");
    assert!(s.deferred_drops.is_empty(), "deferred drops must be empty");
}

// ============================================================
// Thread lifecycle audit tests (2026-03-11)
// Verify patterns from thread.rs + thread_exit.rs audit.
// ============================================================

/// Verify that deferred drops are unreachable from ready queue and blocked list.
/// This is the key safety property of Fix 4: between push to deferred_drops and
/// the next schedule_inner drain, no code path can find or touch the thread.
#[test]
fn deferred_drop_thread_unreachable_from_queues() {
    let mut s = State::new(2);

    // Spawn a thread, run it, exit it.
    let mut t = Thread::new(1);
    t.activate();
    t.mark_exited();
    park_old(&mut s, t, 0);

    assert_eq!(s.deferred_drops.len(), 1);

    // The thread must NOT be findable in ready, blocked, or current.
    assert!(s.ready.iter().all(|t| t.id != 1), "exited thread in ready");
    assert!(
        s.blocked.iter().all(|t| t.id != 1),
        "exited thread in blocked"
    );
    for core in &s.cores {
        if let Some(t) = &core.current {
            assert_ne!(t.id, 1, "exited thread still current on a core");
        }
    }
}

/// Verify that mark_exited is idempotent — calling it twice doesn't panic
/// or corrupt state. This is important because kill_process may call
/// mark_exited on a thread that exit_current_from_syscall also marks.
#[test]
fn mark_exited_is_idempotent() {
    let mut t = Thread::new(1);

    // First from any state.
    t.mark_exited();
    assert!(t.is_exited());

    // Second call — must not panic.
    t.mark_exited();
    assert!(t.is_exited());
}

/// Verify that deschedule on an exited thread is a no-op.
/// This happens in schedule_inner when kill_process marked a running thread
/// as Exited on another core, and the scheduler later deschedules it.
#[test]
fn deschedule_exited_thread_is_noop() {
    let mut t = Thread::new(1);

    t.mark_exited();
    assert!(t.is_exited());

    // deschedule should be a no-op — thread is Exited, not Running.
    t.deschedule();
    assert!(t.is_exited(), "deschedule must not change Exited state");
}

/// Verify the complete thread exit → deferred drop → drain lifecycle.
/// Models the exact sequence in scheduler.rs: thread exits, park_old defers it,
/// next schedule_inner drains deferred_drops (safe because we're on a different
/// thread's stack by then).
#[test]
fn deferred_drop_lifecycle_complete() {
    let mut s = State::new(1);

    // Step 1: Thread is running on core 0.
    let mut t = Thread::new(1);
    t.activate();
    s.cores[0].current = Some(t);

    // Step 2: Thread exits (mark_exited + deschedule + park_old).
    let mut old = s.cores[0].current.take().unwrap();
    old.mark_exited();
    old.deschedule(); // No-op for Exited.
    park_old(&mut s, old, 0);

    assert_eq!(s.deferred_drops.len(), 1, "thread must be deferred");

    // Step 3: Next schedule_inner drains deferred drops (on different stack).
    s.deferred_drops.clear();
    assert!(
        s.deferred_drops.is_empty(),
        "deferred drops must be empty after drain"
    );

    // Step 4: No traces of the thread anywhere.
    assert!(s.ready.is_empty());
    assert!(s.blocked.is_empty());
}

/// Verify that kill_process + exit_current race scenario doesn't duplicate
/// a thread in deferred_drops. Simulates: kill_process marks thread Exited
/// while it's current on a core, then schedule_inner runs on that core.
#[test]
fn kill_then_schedule_no_duplicate_deferred() {
    let mut s = State::new(2);

    // Thread 1 running on core 0, thread 2 running on core 1.
    let mut t1 = Thread::new(1);
    t1.activate();
    s.cores[0].current = Some(t1);

    let mut t2 = Thread::new(2);
    t2.activate();
    s.cores[1].current = Some(t2);

    // kill_process marks thread 1 as Exited (from core 1's context).
    s.cores[0].current.as_mut().unwrap().mark_exited();

    // Core 0's schedule_inner runs: deschedule + park_old.
    let mut old = s.cores[0].current.take().unwrap();
    let was_exited = old.is_exited();
    old.deschedule(); // No-op for Exited.
    park_old(&mut s, old, 0);

    assert!(was_exited, "thread must be detected as exited");
    assert_eq!(s.deferred_drops.len(), 1, "exactly one deferred drop");

    // Drain deferred.
    s.deferred_drops.clear();

    // Nothing should be left.
    assert!(s.ready.is_empty());
    assert!(s.blocked.is_empty());
    assert!(s.deferred_drops.is_empty());
}

/// Verify wake on an exited thread returns false.
/// After a thread exits, wake attempts must fail gracefully.
#[test]
fn wake_exited_thread_returns_false() {
    let mut s = State::new(1);
    let mut t = Thread::new(1);

    t.activate();
    t.mark_exited();
    park_old(&mut s, t, 0); // Goes to deferred_drops, not blocked.

    // Try to wake — should fail (thread is not in blocked list).
    assert!(!try_wake(&mut s, 1));
}

/// Verify that the reap_exited function (modeled here) correctly removes
/// exited threads from the ready queue. This handles the case where
/// kill_process marks a ready-queue thread as Exited.
#[test]
fn reap_removes_exited_from_ready_queue() {
    let mut s = State::new(1);

    // Place threads in ready queue.
    s.ready.push(Thread::new(1));
    s.ready.push(Thread::new(2));
    s.ready.push(Thread::new(3));

    // Mark thread 2 as exited (simulates kill_process).
    s.ready[1].mark_exited();

    // Reap exited threads (mirrors scheduler.rs reap_exited).
    s.ready.retain(|t| !t.is_exited());

    assert_eq!(s.ready.len(), 2);
    assert!(
        s.ready.iter().all(|t| t.id != 2),
        "exited thread not reaped"
    );
    assert!(s.ready.iter().any(|t| t.id == 1));
    assert!(s.ready.iter().any(|t| t.id == 3));
}

/// Verify that reap_exited also cleans the blocked list.
#[test]
fn reap_removes_exited_from_blocked_list() {
    let mut s = State::new(1);

    // Place a blocked thread.
    let mut t = Thread::new(1);
    t.activate();
    t.block();
    s.blocked.push(t);

    // Mark it exited (simulates kill_process).
    s.blocked[0].mark_exited();

    // Reap.
    s.blocked.retain(|t| !t.is_exited());

    assert!(
        s.blocked.is_empty(),
        "exited thread not reaped from blocked list"
    );
}

// ============================================================
// Per-core idle tracking (VAL-IPI-008, VAL-IPI-009, VAL-IPI-010)
// ============================================================

/// VAL-IPI-008: is_idle is false initially for all cores.
#[test]
fn is_idle_false_initially() {
    let s = State::new(4);

    for (i, core) in s.cores.iter().enumerate() {
        assert!(
            !core.is_idle,
            "core {i}: is_idle must be false at initialization"
        );
    }
}

/// VAL-IPI-009: is_idle becomes true when core runs idle thread.
#[test]
fn is_idle_true_when_core_runs_idle_thread() {
    let mut s = State::new(4);

    // Simulate schedule_inner on core 0 with no runnable threads → idle fallback.
    let mut idle = s.cores[0].idle.take().expect("idle thread must exist");
    idle.activate();
    s.cores[0].current = Some(idle);
    s.cores[0].is_idle = true;

    assert!(
        s.cores[0].is_idle,
        "is_idle must be true when core runs idle thread"
    );
    assert!(
        s.cores[0].current.as_ref().unwrap().is_idle(),
        "current thread must be the idle thread"
    );

    check_invariants(&s, "idle_thread_on_core_0");
}

/// VAL-IPI-010: is_idle becomes false when a real thread is scheduled.
#[test]
fn is_idle_false_when_real_thread_scheduled() {
    let mut s = State::new(4);

    // Step 1: Core goes idle (no runnable threads).
    let mut idle = s.cores[0].idle.take().expect("idle thread must exist");
    idle.activate();
    s.cores[0].current = Some(idle);
    s.cores[0].is_idle = true;

    assert!(s.cores[0].is_idle);

    // Step 2: A real thread becomes ready and is scheduled.
    s.ready.push(Thread::new(1));

    // Simulate schedule_inner: park idle, select from ready queue.
    let mut old = s.cores[0].current.take().unwrap();
    old.deschedule();
    park_old(&mut s, old, 0);

    let mut new_thread = s.ready.remove(0);
    new_thread.activate();
    s.cores[0].current = Some(new_thread);
    s.cores[0].is_idle = false;

    assert!(
        !s.cores[0].is_idle,
        "is_idle must be false when a real thread is scheduled"
    );

    check_invariants(&s, "real_thread_on_core_0");
}

/// is_idle is false when old thread continues (no better candidate in queue).
#[test]
fn is_idle_false_when_current_thread_continues() {
    let mut s = State::new(4);

    // A real thread is running and continues (no other threads in queue).
    let mut t = Thread::new(1);
    t.activate();
    s.cores[0].current = Some(t);
    s.cores[0].is_idle = false;

    // Simulate schedule_inner: old thread continues (no other runnable threads).
    // deschedule + re-activate represents the "current continues" path.
    let mut old = s.cores[0].current.take().unwrap();
    old.deschedule();
    old.activate();
    s.cores[0].current = Some(old);
    s.cores[0].is_idle = false; // Must remain false.

    assert!(
        !s.cores[0].is_idle,
        "is_idle must be false when current thread continues"
    );

    check_invariants(&s, "current_continues");
}

/// is_idle transitions correctly across idle → real → idle → real cycles.
#[test]
fn is_idle_lifecycle_multiple_transitions() {
    let mut s = State::new(1);

    // Cycle 1: Go idle.
    let mut idle = s.cores[0].idle.take().unwrap();
    idle.activate();
    s.cores[0].current = Some(idle);
    s.cores[0].is_idle = true;
    assert!(s.cores[0].is_idle);
    check_invariants(&s, "cycle1_idle");

    // Cycle 2: Real thread scheduled → not idle.
    s.ready.push(Thread::new(1));
    let mut old = s.cores[0].current.take().unwrap();
    old.deschedule();
    park_old(&mut s, old, 0);
    let mut t = s.ready.remove(0);
    t.activate();
    s.cores[0].current = Some(t);
    s.cores[0].is_idle = false;
    assert!(!s.cores[0].is_idle);
    check_invariants(&s, "cycle2_real");

    // Cycle 3: Real thread blocks → go idle again.
    let mut old = s.cores[0].current.take().unwrap();
    old.block();
    park_old(&mut s, old, 0);
    let mut idle = s.cores[0].idle.take().unwrap();
    idle.activate();
    s.cores[0].current = Some(idle);
    s.cores[0].is_idle = true;
    assert!(s.cores[0].is_idle);
    check_invariants(&s, "cycle3_idle");

    // Cycle 4: Wake the blocked thread → real thread again.
    try_wake(&mut s, 1);
    let mut old = s.cores[0].current.take().unwrap();
    old.deschedule();
    park_old(&mut s, old, 0);
    let mut t = s.ready.remove(0);
    t.activate();
    s.cores[0].current = Some(t);
    s.cores[0].is_idle = false;
    assert!(!s.cores[0].is_idle);
    check_invariants(&s, "cycle4_real");
}

/// is_idle is per-core — one core idle doesn't affect others.
#[test]
fn is_idle_per_core_independence() {
    let mut s = State::new(4);

    // Core 0: real thread running.
    let mut t = Thread::new(1);
    t.activate();
    s.cores[0].current = Some(t);
    s.cores[0].is_idle = false;

    // Core 1: idle.
    let mut idle = s.cores[1].idle.take().unwrap();
    idle.activate();
    s.cores[1].current = Some(idle);
    s.cores[1].is_idle = true;

    // Core 2: real thread running.
    let mut t2 = Thread::new(2);
    t2.activate();
    s.cores[2].current = Some(t2);
    s.cores[2].is_idle = false;

    // Core 3: idle.
    let mut idle3 = s.cores[3].idle.take().unwrap();
    idle3.activate();
    s.cores[3].current = Some(idle3);
    s.cores[3].is_idle = true;

    assert!(!s.cores[0].is_idle, "core 0 should not be idle");
    assert!(s.cores[1].is_idle, "core 1 should be idle");
    assert!(!s.cores[2].is_idle, "core 2 should not be idle");
    assert!(s.cores[3].is_idle, "core 3 should be idle");

    check_invariants(&s, "per_core_independence");
}

/// Randomized test verifies is_idle consistency through random actions.
/// The check_invariants function now validates is_idle on every step.
#[test]
fn is_idle_consistent_under_randomized_schedule() {
    // The existing randomized_scheduler_state_machine test already validates
    // is_idle via check_invariants (invariant 6). This test runs a focused
    // scenario with more idle transitions.
    let mut rng = Rng::new(0xDEAD);
    let num_cores = 4;
    let mut s = State::new(num_cores);

    for step in 0..200 {
        let label = format!("is_idle_random step={step}");
        let core = rng.range(num_cores);

        // Half the time: spawn a thread. Other half: schedule cycle.
        if rng.range(2) == 0 {
            s.ready.push(Thread::new(step as u64 + 1));
        }

        // Full schedule cycle.
        if let Some(mut old) = s.cores[core].current.take() {
            old.deschedule();
            park_old(&mut s, old, core);
        }
        s.deferred_drops.clear();

        if !s.ready.is_empty() {
            let mut t = s.ready.remove(0);
            t.activate();
            s.cores[core].current = Some(t);
            s.cores[core].is_idle = false;
        } else {
            let mut idle = s.cores[core]
                .idle
                .take()
                .unwrap_or_else(|| panic!("{label}: idle missing for core {core}"));
            idle.activate();
            s.cores[core].current = Some(idle);
            s.cores[core].is_idle = true;
        }

        check_invariants(&s, &label);
    }
}

// ============================================================
// IPI send-on-wake logic
// ============================================================
//
// These tests model the IPI send logic that lives in try_wake_impl,
// spawn_user, and start_suspended_threads. The real kernel calls
// interrupt_controller::GIC.send_ipi(core_id) under the STATE lock.
// We model it as collecting the target core IDs.

/// Determine which core to IPI after adding a thread to the ready queue.
/// Returns None if no IPI should be sent (no idle core, or only self is idle).
fn find_ipi_target(s: &State, current_core: usize) -> Option<usize> {
    for (i, core) in s.cores.iter().enumerate() {
        if i == current_core {
            continue; // No self-IPI
        }
        if core.is_idle {
            return Some(i);
        }
    }
    None
}

/// VAL-IPI-002: try_wake sends IPI to idle target core.
#[test]
fn ipi_sent_to_idle_core_after_wake() {
    let mut s = State::new(4);

    // Core 0 is running a real thread (is_idle = false).
    let mut t0 = Thread::new(100);
    t0.activate();
    s.cores[0].current = Some(t0);
    s.cores[0].is_idle = false;

    // Core 1 is idle.
    let mut idle1 = s.cores[1].idle.take().unwrap();
    idle1.activate();
    s.cores[1].current = Some(idle1);
    s.cores[1].is_idle = true;

    // A thread was blocked, now try_wake moves it to ready.
    let mut blocked_thread = Thread::new(1);
    blocked_thread.activate();
    blocked_thread.block();
    s.blocked.push(blocked_thread);
    assert!(try_wake(&mut s, 1));

    // After wake, check if we should IPI.
    let target = find_ipi_target(&s, 0);
    assert_eq!(target, Some(1), "should IPI idle core 1");
}

/// VAL-IPI-003: try_wake does NOT send IPI to busy core.
#[test]
fn ipi_not_sent_when_all_cores_busy() {
    let mut s = State::new(4);

    // All 4 cores running real threads.
    for i in 0..4 {
        let mut t = Thread::new(100 + i as u64);
        t.activate();
        s.cores[i].current = Some(t);
        s.cores[i].is_idle = false;
    }

    // Wake a blocked thread on core 0.
    let mut blocked_thread = Thread::new(1);
    blocked_thread.activate();
    blocked_thread.block();
    s.blocked.push(blocked_thread);
    assert!(try_wake(&mut s, 1));

    let target = find_ipi_target(&s, 0);
    assert_eq!(target, None, "no IPI when all cores are busy");
}

/// VAL-IPI-004: try_wake does NOT self-IPI.
#[test]
fn ipi_no_self_ipi() {
    let mut s = State::new(4);

    // Core 0 is running and current core.
    let mut t0 = Thread::new(100);
    t0.activate();
    s.cores[0].current = Some(t0);
    s.cores[0].is_idle = false;

    // All other cores busy.
    for i in 1..4 {
        let mut t = Thread::new(100 + i as u64);
        t.activate();
        s.cores[i].current = Some(t);
        s.cores[i].is_idle = false;
    }

    // Make core 0 the only "idle" core — but it's the current core.
    s.cores[0].is_idle = true;

    let target = find_ipi_target(&s, 0);
    assert_eq!(target, None, "must not self-IPI even if current core is idle");
}

/// VAL-IPI-004: Self-IPI skip when current is the only idle core.
#[test]
fn ipi_skip_self_finds_other_idle() {
    let mut s = State::new(4);

    // Core 0 and core 2 idle, core 1 and 3 busy. Current = core 0.
    for i in 0..4 {
        let mut t = Thread::new(100 + i as u64);
        t.activate();
        s.cores[i].current = Some(t);
        s.cores[i].is_idle = i == 0 || i == 2;
    }

    let target = find_ipi_target(&s, 0);
    assert_eq!(target, Some(2), "should IPI core 2, skipping self (core 0)");
}

/// VAL-IPI-011: spawn_user sends IPI to idle core.
#[test]
fn ipi_sent_after_spawn() {
    let mut s = State::new(4);

    // Core 0 busy (spawner).
    let mut t0 = Thread::new(100);
    t0.activate();
    s.cores[0].current = Some(t0);
    s.cores[0].is_idle = false;

    // Core 3 idle.
    let mut idle3 = s.cores[3].idle.take().unwrap();
    idle3.activate();
    s.cores[3].current = Some(idle3);
    s.cores[3].is_idle = true;

    // Spawn a new user thread (goes to ready queue).
    s.ready.push(Thread::new(1));

    let target = find_ipi_target(&s, 0);
    assert_eq!(target, Some(3), "should IPI idle core 3 after spawn");
}

/// VAL-IPI-011: start_suspended_threads sends IPI to idle core.
#[test]
fn ipi_sent_after_start_suspended() {
    let mut s = State::new(4);

    // Core 0 busy.
    let mut t0 = Thread::new(100);
    t0.activate();
    s.cores[0].current = Some(t0);
    s.cores[0].is_idle = false;

    // Core 2 idle.
    let mut idle2 = s.cores[2].idle.take().unwrap();
    idle2.activate();
    s.cores[2].current = Some(idle2);
    s.cores[2].is_idle = true;

    // Move suspended thread to ready (simulates start_suspended_threads).
    s.ready.push(Thread::new(1));

    let target = find_ipi_target(&s, 0);
    assert_eq!(target, Some(2), "should IPI idle core 2 after starting suspended threads");
}

/// VAL-IPI-007: is_idle check and queue insertion under same lock.
/// This test verifies that the model function checks idle state from the
/// same State that contains the ready queue — no separate lock.
#[test]
fn ipi_check_and_push_same_state() {
    let mut s = State::new(4);

    // Core 0 running, core 1 idle.
    let mut t0 = Thread::new(100);
    t0.activate();
    s.cores[0].current = Some(t0);
    s.cores[0].is_idle = false;

    let mut idle1 = s.cores[1].idle.take().unwrap();
    idle1.activate();
    s.cores[1].current = Some(idle1);
    s.cores[1].is_idle = true;

    // Simulate try_wake: push to ready AND check idle in one &mut State call.
    let mut blocked_thread = Thread::new(1);
    blocked_thread.activate();
    blocked_thread.block();
    s.blocked.push(blocked_thread);

    // Under the same &mut s borrow:
    let woke = try_wake(&mut s, 1);
    let target = find_ipi_target(&s, 0);

    assert!(woke, "thread should be woken");
    assert_eq!(target, Some(1), "idle core should be found under same state");
}
