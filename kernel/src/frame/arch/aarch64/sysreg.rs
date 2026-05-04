//! AArch64 system register accessors and barrier instructions.
//!
//! Inline wrappers around `mrs`/`msr` and barrier instructions used by the
//! kernel. All functions are `#[inline(always)]` since they compile to single
//! instructions.
//!
//! ## Naming convention
//!
//! - Read: `reg_name()` — e.g., `esr_el1()`
//! - Write: `set_reg_name(val)` — e.g., `set_vbar_el1(addr)`
//!
//! ## `nomem` policy
//!
//! Per project convention, `nomem` is never used by default. It is added only
//! for `mrs` of truly immutable registers (ID registers, CNTFRQ) where the
//! instruction has no side effects and reads no memory. All other accesses
//! omit `nomem` so LLVM cannot reorder them relative to memory operations.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Internal macros — not exported, used to generate accessors below.
// ---------------------------------------------------------------------------

/// Generate a read accessor for a system register.
/// Default options: `nostack` only (no `nomem`).
macro_rules! sysreg_read {
    ($name:ident, $reg:literal) => {
        #[inline(always)]
        pub fn $name() -> u64 {
            let val: u64;

            // SAFETY: MRS reads a system register into a GPR. All registers
            // below are accessible from EL1.
            unsafe {
                core::arch::asm!(
                    concat!("mrs {val}, ", $reg),
                    val = out(reg) val,
                    options(nostack),
                );
            }

            val
        }
    };
}

/// Generate a read accessor for a truly immutable register.
/// Uses `nomem` because the register value never changes and the instruction
/// has no side effects on the memory system.
macro_rules! sysreg_read_const {
    ($name:ident, $reg:literal) => {
        #[inline(always)]
        pub fn $name() -> u64 {
            let val: u64;

            // SAFETY: MRS of an immutable ID/configuration register. The value
            // is fixed at reset and never changes. nomem is safe — the
            // instruction does not access or affect memory.
            unsafe {
                core::arch::asm!(
                    concat!("mrs {val}, ", $reg),
                    val = out(reg) val,
                    options(nomem, nostack),
                );
            }

            val
        }
    };
}

