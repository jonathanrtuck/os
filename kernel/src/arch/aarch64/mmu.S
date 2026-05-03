// mmu.S — MMU transition trampoline.
//
// This is the single point where the CPU transitions from physical to
// virtual addressing. It is deliberately in assembly because:
//
//   1. The instruction after MSR SCTLR_EL1 must be at an address that
//      is valid in BOTH the pre-enable (physical) and post-enable
//      (virtual) worlds. With an identity map, this is trivially true.
//      With upper-half or KASLR mappings, this function will need to
//      be in a page that is mapped at both addresses.
//
//   2. No compiler-generated code (stack spills, function calls) can
//      appear between the SCTLR write and the ISB. Assembly guarantees
//      the exact instruction sequence.
//
// Current: identity map (VA == PA), so we just enable and return.
// Future:  takes a virtual entry point and branches there after enable.

.section .text
.global __mmu_enable

// __mmu_enable(sctlr: u64)
//
// Enables the MMU by writing SCTLR_EL1 with the given value, then
// synchronizes the instruction stream. The caller must have already
// configured MAIR_EL1, TCR_EL1, TTBR0_EL1, and invalidated TLBs.
//
// x0 = new SCTLR_EL1 value (with M bit set)
__mmu_enable:
    // Ensure all prior page table writes are visible to the MMU walker
    // before we flip the switch.
    dsb     sy
    isb

    // Enable the MMU. The next instruction fetch is translated through
    // the page tables. With identity mapping, the VA equals the PA of
    // the ISB below, so execution continues seamlessly.
    msr     sctlr_el1, x0
    isb

    ret
