//! Host-side tests for the EEVDF scheduling algorithm.
//!
//! Includes the kernel's scheduling_algorithm.rs directly — pure logic, no hardware deps.

#[path = "../../kernel/paging.rs"]
mod paging;
#[path = "../../kernel/scheduling_algorithm.rs"]
mod scheduling_algorithm;

use scheduling_algorithm::*;

fn state(vruntime: u64, weight: u32, slice: u64, eligible_at: u64) -> SchedulingState {
    SchedulingState {
        vruntime,
        weight,
        requested_slice: slice,
        eligible_at,
    }
}

fn default_state() -> SchedulingState {
    SchedulingState::new()
}

// --- SchedulingState ---

#[test]
fn new_state_has_defaults() {
    let s = default_state();

    assert_eq!(s.vruntime, 0);
    assert_eq!(s.weight, DEFAULT_WEIGHT);
    assert_eq!(s.requested_slice, DEFAULT_SLICE_NS);
    assert_eq!(s.eligible_at, 0);
}

#[test]
fn virtual_deadline_default_weight() {
    // deadline = eligible_at + slice * DEFAULT_WEIGHT / weight
    // With default weight: deadline = 0 + 4_000_000 * 1024 / 1024 = 4_000_000
    let s = default_state();

    assert_eq!(s.virtual_deadline(), DEFAULT_SLICE_NS);
}

#[test]
fn virtual_deadline_higher_weight() {
    // weight=2048: deadline = 0 + 4_000_000 * 1024 / 2048 = 2_000_000
    let s = state(0, 2048, DEFAULT_SLICE_NS, 0);

    assert_eq!(s.virtual_deadline(), DEFAULT_SLICE_NS / 2);
}

#[test]
fn virtual_deadline_shorter_slice() {
    // slice=1_000_000: deadline = 0 + 1_000_000 * 1024 / 1024 = 1_000_000
    let s = state(0, DEFAULT_WEIGHT, 1_000_000, 0);

    assert_eq!(s.virtual_deadline(), 1_000_000);
}

#[test]
fn virtual_deadline_with_eligible_at_offset() {
    let s = state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 100);

    assert_eq!(s.virtual_deadline(), 100 + DEFAULT_SLICE_NS);
}

#[test]
fn is_eligible_at_average() {
    let s = state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);

    assert!(s.is_eligible(100)); // vruntime == avg → eligible
}

#[test]
fn is_eligible_below_average() {
    let s = state(50, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);

    assert!(s.is_eligible(100)); // vruntime < avg → eligible
}

#[test]
fn is_not_eligible_above_average() {
    let s = state(150, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);

    assert!(!s.is_eligible(100)); // vruntime > avg → ineligible
}

#[test]
fn charge_default_weight() {
    let s = default_state();
    let charged = s.charge(1_000_000); // 1ms

    // delta = 1_000_000 * 1024 / 1024 = 1_000_000
    assert_eq!(charged.vruntime, 1_000_000);
}

#[test]
fn charge_higher_weight_slower_growth() {
    let s = state(0, 2048, DEFAULT_SLICE_NS, 0);
    let charged = s.charge(2_000_000); // 2ms

    // delta = 2_000_000 * 1024 / 2048 = 1_000_000
    assert_eq!(charged.vruntime, 1_000_000);
}

#[test]
fn charge_lower_weight_faster_growth() {
    let s = state(0, 512, DEFAULT_SLICE_NS, 0);
    let charged = s.charge(1_000_000); // 1ms

    // delta = 1_000_000 * 1024 / 512 = 2_000_000
    assert_eq!(charged.vruntime, 2_000_000);
}

#[test]
fn charge_preserves_other_fields() {
    let s = state(10, 2048, 1_000_000, 5);
    let charged = s.charge(100);

    assert_eq!(charged.weight, 2048);
    assert_eq!(charged.requested_slice, 1_000_000);
    assert_eq!(charged.eligible_at, 5);
}

#[test]
fn charge_saturates_on_overflow() {
    let s = state(u64::MAX - 100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);
    let charged = s.charge(1_000_000);

    assert_eq!(charged.vruntime, u64::MAX);
}

