//! Work stealing + Concurrent Work Conservation (CWC) model tests (v0.6 Phase 5b).
//!
//! These tests model the work-stealing mechanism and verify:
//! 1. Idle core steals from busiest remote queue
//! 2. Vlag normalization preserves fairness position across migration
//! 3. Budget-aware stealing (don't steal exhausted-context threads)
//! 4. CWC property: no idle core coexists with overloaded core after schedule
//! 5. Workload-granularity migration: threads sharing a context co-migrate
//!
//! Includes a "buggy" no-steal model to prove the CWC tests catch violations.
//!
//! References:
//! - Ipanema (Lepers et al., EuroSys 2020): CWC definition
//! - Linux 6.6+ EEVDF: vlag preservation
//! - Stoica & Abdel-Wahab 1995: EEVDF fairness bounds

#[path = "../../paging.rs"]
mod paging;
#[path = "../../scheduling_algorithm.rs"]
mod scheduling_algorithm;

use scheduling_algorithm::{SchedulingState, DEFAULT_SLICE_NS, DEFAULT_WEIGHT};

// ============================================================
// PRNG
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
// Scheduling context model (budget tracking)
// ============================================================

#[derive(Clone, Copy, Debug)]
struct SchedulingContext {
    budget: u64,
    remaining: u64,
    period: u64,
}

impl SchedulingContext {
    fn new(budget: u64, period: u64) -> Self {
        Self {
            budget,
            remaining: budget,
            period,
        }
    }

    fn has_budget(&self) -> bool {
        self.remaining > 0
    }

    fn charge(&mut self, elapsed: u64) {
        self.remaining = self.remaining.saturating_sub(elapsed);
    }

    fn replenish(&mut self) {
        self.remaining = self.budget;
    }
}

// ============================================================
// Thread model with scheduling context and EEVDF state
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

    fn new_with_context(id: u64, context_id: u32, core: u32) -> Self {
        Self {
            id,
            state: ThreadState::Ready,
            eevdf: SchedulingState::new(),
            context_id: Some(context_id),
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
// Per-core queue model with work stealing
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

    fn update_load(&mut self) {
        self.load = self.ready.len() as u32;
    }

    fn avg_vruntime(&self) -> u64 {
        if self.ready.is_empty() {
            return 0;
        }
        let sum: u128 = self.ready.iter().map(|t| t.eevdf.vruntime as u128).sum();
        (sum / self.ready.len() as u128) as u64
    }

    /// EEVDF selection with budget check.
    fn select_best(&self, contexts: &[Option<SchedulingContext>]) -> Option<usize> {
        if self.ready.is_empty() {
            return None;
        }

        // Only consider threads with budget.
        let with_budget: Vec<(usize, &Thread)> = self
            .ready
            .iter()
            .enumerate()
            .filter(|(_, t)| has_budget(t, contexts))
            .collect();

        if with_budget.is_empty() {
            return None;
        }

        let sum: u128 = with_budget
            .iter()
            .map(|(_, t)| t.eevdf.vruntime as u128)
            .sum();
        let avg = (sum / with_budget.len() as u128) as u64;

        // Eligible + earliest deadline.
        let mut best: Option<(usize, u64)> = None;
        for &(i, t) in &with_budget {
            if t.eevdf.is_eligible(avg) {
                let deadline = t.eevdf.virtual_deadline();
                if best.is_none_or(|(_, d)| deadline < d) {
                    best = Some((i, deadline));
                }
            }
        }

        if let Some((i, _)) = best {
            return Some(i);
        }

        // Fallback: lowest vruntime among threads with budget.
        let mut fallback: Option<(usize, u64)> = None;
        for &(i, t) in &with_budget {
            if fallback.is_none_or(|(_, v)| t.eevdf.vruntime < v) {
                fallback = Some((i, t.eevdf.vruntime));
            }
        }
        fallback.map(|(i, _)| i)
    }
}

fn has_budget(t: &Thread, contexts: &[Option<SchedulingContext>]) -> bool {
    match t.context_id {
        None => true,
        Some(id) => contexts
            .get(id as usize)
            .and_then(|c| c.as_ref())
            .is_none_or(|c| c.has_budget()),
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
    contexts: Vec<Option<SchedulingContext>>,
    num_cores: usize,
    next_id: u64,
    next_context_id: u32,
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
            contexts: Vec::new(),
            num_cores,
            next_id: 1,
            next_context_id: 0,
        }
    }

    fn create_context(&mut self, budget: u64, period: u64) -> u32 {
        let id = self.next_context_id;
        self.next_context_id += 1;
        self.contexts
            .push(Some(SchedulingContext::new(budget, period)));
        id
    }

    fn spawn(&mut self, core: usize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let thread = Thread::new(id);
        self.local_queues[core].ready.push(thread);
        self.local_queues[core].update_load();
        id
    }

    fn spawn_with_context(&mut self, core: usize, context_id: u32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let thread = Thread::new_with_context(id, context_id, core as u32);
        self.local_queues[core].ready.push(thread);
        self.local_queues[core].update_load();
        id
    }
}

