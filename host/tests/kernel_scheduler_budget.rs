//! Scheduler budget enforcement integration tests (v0.6).
//!
//! The scheduling_context.rs has 23 unit tests for replenishment math, but
//! nothing verifies the scheduler actually SKIPS exhausted-budget threads.
//!
//! These tests build a focused model that integrates SchedulingContext budget
//! tracking with EEVDF selection, verifying:
//!
//! 1. Exhausted thread skipped — select_best picks thread with remaining budget
//! 2. All threads exhausted → idle (no thread selected)
//! 3. Replenishment restores eligibility
//! 4. Budget charges on run
//! 5. Budget does not go negative (saturates to 0)
//! 6. Mixed budget/no-budget threads (None context = unlimited)
//! 7. Period boundary replenishment
//! 8. Budget exhaustion mid-selection (budget=1, charge 1, then skipped)

#[path = "../../kernel/paging.rs"]
mod paging;
#[path = "../../kernel/scheduling_algorithm.rs"]
mod scheduling_algorithm;
#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;

use scheduling_algorithm::{avg_vruntime, select_next, SchedulingState};
use scheduling_context::SchedulingContext;

// ============================================================
// Model: thread with optional scheduling context
// ============================================================

#[derive(Clone, Debug)]
struct Thread {
    id: u64,
    eevdf: SchedulingState,
    /// Index into the context table, or None (unlimited budget).
    context_idx: Option<usize>,
}

impl Thread {
    fn new(id: u64) -> Self {
        Self {
            id,
            eevdf: SchedulingState::new(),
            context_idx: None,
        }
    }

    fn with_context(id: u64, context_idx: usize) -> Self {
        Self {
            id,
            eevdf: SchedulingState::new(),
            context_idx: Some(context_idx),
        }
    }
}

/// Check whether a thread has budget. Threads without a context always
/// have budget (treated as unlimited). Threads bound to a context require
/// remaining > 0.
fn has_budget(thread: &Thread, contexts: &[SchedulingContext]) -> bool {
    match thread.context_idx {
        None => true,
        Some(idx) => contexts[idx].has_budget(),
    }
}

/// EEVDF selection with budget awareness. Returns index of the best
/// candidate, or None if no thread has budget (idle fallback).
fn select_best(threads: &[Thread], contexts: &[SchedulingContext]) -> Option<usize> {
    if threads.is_empty() {
        return None;
    }

    // Build (SchedulingState, has_budget) pairs for select_next.
    let pairs: Vec<(SchedulingState, bool)> = threads
        .iter()
        .map(|t| (t.eevdf, has_budget(t, contexts)))
        .collect();

    let avg = avg_vruntime(&pairs.iter().map(|(s, _)| *s).collect::<Vec<_>>());
    select_next(&pairs, avg)
}

// ============================================================
// Test 1: Exhausted thread skipped
// ============================================================

#[test]
fn exhausted_thread_skipped() {
    // Thread A: budget exhausted (remaining=0).
    // Thread B: budget available.
    // select_best must pick B.
    let mut contexts = vec![
        SchedulingContext::new(1_000_000, 10_000_000, 0),
        SchedulingContext::new(1_000_000, 10_000_000, 0),
    ];

    // Exhaust context 0.
    contexts[0] = contexts[0].charge(1_000_000);
    assert!(!contexts[0].has_budget());
    assert!(contexts[1].has_budget());

    let threads = vec![
        Thread::with_context(1, 0), // A — exhausted
        Thread::with_context(2, 1), // B — has budget
    ];

    let selected = select_best(&threads, &contexts).unwrap();
    assert_eq!(
        threads[selected].id, 2,
        "should select thread B (has budget)"
    );
}

// ============================================================
// Test 2: All threads exhausted → idle
// ============================================================

#[test]
fn all_threads_exhausted_returns_none() {
    let mut contexts = vec![
        SchedulingContext::new(1_000_000, 10_000_000, 0),
        SchedulingContext::new(1_000_000, 10_000_000, 0),
    ];

    // Exhaust both contexts.
    contexts[0] = contexts[0].charge(1_000_000);
    contexts[1] = contexts[1].charge(1_000_000);

    let threads = vec![Thread::with_context(1, 0), Thread::with_context(2, 1)];

    let selected = select_best(&threads, &contexts);
    assert!(
        selected.is_none(),
        "all budgets exhausted → no thread selected (idle)"
    );
}

// ============================================================
// Test 3: Replenishment restores eligibility
// ============================================================

#[test]
fn replenishment_restores_eligibility() {
    let period = 10_000_000u64; // 10 ms
    let budget = 1_000_000u64; // 1 ms

    let mut ctx = SchedulingContext::new(budget, period, 0);

    // Exhaust budget.
    ctx = ctx.charge(budget);
    assert!(!ctx.has_budget());

    // Simulate time passing to period boundary.
    ctx = ctx.maybe_replenish(period);
    assert!(
        ctx.has_budget(),
        "budget should be restored after replenishment"
    );
    assert_eq!(ctx.remaining, budget);

    // Thread bound to this context should now be selectable.
    let contexts = vec![ctx];
    let threads = vec![Thread::with_context(1, 0)];
    let selected = select_best(&threads, &contexts);
    assert!(selected.is_some(), "replenished thread should be selected");
}

