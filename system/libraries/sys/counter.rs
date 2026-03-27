//! Hardware timer access via inline assembly.

/// Read the AArch64 virtual counter (CNTVCT_EL0).
/// Requires kernel to have enabled EL0 access via CNTKCTL_EL1.EL0VCTEN.
///
/// `nomem` is intentionally omitted: the counter is monotonically increasing
/// hardware state. With `nomem`, LLVM could CSE or hoist repeated reads,
/// returning stale values. Without it, each call reads the current counter.
#[inline(always)]
pub fn counter() -> u64 {
    let val: u64;

    unsafe {
        core::arch::asm!("mrs {0}, cntvct_el0", out(reg) val, options(nostack));
    }

    val
}

/// Read the counter frequency (CNTFRQ_EL0) in Hz.
#[inline(always)]
pub fn counter_freq() -> u64 {
    let val: u64;

    unsafe {
        core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) val, options(nostack, nomem));
    }

    val
}

/// Convert a counter tick count to nanoseconds.
///
/// Uses the split multiply-then-remainder form to avoid u64 overflow:
/// `(ticks / freq) * 1e9 + (ticks % freq) * 1e9 / freq`.
#[inline]
pub fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
    if freq == 0 {
        return 0;
    }
    (ticks / freq) * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
}