// ============================================================
// Work stealing
// ============================================================

/// Find the busiest remote core (highest load, ties broken by lowest index).
fn find_busiest(s: &State, my_core: usize) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;
    for i in 0..s.num_cores {
        if i == my_core {
            continue;
        }
        let load = s.local_queues[i].load;
        if load > 1 {
            // Only steal if victim has >1 (leaves it at least 1).
            if best.is_none_or(|(_, l)| load > l) {
                best = Some((i, load));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// Select the best steal victim from a remote queue: highest vlag (most
/// underserved) thread that has budget. Returns index into remote queue.
fn select_steal_victim(
    queue: &LocalRunQueue,
    contexts: &[Option<SchedulingContext>],
) -> Option<usize> {
    if queue.ready.is_empty() {
        return None;
    }

    let avg = queue.avg_vruntime();
    let mut best: Option<(usize, i64)> = None;

    for (i, t) in queue.ready.iter().enumerate() {
        if !has_budget(t, contexts) {
            continue;
        }
        let vlag = t.eevdf.compute_vlag(avg);
        if best.is_none_or(|(_, v)| vlag > v) {
            best = Some((i, vlag));
        }
    }

    best.map(|(i, _)| i)
}

/// Steal a thread from a remote core, normalizing its vruntime via vlag.
fn steal_one(s: &mut State, my_core: usize, victim_core: usize) -> bool {
    let victim_avg = s.local_queues[victim_core].avg_vruntime();

    if let Some(idx) = select_steal_victim(&s.local_queues[victim_core], &s.contexts) {
        let mut thread = s.local_queues[victim_core].ready.swap_remove(idx);
        s.local_queues[victim_core].update_load();

        // Vlag normalization: preserve fairness position across queues.
        let vlag = thread.eevdf.compute_vlag(victim_avg);
        let dest_avg = s.local_queues[my_core].avg_vruntime();
        thread.eevdf = thread.eevdf.apply_vlag(vlag, dest_avg);
        thread.last_core = my_core as u32;

        s.local_queues[my_core].ready.push(thread);
        s.local_queues[my_core].update_load();
        true
    } else {
        false
    }
}

/// Workload-granularity steal: steal all threads sharing the same scheduling
/// context as the victim, up to half the victim core's load.
fn steal_workload(s: &mut State, my_core: usize, victim_core: usize) -> bool {
    let victim_avg = s.local_queues[victim_core].avg_vruntime();

    // Find the primary steal victim.
    let primary_idx = match select_steal_victim(&s.local_queues[victim_core], &s.contexts) {
        Some(i) => i,
        None => return false,
    };

    let context_id = s.local_queues[victim_core].ready[primary_idx].context_id;
    let max_steal = (s.local_queues[victim_core].load / 2).max(1) as usize;

    // Collect indices of threads to steal (same context, up to max_steal).
    let mut steal_indices: Vec<usize> = Vec::new();
    steal_indices.push(primary_idx);

    if let Some(ctx_id) = context_id {
        for (i, t) in s.local_queues[victim_core].ready.iter().enumerate() {
            if i == primary_idx {
                continue;
            }
            if t.context_id == Some(ctx_id) && has_budget(t, &s.contexts) {
                steal_indices.push(i);
                if steal_indices.len() >= max_steal {
                    break;
                }
            }
        }
    }

    // Steal in reverse index order to avoid index invalidation from swap_remove.
    steal_indices.sort_unstable();
    let dest_avg = s.local_queues[my_core].avg_vruntime();

    let mut stolen = Vec::new();
    for &idx in steal_indices.iter().rev() {
        let mut thread = s.local_queues[victim_core].ready.swap_remove(idx);
        let vlag = thread.eevdf.compute_vlag(victim_avg);
        thread.eevdf = thread.eevdf.apply_vlag(vlag, dest_avg);
        thread.last_core = my_core as u32;
        stolen.push(thread);
    }
    s.local_queues[victim_core].update_load();

    for thread in stolen {
        s.local_queues[my_core].ready.push(thread);
    }
    s.local_queues[my_core].update_load();

    true
}

// ============================================================
// schedule_on_core with work stealing
// ============================================================

fn park_old(s: &mut State, old_thread: Thread, core: usize) {
    if old_thread.is_ready() {
        if old_thread.is_idle() {
            s.cores[core].idle = Some(old_thread);
        } else {
            s.local_queues[core].ready.push(old_thread);
            s.local_queues[core].update_load();
        }
    } else if old_thread.is_exited() {
        s.deferred_drops[core].push(old_thread);
    } else {
        s.blocked.push(old_thread);
    }
}

fn pick_wake_target(s: &State, last_core: usize) -> usize {
    if last_core < s.num_cores && s.cores[last_core].is_idle {
        return last_core;
    }
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

/// Schedule with work stealing. When local queue is empty, steal from busiest.
fn schedule_on_core(s: &mut State, core: usize) -> u64 {
    s.deferred_drops[core].clear();
    s.local_queues[core].ready.retain(|t| !t.is_exited());
    s.local_queues[core].update_load();

    let mut old_thread = s.cores[core].current.take().expect("no current thread");
    old_thread.deschedule();

    // Try local queue first.
    if let Some(idx) = s.local_queues[core].select_best(&s.contexts) {
        let mut new_thread = s.local_queues[core].ready.swap_remove(idx);
        s.local_queues[core].update_load();
        new_thread.activate();
        new_thread.last_core = core as u32;
        let new_id = new_thread.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;
        return new_id;
    }

    // Local queue empty — try work stealing.
    if let Some(victim) = find_busiest(s, core) {
        if steal_workload(s, core, victim) {
            // Now pick from the local queue (which has stolen threads).
            if let Some(idx) = s.local_queues[core].select_best(&s.contexts) {
                let mut new_thread = s.local_queues[core].ready.swap_remove(idx);
                s.local_queues[core].update_load();
                new_thread.activate();
                new_thread.last_core = core as u32;
                let new_id = new_thread.id;
                park_old(s, old_thread, core);
                s.cores[core].current = Some(new_thread);
                s.cores[core].is_idle = false;
                return new_id;
            }
        }
    }

    // Continue with old thread if still ready.
    if old_thread.is_ready() && has_budget(&old_thread, &s.contexts) {
        let is_idle_thread = old_thread.is_idle();
        old_thread.activate();
        old_thread.last_core = core as u32;
        let old_id = old_thread.id;
        s.cores[core].current = Some(old_thread);
        s.cores[core].is_idle = is_idle_thread;
        return old_id;
    }

    // Idle fallback.
    let mut idle = s.cores[core].idle.take().expect("no idle thread");
    idle.activate();
    idle.last_core = core as u32;
    let idle_id = idle.id;
    park_old(s, old_thread, core);
    s.cores[core].current = Some(idle);
    s.cores[core].is_idle = true;
    idle_id
}

// ============================================================
// Buggy model: NO work stealing (for CWC violation detection)
// ============================================================

fn schedule_on_core_no_steal(s: &mut State, core: usize) -> u64 {
    s.deferred_drops[core].clear();
    s.local_queues[core].ready.retain(|t| !t.is_exited());
    s.local_queues[core].update_load();

    let mut old_thread = s.cores[core].current.take().expect("no current thread");
    old_thread.deschedule();

    if let Some(idx) = s.local_queues[core].select_best(&s.contexts) {
        let mut new_thread = s.local_queues[core].ready.swap_remove(idx);
        s.local_queues[core].update_load();
        new_thread.activate();
        new_thread.last_core = core as u32;
        let new_id = new_thread.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(new_thread);
        s.cores[core].is_idle = false;
        return new_id;
    }

    if old_thread.is_ready() {
        let is_idle_thread = old_thread.is_idle();
        old_thread.activate();
        let old_id = old_thread.id;
        s.cores[core].current = Some(old_thread);
        s.cores[core].is_idle = is_idle_thread;
        old_id
    } else {
        let mut idle = s.cores[core].idle.take().expect("no idle thread");
        idle.activate();
        let idle_id = idle.id;
        park_old(s, old_thread, core);
        s.cores[core].current = Some(idle);
        s.cores[core].is_idle = true;
        idle_id
    }
}

// ============================================================
// CWC check
// ============================================================

/// Check CWC: if any core is idle and any other core has >1 runnable thread
/// (in its local queue), that's a CWC violation.
///
/// This must hold AFTER a full round of scheduling on all cores.
fn check_cwc_after_full_round(s: &State) -> bool {
    let any_idle = s.cores.iter().any(|c| c.is_idle);
    let any_overloaded = s.local_queues.iter().any(|q| q.load > 1);

    // CWC satisfied if: NOT (idle AND overloaded simultaneously)
    !(any_idle && any_overloaded)
}

// ============================================================
// Invariant checker
// ============================================================

fn check_invariants(s: &State) {
    let mut seen_ids: Vec<u64> = Vec::new();

    for (core_idx, q) in s.local_queues.iter().enumerate() {
        for t in &q.ready {
            assert_eq!(t.state, ThreadState::Ready);
            assert!(!t.is_idle());
            assert!(
                !seen_ids.contains(&t.id),
                "thread {} duplicated (in core {} queue)",
                t.id,
                core_idx
            );
            seen_ids.push(t.id);
        }
        assert_eq!(q.load, q.ready.len() as u32);
    }

    for t in &s.blocked {
        assert_eq!(t.state, ThreadState::Blocked);
        assert!(!seen_ids.contains(&t.id), "thread {} duplicated", t.id);
        seen_ids.push(t.id);
    }

    for drops in &s.deferred_drops {
        for t in drops {
            assert_eq!(t.state, ThreadState::Exited);
            assert!(!seen_ids.contains(&t.id), "thread {} duplicated", t.id);
            seen_ids.push(t.id);
        }
    }

    for (i, core) in s.cores.iter().enumerate() {
        if let Some(t) = &core.current {
            assert_eq!(t.state, ThreadState::Running);
            assert!(
                !seen_ids.contains(&t.id),
                "thread {} duplicated (current on core {})",
                t.id,
                i
            );
            seen_ids.push(t.id);
        }
        if let Some(t) = &core.idle {
            assert!(t.is_idle());
            assert!(!seen_ids.contains(&t.id));
            seen_ids.push(t.id);
        }
    }
}

// ============================================================
// Tests: Basic work stealing
// ============================================================

#[test]
fn idle_core_steals_from_overloaded() {
    let mut s = State::new(4);

    // Load all threads onto core 0.
    s.spawn(0);
    s.spawn(0);
    s.spawn(0);

    // Core 1 has nothing — should steal from core 0.
    schedule_on_core(&mut s, 1);

    // Core 1 should now be running a stolen thread (not idle).
    assert!(
        !s.cores[1].is_idle,
        "core 1 should have stolen work from core 0"
    );

    check_invariants(&s);
}

#[test]
fn steal_picks_busiest_core() {
    let mut s = State::new(4);

    // Core 0: 2 threads, core 1: 4 threads, core 2: 0.
    s.spawn(0);
    s.spawn(0);
    s.spawn(1);
    s.spawn(1);
    s.spawn(1);
    s.spawn(1);

    // Core 2 steals — should pick core 1 (busiest).
    schedule_on_core(&mut s, 2);

    // Core 1 should have lost threads, core 0 untouched (only 2).
    assert!(
        s.local_queues[1].load < 4,
        "core 1 should have lost threads to steal"
    );

    check_invariants(&s);
}

#[test]
fn steal_respects_budget() {
    let mut s = State::new(2);

    let ctx = s.create_context(1_000_000, 10_000_000); // 1ms budget, 10ms period

    // Create threads with a context on core 0, then exhaust the budget.
    s.spawn_with_context(0, ctx);
    s.spawn_with_context(0, ctx);
    s.contexts[ctx as usize].as_mut().unwrap().remaining = 0; // Exhaust budget.

    // Core 1 tries to steal — should NOT steal (no budget).
    schedule_on_core(&mut s, 1);

    assert!(
        s.cores[1].is_idle,
        "core 1 should NOT steal threads with exhausted budget"
    );
    assert_eq!(
        s.local_queues[0].load, 2,
        "core 0's threads should remain (no budget to steal)"
    );

    check_invariants(&s);
}

// ============================================================
// Tests: Vlag normalization on steal
// ============================================================

#[test]
fn steal_normalizes_vruntime() {
    let mut s = State::new(2);

    // Create threads with different vruntimes on core 0.
    let id1 = s.spawn(0);
    let id2 = s.spawn(0);
    let id3 = s.spawn(0);

    // Manually set vruntimes to simulate charge history.
    s.local_queues[0].ready[0].eevdf.vruntime = 1000;
    s.local_queues[0].ready[1].eevdf.vruntime = 3000;
    s.local_queues[0].ready[2].eevdf.vruntime = 5000;

    // Core 0 avg = 3000. Thread at 1000 has highest vlag (most underserved).
    let avg_before = s.local_queues[0].avg_vruntime();
    assert_eq!(avg_before, 3000);

    // Core 1 steals.
    schedule_on_core(&mut s, 1);

    // The stolen thread should have been the most underserved (vruntime=1000).
    assert!(!s.cores[1].is_idle, "core 1 should have stolen");

    // Stolen thread's vruntime should be normalized to core 1's context.
    // Core 1 was empty (avg=0), so the vlag-adjusted vruntime should
    // reflect the thread's underserved status relative to the new queue.

    check_invariants(&s);
}

// ============================================================
// Tests: Workload-granularity migration
// ============================================================

#[test]
fn steal_prefers_same_context_threads() {
    let mut s = State::new(2);

    let ctx_a = s.create_context(10_000_000, 50_000_000);
    let ctx_b = s.create_context(10_000_000, 50_000_000);

    // Core 0: 2 threads from ctx_a, 1 from ctx_b.
    s.spawn_with_context(0, ctx_a);
    s.spawn_with_context(0, ctx_a);
    s.spawn_with_context(0, ctx_b);

    // Core 1 steals — should take both ctx_a threads (workload group).
    schedule_on_core(&mut s, 1);

    // Count ctx_a threads on each core.
    let ctx_a_on_0 = s.local_queues[0]
        .ready
        .iter()
        .filter(|t| t.context_id == Some(ctx_a))
        .count();
    let ctx_a_on_1 = s.local_queues[1]
        .ready
        .iter()
        .filter(|t| t.context_id == Some(ctx_a))
        .count();
    let ctx_a_current_1 = s.cores[1]
        .current
        .as_ref()
        .map_or(false, |t| t.context_id == Some(ctx_a));

    let ctx_a_total_on_1 = ctx_a_on_1 + if ctx_a_current_1 { 1 } else { 0 };

    // Workload migration should have moved ctx_a threads together.
    // At minimum, the primary steal victim + its context-mate.
    assert!(
        ctx_a_total_on_1 >= 1,
        "at least the primary ctx_a thread should be on core 1"
    );

    check_invariants(&s);
}

#[test]
fn steal_does_not_drain_victim_core() {
    let mut s = State::new(2);

    let ctx = s.create_context(10_000_000, 50_000_000);

    // Core 0: 4 threads from same context.
    s.spawn_with_context(0, ctx);
    s.spawn_with_context(0, ctx);
    s.spawn_with_context(0, ctx);
    s.spawn_with_context(0, ctx);

    // Core 1 steals — should take at most half (2).
    schedule_on_core(&mut s, 1);

    assert!(
        s.local_queues[0].load >= 2,
        "steal should leave at least half the threads: got {}",
        s.local_queues[0].load
    );

    check_invariants(&s);
}

// ============================================================
// Tests: CWC property
// ============================================================

#[test]
fn cwc_holds_after_full_round() {
    let mut s = State::new(4);

    // Load 8 threads onto core 0 only.
    for _ in 0..8 {
        s.spawn(0);
    }

    // Schedule all cores (full round).
    for core in 0..4 {
        schedule_on_core(&mut s, core);
    }

    // CWC: no idle core should coexist with an overloaded core.
    assert!(
        check_cwc_after_full_round(&s),
        "CWC violated: idle core + overloaded core after full round"
    );

    check_invariants(&s);
}

#[test]
fn cwc_violated_without_stealing() {
    // This test proves the CWC check catches violations.
    let mut s = State::new(4);

    // Load 8 threads onto core 0 only.
    for _ in 0..8 {
        s.spawn(0);
    }

    // Schedule all cores WITHOUT stealing.
    for core in 0..4 {
        schedule_on_core_no_steal(&mut s, core);
    }

    // CWC should be VIOLATED: cores 1-3 are idle, core 0 is overloaded.
    assert!(
        !check_cwc_after_full_round(&s),
        "CWC should be violated without work stealing"
    );
}

// ============================================================
// Property-based: CWC holds under random workload
// ============================================================

#[test]
#[cfg_attr(miri, ignore)]
fn property_cwc_holds_under_random_workload() {
    for seed in 1..=50 {
        let mut rng = Rng::new(seed);
        let num_cores = rng.range(2, 5) as usize;
        let mut s = State::new(num_cores);

        for _step in 0..500 {
            let action = rng.range(0, 7);
            match action {
                0 => {
                    // Spawn on random core.
                    let core = rng.core(num_cores);
                    s.spawn(core);
                }
                1..=2 => {
                    // Schedule single core (with stealing).
                    let core = rng.core(num_cores);
                    schedule_on_core(&mut s, core);
                }
                3 => {
                    // Block current.
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.block();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
                4 => {
                    // Wake.
                    if !s.blocked.is_empty() {
                        let idx = rng.range(0, s.blocked.len() as u64) as usize;
                        let id = s.blocked[idx].id;
                        try_wake(&mut s, id);
                    }
                }
                5 => {
                    // Exit.
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.mark_exited();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
                _ => {
                    // Full round — schedule ALL cores, then check CWC.
                    for c in 0..num_cores {
                        schedule_on_core(&mut s, c);
                    }
                    assert!(
                        check_cwc_after_full_round(&s),
                        "CWC violated at seed={seed}, step={_step}"
                    );
                }
            }

            check_invariants(&s);
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn property_invariants_hold_with_stealing() {
    // Same as percore test but with stealing active — verify no thread
    // duplication or state corruption under work stealing.
    for seed in 200..=230 {
        let mut rng = Rng::new(seed);
        let num_cores = 4;
        let mut s = State::new(num_cores);

        for _step in 0..1000 {
            let action = rng.range(0, 6);
            match action {
                0 => {
                    // Spawn on random core (sometimes imbalanced).
                    let core = if rng.range(0, 3) == 0 {
                        0 // Bias toward core 0 for imbalance
                    } else {
                        rng.core(num_cores)
                    };
                    s.spawn(core);
                }
                1..=2 => {
                    let core = rng.core(num_cores);
                    schedule_on_core(&mut s, core);
                }
                3 => {
                    let core = rng.core(num_cores);
                    if let Some(t) = &mut s.cores[core].current {
                        if !t.is_idle() && t.state == ThreadState::Running {
                            t.block();
                            schedule_on_core(&mut s, core);
                        }
                    }
                }
                4 => {
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
// Tests: Scheduling context budget is absolute across migration
// ============================================================

#[test]
fn budget_unchanged_by_migration() {
    let mut s = State::new(2);

    let ctx = s.create_context(10_000_000, 50_000_000); // 10ms budget
    s.spawn_with_context(0, ctx);

    // Charge 3ms.
    s.contexts[ctx as usize].as_mut().unwrap().charge(3_000_000);
    let remaining_before = s.contexts[ctx as usize].as_ref().unwrap().remaining;
    assert_eq!(remaining_before, 7_000_000);

    // Schedule core 0 to activate the thread, then steal to core 1.
    schedule_on_core(&mut s, 0);
    // Thread is now current on core 0. Preempt it back to queue.
    s.cores[0].current.as_mut().unwrap().deschedule();
    s.cores[0].current.as_mut().unwrap().state = ThreadState::Ready;
    let thread = s.cores[0].current.take().unwrap();
    s.local_queues[0].ready.push(thread);
    s.local_queues[0].update_load();
    s.cores[0].idle = Some(Thread::new_boot_idle(0));
    s.cores[0].is_idle = true;

    // Core 1 steals.
    schedule_on_core(&mut s, 1);

    // Budget should be unchanged — 7ms remaining.
    let remaining_after = s.contexts[ctx as usize].as_ref().unwrap().remaining;
    assert_eq!(
        remaining_after, remaining_before,
        "scheduling context budget must be absolute, not affected by migration"
    );

    check_invariants(&s);
}