// ============================================================
// Test 4: Budget charges on run
// ============================================================

#[test]
fn budget_charges_on_run() {
    let budget = 5_000_000u64; // 5 ms
    let period = 50_000_000u64; // 50 ms
    let elapsed = 2_000_000u64; // 2 ms

    let ctx = SchedulingContext::new(budget, period, 0);
    assert_eq!(ctx.remaining, budget);

    // Charge elapsed time (simulates deschedule after running).
    let charged = ctx.charge(elapsed);
    assert_eq!(charged.remaining, budget - elapsed);
    assert!(charged.has_budget());

    // Charge more — partial remaining.
    let charged2 = charged.charge(elapsed);
    assert_eq!(charged2.remaining, budget - 2 * elapsed);
    assert!(charged2.has_budget());

    // Charge the rest — exactly zero remaining.
    let charged3 = charged2.charge(budget - 2 * elapsed);
    assert_eq!(charged3.remaining, 0);
    assert!(!charged3.has_budget());
}

// ============================================================
// Test 5: Budget does not go negative
// ============================================================

#[test]
fn budget_saturates_to_zero() {
    let ctx = SchedulingContext::new(1_000_000, 10_000_000, 0);

    // Charge more than the budget — must saturate, not wrap.
    let charged = ctx.charge(u64::MAX);
    assert_eq!(charged.remaining, 0);
    assert!(!charged.has_budget());

    // Double charge — still zero, no underflow.
    let charged2 = charged.charge(1);
    assert_eq!(charged2.remaining, 0);
}

// ============================================================
// Test 6: Mixed budget/no-budget threads
// ============================================================

#[test]
fn threads_without_context_always_have_budget() {
    let mut contexts = vec![SchedulingContext::new(1_000_000, 10_000_000, 0)];

    // Exhaust context 0.
    contexts[0] = contexts[0].charge(1_000_000);

    let threads = vec![
        Thread::with_context(1, 0), // A — exhausted context
        Thread::new(2),             // B — no context (unlimited)
    ];

    let selected = select_best(&threads, &contexts).unwrap();
    assert_eq!(
        threads[selected].id, 2,
        "thread without context should always be selectable"
    );
}

#[test]
fn all_unlimited_threads_selectable() {
    // No contexts at all — all threads are unlimited.
    let contexts: Vec<SchedulingContext> = vec![];
    let threads = vec![Thread::new(1), Thread::new(2), Thread::new(3)];

    let selected = select_best(&threads, &contexts);
    assert!(
        selected.is_some(),
        "unlimited threads should always be selectable"
    );
}

#[test]
fn mixed_limited_unlimited_selection_prefers_eligible() {
    // Thread 1: limited, has budget.
    // Thread 2: unlimited.
    // Both should be candidates. EEVDF picks by deadline.
    let contexts = vec![SchedulingContext::new(5_000_000, 50_000_000, 0)];
    let threads = vec![
        Thread::with_context(1, 0), // limited, has budget
        Thread::new(2),             // unlimited
    ];

    let selected = select_best(&threads, &contexts);
    assert!(
        selected.is_some(),
        "at least one thread should be selectable"
    );
}

// ============================================================
// Test 7: Period boundary replenishment
// ============================================================

#[test]
fn period_boundary_replenishment() {
    let budget = 2_000_000u64; // 2 ms
    let period = 10_000_000u64; // 10 ms

    let ctx = SchedulingContext::new(budget, period, 0);

    // Exhaust budget.
    let exhausted = ctx.charge(budget);
    assert!(!exhausted.has_budget());

    // Time is still within the first period — no replenishment.
    let still_exhausted = exhausted.maybe_replenish(period - 1);
    assert!(
        !still_exhausted.has_budget(),
        "should not replenish before period boundary"
    );

    // Time reaches period boundary — replenishment.
    let replenished = exhausted.maybe_replenish(period);
    assert!(
        replenished.has_budget(),
        "should replenish at period boundary"
    );
    assert_eq!(replenished.remaining, budget);

    // Next replenishment boundary advances by one period.
    let recharged = replenished.charge(budget);
    let still_empty = recharged.maybe_replenish(period + period - 1);
    assert!(
        !still_empty.has_budget(),
        "should not replenish before second period boundary"
    );
    let replenished2 = recharged.maybe_replenish(2 * period);
    assert!(
        replenished2.has_budget(),
        "should replenish at second period boundary"
    );
}

#[test]
fn skipped_periods_do_not_accumulate_budget() {
    let budget = 1_000_000u64;
    let period = 10_000_000u64;

    let ctx = SchedulingContext::new(budget, period, 0);
    let exhausted = ctx.charge(budget);

    // Skip 5 periods — budget should still replenish to `budget`, not 5x budget.
    let replenished = exhausted.maybe_replenish(5 * period);
    assert_eq!(
        replenished.remaining, budget,
        "no burst allowance: budget replenishes to single period's allocation"
    );
}

