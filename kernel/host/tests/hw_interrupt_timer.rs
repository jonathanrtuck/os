//! Host-side tests for interrupt_controller.rs and timer.rs audit findings.
//!
//! Cannot include kernel modules directly (they target aarch64-unknown-none
//! with inline asm and hardware-specific code). Tests duplicate the pure-logic
//! portions to verify correctness.

// ---------------------------------------------------------------------------
// Timer deadline computation logic (duplicated from kernel timer.rs)
// ---------------------------------------------------------------------------

/// Mirrors the deadline computation in timer::create().
/// Returns the deadline in counter ticks for a given timeout.
///
/// BUG (pre-fix): uses wrapping addition — if `now + delta` overflows u64,
/// the deadline wraps to a small value and the timer fires immediately.
fn compute_deadline_buggy(now: u64, timeout_ns: u64, freq: u64) -> u64 {
    if timeout_ns == 0 {
        now
    } else {
        now.wrapping_add((timeout_ns as u128 * freq as u128 / 1_000_000_000) as u64)
    }
}

/// Mirrors the fixed deadline computation using saturating_add.
/// Large timeouts clamp to u64::MAX instead of wrapping.
fn compute_deadline_fixed(now: u64, timeout_ns: u64, freq: u64) -> u64 {
    if timeout_ns == 0 {
        now
    } else {
        let delta = (timeout_ns as u128 * freq as u128 / 1_000_000_000) as u64;
        now.saturating_add(delta)
    }
}

/// Mirrors the check_expired comparison: `now >= deadline`.
fn is_expired(now: u64, deadline: u64) -> bool {
    now >= deadline
}

/// Mirrors counter_to_ns from timer.rs.
fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
    if freq == 0 {
        return 0;
    }
    (ticks as u128 * 1_000_000_000 / freq as u128) as u64
}

// ---------------------------------------------------------------------------
// GIC register access logic (duplicated from kernel interrupt_controller.rs)
// ---------------------------------------------------------------------------

/// Mirrors the ISENABLER/ICENABLER register+bit computation from
/// enable_irq/disable_irq.
fn gic_enable_reg_offset(irq: u32) -> (usize, u32) {
    let reg_offset = (irq / 32) as usize * 4;
    let bit = 1u32 << (irq % 32);
    (reg_offset, bit)
}

/// Mirrors the ITARGETSR offset computation from enable_irq.
fn gic_target_offset(irq: u32) -> usize {
    irq as usize
}

// ---------------------------------------------------------------------------
// Tests: Timer deadline overflow
// ---------------------------------------------------------------------------

#[test]
fn deadline_wraps_on_large_timeout_buggy() {
    // Counter near u64::MAX, large timeout.
    // At 62.5 MHz, 1 second = 62_500_000 ticks.
    let freq = 62_500_000u64;
    let now = u64::MAX - 1_000_000; // Counter near wrap
    let timeout_ns = 1_000_000_000; // 1 second

    let deadline = compute_deadline_buggy(now, timeout_ns, freq);

    // BUG: deadline wrapped around — it's now LESS than `now`,
    // meaning the timer fires immediately.
    assert!(
        deadline < now,
        "Expected buggy deadline to wrap below now; got deadline={deadline}, now={now}"
    );
    // Timer would fire immediately — this is the bug.
    assert!(
        is_expired(now, deadline),
        "Timer with 1s timeout should NOT fire immediately"
    );
}

#[test]
fn deadline_saturates_on_large_timeout_fixed() {
    let freq = 62_500_000u64;
    let now = u64::MAX - 1_000_000;
    let timeout_ns = 1_000_000_000; // 1 second

    let deadline = compute_deadline_fixed(now, timeout_ns, freq);

    // Fixed version saturates to u64::MAX — timer never fires early.
    assert_eq!(deadline, u64::MAX);
    // With the counter still at `now`, the timer should NOT have expired.
    assert!(!is_expired(now, deadline));
}

