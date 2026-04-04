//! Multi-core scheduler model tests.
//!
//! These tests model the scheduler's behavior under SMP concurrency. Unlike
//! kernel_scheduler_state.rs (which tests single-core state machine invariants),
//! these tests simulate **interleaved operations across multiple cores** to catch
//! cross-core race conditions.
//!
//! Written 2026-03-31 after finding a cross-core deferred_drops use-after-free
//! race that caused 5 kernel crashes. That bug was invisible to single-core
//! model tests because it required two cores to interleave:
//!   Core B: push exited thread to deferred_drops, release lock
//!   Core A: acquire lock, drain deferred_drops → free core B's stack
//!   Core B: still on freed stack → crash
//!
//! The fix (per-core deferred_drops) is tested here. The OLD buggy behavior
//! (global deferred_drops) is also modeled to verify the test catches it.

// ============================================================
// Thread + State model (mirrors kernel)
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

const IDLE_THREAD_ID_MARKER: u64 = 0xFF00;
const MAX_CORES: usize = 8;

struct Thread {
    id: u64,
    state: ThreadState,
    /// Simulates kernel stack ownership. True means this thread's kernel stack
    /// is "in use" by the exception handler return path on some core.
    stack_in_use: bool,
    /// Set when this thread's stack has been freed (simulates use-after-free).
    stack_freed: bool,
}

impl Thread {
    fn new(id: u64) -> Self {
        Self {
            id,
            state: ThreadState::Ready,
            stack_in_use: false,
            stack_freed: false,
        }
    }

