// secondary_entry.S — Entry point for secondary cores after PSCI CPU_ON.
//
// On entry (PSCI spec DEN0022E §5.1.3):
//   - x0 = context_id (BSP passes stack_top for this core)
//   - All interrupts masked (DAIF = 0b1111)
//   - May enter at EL2 (hypervisor default) or EL1
//
// Responsibilities:
//   1. Drop to EL1 if entered at EL2 (same as BSP boot.S)
//   2. Set per-core stack pointer (from x0/x20)
//   3. Read core ID from MPIDR
//   4. Enable FP/ASIMD
//   5. Jump to Rust secondary_main(core_id)

.section .text
.global __secondary_entry
__secondary_entry:
    // Save context_id (stack_top) in a callee-saved register that
    // survives the EL2→EL1 ERET.
    mov     x20, x0

    // Check if we entered at EL2. If so, drop to EL1.
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #2
    b.ne    .Lsec_el1

    // --- EL2 → EL1 transition (same as BSP boot.S) ---
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0

    mrs     x0, cnthctl_el2
    orr     x0, x0, #3
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr

    mrs     x0, cptr_el2
    bic     x0, x0, #(1 << 10)
    msr     cptr_el2, x0

    // Return to .Lsec_el1 at EL1h with interrupts masked.
    mov     x0, #(0b1111 << 6 | 0b0101)
    msr     spsr_el2, x0
    adr     x0, .Lsec_el1
    msr     elr_el2, x0
    isb
    eret

.Lsec_el1:
    // Use SP_EL1 (kernel stack pointer).
    msr     spsel, #1
    isb
    mov     sp, x20

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
