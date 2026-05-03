// secondary_entry.S — Entry point for secondary cores after PSCI CPU_ON.
//
// On entry (guaranteed by PSCI spec DEN0022E §5.1.3):
//   - Execution at EL1 (hypervisor performed EL2→EL1 transition)
//   - MMU off, caches off
//   - x0 = context_id (BSP passes stack_top for this core)
//   - All interrupts masked (DAIF = 0b1111)
//
// Responsibilities:
//   1. Set per-core stack pointer (from x0)
//   2. Read core ID from MPIDR
//   3. Enable FP/ASIMD
//   4. Jump to Rust secondary_main(core_id)

.section .text
.global __secondary_entry
__secondary_entry:
    // x0 = stack_top (passed as PSCI context_id by BSP).
    // Use SP_EL1 (kernel stack pointer).
    msr     spsel, #1
    isb
    mov     sp, x0

    // Extract core_id from MPIDR_EL1 Aff0 field.
    mrs     x0, mpidr_el1
    and     x0, x0, #0xFF

    // Enable FP/ASIMD — otherwise any float instruction faults.
    mrs     x1, cpacr_el1
    orr     x1, x1, #(3 << 20)
    msr     cpacr_el1, x1
    isb

    // Jump to Rust. x0 = core_id.
    bl      secondary_main

    // Should not return, but park defensively.
.Lsecondary_park:
    wfe
    b       .Lsecondary_park
