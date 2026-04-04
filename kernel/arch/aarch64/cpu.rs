//! AArch64 CPU primitives.
//!
//! Barriers, idle hints, and system register reads for diagnostics.

/// Data Synchronization Barrier (inner-shareable).
///
/// Ensures all prior memory writes are visible to other cores before
/// subsequent instructions execute.
#[inline(always)]
pub fn dsb_ish() {
    // SAFETY: DSB ISH is a barrier with side effects. No `nomem` —
    // LLVM must not reorder memory accesses past this.
    unsafe {
        core::arch::asm!("dsb ish", options(nostack));
    }
}
/// Read ELR_EL1 (Exception Link Register).
#[inline(always)]
pub fn read_elr() -> u64 {
    let val: u64;

    // SAFETY: Reads ELR_EL1 (Exception Link Register). Only valid at EL1 during exception
    // handling. `nomem` is correct: MRS of a CPU register has no memory side effects.
    unsafe {
        core::arch::asm!("mrs {}, elr_el1", out(reg) val, options(nostack, nomem));
    }

    val
}
/// Read ESR_EL1 (Exception Syndrome Register).
#[inline(always)]
pub fn read_esr() -> u64 {
    let val: u64;

    // SAFETY: Read-only query of exception syndrome register. `nomem`
    // correct — no memory side effects.
    unsafe {
        core::arch::asm!("mrs {}, esr_el1", out(reg) val, options(nostack, nomem));
    }

    val
}
/// Read FAR_EL1 (Fault Address Register).
#[inline(always)]
pub fn read_far() -> u64 {
    let val: u64;

    // SAFETY: Reads FAR_EL1 (Fault Address Register), set by the MMU on translation faults.
    // Only meaningful during a synchronous exception. `nomem` is correct: pure register read.
    unsafe {
        core::arch::asm!("mrs {}, far_el1", out(reg) val, options(nostack, nomem));
    }

    val
}
/// Read SP (current stack pointer) for diagnostics.
#[inline(always)]
pub fn read_sp() -> u64 {
    let val: u64;

    // SAFETY: Reads the current stack pointer. Always valid at EL1. `nomem` is correct:
    // MOV from SP is a pure register read with no memory side effects.
    unsafe {
        core::arch::asm!("mov {}, sp", out(reg) val, options(nostack, nomem));
    }

    val
}
/// Read TPIDR_EL1 (current thread pointer) for diagnostics.
#[inline(always)]
pub fn read_tpidr() -> u64 {
    let val: u64;

    // SAFETY: Reading TPIDR_EL1. No `nomem` — TPIDR is set by the
    // scheduler and must not be CSE'd across context switches.
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el1", out(reg) val, options(nostack));
    }

    val
}
/// Wait For Interrupt — halt the core until an IRQ or FIQ arrives.
///
/// Used in idle loops. WFI (not WFE) because IPIs (SGI via GICv3)
/// wake WFI but not WFE.
#[inline(always)]
pub fn wait_for_interrupt() {
    // SAFETY: WFI is a hint instruction with no memory side effects.
    // `nomem` is correct — it's purely a power management hint.
    unsafe {
        core::arch::asm!("wfi", options(nostack, nomem));
    }
}
