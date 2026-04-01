//! AArch64 generic timer (virtual timer, EL1).
//!
//! Wraps CNTV_TVAL_EL0, CNTVCT_EL0, CNTFRQ_EL0, CNTKCTL_EL1, CNTV_CTL_EL0.

/// Program CNTV_TVAL_EL0 with the given number of counter ticks.
///
/// Writing TVAL also clears the timer condition (de-asserts the interrupt).
#[inline(always)]
pub fn program_tval(tval: u64) {
    // SAFETY: Writing CNTV_TVAL_EL0 reprograms the virtual timer countdown
    // and de-asserts the interrupt line. ISB ensures the new countdown is
    // committed before returning. `nomem` intentionally omitted (side effects).
    // `nostack` correct.
    unsafe {
        core::arch::asm!(
            "msr cntv_tval_el0, {tval}",
            "isb",
            tval = in(reg) tval,
            options(nostack)
        );
    }
}

/// Read the hardware counter (CNTVCT_EL0). Monotonic, sub-tick precision.
///
/// Uses the virtual counter, which equals the physical counter when
/// CNTVOFF_EL2 = 0 (set by boot.S and QEMU HVF).
#[inline(always)]
pub fn counter() -> u64 {
    let cnt: u64;

    // SAFETY: Reading CNTVCT_EL0 (virtual counter). Monotonically
    // increasing hardware state. `nomem` intentionally omitted so LLVM
    // cannot CSE or hoist repeated reads. `nostack` correct.
    unsafe {
        core::arch::asm!("mrs {0}, cntvct_el0", out(reg) cnt, options(nostack));
    }

    cnt
}

/// Read the counter frequency (CNTFRQ_EL0) in Hz.
///
/// Set by firmware at boot. Immutable after init.
pub fn read_frequency() -> u64 {
    let freq: u64;

    // SAFETY: Reading CNTFRQ_EL0 — the counter frequency set by firmware
    // at boot. Read-only, immutable after firmware init. `nomem` is correct
    // here (unlike CNTVCT_EL0) because the value never changes — LLVM may
    // freely CSE/hoist this read. `nostack` correct.
    unsafe {
        core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nostack, nomem));
    }

    freq
}

/// Allow EL0 (userspace) to read CNTVCT_EL0 (virtual counter).
///
/// Sets CNTKCTL_EL1 bit 1 (EL0VCTEN).
pub fn enable_el0_counter() {
    // SAFETY: Writing CNTKCTL_EL1. The register controls EL0 access to
    // timer/counter registers. Setting bit 1 is a hardware side-effect
    // (enables userspace reads), so nomem is omitted.
    unsafe {
        core::arch::asm!(
            "mrs x0, cntkctl_el1",
            "orr x0, x0, #2",
            "msr cntkctl_el1, x0",
            out("x0") _,
            options(nostack),
        );
    }
}

/// Enable the virtual timer (CNTV_CTL_EL0 = ENABLE=1, IMASK=0).
pub fn enable_virtual_timer() {
    // SAFETY: Writing CNTV_CTL_EL0 with ENABLE=1, IMASK=0 starts generating
    // virtual timer IRQs. `nomem` intentionally omitted (side effects).
    // `nostack` correct.
    unsafe {
        core::arch::asm!(
            "mov x0, #1",
            "msr cntv_ctl_el0, x0",
            out("x0") _,
            options(nostack)
        );
    }
}

/// Unmask IRQs at the CPU level (clear DAIF.I).
///
/// This is the final gate after GIC routing is configured.
pub fn unmask_irqs() {
    // SAFETY: Writing DAIFCLR with bit 1 clears DAIF.I, unmasking IRQs at
    // the CPU level. `nostack` correct. `nomem` intentionally omitted:
    // unmasking IRQs is a side effect that affects control flow.
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack));
    }
}
