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

    assert!(s.ready.is_empty(), "exited thread must not be in ready queue");
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
    for seed in 1..=50 {
        let mut rng = Rng::new(seed);
        let num_cores = 4;
        let mut s = State::new(num_cores);
        let mut next_id: u64 = 1;
        let mut live_ids: Vec<u64> = Vec::new();

        for step in 0..500 {
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
                    } else {
                        let mut idle = s.cores[core]
                            .idle
                            .take()
                            .unwrap_or_else(|| panic!("{label}: idle missing for core {core}"));
                        idle.activate();
                        s.cores[core].current = Some(idle);
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
    }

    // Rapid block/wake cycles (1000 iterations).
    let mut rng = Rng::new(42);

    for step in 0..1000 {
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
        } else if s.cores[core].current.is_none() {
            let mut idle = s.cores[core].idle.take().expect("idle missing");
            idle.activate();
            s.cores[core].current = Some(idle);
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
    while !s.ready.is_empty() || s.cores.iter().any(|c| c.current.as_ref().map_or(false, |t| !t.is_idle())) {
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
                let mut idle = s.cores[core].idle.take().unwrap_or_else(|| Thread::new_idle(core as u64));
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