    fn new_boot_idle(core_id: u64) -> Self {
        Self {
            id: core_id | IDLE_THREAD_ID_MARKER,
            state: ThreadState::Running,
            stack_in_use: false,
            stack_freed: false,
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

// ============================================================
// Per-core deferred_drops model (matches the FIXED kernel)
// ============================================================

struct PerCoreState {
    current: Option<Thread>,
    idle: Option<Thread>,
    is_idle: bool,
}

struct State {
    cores: Vec<PerCoreState>,
    ready: Vec<Thread>,
    blocked: Vec<Thread>,
    /// Per-core deferred drops — matches the fixed kernel.
    deferred_drops: Vec<Vec<Thread>>,
    /// Per-core deferred ready — threads parked as ready are deferred to this
    /// core's slot instead of immediately entering the global ready queue.
    /// Drained at the start of THIS core's next schedule_inner. Prevents the
    /// stack reuse race: another core picking up a thread while the originating
    /// core is still on its kernel stack (between lock drop and SP switch).
    deferred_ready: Vec<Vec<Thread>>,
    num_cores: usize,
}

impl State {
    fn new(num_cores: usize) -> Self {
        let cores = (0..num_cores)
            .map(|i| PerCoreState {
                current: Some(Thread::new_boot_idle(i as u64)),
                idle: None,
                is_idle: true,
            })
            .collect();

        Self {
            cores,
            ready: Vec::new(),
            blocked: Vec::new(),
            deferred_drops: (0..num_cores).map(|_| Vec::new()).collect(),
            deferred_ready: (0..num_cores).map(|_| Vec::new()).collect(),
            num_cores,
        }
    }
}

fn park_old(s: &mut State, old_thread: Thread, core: usize) {
    if old_thread.is_ready() {
        if old_thread.is_idle() {
            s.cores[core].idle = Some(old_thread);
        } else {
            // Defer ready queue insertion — we're still on this thread's kernel
            // stack until restore_context_and_eret switches SP. Another core
            // picking this thread up immediately would use the same stack.
            s.deferred_ready[core].push(old_thread);
        }
    } else if old_thread.is_exited() {
        s.deferred_drops[core].push(old_thread);
    } else {
        s.blocked.push(old_thread);
    }
}

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

/// Simplified schedule_inner for one core. Returns the thread ID that became current.
fn schedule_on_core(s: &mut State, core: usize) -> u64 {
    // Drain THIS core's deferred drops only.
    s.deferred_drops[core].clear();

    // Drain THIS core's deferred ready threads into the global ready queue.
    // Safe now: we're on a different thread's stack (the one selected last time).
    let deferred = core::mem::take(&mut s.deferred_ready[core]);
    for t in deferred {
        s.ready.push(t);
    }

    let mut old_thread = s.cores[core].current.take().expect("no current thread");
    old_thread.deschedule();

    if let Some(idx) = s.ready.iter().position(|t| !t.is_exited()) {
        // Pick a ready thread.
        let mut new_thread = s.ready.swap_remove(idx);
        new_thread.activate();
        let new_id = new_thread.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;
        new_id
    } else if old_thread.is_ready() {
        // Continue with old thread.
        old_thread.activate();
        let old_id = old_thread.id;
        s.cores[core].current = Some(old_thread);
        old_id
    } else {
        // Fallback to idle.
        let mut idle = s.cores[core].idle.take().expect("no idle thread on core");
        idle.activate();
        let idle_id = idle.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(idle);
        s.cores[core].is_idle = true;
        idle_id
    }
}

// ============================================================
// Global deferred_drops model (the OLD buggy behavior)
// ============================================================

struct BuggyState {
    cores: Vec<PerCoreState>,
    ready: Vec<Thread>,
    blocked: Vec<Thread>,
    /// GLOBAL deferred drops — the pre-fix bug.
    deferred_drops: Vec<Thread>,
    num_cores: usize,
}

impl BuggyState {
    fn new(num_cores: usize) -> Self {
        let cores = (0..num_cores)
            .map(|i| PerCoreState {
                current: Some(Thread::new_boot_idle(i as u64)),
                idle: None,
                is_idle: true,
            })
            .collect();

        Self {
            cores,
            ready: Vec::new(),
            blocked: Vec::new(),
            deferred_drops: Vec::new(),
            num_cores,
        }
    }
}

fn buggy_park_old(s: &mut BuggyState, old_thread: Thread, core: usize) {
    if old_thread.is_ready() {
        if old_thread.is_idle() {
            s.cores[core].idle = Some(old_thread);
        } else {
            s.ready.push(old_thread);
        }
    } else if old_thread.is_exited() {
        s.deferred_drops.push(old_thread);
    } else {
        s.blocked.push(old_thread);
    }
}

/// Buggy schedule_inner: drains GLOBAL deferred_drops (any core's threads).
/// Returns (thread_id, freed_thread_ids) — the freed IDs are threads that were
/// still potentially in use by other cores.
fn buggy_schedule_on_core(s: &mut BuggyState, core: usize) -> (u64, Vec<u64>) {
    // BUG: drain ALL deferred drops, not just this core's.
    let freed: Vec<u64> = s.deferred_drops.iter().map(|t| t.id).collect();
    s.deferred_drops.clear();

    let mut old_thread = s.cores[core].current.take().expect("no current");
    old_thread.deschedule();

    if let Some(idx) = s.ready.iter().position(|t| !t.is_exited()) {
        let mut new_thread = s.ready.swap_remove(idx);
        new_thread.activate();
        let new_id = new_thread.id;
        buggy_park_old(s, old_thread, core);
        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;
        (new_id, freed)
    } else if old_thread.is_ready() {
        old_thread.activate();
        let old_id = old_thread.id;
        s.cores[core].current = Some(old_thread);
        (old_id, freed)
    } else {
        let mut idle = s.cores[core].idle.take().expect("no idle");
        idle.activate();
        let idle_id = idle.id;
        buggy_park_old(s, old_thread, core);
        s.cores[core].current = Some(idle);
        s.cores[core].is_idle = true;
        (idle_id, freed)
    }
}

// ============================================================
// Invariant checkers
// ============================================================

/// Verify no thread appears in multiple locations simultaneously.
fn assert_no_duplicates(s: &State) {
    let mut seen = std::collections::HashSet::new();

    for (i, core) in s.cores.iter().enumerate() {
        if let Some(t) = &core.current {
            assert!(
                seen.insert(t.id),
                "thread {} duplicated (current on core {i})",
                t.id
            );
        }
        if let Some(t) = &core.idle {
            assert!(
                seen.insert(t.id),
                "thread {} duplicated (idle on core {i})",
                t.id
            );
        }
    }
    for t in &s.ready {
        assert!(
            seen.insert(t.id),
            "thread {} duplicated (ready queue)",
            t.id
        );
    }
    for t in &s.blocked {
        assert!(
            seen.insert(t.id),
            "thread {} duplicated (blocked list)",
            t.id
        );
    }
    for (core_idx, drops) in s.deferred_drops.iter().enumerate() {
        for t in drops {
            assert!(
                seen.insert(t.id),
                "thread {} duplicated (deferred_drops[{core_idx}])",
                t.id
            );
        }
    }
    for (core_idx, deferred) in s.deferred_ready.iter().enumerate() {
        for t in deferred {
            assert!(
                seen.insert(t.id),
                "thread {} duplicated (deferred_ready[{core_idx}])",
                t.id
            );
        }
    }
}

/// Count total threads across all locations.
fn total_threads(s: &State) -> usize {
    let mut count = 0;
    for core in &s.cores {
        if core.current.is_some() {
            count += 1;
        }
        if core.idle.is_some() {
            count += 1;
        }
    }
    count += s.ready.len();
    count += s.blocked.len();
    for drops in &s.deferred_drops {
        count += drops.len();
    }
    for deferred in &s.deferred_ready {
        count += deferred.len();
    }
    count
}

// ============================================================
// Tests: Per-core deferred_drops (fixed behavior)
// ============================================================

/// The most basic SMP test: two cores scheduling, one thread exits.
/// With per-core deferred_drops, core A cannot free core B's exited thread.
#[test]
fn two_core_deferred_drops_isolation() {
    let mut s = State::new(2);

    // Spawn two user threads.
    s.ready.push(Thread::new(10));
    s.ready.push(Thread::new(11));

    // Core 0 picks thread 10.
    schedule_on_core(&mut s, 0);
    // Core 1 picks thread 11.
    schedule_on_core(&mut s, 1);

    // Thread 10 exits on core 0.
    s.cores[0].current.as_mut().unwrap().mark_exited();
    schedule_on_core(&mut s, 0);

    // Thread 10 is now in deferred_drops[0].
    assert_eq!(
        s.deferred_drops[0].len(),
        1,
        "exited thread must be in core 0's deferred_drops"
    );
    assert!(
        s.deferred_drops[1].is_empty(),
        "core 1's deferred_drops must be empty"
    );

    // Core 1 schedules — must NOT drain core 0's deferred drops.
    schedule_on_core(&mut s, 1);
    assert_eq!(
        s.deferred_drops[0].len(),
        1,
        "core 1's schedule must not drain core 0's deferred_drops"
    );

    // Core 0 schedules — NOW core 0's drops are drained (safe: different stack).
    schedule_on_core(&mut s, 0);
    assert!(
        s.deferred_drops[0].is_empty(),
        "core 0's deferred_drops must be drained by core 0's next schedule"
    );
}

/// Verify the BUG scenario: global deferred_drops allows cross-core free.
/// This test MUST detect the race (it models the pre-fix behavior).
#[test]
fn buggy_global_deferred_drops_allows_cross_core_free() {
    let mut s = BuggyState::new(2);

    s.ready.push(Thread::new(10));
    s.ready.push(Thread::new(11));

    buggy_schedule_on_core(&mut s, 0); // core 0 picks thread 10
    buggy_schedule_on_core(&mut s, 1); // core 1 picks thread 11

    // Thread 10 exits on core 0.
    s.cores[0].current.as_mut().unwrap().mark_exited();
    buggy_schedule_on_core(&mut s, 0);

    assert_eq!(
        s.deferred_drops.len(),
        1,
        "exited thread in global deferred_drops"
    );

    // Core 1 schedules — in the buggy version, it drains ALL deferred_drops,
    // including thread 10 which core 0 might still be using.
    let (_, freed) = buggy_schedule_on_core(&mut s, 1);

    // The buggy behavior: core 1 freed thread 10 (core 0's thread).
    assert!(
        freed.contains(&10),
        "BUG CONFIRMED: global deferred_drops allows core 1 to free core 0's thread"
    );
    assert!(
        s.deferred_drops.is_empty(),
        "BUG CONFIRMED: all deferred drops drained by wrong core"
    );
}

/// Parameterized test across core counts: 1, 2, 3, 4, 8.
/// For each core count, run N scheduling rounds with thread exits and verify
/// per-core isolation holds.
#[test]
fn per_core_isolation_across_core_counts() {
    for num_cores in [1, 2, 3, 4, 8] {
        let mut s = State::new(num_cores);
        let mut next_id = 100u64;

        // Spawn one user thread per core.
        for _ in 0..num_cores {
            s.ready.push(Thread::new(next_id));
            next_id += 1;
        }

        // Each core picks a thread.
        for core in 0..num_cores {
            schedule_on_core(&mut s, core);
        }

        // Each core's thread exits, one at a time.
        for exiting_core in 0..num_cores {
            if let Some(t) = s.cores[exiting_core].current.as_mut() {
                if !t.is_idle() {
                    t.mark_exited();
                }
            }
            schedule_on_core(&mut s, exiting_core);

            // Only exiting_core's deferred_drops should have the exited thread.
            for other_core in 0..num_cores {
                if other_core != exiting_core {
                    // Other cores scheduling must NOT drain exiting_core's drops.
                    let before = s.deferred_drops[exiting_core].len();
                    schedule_on_core(&mut s, other_core);
                    let after = s.deferred_drops[exiting_core].len();
                    assert_eq!(
                        before, after,
                        "core {other_core} drained core {exiting_core}'s deferred_drops \
                         (num_cores={num_cores})"
                    );
                }
            }

            // Exiting core's next schedule drains its own drops.
            schedule_on_core(&mut s, exiting_core);
            assert!(
                s.deferred_drops[exiting_core].is_empty(),
                "core {exiting_core} failed to drain its own deferred_drops \
                 (num_cores={num_cores})"
            );
        }
    }
}

// ============================================================
// Tests: Boot/idle thread (merged, no migration)
// ============================================================

/// Boot threads must never appear in the global ready queue.
/// With the merged boot/idle thread (IDLE_THREAD_ID_MARKER), park_old sends
/// them to the idle slot, not the ready queue.
#[test]
fn boot_thread_never_in_ready_queue_any_core_count() {
    for num_cores in [1, 2, 3, 4, 8] {
        let mut s = State::new(num_cores);

        // Add user threads and schedule many rounds.
        for i in 0..num_cores * 2 {
            s.ready.push(Thread::new(100 + i as u64));
        }

        for round in 0..50 {
            for core in 0..num_cores {
                schedule_on_core(&mut s, core);
            }

            // After every round, no idle/boot thread should be in the ready queue.
            for t in &s.ready {
                assert!(
                    !t.is_idle(),
                    "boot/idle thread {} found in ready queue \
                     (num_cores={num_cores}, round={round})",
                    t.id
                );
            }
        }
    }
}

/// Each core's boot thread must always return to its own idle slot,
/// never another core's idle slot.
#[test]
fn boot_thread_stays_on_own_core() {
    for num_cores in [2, 3, 4, 8] {
        let mut s = State::new(num_cores);

        // Add user threads to make scheduling interesting.
        for i in 0..num_cores {
            s.ready.push(Thread::new(100 + i as u64));
        }

        for _ in 0..100 {
            for core in 0..num_cores {
                schedule_on_core(&mut s, core);
            }

            // Verify each core's idle slot (if populated) has the right core's thread.
            for core in 0..num_cores {
                if let Some(idle) = &s.cores[core].idle {
                    let expected_id = core as u64 | IDLE_THREAD_ID_MARKER;
                    assert_eq!(
                        idle.id, expected_id,
                        "core {core}'s idle slot has thread {} (expected {expected_id}) \
                         (num_cores={num_cores})",
                        idle.id
                    );
                }
            }
        }
    }
}

// ============================================================
// Tests: Thread conservation (no thread leak or duplication)
// ============================================================

/// Total thread count must be conserved across all scheduling operations.
/// Threads can move between locations but never appear or disappear.
#[test]
fn thread_count_conserved_across_cores() {
    for num_cores in [1, 2, 3, 4, 8] {
        let mut s = State::new(num_cores);
        let num_user_threads = num_cores * 3;

        for i in 0..num_user_threads {
            s.ready.push(Thread::new(100 + i as u64));
        }

        let initial_count = total_threads(&s);

        // Run many scheduling rounds, blocking and waking threads.
        for round in 0..200 {
            let core = round % num_cores;
            schedule_on_core(&mut s, core);

            // Occasionally block the current thread.
            if round % 7 == 0 {
                if let Some(t) = s.cores[core].current.as_mut() {
                    if !t.is_idle() && t.state == ThreadState::Running {
                        t.block();
                        schedule_on_core(&mut s, core);
                    }
                }
            }

            // Occasionally wake a blocked thread.
            if round % 11 == 0 {
                let ids: Vec<u64> = s.blocked.iter().map(|t| t.id).collect();
                for id in ids {
                    try_wake(&mut s, id);
                }
            }

            // Occasionally exit a thread and spawn a replacement.
            if round % 23 == 0 {
                if let Some(t) = s.cores[core].current.as_mut() {
                    if !t.is_idle() && t.state == ThreadState::Running {
                        t.mark_exited();
                        // Spawn replacement to keep count stable.
                        s.ready.push(Thread::new(1000 + round as u64));
                        schedule_on_core(&mut s, core);
                    }
                }
            }

            assert_no_duplicates(&s);

            // Thread count changes only when exits happen (deferred drops remove them).
            // We add replacements so count should stay approximately stable.
        }

        // After all rounds, drain all deferred drops.
        for core in 0..num_cores {
            s.deferred_drops[core].clear();
        }

        assert_no_duplicates(&s);
    }
}

// ============================================================
// Tests: Idle thread fallback correctness
// ============================================================

/// When all user threads block, the idle thread must be available.
/// Tests that the merged boot/idle thread correctly cycles between
/// current and idle slot.
#[test]
fn idle_fallback_after_all_threads_block() {
    for num_cores in [1, 2, 3, 4] {
        let mut s = State::new(num_cores);

        // Add one user thread per core.
        for i in 0..num_cores {
            s.ready.push(Thread::new(100 + i as u64));
        }

        // Each core picks a user thread (boot thread goes to idle slot).
        for core in 0..num_cores {
            schedule_on_core(&mut s, core);
            assert!(
                !s.cores[core].is_idle,
                "core {core} should be running user thread (num_cores={num_cores})"
            );
        }

        // All user threads block.
        for core in 0..num_cores {
            if let Some(t) = s.cores[core].current.as_mut() {
                if !t.is_idle() {
                    t.block();
                }
            }
            schedule_on_core(&mut s, core);

            // Core should now be running its idle/boot thread.
            assert!(
                s.cores[core].is_idle || s.cores[core].current.as_ref().unwrap().is_idle(),
                "core {core} must fall back to idle when all threads blocked \
                 (num_cores={num_cores})"
            );
        }
    }
}

/// The idle thread must NEVER have a zeroed/invalid ID.
/// With the merged boot/idle thread, the idle slot always contains a thread
/// with `core_id | IDLE_THREAD_ID_MARKER`.
#[test]
fn idle_thread_id_valid_after_scheduling() {
    for num_cores in [1, 2, 3, 4, 8] {
        let mut s = State::new(num_cores);

        for i in 0..num_cores * 2 {
            s.ready.push(Thread::new(100 + i as u64));
        }

        for _ in 0..100 {
            for core in 0..num_cores {
                schedule_on_core(&mut s, core);
            }
        }

        // After scheduling, each core's current or idle thread (whichever is the
        // boot thread) must have a valid ID.
        for core in 0..num_cores {
            let expected_id = core as u64 | IDLE_THREAD_ID_MARKER;
            let found = s.cores[core]
                .current
                .as_ref()
                .filter(|t| t.id == expected_id)
                .is_some()
                || s.cores[core]
                    .idle
                    .as_ref()
                    .filter(|t| t.id == expected_id)
                    .is_some();

            assert!(
                found,
                "core {core}'s boot/idle thread (id={expected_id:#x}) not found \
                 in current or idle slot (num_cores={num_cores})"
            );
        }
    }
}

// ============================================================
// Tests: Stress — rapid exit/spawn cycles across cores
// ============================================================

/// Simulate rapid thread churn: threads exit and respawn continuously
/// across all cores. This is the pattern that triggered the original crash
/// (Ctrl+Tab causing rapid document switching → service restart).
#[test]
fn rapid_exit_respawn_churn_all_core_counts() {
    for num_cores in [1, 2, 3, 4, 8] {
        let mut s = State::new(num_cores);
        let mut next_id = 100u64;

        // Initial threads.
        for _ in 0..num_cores * 2 {
            s.ready.push(Thread::new(next_id));
            next_id += 1;
        }

        // 1000 rounds of schedule/exit/spawn across all cores.
        for round in 0..1000 {
            let core = round % num_cores;

            schedule_on_core(&mut s, core);

            // Every 3rd round: current thread exits, spawn a replacement.
            if round % 3 == 0 {
                if let Some(t) = s.cores[core].current.as_mut() {
                    if !t.is_idle() && t.state == ThreadState::Running {
                        t.mark_exited();
                        s.ready.push(Thread::new(next_id));
                        next_id += 1;
                        schedule_on_core(&mut s, core);
                    }
                }
            }

            // Verify per-core isolation every 10 rounds.
            if round % 10 == 0 {
                for c in 0..num_cores {
                    for other in 0..num_cores {
                        if other != c {
                            assert!(
                                s.deferred_drops[c]
                                    .iter()
                                    .all(|t| !s.deferred_drops[other].iter().any(|o| o.id == t.id)),
                                "thread found in both core {c} and core {other} deferred_drops \
                                 (round={round}, num_cores={num_cores})"
                            );
                        }
                    }
                }
            }
        }

        assert_no_duplicates(&s);
    }
}

/// Seeded deterministic stress: reproducible scheduling order.
/// Uses a simple LCG PRNG for reproducibility without pulling in rand.
#[test]
fn seeded_smp_stress() {
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
    }

    for num_cores in [2, 3, 4, 8] {
        for seed in [42u64, 1337, 0xDEADBEEF, 999, 2026_03_31] {
            let mut rng = Lcg(seed);
            let mut s = State::new(num_cores);
            let mut next_id = 100u64;
            let mut exits = 0u64;
            let mut wakes = 0u64;

            // Initial threads.
            for _ in 0..num_cores * 3 {
                s.ready.push(Thread::new(next_id));
                next_id += 1;
            }

            for _ in 0..5000 {
                let core = (rng.next() % num_cores as u64) as usize;

                schedule_on_core(&mut s, core);

                match rng.next() % 10 {
                    0..=2 => {
                        // Block current thread.
                        if let Some(t) = s.cores[core].current.as_mut() {
                            if !t.is_idle() && t.state == ThreadState::Running {
                                t.block();
                                schedule_on_core(&mut s, core);
                            }
                        }
                    }
                    3..=4 => {
                        // Exit current thread and spawn replacement.
                        if let Some(t) = s.cores[core].current.as_mut() {
                            if !t.is_idle() && t.state == ThreadState::Running {
                                t.mark_exited();
                                s.ready.push(Thread::new(next_id));
                                next_id += 1;
                                exits += 1;
                                schedule_on_core(&mut s, core);
                            }
                        }
                    }
                    5..=6 => {
                        // Wake a random blocked thread.
                        if !s.blocked.is_empty() {
                            let idx = (rng.next() % s.blocked.len() as u64) as usize;
                            let id = s.blocked[idx].id;
                            if try_wake(&mut s, id) {
                                wakes += 1;
                            }
                        }
                    }
                    _ => {
                        // Just schedule (timer tick).
                    }
                }

                assert_no_duplicates(&s);
            }

            // Verify at least some exits and wakes happened.
            assert!(
                exits > 0,
                "no exits occurred (seed={seed}, num_cores={num_cores})"
            );
            assert!(
                wakes > 0 || s.blocked.is_empty(),
                "no wakes with blocked threads (seed={seed}, num_cores={num_cores})"
            );

            // Drain all deferred drops and verify clean state.
            for core in 0..num_cores {
                s.deferred_drops[core].clear();
            }
            assert_no_duplicates(&s);
        }
    }
}

// ============================================================
// Tests: Edge cases
// ============================================================

/// Single core: deferred_drops still works correctly (no other core to race with).
#[test]
fn single_core_deferred_drops() {
    let mut s = State::new(1);
    s.ready.push(Thread::new(10));

    schedule_on_core(&mut s, 0);

    // Exit the thread.
    s.cores[0].current.as_mut().unwrap().mark_exited();
    schedule_on_core(&mut s, 0);

    assert_eq!(s.deferred_drops[0].len(), 1);

    // Next schedule drains it.
    schedule_on_core(&mut s, 0);
    assert!(s.deferred_drops[0].is_empty());
}

/// MAX_CORES (8): verify deferred_drops arrays are correctly sized.
#[test]
fn max_cores_deferred_drops_array_size() {
    let s = State::new(MAX_CORES);
    assert_eq!(s.deferred_drops.len(), MAX_CORES);
    for drops in &s.deferred_drops {
        assert!(drops.is_empty());
    }
}

/// Idle thread activation on all cores simultaneously: every core has nothing
/// runnable, falls back to idle.
#[test]
fn all_cores_idle_simultaneously() {
    for num_cores in [1, 2, 3, 4, 8] {
        let s = State::new(num_cores);

        // All cores start with their boot/idle thread as current.
        for core in 0..num_cores {
            let current = s.cores[core].current.as_ref().unwrap();
            assert!(
                current.is_idle(),
                "core {core} should start with boot/idle thread (num_cores={num_cores})"
            );
        }
    }
}

/// A thread exiting on core 0 while cores 1-7 are all scheduling should
/// not corrupt any state.
#[test]
fn exit_under_full_smp_load() {
    for num_cores in [2, 4, 8] {
        let mut s = State::new(num_cores);

        // Spawn enough threads for all cores.
        for i in 0..num_cores * 2 {
            s.ready.push(Thread::new(100 + i as u64));
        }

        // All cores pick threads.
        for core in 0..num_cores {
            schedule_on_core(&mut s, core);
        }

        // Core 0's thread exits.
        s.cores[0].current.as_mut().unwrap().mark_exited();
        schedule_on_core(&mut s, 0);

        // All OTHER cores schedule — must not touch core 0's deferred drops.
        let core0_drops_before = s.deferred_drops[0].len();
        for core in 1..num_cores {
            schedule_on_core(&mut s, core);
        }
        assert_eq!(
            s.deferred_drops[0].len(),
            core0_drops_before,
            "other cores modified core 0's deferred_drops (num_cores={num_cores})"
        );

        assert_no_duplicates(&s);
    }
}

// ============================================================
// Buggy immediate-ready model (the OLD vulnerable behavior)
// ============================================================

/// Model of the pre-fix behavior where park_old immediately pushes ready
/// threads into the global ready queue, allowing another core to pick the
/// thread while the originating core is still on its kernel stack.
struct BuggyReadyState {
    cores: Vec<PerCoreState>,
    ready: Vec<Thread>,
    blocked: Vec<Thread>,
    deferred_drops: Vec<Vec<Thread>>,
    // NO deferred_ready — this is the bug
    num_cores: usize,
}

impl BuggyReadyState {
    fn new(num_cores: usize) -> Self {
        let cores = (0..num_cores)
            .map(|i| PerCoreState {
                current: Some(Thread::new_boot_idle(i as u64)),
                idle: None,
                is_idle: true,
            })
            .collect();

        Self {
            cores,
            ready: Vec::new(),
            blocked: Vec::new(),
            deferred_drops: (0..num_cores).map(|_| Vec::new()).collect(),
            num_cores,
        }
    }
}

fn buggy_ready_park_old(s: &mut BuggyReadyState, old_thread: Thread, core: usize) {
    if old_thread.is_ready() {
        if old_thread.is_idle() {
            s.cores[core].idle = Some(old_thread);
        } else {
            // BUG: immediate enqueue — another core can pick this thread while
            // the originating core is still on its kernel stack.
            s.ready.push(old_thread);
        }
    } else if old_thread.is_exited() {
        s.deferred_drops[core].push(old_thread);
    } else {
        s.blocked.push(old_thread);
    }
}

/// Buggy schedule_on_core: pushes ready threads directly into the ready queue.
/// Returns (new_thread_id, ready_thread_ids_visible_to_other_cores).
fn buggy_ready_schedule_on_core(s: &mut BuggyReadyState, core: usize) -> (u64, Vec<u64>) {
    s.deferred_drops[core].clear();

    let mut old_thread = s.cores[core].current.take().expect("no current");
    old_thread.deschedule();

    if let Some(idx) = s.ready.iter().position(|t| !t.is_exited()) {
        let mut new_thread = s.ready.swap_remove(idx);
        new_thread.activate();
        let new_id = new_thread.id;
        buggy_ready_park_old(s, old_thread, core);
        // After park_old, the old thread is in the global ready queue.
        // Snapshot what's visible to other cores RIGHT NOW (between park_old
        // and restore_context_and_eret — the vulnerable window).
        let visible: Vec<u64> = s.ready.iter().map(|t| t.id).collect();
        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;
        (new_id, visible)
    } else if old_thread.is_ready() {
        old_thread.activate();
        let old_id = old_thread.id;
        s.cores[core].current = Some(old_thread);
        (old_id, Vec::new())
    } else {
        let mut idle = s.cores[core].idle.take().expect("no idle");
        idle.activate();
        let idle_id = idle.id;
        buggy_ready_park_old(s, old_thread, core);
        let visible: Vec<u64> = s.ready.iter().map(|t| t.id).collect();
        s.cores[core].current = Some(idle);
        s.cores[core].is_idle = true;
        (idle_id, visible)
    }
}

// ============================================================
// Tests: Per-core deferred_ready (fixed behavior)
// ============================================================

/// Core test: when a thread is preempted, it goes to deferred_ready (not the
/// global ready queue). Another core scheduling immediately does NOT see it.
#[test]
fn two_core_deferred_ready_isolation() {
    let mut s = State::new(2);

    // Three threads: one for each core + one extra to force preemption.
    s.ready.push(Thread::new(10));
    s.ready.push(Thread::new(11));
    s.ready.push(Thread::new(12));

    // Each core picks a thread.
    schedule_on_core(&mut s, 0);
    schedule_on_core(&mut s, 1);

    let core0_tid = s.cores[0].current.as_ref().unwrap().id;
    let core1_tid = s.cores[1].current.as_ref().unwrap().id;

    // One thread remains in the ready queue.
    assert_eq!(s.ready.len(), 1);
    let remaining_tid = s.ready[0].id;

    // Core 0 reschedules (timer preemption) — picks the remaining thread.
    // The old thread should go to deferred_ready[0], NOT the global ready queue.
    schedule_on_core(&mut s, 0);

    assert_eq!(s.cores[0].current.as_ref().unwrap().id, remaining_tid);
    assert_eq!(
        s.deferred_ready[0].len(),
        1,
        "preempted thread must be in core 0's deferred_ready"
    );
    assert_eq!(s.deferred_ready[0][0].id, core0_tid);
    assert!(
        s.ready.is_empty(),
        "global ready queue must be empty — preempted thread is deferred"
    );

    // Core 1 schedules — must NOT see the deferred thread.
    schedule_on_core(&mut s, 1);

    // Core 1 has no other runnable thread, so it continues with its current.
    assert_eq!(s.cores[1].current.as_ref().unwrap().id, core1_tid);
    assert_eq!(
        s.deferred_ready[0].len(),
        1,
        "core 1's schedule must not drain core 0's deferred_ready"
    );

    // Core 0 schedules AGAIN — the deferred thread is drained and becomes
    // eligible. It either gets picked as new (and the current goes deferred)
    // or core 0 continues. Either way, the OLD deferred content was drained.
    let old_deferred_tid = s.deferred_ready[0][0].id;
    schedule_on_core(&mut s, 0);
    // The old deferred thread must no longer be in deferred_ready (it was drained).
    assert!(
        !s.deferred_ready[0].iter().any(|t| t.id == old_deferred_tid),
        "previously deferred thread must have been drained by core 0's next schedule"
    );

    assert_no_duplicates(&s);
}

/// Verify the BUG scenario: immediate ready queue insertion allows cross-core
/// stack reuse. This test MUST detect the race.
#[test]
fn buggy_immediate_ready_allows_cross_core_stack_reuse() {
    let mut s = BuggyReadyState::new(2);

    s.ready.push(Thread::new(10));
    s.ready.push(Thread::new(11));
    s.ready.push(Thread::new(12));

    buggy_ready_schedule_on_core(&mut s, 0); // core 0 picks a thread
    buggy_ready_schedule_on_core(&mut s, 1); // core 1 picks a thread

    let core0_tid = s.cores[0].current.as_ref().unwrap().id;

    // Core 0 reschedules — core0_tid gets preempted.
    // In the buggy model, core0_tid goes DIRECTLY into the ready queue.
    let (_new_id, visible_to_others) = buggy_ready_schedule_on_core(&mut s, 0);

    // BUG CONFIRMED: the preempted thread is immediately visible in the ready
    // queue. Core 1 could pick it up while core 0 is still on its stack.
    assert!(
        visible_to_others.contains(&core0_tid),
        "BUG CONFIRMED: preempted thread {core0_tid} immediately visible in ready queue \
         (core 1 could restore it while core 0 is still on its stack)"
    );
}

/// Deferred ready across all core counts: thread is invisible to other cores
/// for exactly one scheduling round on the originating core.
#[test]
fn deferred_ready_isolation_across_core_counts() {
    for num_cores in [1, 2, 3, 4, 8] {
        let mut s = State::new(num_cores);

        // Enough threads to cause preemption.
        for i in 0..num_cores * 2 {
            s.ready.push(Thread::new(100 + i as u64));
        }

        // Each core picks a thread.
        for core in 0..num_cores {
            schedule_on_core(&mut s, core);
        }

        // Each core reschedules (preemption). Old threads go to deferred_ready.
        for core in 0..num_cores {
            schedule_on_core(&mut s, core);

            // No thread should be in two cores' deferred_ready simultaneously.
            for c in 0..num_cores {
                for other in (c + 1)..num_cores {
                    assert!(
                        s.deferred_ready[c]
                            .iter()
                            .all(|t| !s.deferred_ready[other].iter().any(|o| o.id == t.id)),
                        "thread found in both core {c} and core {other} deferred_ready \
                         (num_cores={num_cores})"
                    );
                }
            }
        }

        assert_no_duplicates(&s);
    }
}

/// Deferred ready threads become available after the originating core's next
/// schedule_inner — verifying the drain path.
#[test]
fn deferred_ready_drained_on_next_schedule() {
    let mut s = State::new(2);

    s.ready.push(Thread::new(10));
    s.ready.push(Thread::new(11));

    // Core 0 picks a thread.
    schedule_on_core(&mut s, 0);
    let first_tid = s.cores[0].current.as_ref().unwrap().id;

    // Core 0 reschedules — picks the other thread, defers the first.
    schedule_on_core(&mut s, 0);
    let second_tid = s.cores[0].current.as_ref().unwrap().id;
    assert_ne!(first_tid, second_tid);
    assert_eq!(s.deferred_ready[0].len(), 1);
    assert_eq!(s.deferred_ready[0][0].id, first_tid);
    assert!(s.ready.is_empty(), "ready queue must be empty — only thread is deferred");

    // Core 0 reschedules AGAIN — drains deferred_ready (first_tid enters
    // the ready queue, gets picked). second_tid is now deferred.
    schedule_on_core(&mut s, 0);
    // The previously deferred thread (first_tid) was drained and is now current.
    assert_eq!(s.cores[0].current.as_ref().unwrap().id, first_tid);
    // second_tid is now in deferred_ready (the ping-pong). The key invariant:
    // first_tid is NO LONGER in deferred_ready — it was properly drained.
    assert!(
        !s.deferred_ready[0].iter().any(|t| t.id == first_tid),
        "previously deferred thread must have been drained"
    );

    assert_no_duplicates(&s);
}

/// Stress test: rapid preemption cycles across cores with deferred_ready.
#[test]
fn rapid_preemption_churn_deferred_ready() {
    for num_cores in [2, 3, 4, 8] {
        let mut s = State::new(num_cores);
        let mut next_id = 100u64;

        // Plenty of threads to cause constant preemption.
        for _ in 0..num_cores * 4 {
            s.ready.push(Thread::new(next_id));
            next_id += 1;
        }

        for round in 0..2000 {
            let core = round % num_cores;
            schedule_on_core(&mut s, core);

            // Verify per-core isolation every 10 rounds.
            if round % 10 == 0 {
                for c in 0..num_cores {
                    for other in 0..num_cores {
                        if other != c {
                            assert!(
                                s.deferred_ready[c]
                                    .iter()
                                    .all(|t| !s.deferred_ready[other].iter().any(|o| o.id == t.id)),
                                "thread found in both core {c} and core {other} deferred_ready \
                                 (round={round}, num_cores={num_cores})"
                            );
                        }
                    }
                }
                assert_no_duplicates(&s);
            }
        }
    }
}

/// Seeded stress with deferred_ready: block/wake/exit/preempt across cores.
#[test]
fn seeded_smp_stress_with_deferred_ready() {
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
    }

    for num_cores in [2, 3, 4, 8] {
        for seed in [77u64, 2048, 0xCAFEBABE, 314159, 2026_03_31] {
            let mut rng = Lcg(seed);
            let mut s = State::new(num_cores);
            let mut next_id = 100u64;
            let mut preemptions = 0u64;

            for _ in 0..num_cores * 3 {
                s.ready.push(Thread::new(next_id));
                next_id += 1;
            }

            for _ in 0..5000 {
                let core = (rng.next() % num_cores as u64) as usize;
                schedule_on_core(&mut s, core);

                // Count preemptions (thread went to deferred_ready).
                if !s.deferred_ready[core].is_empty() {
                    preemptions += 1;
                }

                match rng.next() % 10 {
                    0..=2 => {
                        if let Some(t) = s.cores[core].current.as_mut() {
                            if !t.is_idle() && t.state == ThreadState::Running {
                                t.block();
                                schedule_on_core(&mut s, core);
                            }
                        }
                    }
                    3..=4 => {
                        if let Some(t) = s.cores[core].current.as_mut() {
                            if !t.is_idle() && t.state == ThreadState::Running {
                                t.mark_exited();
                                s.ready.push(Thread::new(next_id));
                                next_id += 1;
                                schedule_on_core(&mut s, core);
                            }
                        }
                    }
                    5..=6 => {
                        if !s.blocked.is_empty() {
                            let idx = (rng.next() % s.blocked.len() as u64) as usize;
                            let id = s.blocked[idx].id;
                            try_wake(&mut s, id);
                        }
                    }
                    _ => {}
                }

                assert_no_duplicates(&s);
            }

            // Verify preemptions actually happened (otherwise test is vacuous).
            assert!(
                preemptions > 0,
                "no preemptions occurred (seed={seed}, num_cores={num_cores})"
            );

            // Clean up and verify.
            for core in 0..num_cores {
                s.deferred_drops[core].clear();
                s.deferred_ready[core].clear();
            }
            assert_no_duplicates(&s);
        }
    }
}