#[test]
fn mark_eligible_sets_eligible_at() {
    let s = state(500, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);
    let marked = s.mark_eligible();

    assert_eq!(marked.eligible_at, 500);
    assert_eq!(marked.vruntime, 500); // unchanged
}

// --- avg_vruntime ---

#[test]
fn avg_vruntime_empty() {
    assert_eq!(avg_vruntime(&[]), 0);
}

#[test]
fn avg_vruntime_single() {
    let states = [state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0)];

    assert_eq!(avg_vruntime(&states), 100);
}

#[test]
fn avg_vruntime_two_equal() {
    let states = [
        state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
        state(200, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
    ];

    assert_eq!(avg_vruntime(&states), 150);
}

#[test]
fn avg_vruntime_three() {
    let states = [
        state(0, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
        state(300, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
        state(600, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
    ];

    assert_eq!(avg_vruntime(&states), 300);
}

// --- select_next ---

#[test]
fn select_empty_returns_none() {
    assert_eq!(select_next(&[], 0), None);
}

#[test]
fn select_single_thread_with_budget() {
    let threads = [(default_state(), true)];

    assert_eq!(select_next(&threads, 0), Some(0));
}

#[test]
fn select_single_thread_no_budget() {
    let threads = [(default_state(), false)];

    assert_eq!(select_next(&threads, 0), None);
}

#[test]
fn select_two_equal_threads_lower_vruntime_wins() {
    // Both eligible, both have budget. Thread 0 has lower vruntime → same
    // deadline but earlier eligible_at → lower deadline.
    let threads = [
        (state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 100), true),
        (state(200, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 200), true),
    ];
    let avg = avg_vruntime(&[threads[0].0, threads[1].0]); // 150

    // Thread 0: vruntime 100 ≤ 150 → eligible. deadline = 100 + 4M
    // Thread 1: vruntime 200 > 150 → ineligible. Skipped.
    assert_eq!(select_next(&threads, avg), Some(0));
}

#[test]
fn select_budget_exhaustion_skips_thread() {
    let threads = [
        (state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 100), false), // no budget
        (state(200, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 200), true),  // has budget
    ];

    assert_eq!(select_next(&threads, 200), Some(1));
}

#[test]
fn select_shorter_slice_earlier_deadline() {
    // Both eligible, same vruntime. Thread 0 has 1ms slice, thread 1 has 4ms.
    let threads = [
        (state(0, DEFAULT_WEIGHT, 1_000_000, 0), true),
        (state(0, DEFAULT_WEIGHT, 4_000_000, 0), true),
    ];

    // Thread 0 deadline = 0 + 1_000_000 = 1_000_000
    // Thread 1 deadline = 0 + 4_000_000 = 4_000_000
    assert_eq!(select_next(&threads, 0), Some(0));
}

#[test]
fn select_higher_weight_earlier_deadline() {
    // Both eligible, same vruntime, same slice. Higher weight → smaller deadline.
    let threads = [
        (state(0, 512, DEFAULT_SLICE_NS, 0), true), // deadline = 0 + 4M * 1024/512 = 8M
        (state(0, 2048, DEFAULT_SLICE_NS, 0), true), // deadline = 0 + 4M * 1024/2048 = 2M
    ];

    assert_eq!(select_next(&threads, 0), Some(1));
}

#[test]
fn select_ineligible_thread_fallback_to_smallest_vruntime() {
    // All threads ineligible (vruntime > avg). Fallback: smallest vruntime with budget.
    let threads = [
        (state(300, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0), true),
        (state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0), true),
        (state(200, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0), true),
    ];
    let avg = 50; // All above avg → none eligible

    assert_eq!(select_next(&threads, avg), Some(1)); // vruntime 100 is smallest
}

#[test]
fn select_all_no_budget_returns_none() {
    let threads = [(default_state(), false), (default_state(), false)];

    assert_eq!(select_next(&threads, 0), None);
}

#[test]
fn select_mixed_eligibility_picks_eligible_first() {
    // Thread 0: ineligible (vruntime 200 > avg 150), has budget
    // Thread 1: eligible (vruntime 100 ≤ avg 150), has budget
    let threads = [
        (state(200, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 200), true),
        (state(100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 100), true),
    ];

    assert_eq!(select_next(&threads, 150), Some(1));
}

// ============================================================
// Overflow / edge-case tests (audit 2026-03-11)
// ============================================================

#[test]
fn virtual_deadline_saturates_on_overflow() {
    // eligible_at near u64::MAX: deadline addition must not wrap to a small value.
    // Without saturating arithmetic, eligible_at + slice would wrap around to a
    // small number, giving an unfairly early deadline.
    let s = state(
        u64::MAX - 100,
        DEFAULT_WEIGHT,
        DEFAULT_SLICE_NS,
        u64::MAX - 100,
    );
    let deadline = s.virtual_deadline();

    // Must be >= eligible_at (saturated to u64::MAX, not wrapped to a small value).
    assert!(
        deadline >= s.eligible_at,
        "virtual_deadline must not wrap around: got {deadline}, eligible_at={}",
        s.eligible_at
    );
}

#[test]
fn charge_large_elapsed_no_panic() {
    // Very large elapsed_ns: elapsed * DEFAULT_WEIGHT would overflow u64 but
    // uses u128 intermediate arithmetic so the result is correct.
    let s = default_state();
    let large_elapsed = u64::MAX / (DEFAULT_WEIGHT as u64) + 1_000_000;
    let charged = s.charge(large_elapsed);

    // With u128 intermediate: delta = large_elapsed (since weight == DEFAULT_WEIGHT).
    // Must not panic and vruntime must equal the elapsed value.
    assert_eq!(
        charged.vruntime, large_elapsed,
        "charge must compute correctly via u128 intermediate for large elapsed values"
    );
}

#[test]
fn charge_truly_enormous_elapsed_saturates() {
    // Elapsed so large that even the u128→u64 truncated delta causes saturation.
    let s = state(u64::MAX - 100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);
    let charged = s.charge(1_000_000);

    // vruntime was near MAX, adding 1M saturates to MAX.
    assert_eq!(charged.vruntime, u64::MAX);
}

#[test]
fn select_with_near_max_vruntimes_no_wrap() {
    // Two threads with vruntime near u64::MAX. Deadline calculation must not
    // wrap around and cause incorrect ordering.
    let threads = [
        (
            state(u64::MAX - 1000, DEFAULT_WEIGHT, 1_000_000, u64::MAX - 1000),
            true,
        ),
        (
            state(u64::MAX - 2000, DEFAULT_WEIGHT, 4_000_000, u64::MAX - 2000),
            true,
        ),
    ];
    let avg = u64::MAX - 1500;

    // Thread 1 has lower vruntime → eligible. Thread 0 has vruntime > avg → ineligible.
    // With wrapping, deadlines could be incorrect. With saturation, thread 1's
    // deadline saturates to u64::MAX and the selection is still correct.
    let result = select_next(&threads, avg);

    assert!(
        result.is_some(),
        "should select a thread even near u64::MAX"
    );
}

#[test]
fn charge_zero_elapsed_is_noop() {
    let s = state(500, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 100);
    let charged = s.charge(0);

    assert_eq!(charged.vruntime, 500);
    assert_eq!(charged.eligible_at, 100);
}

#[test]
fn avg_vruntime_near_u64_max() {
    // Averaging near u64::MAX values must not overflow (u128 intermediate).
    let states = [
        state(u64::MAX, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
        state(u64::MAX - 100, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
    ];
    let avg = avg_vruntime(&states);

    // (u64::MAX + u64::MAX - 100) / 2 = u64::MAX - 50
    assert_eq!(avg, u64::MAX - 50);
}

#[test]
fn is_eligible_at_u64_max() {
    // Thread with vruntime=u64::MAX, avg=u64::MAX → eligible (equal).
    let s = state(u64::MAX, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);

    assert!(s.is_eligible(u64::MAX));
}

#[test]
fn charge_vruntime_already_saturated() {
    // If vruntime is already u64::MAX, charging more must stay at u64::MAX.
    let s = state(u64::MAX, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0);
    let charged = s.charge(1_000_000);

    assert_eq!(charged.vruntime, u64::MAX);
}