#[test]
fn deadline_normal_case_no_overflow() {
    let freq = 62_500_000u64;
    let now = 1_000_000;
    let timeout_ns = 100_000_000; // 100ms

    let deadline_buggy = compute_deadline_buggy(now, timeout_ns, freq);
    let deadline_fixed = compute_deadline_fixed(now, timeout_ns, freq);

    // For normal values, both should agree.
    assert_eq!(deadline_buggy, deadline_fixed);
    // 100ms at 62.5 MHz = 6_250_000 ticks.
    let expected = now + 6_250_000;
    assert_eq!(deadline_buggy, expected);
    // Not yet expired.
    assert!(!is_expired(now, deadline_buggy));
    // Expired after the deadline passes.
    assert!(is_expired(expected, deadline_buggy));
    assert!(is_expired(expected + 1, deadline_buggy));
}

#[test]
fn deadline_zero_timeout_expires_immediately() {
    let freq = 62_500_000u64;
    let now = 500_000;

    let deadline = compute_deadline_fixed(now, 0, freq);
    assert_eq!(deadline, now);
    assert!(is_expired(now, deadline));
}

#[test]
fn deadline_max_timeout_saturates() {
    // At 62.5 MHz, u64::MAX ns ≈ 1.15e18 ticks. With `now` near u64::MAX,
    // `now + delta` would overflow — saturating_add clamps to u64::MAX.
    let freq = 62_500_000u64;
    let now = u64::MAX - 100; // Counter near max

    let deadline = compute_deadline_fixed(now, u64::MAX, freq);
    // Must saturate to u64::MAX, not wrap around.
    assert_eq!(deadline, u64::MAX);
    assert!(!is_expired(now, deadline));
}

#[test]
fn deadline_counter_at_zero() {
    let freq = 62_500_000u64;
    let now = 0;
    let timeout_ns = 1_000_000_000; // 1 second

    let deadline = compute_deadline_fixed(now, timeout_ns, freq);
    assert_eq!(deadline, 62_500_000);
    assert!(!is_expired(now, deadline));
}

// ---------------------------------------------------------------------------
// Tests: counter_to_ns conversion
// ---------------------------------------------------------------------------

#[test]
fn counter_to_ns_zero_freq_returns_zero() {
    assert_eq!(counter_to_ns(1000, 0), 0);
}

#[test]
fn counter_to_ns_one_second() {
    let freq = 62_500_000u64;
    let ns = counter_to_ns(freq, freq);
    assert_eq!(ns, 1_000_000_000);
}

#[test]
fn counter_to_ns_large_ticks_no_overflow() {
    // u64::MAX ticks at 62.5 MHz should not overflow (uses u128 intermediate).
    let freq = 62_500_000u64;
    let ns = counter_to_ns(u64::MAX, freq);
    // ~294 billion seconds in nanoseconds, truncated to u64.
    assert!(ns > 0);
}

#[test]
fn counter_to_ns_zero_ticks() {
    let freq = 62_500_000u64;
    assert_eq!(counter_to_ns(0, freq), 0);
}

// ---------------------------------------------------------------------------
// Tests: GIC register offset computation
// ---------------------------------------------------------------------------

#[test]
fn gic_enable_irq_0_is_bit_0() {
    let (offset, bit) = gic_enable_reg_offset(0);
    assert_eq!(offset, 0);
    assert_eq!(bit, 1);
}

#[test]
fn gic_enable_irq_31_is_bit_31() {
    let (offset, bit) = gic_enable_reg_offset(31);
    assert_eq!(offset, 0);
    assert_eq!(bit, 1 << 31);
}

#[test]
fn gic_enable_irq_32_is_next_register() {
    let (offset, bit) = gic_enable_reg_offset(32);
    assert_eq!(offset, 4); // Next 4-byte register
    assert_eq!(bit, 1);
}

#[test]
fn gic_enable_irq_63_is_second_register_bit_31() {
    let (offset, bit) = gic_enable_reg_offset(63);
    assert_eq!(offset, 4);
    assert_eq!(bit, 1 << 31);
}

#[test]
fn gic_enable_irq_1020_max_spi() {
    // Max GIC IRQ is 1020 (ID 1023 is spurious).
    let (offset, bit) = gic_enable_reg_offset(1020);
    assert_eq!(offset, (1020 / 32) as usize * 4);
    assert_eq!(bit, 1 << (1020 % 32));
}

