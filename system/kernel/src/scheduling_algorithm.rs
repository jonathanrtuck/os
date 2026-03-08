//! Pure EEVDF (Earliest Eligible Virtual Deadline First) algorithm.
//!
//! Stateless selection logic — operates on slices of per-thread state. No
//! allocation, no locking, no hardware access. Fully host-testable.
//!
//! # Algorithm
//!
//! Each thread has a virtual runtime (vruntime) that tracks weighted CPU
//! consumption. Eligibility: a thread is eligible when its vruntime ≤
//! avg_vruntime (lag ≥ 0, meaning it hasn't overconsumed). Among eligible
//! threads with remaining budget, pick the one with the earliest virtual
//! deadline (= eligible_at + slice * DEFAULT_WEIGHT / weight).
//!
//! Shorter requested slices yield earlier deadlines (lower latency) without
//! increasing total CPU share. Higher weights slow vruntime growth (more
//! share). No heuristics.
//!
//! See: Stoica & Abdel-Wahab 1995, Linux kernel 6.6+ EEVDF.

/// Default requested time slice: 4 ms.
pub const DEFAULT_SLICE_NS: u64 = 4_000_000;
/// Default weight (analogous to Linux nice 0 = 1024).
pub const DEFAULT_WEIGHT: u32 = 1024;

/// Per-thread EEVDF scheduling state.
#[derive(Clone, Copy, Debug)]
pub struct State {
    /// Accumulated weighted CPU time (ns * DEFAULT_WEIGHT / weight).
    pub vruntime: u64,
    /// Thread weight. Higher = more CPU share (vruntime grows slower).
    pub weight: u32,
    /// Requested time slice in ns. Shorter = earlier deadline = lower latency.
    pub requested_slice: u64,
    /// vruntime at which this thread became eligible (used for deadline calc).
    pub eligible_at: u64,
}

impl State {
    pub const fn new() -> Self {
        Self {
            vruntime: 0,
            weight: DEFAULT_WEIGHT,
            requested_slice: DEFAULT_SLICE_NS,
            eligible_at: 0,
        }
    }

    /// Charge `elapsed_ns` of CPU time, returning updated state.
    /// vruntime grows inversely with weight: elapsed * DEFAULT_WEIGHT / weight.
    pub fn charge(&self, elapsed_ns: u64) -> Self {
        let delta = elapsed_ns * DEFAULT_WEIGHT as u64 / self.weight as u64;

        Self {
            vruntime: self.vruntime.saturating_add(delta),
            eligible_at: self.eligible_at,
            ..*self
        }
    }
    /// Is this thread eligible to run? Eligible when vruntime ≤ avg (lag ≥ 0).
    pub fn is_eligible(&self, avg_vruntime: u64) -> bool {
        self.vruntime <= avg_vruntime
    }
    /// Update eligible_at to current vruntime (thread just became eligible).
    pub fn mark_eligible(&self) -> Self {
        Self {
            eligible_at: self.vruntime,
            ..*self
        }
    }
    /// Virtual deadline = eligible_at + slice * DEFAULT_WEIGHT / weight.
    /// Shorter slices or higher weights → earlier deadlines.
    pub fn virtual_deadline(&self) -> u64 {
        self.eligible_at + self.requested_slice * DEFAULT_WEIGHT as u64 / self.weight as u64
    }
}

/// Compute the average vruntime across all threads.
/// Returns 0 for an empty slice (everything is eligible).
pub fn avg_vruntime(states: &[State]) -> u64 {
    if states.is_empty() {
        return 0;
    }

    let sum: u128 = states.iter().map(|s| s.vruntime as u128).sum();

    (sum / states.len() as u128) as u64
}
/// Select the next thread to run.
///
/// `threads` is a slice of `(State, has_budget)` pairs. Returns the
/// index of the best candidate, or None if the slice is empty.
///
/// Algorithm:
/// 1. Filter to threads with budget AND eligible (vruntime ≤ avg).
/// 2. Among those, pick the one with the earliest virtual deadline.
/// 3. Fallback: if no thread is eligible, pick the thread with the
///    smallest vruntime (it's the most behind — let it catch up).
pub fn select_next(threads: &[(State, bool)], avg_vruntime: u64) -> Option<usize> {
    if threads.is_empty() {
        return None;
    }

    // Primary: eligible + has budget → earliest deadline.
    let mut best: Option<(usize, u64)> = None;

    for (i, (state, has_budget)) in threads.iter().enumerate() {
        if !has_budget {
            continue;
        }
        if !state.is_eligible(avg_vruntime) {
            continue;
        }

        let deadline = state.virtual_deadline();

        if best.map_or(true, |(_, d)| deadline < d) {
            best = Some((i, deadline));
        }
    }

    if let Some((idx, _)) = best {
        return Some(idx);
    }

    // Fallback: no eligible thread. Pick smallest vruntime with budget.
    let mut fallback: Option<(usize, u64)> = None;

    for (i, (state, has_budget)) in threads.iter().enumerate() {
        if !has_budget {
            continue;
        }
        if fallback.map_or(true, |(_, v)| state.vruntime < v) {
            fallback = Some((i, state.vruntime));
        }
    }

    fallback.map(|(idx, _)| idx)
}
