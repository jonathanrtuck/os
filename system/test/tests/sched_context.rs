//! Host-side tests for scheduling context budget management.
//!
//! Includes the kernel's scheduling_context.rs directly — pure logic, no hardware deps.

#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;

use scheduling_context::*;

// --- SchedulingContext::new ---

#[test]
fn new_context_has_full_budget() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);

    assert_eq!(ctx.budget, 2_000_000);
    assert_eq!(ctx.period, 10_000_000);
    assert_eq!(ctx.remaining, 2_000_000);
    assert_eq!(ctx.replenish_at, 10_000_000);
    assert!(ctx.has_budget());
}

#[test]
fn new_context_with_nonzero_now() {
    let ctx = SchedulingContext::new(1_000_000, 5_000_000, 100_000_000);

    assert_eq!(ctx.replenish_at, 105_000_000);
}

// --- charge ---

#[test]
fn charge_decrements_remaining() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let charged = ctx.charge(500_000);

    assert_eq!(charged.remaining, 1_500_000);
    assert!(charged.has_budget());
}

#[test]
fn charge_to_zero() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let charged = ctx.charge(2_000_000);

    assert_eq!(charged.remaining, 0);
    assert!(!charged.has_budget());
}

#[test]
fn charge_saturates_at_zero() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let charged = ctx.charge(5_000_000); // More than budget

    assert_eq!(charged.remaining, 0);
    assert!(!charged.has_budget());
}

#[test]
fn charge_preserves_other_fields() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let charged = ctx.charge(500_000);

    assert_eq!(charged.budget, 2_000_000);
    assert_eq!(charged.period, 10_000_000);
    assert_eq!(charged.replenish_at, 10_000_000);
}

// --- maybe_replenish ---

#[test]
fn replenish_before_due_is_noop() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let drained = ctx.charge(2_000_000);
    let replenished = drained.maybe_replenish(5_000_000); // Before replenish_at=10M

    assert_eq!(replenished.remaining, 0);
    assert!(!replenished.has_budget());
}

#[test]
fn replenish_at_exact_boundary() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let drained = ctx.charge(2_000_000);
    let replenished = drained.maybe_replenish(10_000_000);

    assert_eq!(replenished.remaining, 2_000_000);
    assert_eq!(replenished.replenish_at, 20_000_000);
    assert!(replenished.has_budget());
}

#[test]
fn replenish_after_boundary() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let drained = ctx.charge(2_000_000);
    let replenished = drained.maybe_replenish(13_000_000);

    assert_eq!(replenished.remaining, 2_000_000);
    assert_eq!(replenished.replenish_at, 20_000_000);
}

#[test]
fn replenish_skipped_periods_no_accumulation() {
    // If multiple periods pass without the thread running, budget should
    // NOT accumulate. It gets one fresh budget, not multiple.
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let drained = ctx.charge(2_000_000);
    let replenished = drained.maybe_replenish(35_000_000); // 3.5 periods later

    assert_eq!(replenished.remaining, 2_000_000); // Only one budget
    assert_eq!(replenished.replenish_at, 40_000_000); // Skipped to period 4
}

#[test]
fn replenish_partial_budget_resets_to_full() {
    let ctx = SchedulingContext::new(2_000_000, 10_000_000, 0);
    let partial = ctx.charge(500_000); // remaining = 1.5M
    let replenished = partial.maybe_replenish(10_000_000);

    assert_eq!(replenished.remaining, 2_000_000); // Full reset
}

#[test]
fn charge_then_replenish_cycle() {
    let ctx = SchedulingContext::new(1_000_000, 5_000_000, 0);

    // Period 1: use all budget
    let c1 = ctx.charge(1_000_000);

    assert!(!c1.has_budget());

    // Replenish at period boundary
    let c2 = c1.maybe_replenish(5_000_000);

    assert!(c2.has_budget());
    assert_eq!(c2.remaining, 1_000_000);

    // Period 2: use half
    let c3 = c2.charge(500_000);

    assert!(c3.has_budget());
    assert_eq!(c3.remaining, 500_000);

    // Replenish at next boundary
    let c4 = c3.maybe_replenish(10_000_000);

    assert_eq!(c4.remaining, 1_000_000);
    assert_eq!(c4.replenish_at, 15_000_000);
}

// --- validate_params ---

#[test]
fn validate_good_params() {
    assert!(validate_params(1_000_000, 5_000_000));
    assert!(validate_params(MIN_BUDGET_NS, MIN_PERIOD_NS));
    assert!(validate_params(MAX_PERIOD_NS, MAX_PERIOD_NS)); // budget == period is OK
}

#[test]
fn validate_budget_too_small() {
    assert!(!validate_params(MIN_BUDGET_NS - 1, MIN_PERIOD_NS));
}

#[test]
fn validate_period_too_small() {
    assert!(!validate_params(MIN_BUDGET_NS, MIN_PERIOD_NS - 1));
}

#[test]
fn validate_period_too_large() {
    assert!(!validate_params(MIN_BUDGET_NS, MAX_PERIOD_NS + 1));
}

#[test]
fn validate_budget_exceeds_period() {
    assert!(!validate_params(5_000_000, 1_000_000));
}
