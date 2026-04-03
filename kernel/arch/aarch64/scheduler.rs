//! AArch64 scheduler primitives.
//!
//! TPIDR_EL1 (current thread pointer) and TTBR0_EL1 (address space switch).

/// Set TPIDR_EL1 to point at the current thread's Context.
///
/// exception.S reads TPIDR_EL1 to locate the save area on exception entry.
/// Must be called under the scheduler lock — no `nomem` because the compiler
/// must not reorder this past the lock release.
#[inline(always)]
pub unsafe fn set_current_thread(ctx_ptr: usize) {
    // SAFETY: Writing TPIDR_EL1 is valid at EL1. The caller guarantees
    // ctx_ptr points to a valid Context that will remain live. `nostack`
    // is correct. No `nomem` — the compiler must not reorder this past
    // the lock release (which restores DAIF and re-enables IRQs).
    core::arch::asm!(
        "msr tpidr_el1, {0}",
        in(reg) ctx_ptr,
        options(nostack),
    );
}
/// Switch the user address space (TTBR0_EL1) with TLB invalidation.
///
/// Invalidates the old ASID's TLB entries, then writes the new TTBR0
/// value (which encodes both the page table root PA and the ASID).
///
/// # Safety
///
/// `new_ttbr0` must be a valid TTBR0_EL1 value (root PA | ASID << 48).
/// `old_asid` must be the ASID of the outgoing address space.
#[inline(always)]
pub unsafe fn switch_address_space(old_asid: u64, new_ttbr0: u64) {
    // ASIDE1IS Xt encoding: ASID in bits [63:48], other bits RES0.
    let aside_arg = old_asid << 48;

    // SAFETY: DSB/TLBI/DSB/MSR/ISB sequence is the ARM-mandated way to
    // switch address spaces. DSBs ensure ordering, TLBI invalidates the
    // old ASID, MSR TTBR0 installs the new page table, ISB synchronizes
    // the pipeline. `nomem` intentionally omitted — TTBR swap and TLB
    // invalidation are massive side effects.
    core::arch::asm!(
        "dsb ishst",
        "tlbi aside1is, {asid}",
        "dsb ish",
        "msr ttbr0_el1, {new}",
        "isb",
        asid = in(reg) aside_arg,
        new = in(reg) new_ttbr0,
        options(nostack),
    );
}
