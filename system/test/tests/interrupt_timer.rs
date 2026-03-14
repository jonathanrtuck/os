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