#[test]
fn gic_target_offset_spi_first() {
    // SPIs start at 32, ITARGETSR is byte-indexed.
    assert_eq!(gic_target_offset(32), 32);
}

#[test]
fn gic_target_offset_ppi() {
    // PPIs (< 32) have read-only ITARGETSR — enable_irq skips the write.
    // But the offset computation itself should still work.
    assert_eq!(gic_target_offset(16), 16);
}

// ---------------------------------------------------------------------------
// Tests: Interrupt forwarding table logic (duplicated from interrupt.rs)
// ---------------------------------------------------------------------------

/// Mirrors the interrupt registration table.
struct InterruptTable {
    slots: [Option<u32>; 32],
}

impl InterruptTable {
    fn new() -> Self {
        Self {
            slots: [const { None }; 32],
        }
    }

    fn register(&mut self, irq: u32) -> Option<u8> {
        // Reject duplicate registration.
        for slot in self.slots.iter() {
            if *slot == Some(irq) {
                return None;
            }
        }

        for i in 0..32 {
            if self.slots[i].is_none() {
                self.slots[i] = Some(irq);
                return Some(i as u8);
            }
        }

        None
    }

    fn handle_irq(&self, irq: u32) -> Option<u8> {
        for i in 0..32 {
            if self.slots[i] == Some(irq) {
                return Some(i as u8);
            }
        }
        None
    }

    fn destroy(&mut self, id: u8) -> Option<u32> {
        self.slots[id as usize].take()
    }
}

#[test]
fn interrupt_register_and_lookup() {
    let mut table = InterruptTable::new();
    let id = table.register(42).unwrap();
    assert_eq!(table.handle_irq(42), Some(id));
}

#[test]
fn interrupt_reject_duplicate() {
    let mut table = InterruptTable::new();
    table.register(42).unwrap();
    assert_eq!(table.register(42), None);
}

#[test]
fn interrupt_table_full() {
    let mut table = InterruptTable::new();
    for i in 0..32 {
        assert!(table.register(i).is_some());
    }
    assert_eq!(table.register(100), None);
}

#[test]
fn interrupt_destroy_and_reregister() {
    let mut table = InterruptTable::new();
    let id = table.register(42).unwrap();
    let irq = table.destroy(id).unwrap();
    assert_eq!(irq, 42);
    assert_eq!(table.handle_irq(42), None);
    // Can re-register after destroy.
    assert!(table.register(42).is_some());
}

#[test]
fn interrupt_unregistered_irq_returns_none() {
    let table = InterruptTable::new();
    assert_eq!(table.handle_irq(99), None);
}

// ---------------------------------------------------------------------------
// Tests: GIC init barrier requirements
// ---------------------------------------------------------------------------
// These tests verify the pattern: GIC init functions must issue DSB+ISB
// after writing CTLR/PMR registers. Since we can't execute aarch64 asm
// on the host, we test the structural requirement by simulating the
// init sequence and checking that barriers are "present" in the sequence.

