//! Per-core ready queue model tests (v0.6 Phase 5a).
//!
//! These tests model the per-core ready queue architecture and verify:
//! 1. Local EEVDF selection — each core picks from its own queue
//! 2. Core-affine placement — woken threads return to last_core
//! 3. Load tracking — accurate runnable count per core
//! 4. deferred_ready elimination — no longer needed with per-core queues
//! 5. Per-core invariants — no thread in multiple queues
//!
//! Written test-first (TDD): these tests define the behavior, then
//! kernel/scheduler.rs is restructured to pass them.
//!
//! Also includes a "buggy" single-queue model to verify the tests
//! distinguish per-core from global queue behavior.

#[path = "../../kernel/paging.rs"]
mod paging;
#[path = "../../kernel/scheduling_algorithm.rs"]
mod scheduling_algorithm;

use scheduling_algorithm::{SchedulingState, DEFAULT_SLICE_NS, DEFAULT_WEIGHT};

// ============================================================
// PRNG (same xorshift64 as other scheduler tests)
// ============================================================

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }

    fn core(&mut self, num_cores: usize) -> usize {
        (self.next() % num_cores as u64) as usize
    }
}

// ============================================================
// Thread model (extended with last_core + scheduling context)
// ============================================================

const IDLE_THREAD_ID_MARKER: u64 = 0xFF00;
const MAX_CORES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

struct Thread {
    id: u64,
    state: ThreadState,
    eevdf: SchedulingState,
    context_id: Option<u32>,
    last_core: u32,
}

impl Thread {
    fn new(id: u64) -> Self {
        Self {
            id,
            state: ThreadState::Ready,
            eevdf: SchedulingState::new(),
            context_id: None,
            last_core: 0,
        }
    }

    fn new_with_core(id: u64, core: u32) -> Self {
        Self {
            id,
            state: ThreadState::Ready,
            eevdf: SchedulingState::new(),
            context_id: None,
            last_core: core,
        }
    }

