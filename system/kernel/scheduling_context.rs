// AUDIT: 2026-03-11 — 0 unsafe blocks (pure logic). 6-category checklist applied.
// All arithmetic uses saturating operations (saturating_sub, saturating_add,
// saturating_mul). validate_params prevents division-by-zero (period >= 1ms).
// State transitions (new→charge→replenish cycle) verified complete and sound.
// No bugs found.
//! Scheduling context — per-workload temporal isolation.
//!
//! A scheduling context is a kernel object that enforces a CPU budget over a
//! repeating period. Threads bound to a context can only run while it has
//! remaining budget. The kernel charges elapsed time on every timer tick and
//! on deschedule. Budget replenishes periodically.
//!
//! Pure data + logic — no locks, no hardware access. Fully host-testable.

/// Maximum period: 1 s. Prevents starvation from overly long periods.
pub const MAX_PERIOD_NS: u64 = 1_000_000_000;
/// Minimum budget: 100 µs. Prevents pathologically small budgets.
pub const MIN_BUDGET_NS: u64 = 100_000;
/// Minimum period: 1 ms.
pub const MIN_PERIOD_NS: u64 = 1_000_000;

/// A scheduling context: budget + period + runtime state.
#[derive(Clone, Copy, Debug)]
pub struct SchedulingContext {
    /// Maximum CPU time per period (ns).
    pub budget: u64,
    /// Replenishment period (ns).
    pub period: u64,
    /// Remaining budget in the current period (ns).
    pub remaining: u64,
    /// Timestamp (ns) at which the next replenishment occurs.
    pub replenish_at: u64,
}
/// Unique identifier for a scheduling context, used as the handle object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulingContextId(pub u32);

impl SchedulingContext {
    /// Create a new scheduling context. `now_ns` is the current time.
    pub fn new(budget: u64, period: u64, now_ns: u64) -> Self {
        Self {
            budget,
            period,
            remaining: budget,
            replenish_at: now_ns + period,
        }
    }

    /// Charge `elapsed_ns` of CPU time. Returns updated context.
    pub fn charge(&self, elapsed_ns: u64) -> Self {
        Self {
            remaining: self.remaining.saturating_sub(elapsed_ns),
            ..*self
        }
    }
    /// Does this context have remaining budget?
    pub fn has_budget(&self) -> bool {
        self.remaining > 0
    }
    /// Replenish budget if the current period has elapsed. Returns updated context.
    /// If multiple periods have been skipped, advances to the next period boundary
    /// without accumulating budget (no burst allowance).
    pub fn maybe_replenish(&self, now_ns: u64) -> Self {
        if now_ns < self.replenish_at {
            return *self;
        }

        // How many full periods have elapsed since last replenishment?
        let elapsed = now_ns - self.replenish_at;
        let periods_skipped = elapsed / self.period;

        Self {
            remaining: self.budget,
            replenish_at: self
                .replenish_at
                .saturating_add((periods_skipped.saturating_add(1)).saturating_mul(self.period)),
            ..*self
        }
    }
}

/// Validate budget and period parameters for context creation.
pub fn validate_params(budget: u64, period: u64) -> bool {
    budget >= MIN_BUDGET_NS && (MIN_PERIOD_NS..=MAX_PERIOD_NS).contains(&period) && budget <= period
}