/// Simulates a GIC init sequence. Each step is either a write or a barrier.
#[derive(Debug, Clone, PartialEq)]
enum GicOp {
    Write(&'static str, u32),
    DsbIsb,
}

/// Mirrors the FIXED init_distributor: write CTLR=1, then DSB+ISB.
fn init_distributor_fixed() -> Vec<GicOp> {
    vec![GicOp::Write("GICD_CTLR", 1), GicOp::DsbIsb]
}

/// Mirrors the FIXED init_cpu_interface: write PMR=0xFF, CTLR=1, then DSB+ISB.
fn init_cpu_interface_fixed() -> Vec<GicOp> {
    vec![
        GicOp::Write("GICC_PMR", 0xFF),
        GicOp::Write("GICC_CTLR", 1),
        GicOp::DsbIsb,
    ]
}

/// Mirrors the BUGGY init_distributor: write CTLR=1, NO barrier.
fn init_distributor_buggy() -> Vec<GicOp> {
    vec![GicOp::Write("GICD_CTLR", 1)]
}

/// Mirrors the BUGGY init_cpu_interface: write PMR=0xFF, CTLR=1, NO barrier.
fn init_cpu_interface_buggy() -> Vec<GicOp> {
    vec![GicOp::Write("GICC_PMR", 0xFF), GicOp::Write("GICC_CTLR", 1)]
}

/// Check that the last operation in a GIC init sequence is a barrier.
fn ends_with_barrier(ops: &[GicOp]) -> bool {
    ops.last() == Some(&GicOp::DsbIsb)
}

#[test]
fn gic_init_distributor_buggy_missing_barrier() {
    let ops = init_distributor_buggy();
    assert!(
        !ends_with_barrier(&ops),
        "Buggy init_distributor should NOT end with a barrier"
    );
}

#[test]
fn gic_init_cpu_interface_buggy_missing_barrier() {
    let ops = init_cpu_interface_buggy();
    assert!(
        !ends_with_barrier(&ops),
        "Buggy init_cpu_interface should NOT end with a barrier"
    );
}

#[test]
fn gic_init_distributor_fixed_has_barrier() {
    let ops = init_distributor_fixed();
    assert!(
        ends_with_barrier(&ops),
        "Fixed init_distributor must end with DSB+ISB barrier"
    );
}

#[test]
fn gic_init_cpu_interface_fixed_has_barrier() {
    let ops = init_cpu_interface_fixed();
    assert!(
        ends_with_barrier(&ops),
        "Fixed init_cpu_interface must end with DSB+ISB barrier"
    );
}

// ---------------------------------------------------------------------------
// Tests: Tickless idle — reprogram_next_deadline logic
// ---------------------------------------------------------------------------
// These tests model the deadline computation that reprogram_next_deadline
// must perform: pick the minimum of timer objects, scheduler quantum expiry,
// and scheduling context replenishment deadlines.

/// Maximum CNTP_TVAL value for "no deadline" (long sleep).
/// u32::MAX ticks ≈ 68s at 62.5 MHz.
const MAX_TVAL: u64 = u32::MAX as u64;

/// Result of computing the next deadline.
#[derive(Debug, PartialEq)]
enum TimerAction {
    /// Fire immediately (deadline is in the past).
    Immediate,
    /// Program CNTP_TVAL with this many ticks.
    Program(u64),
    /// No deadline — program long interval (u32::MAX ticks).
    LongSleep,
}

/// Model of reprogram_next_deadline logic.
///
/// `timer_deadlines`: active timer object deadlines in counter ticks.
/// `quantum_deadline`: current thread's quantum expiry in counter ticks (None if idle/unlimited).
/// `replenish_deadline`: earliest scheduling context replenish_at in counter ticks (None if none).
/// `now`: current counter value.
fn compute_next_deadline(
    timer_deadlines: &[u64],
    quantum_deadline: Option<u64>,
    replenish_deadline: Option<u64>,
    now: u64,
) -> TimerAction {
    // Find earliest timer object deadline.
    let earliest_timer = timer_deadlines.iter().copied().min();

    // Collect all deadline sources.
    let mut earliest: Option<u64> = earliest_timer;

    if let Some(q) = quantum_deadline {
        earliest = Some(earliest.map_or(q, |e| e.min(q)));
    }
    if let Some(r) = replenish_deadline {
        earliest = Some(earliest.map_or(r, |e| e.min(r)));
    }

    match earliest {
        None => TimerAction::LongSleep,
        Some(deadline) if deadline <= now => TimerAction::Immediate,
        Some(deadline) => {
            let delta = deadline - now;
            if delta > MAX_TVAL {
                TimerAction::Program(MAX_TVAL)
            } else {
                TimerAction::Program(delta)
            }
        }
    }
}

#[test]
fn tickless_selects_earliest_of_timer_objects() {
    // VAL-TICK-002: Given timer objects with various deadlines, selects minimum.
    let now = 1_000_000u64;
    let timers = [now + 500, now + 100, now + 300];

    let action = compute_next_deadline(&timers, None, None, now);

    assert_eq!(action, TimerAction::Program(100));
}

#[test]
fn tickless_empty_timer_table_long_sleep() {
    // VAL-TICK-003: No timer objects, no scheduler deadline → long sleep.
    let now = 1_000_000u64;

    let action = compute_next_deadline(&[], None, None, now);

    assert_eq!(action, TimerAction::LongSleep);
}

#[test]
fn tickless_past_deadline_fires_immediately() {
    // VAL-TICK-006: Deadline in the past → immediate fire.
    let now = 1_000_000u64;
    let timers = [now - 100]; // Already expired

    let action = compute_next_deadline(&timers, None, None, now);

    assert_eq!(action, TimerAction::Immediate);
}

#[test]
fn tickless_deadline_exactly_at_now_fires_immediately() {
    // Edge case: deadline == now → fires immediately (>= comparison).
    let now = 1_000_000u64;
    let timers = [now];

    let action = compute_next_deadline(&timers, None, None, now);

    assert_eq!(action, TimerAction::Immediate);
}

#[test]
fn tickless_quantum_shorter_than_timer() {
    // VAL-TICK-004: Quantum expiry (2ms) shorter than nearest timer (500ms).
    let freq = 62_500_000u64;
    let now = 1_000_000u64;
    let quantum_2ms = now + (freq * 2 / 1000); // 2ms in ticks = 125_000
    let timer_500ms = now + (freq / 2); // 500ms in ticks = 31_250_000

    let action = compute_next_deadline(&[timer_500ms], Some(quantum_2ms), None, now);

    assert_eq!(action, TimerAction::Program(quantum_2ms - now));
}

#[test]
fn tickless_timer_shorter_than_quantum() {
    // VAL-TICK-005: Timer deadline shorter than quantum.
    let freq = 62_500_000u64;
    let now = 1_000_000u64;
    let timer_1ms = now + (freq / 1000); // 1ms = 62_500 ticks
    let quantum_10ms = now + (freq * 10 / 1000); // 10ms

    let action = compute_next_deadline(&[timer_1ms], Some(quantum_10ms), None, now);

    assert_eq!(action, TimerAction::Program(timer_1ms - now));
}

#[test]
fn tickless_minimum_of_timer_and_quantum() {
    // VAL-TICK-005: picks the earliest of timer deadline and quantum expiry.
    let now = 1_000_000u64;

    // Case A: timer is earlier
    let action_a = compute_next_deadline(&[now + 100], Some(now + 200), None, now);
    assert_eq!(action_a, TimerAction::Program(100));

    // Case B: quantum is earlier
    let action_b = compute_next_deadline(&[now + 200], Some(now + 100), None, now);
    assert_eq!(action_b, TimerAction::Program(100));
}

#[test]
fn tickless_replenishment_is_earliest() {
    // VAL-TICK-018: replenishment deadline earlier than timer and quantum.
    let now = 1_000_000u64;
    let replenish = now + 50;
    let timer = now + 200;
    let quantum = now + 300;

    let action = compute_next_deadline(&[timer], Some(quantum), Some(replenish), now);

    assert_eq!(action, TimerAction::Program(50));
}

#[test]
fn tickless_replenishment_with_no_other_deadlines() {
    // VAL-TICK-018: replenishment is the ONLY deadline.
    let now = 1_000_000u64;
    let replenish = now + 1_000;

    let action = compute_next_deadline(&[], None, Some(replenish), now);

    assert_eq!(action, TimerAction::Program(1_000));
}

#[test]
fn tickless_huge_delta_capped_to_max_tval() {
    // Deadline very far in the future → cap to u32::MAX ticks.
    let now = 1_000_000u64;
    let far_future = now + MAX_TVAL + 1_000;

    let action = compute_next_deadline(&[far_future], None, None, now);

    assert_eq!(action, TimerAction::Program(MAX_TVAL));
}

#[test]
fn tickless_quantum_only_no_timers() {
    // Only quantum deadline, no timer objects.
    let now = 1_000_000u64;
    let quantum = now + 625_000; // 10ms at 62.5 MHz

    let action = compute_next_deadline(&[], Some(quantum), None, now);

    assert_eq!(action, TimerAction::Program(625_000));
}

#[test]
fn tickless_multiple_timers_with_quantum() {
    // Multiple timer objects + quantum: picks absolute minimum.
    let now = 1_000_000u64;
    let timers = [now + 500, now + 100, now + 300];
    let quantum = now + 50;

    let action = compute_next_deadline(&timers, Some(quantum), None, now);

    assert_eq!(action, TimerAction::Program(50));
}

#[test]
fn tickless_all_three_sources_quantum_wins() {
    // All three sources present, quantum is earliest.
    let now = 1_000_000u64;

    let action = compute_next_deadline(&[now + 300], Some(now + 100), Some(now + 200), now);

    assert_eq!(action, TimerAction::Program(100));
}

#[test]
fn tickless_past_quantum_fires_immediately() {
    // Quantum already expired → immediate fire.
    let now = 1_000_000u64;

    let action = compute_next_deadline(&[], Some(now - 10), None, now);

    assert_eq!(action, TimerAction::Immediate);
}

#[test]
fn tickless_past_replenishment_fires_immediately() {
    // Replenishment already due → immediate fire.
    let now = 1_000_000u64;

    let action = compute_next_deadline(&[], None, Some(now - 5), now);

    assert_eq!(action, TimerAction::Immediate);
}

// ---------------------------------------------------------------------------
// Tests: Scheduling context budget + replenishment under tickless
// ---------------------------------------------------------------------------

#[path = "../../scheduling_context.rs"]
mod scheduling_context;

#[test]
fn tickless_context_budget_enforcement() {
    // VAL-TICK-015: Thread with 10ms/50ms context, timer fires at quantum boundary.
    use scheduling_context::SchedulingContext;

    let budget_ns = 10_000_000u64; // 10ms
    let period_ns = 50_000_000u64; // 50ms
    let ctx = SchedulingContext::new(budget_ns, period_ns, 0);
    let freq = 62_500_000u64;

    // After running for 5ms, 5ms remaining.
    let after_5ms = ctx.charge(5_000_000);
    assert!(after_5ms.has_budget());
    assert_eq!(after_5ms.remaining, 5_000_000);

    // The quantum deadline should be now + 5ms in counter ticks.
    let quantum_deadline_ticks = (after_5ms.remaining as u128 * freq as u128 / 1_000_000_000) as u64;
    // 5ms at 62.5 MHz = 312_500 ticks
    assert_eq!(quantum_deadline_ticks, 312_500);

    // After running the full 10ms, budget exhausted.
    let exhausted = ctx.charge(10_000_000);
    assert!(!exhausted.has_budget());
    assert_eq!(exhausted.remaining, 0);
}

#[test]
fn tickless_context_replenishment_after_period() {
    // VAL-TICK-015 + VAL-TICK-018: Budget replenishes after period even with no other deadlines.
    use scheduling_context::SchedulingContext;

    let ctx = SchedulingContext::new(10_000_000, 50_000_000, 0);

    // Exhaust budget.
    let exhausted = ctx.charge(10_000_000);
    assert!(!exhausted.has_budget());
    assert_eq!(exhausted.replenish_at, 50_000_000); // replenish at 50ms

    // Replenish at 50ms.
    let replenished = exhausted.maybe_replenish(50_000_000);
    assert!(replenished.has_budget());
    assert_eq!(replenished.remaining, 10_000_000);
    assert_eq!(replenished.replenish_at, 100_000_000); // next replenish at 100ms

    // Under tickless, the timer must fire at replenish_at so the scheduler
    // can replenish the context and schedule the thread. Convert to ticks:
    let freq = 62_500_000u64;
    let replenish_ticks = (exhausted.replenish_at as u128 * freq as u128 / 1_000_000_000) as u64;
    // 50ms at 62.5 MHz = 3_125_000 ticks
    assert_eq!(replenish_ticks, 3_125_000);

    // reprogram_next_deadline should program this deadline.
    let now_ticks = 0u64;
    let action = compute_next_deadline(&[], None, Some(replenish_ticks), now_ticks);
    assert_eq!(action, TimerAction::Program(replenish_ticks));
}
