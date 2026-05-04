// boot.S — Minimal boot trampoline for QEMU virt (aarch64).
//
// Responsibilities (and nothing more):
//   1. Park secondary cores (defensive — with PSCI CPU_ON, secondaries start at
//      __secondary_entry, not _start. The park catches firmware that
//      incorrectly boots all cores at the same entry point)
//   2. Set up a stack
//   3. Zero BSS
//   4. Drop from EL2 to EL1 (QEMU enters at EL2 by default)
//   5. Enable FP/ASIMD
//   6. Jump to Rust
//
// The kernel is linked at upper-half VAs but this code executes at the
// physical load address (identity-mapped via TTBR0 before MMU enable).
// All address references use adrp/add (PC-relative) which resolves to
// physical addresses regardless of the link VMA.

.section .text.boot, "ax"
.global _start
_start:
    // Save the DTB pointer (passed in x0 by firmware/hypervisor) before
    // any register use. x19 is callee-saved and survives the entire boot
    // sequence including the EL2→EL1 ERET.
    mov     x19, x0

    // Park secondary cores — only core 0 boots.
    mrs     x0, mpidr_el1
    and     x0, x0, #0xFF
    cbnz    x0, .Lpark

    // Set up a physical stack for early boot (PC-relative: resolves to PA).
    adrp    x0, __stack_top
    add     x0, x0, :lo12:__stack_top
    mov     sp, x0

    // Zero BSS (PC-relative addresses).
    adrp    x1, __bss_start
    add     x1, x1, :lo12:__bss_start
    adrp    x2, __bss_end
    add     x2, x2, :lo12:__bss_end
.Lzero_bss:
    cmp     x1, x2
    b.ge    .Lbss_done
    str     xzr, [x1], #8
    b       .Lzero_bss
.Lbss_done:

    // Check if we entered at EL2 (QEMU default). If so, drop to EL1.
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #2
    b.ne    .Lel1

    // --- EL2 → EL1 transition ---

    // HCR_EL2.RW = 1: EL1 executes in AArch64.
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0

    // Let EL1 access the physical timer.
    mrs     x0, cnthctl_el2
    orr     x0, x0, #3
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr

    // Don't trap FP/ASIMD at EL2.
    mrs     x0, cptr_el2
    bic     x0, x0, #(1 << 10)
    msr     cptr_el2, x0

    // Return to .Lel1 at EL1h with interrupts masked.
    // DAIF = 0b1111 (bits [9:6]) masks all async exceptions until the
    // kernel installs VBAR_EL1 and SP_EL1. Without this, any pending
    // IRQ/FIQ/SError between the eret and exception::init() would vector
    // through an uninitialized table with a zero stack pointer.
    mov     x0, #(0b1111 << 6 | 0b0101)
    msr     spsr_el2, x0
    adr     x0, .Lel1
    msr     elr_el2, x0
    isb
    eret

.Lel1:
    // Use SP_EL1.
    msr     spsel, #1
    isb
    adrp    x0, __stack_top
    add     x0, x0, :lo12:__stack_top
    mov     sp, x0

    // Enable FP/ASIMD (otherwise any float instruction faults).
    mrs     x0, cpacr_el1
    orr     x0, x0, #(3 << 20)
    msr     cpacr_el1, x0
    isb

    // Jump to Rust with DTB pointer as first argument.
    mov     x0, x19
    bl      kernel_main
    b       .Lpark

.Lpark:
    wfe
    b       .Lpark