/// Generate a write accessor for a system register.
/// Default options: `nostack` only (no `nomem`).
macro_rules! sysreg_write {
    ($name:ident, $reg:literal) => {
        #[inline(always)]
        pub fn $name(val: u64) {
            // SAFETY: MSR writes a GPR into a system register. All registers
            // below are writable from EL1. The caller is responsible for
            // providing a valid value and following with ISB if required.
            unsafe {
                core::arch::asm!(
                    concat!("msr ", $reg, ", {val}"),
                    val = in(reg) val,
                    options(nostack),
                );
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Barriers
// ---------------------------------------------------------------------------

/// Instruction Synchronization Barrier.
///
/// Forces the CPU to discard all prefetched instructions and re-fetch from
/// the current PC. Required after writes to system registers that affect
/// instruction execution (SCTLR, VBAR, TCR, TTBR, etc.).
#[inline(always)]
pub fn isb() {
    // SAFETY: ISB is a barrier hint. It synchronizes the instruction stream
    // but does not read or write memory. However, we omit `nomem` because
    // its purpose is to order operations — LLVM must not move memory
    // accesses past it.
    unsafe {
        core::arch::asm!("isb", options(nostack));
    }
}

/// Data Synchronization Barrier — full system scope.
///
/// Ensures all preceding memory accesses (loads, stores, cache maintenance)
/// complete before any subsequent instruction executes. Required before
/// enabling/disabling the MMU or after TLB invalidation.
#[inline(always)]
pub fn dsb_sy() {
    // SAFETY: DSB SY is a barrier hint that enforces memory ordering. It
    // does not access memory itself but prevents reordering. No `nomem`.
    unsafe {
        core::arch::asm!("dsb sy", options(nostack));
    }
}

/// Data Synchronization Barrier — inner-shareable scope.
///
/// Like `dsb_sy` but limited to the inner-shareable domain (all cores
/// sharing the same inner-shareable memory). Used after TLB invalidation
/// that targets the inner-shareable domain.
#[inline(always)]
pub fn dsb_ish() {
    // SAFETY: DSB ISH is a barrier hint. Same constraints as dsb_sy.
    unsafe {
        core::arch::asm!("dsb ish", options(nostack));
    }
}

/// Data Synchronization Barrier — inner-shareable, store-only.
///
/// Ensures all preceding stores in the inner-shareable domain complete
/// before any subsequent instruction. Used before TLB invalidation to
/// guarantee page table writes are visible to other cores' hardware
/// walkers.
#[inline(always)]
pub fn dsb_ishst() {
    // SAFETY: DSB ISHST is a store-only barrier hint. Same constraints as
    // dsb_ish but limited to store operations.
    unsafe {
        core::arch::asm!("dsb ishst", options(nostack));
    }
}

/// TLB Invalidate All — EL1, inner-shareable.
///
/// Invalidates all TLB entries for EL1 across all cores in the inner-
/// shareable domain. Must be followed by `dsb_ish(); isb();`.
#[inline(always)]
pub fn tlbi_vmalle1is() {
    // SAFETY: TLBI is a TLB maintenance instruction that invalidates cached
    // translations. It affects the memory system (future page walks will
    // re-read page tables). No `nomem` — LLVM must not reorder memory
    // accesses past this.
    unsafe {
        core::arch::asm!("tlbi vmalle1is", options(nostack));
    }
}

// ---------------------------------------------------------------------------
// Immutable registers (read-only at EL1, nomem safe)
// ---------------------------------------------------------------------------

sysreg_read_const!(mpidr_el1, "mpidr_el1");
sysreg_read_const!(current_el, "CurrentEL");
sysreg_read_const!(cntfrq_el0, "cntfrq_el0");
sysreg_read_const!(id_aa64mmfr0_el1, "id_aa64mmfr0_el1");
sysreg_read_const!(id_aa64isar0_el1, "id_aa64isar0_el1");

// ---------------------------------------------------------------------------
// Exception handling
// ---------------------------------------------------------------------------

sysreg_read!(esr_el1, "esr_el1");
sysreg_read!(far_el1, "far_el1");
sysreg_read!(elr_el1, "elr_el1");
sysreg_read!(spsr_el1, "spsr_el1");
sysreg_read!(daif, "daif");

sysreg_write!(set_vbar_el1, "vbar_el1");
sysreg_write!(set_elr_el1, "elr_el1");
sysreg_write!(set_spsr_el1, "spsr_el1");
sysreg_write!(set_daif, "daif");

/// Unmask IRQs (clear PSTATE.I). After this, the CPU will take IRQ exceptions.
#[inline(always)]
pub fn enable_irqs() {
    // SAFETY: DAIFClr with immediate #2 clears the I bit (bit 1 of the 4-bit
    // DAIF field). This enables IRQ delivery. No `nomem` — changing interrupt
    // masking has observable side effects.
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack));
    }
}

/// Mask IRQs (set PSTATE.I). After this, IRQs are held pending until unmasked.
#[inline(always)]
pub fn disable_irqs() {
    // SAFETY: DAIFSet with immediate #2 sets the I bit, masking IRQs.
    unsafe {
        core::arch::asm!("msr daifset, #2", options(nostack));
    }
}

// ---------------------------------------------------------------------------
// MMU and address translation
// ---------------------------------------------------------------------------

sysreg_read!(sctlr_el1, "sctlr_el1");
sysreg_read!(tcr_el1, "tcr_el1");
sysreg_read!(ttbr0_el1, "ttbr0_el1");
sysreg_read!(ttbr1_el1, "ttbr1_el1");
sysreg_read!(mair_el1, "mair_el1");

sysreg_write!(set_sctlr_el1, "sctlr_el1");
sysreg_write!(set_tcr_el1, "tcr_el1");
sysreg_write!(set_ttbr0_el1, "ttbr0_el1");
sysreg_write!(set_ttbr1_el1, "ttbr1_el1");
sysreg_write!(set_mair_el1, "mair_el1");

// ---------------------------------------------------------------------------
// Timer
// ---------------------------------------------------------------------------

sysreg_read!(cntpct_el0, "cntpct_el0");
sysreg_read!(cntvct_el0, "cntvct_el0");
sysreg_read!(cntv_ctl_el0, "cntv_ctl_el0");
sysreg_read!(cntv_cval_el0, "cntv_cval_el0");

sysreg_write!(set_cntv_ctl_el0, "cntv_ctl_el0");
sysreg_write!(set_cntv_cval_el0, "cntv_cval_el0");
sysreg_write!(set_cntv_tval_el0, "cntv_tval_el0");

// ---------------------------------------------------------------------------
// GICv3 CPU interface (ICC system registers)
// ---------------------------------------------------------------------------

sysreg_read!(icc_sre_el1, "icc_sre_el1");
sysreg_read!(icc_iar1_el1, "icc_iar1_el1");
sysreg_read!(icc_ctlr_el1, "icc_ctlr_el1");

sysreg_write!(set_icc_sre_el1, "icc_sre_el1");
sysreg_write!(set_icc_pmr_el1, "icc_pmr_el1");
sysreg_write!(set_icc_bpr1_el1, "icc_bpr1_el1");
sysreg_write!(set_icc_igrpen1_el1, "icc_igrpen1_el1");
sysreg_write!(set_icc_eoir1_el1, "icc_eoir1_el1");

// ---------------------------------------------------------------------------
// TLBI operations (per-page, per-ASID)
// ---------------------------------------------------------------------------

/// TLBI VAE1IS — invalidate a single page by VA + ASID (inner-shareable).
/// Argument format: ASID[63:48] | VA[43:12] (the VA shifted right by 12).
#[inline(always)]
pub fn tlbi_vae1is(asid_va: u64) {
    // SAFETY: TLBI invalidates a single TLB entry. It affects the memory
    // translation system. No nomem.
    unsafe {
        core::arch::asm!("tlbi vae1is, {val}", val = in(reg) asid_va, options(nostack));
    }
}

/// TLBI ASIDE1IS — invalidate all entries for an ASID (inner-shareable).
#[inline(always)]
pub fn tlbi_aside1is(asid: u64) {
    let val = asid << 48;
    // SAFETY: TLBI invalidates all TLB entries for the given ASID.
    unsafe {
        core::arch::asm!("tlbi aside1is, {val}", val = in(reg) val, options(nostack));
    }
}

// ---------------------------------------------------------------------------
// FP/SIMD control
// ---------------------------------------------------------------------------

sysreg_read!(cpacr_el1, "cpacr_el1");
sysreg_write!(set_cpacr_el1, "cpacr_el1");

// ---------------------------------------------------------------------------
// Per-CPU data
// ---------------------------------------------------------------------------

sysreg_read!(tpidr_el1, "tpidr_el1");
sysreg_write!(set_tpidr_el1, "tpidr_el1");

// ---------------------------------------------------------------------------
// Entropy (FEAT_RNG)
// ---------------------------------------------------------------------------

/// Read a 64-bit random number from the hardware RNG (RNDR).
///
/// Returns `Some(value)` on success, `None` if the entropy pool is
/// temporarily exhausted. Requires FEAT_RNG — check `id_aa64isar0_el1()`
/// bits \[63:60\] before calling.
#[inline(always)]
pub fn rndr() -> Option<u64> {
    let val: u64;
    let success: u64;

    // SAFETY: RNDR (S3_3_C2_C4_0) reads from the hardware RNG. It sets
    // NZCV: Z=0 on success, Z=1 on failure (entropy exhausted). The `cset`
    // captures the Z flag immediately. No `nomem` — RNDR has side effects
    // (drains the entropy pool) and returns a different value each call.
    unsafe {
        core::arch::asm!(
            "mrs {val}, s3_3_c2_c4_0",
            "cset {success}, ne",
            val = out(reg) val,
            success = out(reg) success,
            options(nostack),
        );
    }

    if success != 0 { Some(val) } else { None }
}
