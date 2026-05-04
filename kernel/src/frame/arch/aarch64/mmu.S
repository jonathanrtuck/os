// mmu.S — MMU enable trampoline.
//
// This function lives in .text.boot, which is identity-mapped in TTBR0
// at the physical load address. After the MSR SCTLR_EL1 that enables the
// MMU, the ISB resolves because the physical address is still valid in
// TTBR0. The ret returns to the Rust caller (configure_and_enable),
// which then transitions to the TTBR1 upper-half address space.
//
// This must be in assembly because no compiler-generated code can appear
// between the SCTLR write and the ISB.

.section .text.boot, "ax"
.global __mmu_enable

// __mmu_enable(sctlr: u64)
//
// Enables the MMU by writing SCTLR_EL1. Returns to caller.
// Caller must have configured MAIR, TCR, TTBR0, TTBR1, and flushed TLBs.
//
// x0 = new SCTLR_EL1 value (with M bit set)
__mmu_enable:
    dsb     sy
    isb
    msr     sctlr_el1, x0
    isb
    ret