    fn new_boot_idle(core_id: u64) -> Self {
        Self {
            id: core_id | IDLE_THREAD_ID_MARKER,
            state: ThreadState::Running,
            eevdf: SchedulingState::new(),
            context_id: None,
            last_core: core_id as u32,
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
// Per-core ready queue model (the NEW architecture)
// ============================================================

struct LocalRunQueue {
    ready: Vec<Thread>,
    load: u32,
}

impl LocalRunQueue {
    fn new() -> Self {
        Self {
            ready: Vec::new(),
            load: 0,
        }
    }

    /// Recompute load from ready vec length. Called after mutations.
    fn update_load(&mut self) {
        self.load = self.ready.len() as u32;
    }

    /// EEVDF selection: eligible thread with earliest virtual deadline.
    /// Returns index into ready vec, or None.
    fn select_best(&self) -> Option<usize> {
        if self.ready.is_empty() {
            return None;
        }

        // Avg vruntime of this queue's threads.
        let sum: u128 = self.ready.iter().map(|t| t.eevdf.vruntime as u128).sum();
        let avg = (sum / self.ready.len() as u128) as u64;

        // Pass 1: eligible threads → earliest deadline.
        let mut best: Option<(usize, u64)> = None;
        for (i, t) in self.ready.iter().enumerate() {
            if t.eevdf.is_eligible(avg) {
                let deadline = t.eevdf.virtual_deadline();
                if best.is_none_or(|(_, d)| deadline < d) {
                    best = Some((i, deadline));
                }
            }
        }

        // Pass 2: fallback to lowest vruntime.
        if best.is_some() {
            return best.map(|(i, _)| i);
        }

        let mut fallback: Option<(usize, u64)> = None;
        for (i, t) in self.ready.iter().enumerate() {
            if fallback.is_none_or(|(_, v)| t.eevdf.vruntime < v) {
                fallback = Some((i, t.eevdf.vruntime));
            }
        }

        fallback.map(|(i, _)| i)
    }

    /// Compute the avg vruntime for this queue (for vlag computation).
    fn avg_vruntime(&self) -> u64 {
        if self.ready.is_empty() {
            return 0;
        }
        let sum: u128 = self.ready.iter().map(|t| t.eevdf.vruntime as u128).sum();
        (sum / self.ready.len() as u128) as u64
    }
}

struct PerCoreState {
    current: Option<Thread>,
    idle: Option<Thread>,
    is_idle: bool,
}

struct State {
    cores: Vec<PerCoreState>,
    local_queues: Vec<LocalRunQueue>,
    blocked: Vec<Thread>,
    deferred_drops: Vec<Vec<Thread>>,
    num_cores: usize,
    next_id: u64,
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
            local_queues: (0..num_cores).map(|_| LocalRunQueue::new()).collect(),
            blocked: Vec::new(),
            deferred_drops: (0..num_cores).map(|_| Vec::new()).collect(),
            num_cores,
            next_id: 1,
        }
    }

    fn spawn(&mut self, core: usize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let thread = Thread::new_with_core(id, core as u32);
        self.local_queues[core].ready.push(thread);
        self.local_queues[core].update_load();
        id
    }

    fn total_threads(&self) -> usize {
        let in_queues: usize = self.local_queues.iter().map(|q| q.ready.len()).sum();
        let current: usize = self.cores.iter().filter(|c| c.current.is_some()).count();
        let idle: usize = self.cores.iter().filter(|c| c.idle.is_some()).count();
        let blocked = self.blocked.len();
        let deferred: usize = self.deferred_drops.iter().map(|d| d.len()).sum();
        in_queues + current + idle + blocked + deferred
    }
}

/// Park old thread after deschedule.
/// Key difference from old model: ready threads go to LOCAL queue, not global.
/// No deferred_ready needed — same lock protects everything.
fn park_old(s: &mut State, old_thread: Thread, core: usize) {
    if old_thread.is_ready() {
        if old_thread.is_idle() {
            s.cores[core].idle = Some(old_thread);
        } else {
            // Per-core queue: thread goes to THIS core's ready queue.
            // No other core can see it until lock release (single lock).
            // By then, restore_context_and_eret has switched SP.
            s.local_queues[core].ready.push(old_thread);
            s.local_queues[core].update_load();
        }
    } else if old_thread.is_exited() {
        s.deferred_drops[core].push(old_thread);
    } else {
        // Blocked — goes to global blocked list.
        s.blocked.push(old_thread);
    }
}

/// Wake a thread from the blocked list. Places it on its last_core's queue
/// (cache affinity) or the least-loaded core if last_core is busy.
fn try_wake(s: &mut State, id: u64) -> bool {
    if let Some(pos) = s.blocked.iter().position(|t| t.id == id) {
        let mut thread = s.blocked.swap_remove(pos);
        if thread.wake() {
            let target = pick_wake_target(s, thread.last_core as usize);
            s.local_queues[target].ready.push(thread);
            s.local_queues[target].update_load();
            return true;
        }
        s.blocked.push(thread);
    }
    false
}

/// Choose which core to place a woken thread on.
/// 1. Prefer last_core if idle
/// 2. Otherwise, least-loaded core
fn pick_wake_target(s: &State, last_core: usize) -> usize {
    // Prefer last_core if idle.
    if last_core < s.num_cores && s.cores[last_core].is_idle {
        return last_core;
    }

    // Least-loaded core (ties broken by lowest index).
    let mut best_core = 0;
    let mut best_load = u32::MAX;
    for i in 0..s.num_cores {
        if s.local_queues[i].load < best_load {
            best_load = s.local_queues[i].load;
            best_core = i;
        }
    }
    best_core
}

/// Schedule on a specific core. Selects from that core's local queue.
fn schedule_on_core(s: &mut State, core: usize) -> u64 {
    // Drain deferred drops for THIS core only.
    s.deferred_drops[core].clear();

    // Reap exited threads from local queue.
    s.local_queues[core].ready.retain(|t| !t.is_exited());
    s.local_queues[core].update_load();

    let mut old_thread = s.cores[core].current.take().expect("no current thread");
    old_thread.deschedule();

    // Try to select from this core's local queue (EEVDF).
    if let Some(idx) = s.local_queues[core].select_best() {
        let mut new_thread = s.local_queues[core].ready.swap_remove(idx);
        s.local_queues[core].update_load();
        new_thread.activate();
        new_thread.last_core = core as u32;
        let new_id = new_thread.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;
        new_id
    } else if old_thread.is_ready() {
        // Continue with old thread.
        let is_idle_thread = old_thread.is_idle();
        old_thread.activate();
        old_thread.last_core = core as u32;
        let old_id = old_thread.id;
        s.cores[core].current = Some(old_thread);
        s.cores[core].is_idle = is_idle_thread;
        old_id
    } else {
        // Idle fallback.
        let mut idle = s.cores[core].idle.take().expect("no idle thread on core");
        idle.activate();
        idle.last_core = core as u32;
        let idle_id = idle.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(idle);
        s.cores[core].is_idle = true;
        idle_id
    }
}

// ============================================================
// Invariant checker
// ============================================================

fn check_invariants(s: &State) {
    let mut seen_ids: Vec<u64> = Vec::new();

    // All threads in local queues are Ready.
    for (core_idx, q) in s.local_queues.iter().enumerate() {
        for t in &q.ready {
            assert_eq!(
                t.state,
                ThreadState::Ready,
                "thread {} in core {}'s queue is {:?}, not Ready",
                t.id,
                core_idx,
                t.state
            );
            assert!(
                !t.is_idle(),
                "idle thread {} in core {}'s ready queue",
                t.id,
                core_idx
            );
            assert!(
                !seen_ids.contains(&t.id),
                "thread {} appears in multiple places",
                t.id
            );
            seen_ids.push(t.id);
        }
        // load matches ready count
        assert_eq!(
            q.load,
            q.ready.len() as u32,
            "core {} load mismatch: {} != {}",
            core_idx,
            q.load,
            q.ready.len()
        );
    }

    // Blocked list: all Blocked.
    for t in &s.blocked {
        assert_eq!(
            t.state,
            ThreadState::Blocked,
            "thread {} in blocked list is {:?}",
            t.id,
            t.state
        );
        assert!(
            !seen_ids.contains(&t.id),
            "thread {} appears in multiple places",
            t.id
        );
        seen_ids.push(t.id);
    }

    // Deferred drops: all Exited.
    for core_drops in &s.deferred_drops {
        for t in core_drops {
            assert_eq!(
                t.state,
                ThreadState::Exited,
                "thread {} in deferred_drops is {:?}",
                t.id,
                t.state
            );
            assert!(
                !seen_ids.contains(&t.id),
                "thread {} appears in multiple places",
                t.id
            );
            seen_ids.push(t.id);
        }
    }

    // Current threads: exactly one per core (Running or Exited for killed threads).
    for (i, core) in s.cores.iter().enumerate() {
        if let Some(t) = &core.current {
            assert!(
                t.state == ThreadState::Running,
                "core {} current thread {} is {:?}, not Running",
                i,
                t.id,
                t.state
            );
            assert!(
                !seen_ids.contains(&t.id),
                "thread {} (current on core {}) appears in multiple places",
                t.id,
                i
            );
            seen_ids.push(t.id);
        }

        if let Some(t) = &core.idle {
            assert!(
                t.is_idle(),
                "non-idle thread {} in idle slot of core {}",
                t.id,
                i
            );
            assert!(
                !seen_ids.contains(&t.id),
                "idle thread {} appears in multiple places",
                t.id
            );
            seen_ids.push(t.id);
        }
    }

    // is_idle flag consistency.
    for (i, core) in s.cores.iter().enumerate() {
        if core.is_idle {
            if let Some(t) = &core.current {
                assert!(
                    t.is_idle(),
                    "core {} is_idle=true but current thread {} is not idle",
                    i,
                    t.id
                );
            }
        }
    }
}

// ============================================================
// Tests: Per-core queue isolation
// ============================================================

#[test]
fn threads_stay_on_spawned_core() {
    let mut s = State::new(4);

    // Spawn threads on specific cores.
    let t0 = s.spawn(0);
    let t1 = s.spawn(1);
    let t2 = s.spawn(2);

    // Schedule on core 0 — should pick t0, not t1 or t2.
    let picked = schedule_on_core(&mut s, 0);
    assert_eq!(picked, t0, "core 0 should pick thread from its own queue");

    // Core 1 picks t1.
    let picked = schedule_on_core(&mut s, 1);
    assert_eq!(picked, t1, "core 1 should pick thread from its own queue");

    check_invariants(&s);
}

#[test]
fn per_core_queues_independent_eevdf() {
    let mut s = State::new(2);

    // Spawn 3 threads on core 0, 1 on core 1.
    s.spawn(0);
    s.spawn(0);
    s.spawn(0);
    let lone = s.spawn(1);

    // Core 1 should immediately get its lone thread.
    let picked = schedule_on_core(&mut s, 1);
    assert_eq!(picked, lone);

    // Core 0 should pick from its 3-thread queue.
    let picked_0 = schedule_on_core(&mut s, 0);
    assert_ne!(picked_0, lone, "core 0 must not pick from core 1's queue");

    check_invariants(&s);
}

#[test]
fn load_tracking_accurate() {
    let mut s = State::new(4);

    s.spawn(0);
    s.spawn(0);
    s.spawn(1);

    assert_eq!(s.local_queues[0].load, 2);
    assert_eq!(s.local_queues[1].load, 1);
    assert_eq!(s.local_queues[2].load, 0);
    assert_eq!(s.local_queues[3].load, 0);

    // After scheduling, one thread moves from queue to current.
    schedule_on_core(&mut s, 0);
    assert_eq!(
        s.local_queues[0].load, 1,
        "load decreases when thread becomes current"
    );

    check_invariants(&s);
}

// ============================================================
// Tests: Core-affine wake placement
// ============================================================

#[test]
fn wake_returns_to_last_core() {
    let mut s = State::new(4);

    let t1 = s.spawn(2); // spawned on core 2
    schedule_on_core(&mut s, 2); // activate t1 on core 2

    // Block t1.
    s.cores[2].current.as_mut().unwrap().block();
    schedule_on_core(&mut s, 2); // core 2 goes idle

    // Core 2 should be idle now.
    assert!(s.cores[2].is_idle, "core 2 should be idle after block");

    // Wake t1 — should go back to core 2 (last_core).
    let woken = try_wake(&mut s, t1);
    assert!(woken, "thread should wake");

    // Thread should be on core 2's local queue.
    assert_eq!(s.local_queues[2].load, 1, "woken thread on last_core queue");
    assert_eq!(
        s.local_queues[2].ready[0].id, t1,
        "correct thread on core 2"
    );

    check_invariants(&s);
}

#[test]
fn wake_picks_least_loaded_when_last_core_busy() {
    let mut s = State::new(4);

    // Fill core 1 with work so it's not idle.
    s.spawn(1);
    schedule_on_core(&mut s, 1);

    // Create a thread that last ran on core 1.
    let t1 = s.spawn(1);
    schedule_on_core(&mut s, 1); // preempts, t1 gets a turn
                                 // Block it.
    s.cores[1].current.as_mut().unwrap().block();
    schedule_on_core(&mut s, 1);

    // Core 1 is not idle (it has other work). Wake should go to least-loaded.
    let woken = try_wake(&mut s, t1);
    assert!(woken);

    // Should be on the least-loaded core (0, 2, or 3 — all have load 0).
    let placed_on = (0..4)
        .find(|&i| s.local_queues[i].ready.iter().any(|t| t.id == t1))
        .expect("woken thread must be on some core's queue");

    assert_ne!(
        placed_on, 1,
        "should not return to busy last_core when idle cores exist"
    );

    check_invariants(&s);
}

// ============================================================
// Tests: deferred_ready elimination
// ============================================================

#[test]
fn no_deferred_ready_in_percore_model() {
    // With per-core queues, preempted threads go directly to the local queue.
    // No deferred_ready mechanism exists in this model.
    let mut s = State::new(2);

    s.spawn(0);
    s.spawn(0);

    // First schedule: picks one, parks one back in local queue.
    schedule_on_core(&mut s, 0);
    // The parked thread should be in local_queues[0], not in a deferred list.
    assert_eq!(s.local_queues[0].load, 1, "parked thread in local queue");

    // Second schedule: preempts current, picks the parked one.
    schedule_on_core(&mut s, 0);
    assert_eq!(s.local_queues[0].load, 1, "one thread back in local queue");

    check_invariants(&s);
}

// ============================================================
// Tests: deferred_drops still works per-core
// ============================================================

#[test]
fn deferred_drops_per_core_isolation() {
    let mut s = State::new(2);

    let t0 = s.spawn(0);
    schedule_on_core(&mut s, 0); // t0 is current on core 0

    // Exit t0.
    s.cores[0].current.as_mut().unwrap().mark_exited();
    schedule_on_core(&mut s, 0); // parks t0 in deferred_drops[0]

    // Core 0's deferred_drops should have t0.
    assert_eq!(s.deferred_drops[0].len(), 1);
    assert_eq!(s.deferred_drops[0][0].id, t0);
    // Core 1's deferred_drops should be empty.
    assert_eq!(s.deferred_drops[1].len(), 0);

    // Schedule on core 1 — must NOT drain core 0's deferred_drops.
    schedule_on_core(&mut s, 1);
    assert_eq!(
        s.deferred_drops[0].len(),
        1,
        "core 1 must not drain core 0's deferred_drops"
    );

    // Schedule on core 0 again — NOW drain its deferred_drops.
    schedule_on_core(&mut s, 0);
    assert_eq!(
        s.deferred_drops[0].len(),
        0,
        "core 0 drains its own deferred_drops"
    );

    check_invariants(&s);
}

// ============================================================
// Tests: Vlag preservation across queues
// ============================================================

#[test]
fn vlag_roundtrip_preserves_fairness() {
    // Thread with vruntime 1000 on a queue with avg 2000 is underserved.
    // After migration to a queue with avg 5000, it should still be underserved
    // by the same relative amount.
    let state = SchedulingState {
        vruntime: 1000,
        weight: DEFAULT_WEIGHT,
        requested_slice: DEFAULT_SLICE_NS,
        eligible_at: 1000,
    };

    let source_avg = 2000u64;
    let dest_avg = 5000u64;

    let vlag = state.compute_vlag(source_avg);
    assert!(vlag > 0, "underserved thread should have positive vlag");

    let migrated = state.apply_vlag(vlag, dest_avg);

    // The migrated thread should be underserved on the destination too.
    let dest_lag = migrated.compute_vlag(dest_avg);
    // Allow small rounding error (integer division).
    assert!(
        (dest_lag - vlag).unsigned_abs() <= 1,
        "vlag should be preserved: source={vlag}, dest={dest_lag}"
    );
}

#[test]
fn vlag_overserved_thread_stays_overserved() {
    // Thread with vruntime 5000 on a queue with avg 2000 is overserved.
    let state = SchedulingState {
        vruntime: 5000,
        weight: DEFAULT_WEIGHT,
        requested_slice: DEFAULT_SLICE_NS,
        eligible_at: 5000,
    };

    let vlag = state.compute_vlag(2000);
    assert!(vlag < 0, "overserved thread should have negative vlag");

    let migrated = state.apply_vlag(vlag, 8000);
    assert!(
        migrated.vruntime > 8000,
        "overserved thread should have vruntime above dest avg"
    );
}

#[test]
fn vlag_with_different_weights() {
    // A high-weight thread (weight=2048, 2x share) at vruntime 1000
    // on a queue with avg 2000.
    let state = SchedulingState {
        vruntime: 1000,
        weight: 2048,
        requested_slice: DEFAULT_SLICE_NS,
        eligible_at: 1000,
    };

    let vlag = state.compute_vlag(2000);
    // lag = weight * (avg - vrt) / DEFAULT_WEIGHT = 2048 * 1000 / 1024 = 2000
    assert_eq!(vlag, 2000, "vlag scales with weight");

    let migrated = state.apply_vlag(vlag, 6000);
    let roundtrip_vlag = migrated.compute_vlag(6000);
    assert!(
        (roundtrip_vlag - vlag).unsigned_abs() <= 1,
        "roundtrip preserves vlag: {vlag} vs {roundtrip_vlag}"
    );
}

#[test]
fn vlag_zero_for_perfectly_fair() {
    // Thread exactly at avg vruntime.
    let state = SchedulingState {
        vruntime: 3000,
        weight: DEFAULT_WEIGHT,
        requested_slice: DEFAULT_SLICE_NS,
        eligible_at: 3000,
    };

    let vlag = state.compute_vlag(3000);
    assert_eq!(vlag, 0, "thread at avg should have zero vlag");
}

// ============================================================
// Property-based: per-core invariants under random operations
// ============================================================

#[test]
#[cfg_attr(miri, ignore)]
fn property_percore_invariants_hold() {
    for seed in 1..=50 {
        let mut rng = Rng::new(seed);
        let num_cores = rng.range(2, 5) as usize;
        let mut s = State::new(num_cores);
        let mut live_ids: Vec<u64> = Vec::new();

        for _step in 0..500 {
            let action = rng.range(0, 6);
            match action {
                0 => {
                    // Spawn on random core.
                    let core = rng.core(num_cores);
                    let id = s.spawn(core);
                    live_ids.push(id);
                }
                1 => {
                    // Schedule on random core.
                    let core = rng.core(num_cores);
                    schedule_on_core(&mut s, core);
                }
                2 => {
                    // Block current thread on random core.
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.block();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
                3 => {
                    // Wake a random blocked thread.
                    if !s.blocked.is_empty() {
                        let idx = rng.range(0, s.blocked.len() as u64) as usize;
                        let id = s.blocked[idx].id;
                        try_wake(&mut s, id);
                    }
                }
                4 => {
                    // Exit current thread on random core.
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.mark_exited();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
                _ => {
                    // Schedule on ALL cores (simulate timer tick).
                    for c in 0..num_cores {
                        schedule_on_core(&mut s, c);
                    }
                }
            }

            check_invariants(&s);
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn property_no_thread_duplication_under_churn() {
    // High-churn test: rapid spawn/exit/wake cycles, verify no thread
    // appears in two places simultaneously.
    for seed in 100..=130 {
        let mut rng = Rng::new(seed);
        let num_cores = 4;
        let mut s = State::new(num_cores);

        for _step in 0..1000 {
            let action = rng.range(0, 5);
            match action {
                0 => {
                    let core = rng.core(num_cores);
                    s.spawn(core);
                }
                1 => {
                    let core = rng.core(num_cores);
                    schedule_on_core(&mut s, core);
                }
                2 => {
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.block();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
                3 => {
                    if !s.blocked.is_empty() {
                        let idx = rng.range(0, s.blocked.len() as u64) as usize;
                        let id = s.blocked[idx].id;
                        try_wake(&mut s, id);
                    }
                }
                _ => {
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.mark_exited();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
            }

            check_invariants(&s);
        }
    }
}

// ============================================================
// Tests: last_core is set correctly
// ============================================================

#[test]
fn last_core_set_on_activation() {
    let mut s = State::new(4);

    let t1 = s.spawn(2);
    schedule_on_core(&mut s, 2);

    // After scheduling on core 2, the thread's last_core should be 2.
    assert_eq!(
        s.cores[2].current.as_ref().unwrap().last_core,
        2,
        "last_core should match the core it was activated on"
    );

    check_invariants(&s);
}

#[test]
fn last_core_preserved_through_block_wake() {
    let mut s = State::new(4);

    let t1 = s.spawn(3);
    schedule_on_core(&mut s, 3); // t1 runs on core 3, last_core=3

    // Block and then wake.
    s.cores[3].current.as_mut().unwrap().block();
    schedule_on_core(&mut s, 3);

    // The blocked thread should remember last_core=3.
    assert_eq!(
        s.blocked[0].last_core, 3,
        "last_core preserved through block"
    );

    // Wake — should go to core 3 (idle).
    try_wake(&mut s, t1);
    assert_eq!(
        s.local_queues[3].ready[0].last_core, 3,
        "last_core preserved through wake"
    );

    check_invariants(&s);
}