// ============================================================
// Test 8: Budget exhaustion mid-selection
// ============================================================

#[test]
fn budget_exhaustion_after_charge_causes_skip() {
    let budget = 1u64; // Minimum: 1 ns remaining.
    let period = 10_000_000u64;

    let contexts = vec![
        SchedulingContext::new(budget, period, 0), // context 0: budget=1
        SchedulingContext::new(1_000_000, period, 0), // context 1: ample budget
    ];

    let threads = vec![
        Thread::with_context(1, 0), // thread A: budget=1
        Thread::with_context(2, 1), // thread B: ample
    ];

    // At this point, thread A still has budget (1 ns). Both are selectable.
    let selected = select_best(&threads, &contexts);
    assert!(selected.is_some());

    // Simulate running thread A for 1 ns — budget exhausted.
    let mut contexts_after = contexts.clone();
    contexts_after[0] = contexts_after[0].charge(1);
    assert!(!contexts_after[0].has_budget());

    // Next selection skips thread A (exhausted), picks thread B.
    let selected_after = select_best(&threads, &contexts_after).unwrap();
    assert_eq!(
        threads[selected_after].id, 2,
        "exhausted thread A should be skipped in next selection"
    );
}

#[test]
fn single_thread_budget_exhaustion_goes_idle() {
    // Only one thread, budget=1. After charge, no threads selectable.
    let mut contexts = vec![SchedulingContext::new(1, 10_000_000, 0)];
    let threads = vec![Thread::with_context(1, 0)];

    // Thread is selectable before charge.
    assert!(select_best(&threads, &contexts).is_some());

    // Charge 1 ns — exhausted.
    contexts[0] = contexts[0].charge(1);
    assert!(
        select_best(&threads, &contexts).is_none(),
        "single exhausted thread → idle"
    );
}

// ============================================================
// Additional: verify EEVDF still picks earliest deadline among
// threads with budget
// ============================================================

#[test]
fn eevdf_deadline_respected_among_budgeted_threads() {
    // Three threads with budget, different requested slices.
    // Shorter slice → earlier deadline → selected first.
    let contexts = vec![SchedulingContext::new(5_000_000, 50_000_000, 0)];

    let threads = vec![
        Thread {
            id: 1,
            eevdf: SchedulingState {
                vruntime: 0,
                weight: scheduling_algorithm::DEFAULT_WEIGHT,
                requested_slice: 8_000_000, // 8 ms — later deadline
                eligible_at: 0,
            },
            context_idx: Some(0),
        },
        Thread {
            id: 2,
            eevdf: SchedulingState {
                vruntime: 0,
                weight: scheduling_algorithm::DEFAULT_WEIGHT,
                requested_slice: 1_000_000, // 1 ms — earliest deadline
                eligible_at: 0,
            },
            context_idx: Some(0),
        },
        Thread {
            id: 3,
            eevdf: SchedulingState {
                vruntime: 0,
                weight: scheduling_algorithm::DEFAULT_WEIGHT,
                requested_slice: 4_000_000, // 4 ms — middle deadline
                eligible_at: 0,
            },
            context_idx: Some(0),
        },
    ];

    let selected = select_best(&threads, &contexts).unwrap();
    assert_eq!(
        threads[selected].id, 2,
        "EEVDF should pick thread with earliest virtual deadline (shortest slice)"
    );
}

#[test]
fn exhausted_context_shared_by_multiple_threads() {
    // Two threads share the same exhausted context. Neither should be selected.
    let mut contexts = vec![SchedulingContext::new(1_000_000, 10_000_000, 0)];
    contexts[0] = contexts[0].charge(1_000_000);

    let threads = vec![Thread::with_context(1, 0), Thread::with_context(2, 0)];

    assert!(
        select_best(&threads, &contexts).is_none(),
        "all threads sharing an exhausted context → idle"
    );
}

#[test]
fn replenish_shared_context_restores_all_threads() {
    // Two threads share a context. Exhaust it, replenish it,
    // verify both become selectable again.
    let period = 10_000_000u64;
    let budget = 1_000_000u64;

    let mut contexts = vec![SchedulingContext::new(budget, period, 0)];
    contexts[0] = contexts[0].charge(budget);
    assert!(!contexts[0].has_budget());

    let threads = vec![Thread::with_context(1, 0), Thread::with_context(2, 0)];

    // Before replenishment — both skipped.
    assert!(select_best(&threads, &contexts).is_none());

    // Replenish.
    contexts[0] = contexts[0].maybe_replenish(period);
    assert!(contexts[0].has_budget());

    // After replenishment — one of them is selected.
    let selected = select_best(&threads, &contexts);
    assert!(
        selected.is_some(),
        "replenished shared context restores eligibility for both threads"
    );
}
