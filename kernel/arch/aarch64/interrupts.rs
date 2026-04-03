//! AArch64 interrupt masking primitives.
//!
//! Provides `IrqState` and mask/restore operations using the DAIF register.

/// Saved interrupt state (DAIF register value on aarch64).
///
/// Opaque type — only `mask_all` creates it, only `restore` consumes it.
/// Copy is intentional: the guard stores this and passes a copy to `restore`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct IrqState(u64);

/// Save the current interrupt state and mask IRQs (set DAIF.I).
///
/// Returns the saved state for later restoration via `restore()`.
///
/// IMPORTANT: no `nomem` — LLVM must treat these as memory barriers.
/// With `nomem`, LLVM can reorder memory operations past the DAIF
/// masking, allowing lock-protected accesses to execute with interrupts
/// enabled (race condition that manifests at opt-level 3).
#[inline(always)]
pub fn mask_all() -> IrqState {
    let saved_daif: u64;

    // SAFETY: Reading and writing DAIF is valid at EL1. `nostack` is
    // correct (no stack manipulation). No `nomem` — intentional, see above
    // and Fix 6 analysis.
    unsafe {
        core::arch::asm!("mrs {}, daif", out(reg) saved_daif, options(nostack));
        core::arch::asm!("msr daifset, #2", options(nostack));
    }

    IrqState(saved_daif)
}
/// Restore a previously saved interrupt state.
///
/// SAFETY contract: `saved` must be a value returned by `mask_all()`.
/// This is enforced by the type system — `IrqState` is only constructible
/// via `mask_all()`.
#[inline(always)]
pub fn restore(saved: IrqState) {
    // SAFETY: Restoring DAIF to a value previously read from this register
    // is always valid at EL1. `nostack` is correct (no stack manipulation).
    // No `nomem` — the compiler must not reorder memory accesses past this
    // IRQ state restoration (Fix 6).
    unsafe {
        core::arch::asm!("msr daif, {}", in(reg) saved.0, options(nostack));
    }
}
