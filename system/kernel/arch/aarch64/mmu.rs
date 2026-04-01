//! AArch64 MMU helpers.
//!
//! TLB invalidation, address translation checks (AT S1E0R/W).

/// Invalidate a single page TLB entry by VA and ASID.
///
/// DSB ISHST → TLBI VALE1IS → DSB ISH → ISB.
#[inline(always)]
pub fn tlbi_page(va: u64, asid: u64) {
    // SAFETY: TLB invalidation with proper barrier sequence.
    // The VA is shifted right by 12 (page-aligned) and ASID in bits [63:48].
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vale1is, {va}",
            "dsb ish",
            "isb",
            va = in(reg) (va >> 12) | (asid << 48),
            options(nostack)
        );
    }
}

/// Invalidate all TLB entries for an ASID.
///
/// DSB ISHST → TLBI ASIDE1IS → DSB ISH → ISB.
#[inline(always)]
pub fn tlbi_asid(asid: u64) {
    // SAFETY: TLB invalidation with proper barrier sequence.
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi aside1is, {v}",
            "dsb ish",
            "isb",
            v = in(reg) asid << 48,
            options(nostack)
        );
    }
}

/// Break-before-make TLB invalidation for a page.
///
/// Used when overwriting a valid descriptor — ARMv8 ARM B2.2.1 requires
/// invalidating the old entry before writing the new one.
/// DSB ISH → TLBI VALE1IS → DSB ISH (no ISB — caller writes new descriptor next).
#[inline(always)]
pub fn tlbi_bbm(va: u64, asid: u64) {
    unsafe {
        core::arch::asm!(
            "dsb ish",
            "tlbi vale1is, {va}",
            "dsb ish",
            va = in(reg) (va >> 12) | (asid << 48),
            options(nostack)
        );
    }
}

/// Broadcast TLB invalidation — all entries, all ASIDs.
///
/// DSB ISHST → TLBI VMALLE1IS → DSB ISH → ISB.
#[inline(always)]
pub fn tlbi_all() {
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nostack)
        );
    }
}

/// Local-core TLB invalidation — all entries (boot-time, single core).
///
/// DSB ISHST → TLBI VMALLE1 → DSB ISH → ISB.
/// Uses `vmalle1` (not `vmalle1is`) because secondary cores aren't started yet.
#[inline(always)]
pub fn tlbi_all_local() {
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack)
        );
    }
}

/// Check if a user virtual address is writable by EL0.
///
/// Uses AT S1E0W to perform a stage-1 translation check.
/// Returns true if the page is mapped and writable from EL0.
#[inline(always)]
pub fn is_user_page_writable(va: u64) -> bool {
    let par: u64;

    // SAFETY: AT S1E0W is a privileged instruction that performs address
    // translation without memory access — it only writes to PAR_EL1. The
    // ISB ensures PAR_EL1 is visible before the mrs reads it. Single asm
    // block prevents LLVM reordering.
    unsafe {
        core::arch::asm!(
            "at s1e0w, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack)
        );
    }

    // PAR_EL1 bit 0: 0 = translation succeeded, 1 = fault.
    par & 1 == 0
}

/// Check if a user virtual address is readable by EL0, returning PAR_EL1.
///
/// Uses AT S1E0R to perform a stage-1 translation check.
/// Returns the raw PAR_EL1 value. Bit 0 = 1 means fault.
/// On success, bits [47:12] contain the physical address.
#[inline(always)]
pub fn translate_user_read(va: u64) -> u64 {
    let par: u64;

    // SAFETY: AT S1E0R is a privileged instruction that performs address
    // translation without memory access — it only writes to PAR_EL1. The
    // ISB ensures PAR_EL1 is visible before the mrs reads it. Single asm
    // block prevents LLVM reordering.
    unsafe {
        core::arch::asm!(
            "at s1e0r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack)
        );
    }

    par
}
