//! AArch64 per-core identity.

/// Read the current core's MPIDR affinity (bits [7:0]).
#[inline(always)]
pub fn core_id() -> u32 {
    let mpidr: u64;

    // SAFETY: MPIDR_EL1 is a read-only CPU identification register.
    // It does not access memory (nomem correct) and has no side effects.
    // The register value is stable for the lifetime of the core.
    // nostack is correct — no stack operations in the asm block.
    unsafe {
        core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nostack, nomem));
    }

    (mpidr & 0xFF) as u32
}
