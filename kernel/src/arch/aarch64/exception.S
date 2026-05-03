// exception.S — AArch64 exception vector table and context save/restore.
//
// The vector table has 16 entries (4 exception types x 4 source contexts),
// each 128 bytes (0x80). VBAR_EL1 must be 2KB-aligned.
//
// Each entry saves x0-x1 to get scratch registers, stashes the source ID
// in the TrapFrame's pad slot, and branches to a common handler that saves
// the remaining context and calls into Rust.
//
// TrapFrame layout (816 bytes, 16-byte aligned):
//
//   Offset  Field
//   0       x0
//   8       x1
//   ...
//   232     x29
//   240     x30
//   248     elr_el1
//   256     spsr_el1
//   264     esr_el1
//   272     far_el1
//   280     (source ID, reused as pad for alignment)
//   288     q0  (128-bit FP/SIMD)
//   304     q1
//   ...
//   768     q30
//   784     q31
//   800     fpcr
//   808     fpsr
//
// Optimization note: FP/SIMD state is saved/restored eagerly on every
// exception. This is simple and correct but costs ~100 cycles per entry/exit.
// When this becomes a bottleneck, switch to lazy save: disable FP access via
// CPACR_EL1, trap on first FP use, and save/restore only when the interrupted
// context actually used FP registers.
//
// Register state checklist (audit whenever exception entry/exit changes):
//   [x] GPRs x0-x30
//   [x] ELR_EL1 (return address)
//   [x] SPSR_EL1 (saved PSTATE)
//   [x] ESR_EL1 (exception syndrome — saved for diagnostics, not restored)
//   [x] FAR_EL1 (fault address — saved for diagnostics, not restored)
//   [x] FP/SIMD q0-q31 (128-bit NEON/VFP registers)
//   [x] FPCR (floating-point control register)
//   [x] FPSR (floating-point status register)
//   [ ] SVE state (not applicable — SVE not enabled)
//   [ ] SP_EL0 (only needed when handling exceptions from EL0)

// ---------------------------------------------------------------------------
// Vector entry macro — inlined at each of the 16 vector positions.
//
// Allocates the TrapFrame, saves x0-x1 as scratch, writes the source ID
// into the pad slot, and branches to the common handler. Five instructions
// (20 bytes), well within the 128-byte entry limit.
// ---------------------------------------------------------------------------
.macro VECTOR_ENTRY source
.align 7
    sub     sp, sp, #816
    stp     x0, x1, [sp, #16 * 0]
    mov     x0, #\source
    str     x0, [sp, #280]
    b       __exception_common
.endm

// ---------------------------------------------------------------------------
// Vector table — 16 entries, 2KB aligned.
// ---------------------------------------------------------------------------
.section .text
.align 11
.global __vectors

__vectors:
    // Current EL, SP_EL0 (EL1t — kernel does not use this mode)
    VECTOR_ENTRY 0      // Synchronous
    VECTOR_ENTRY 1      // IRQ
    VECTOR_ENTRY 2      // FIQ
    VECTOR_ENTRY 3      // SError

    // Current EL, SP_EL1 (EL1h — kernel's active exception vectors)
    VECTOR_ENTRY 4      // Synchronous
    VECTOR_ENTRY 5      // IRQ
    VECTOR_ENTRY 6      // FIQ
    VECTOR_ENTRY 7      // SError

    // Lower EL, AArch64 (EL0 — future userspace)
    VECTOR_ENTRY 8      // Synchronous
    VECTOR_ENTRY 9      // IRQ
    VECTOR_ENTRY 10     // FIQ
    VECTOR_ENTRY 11     // SError

    // Lower EL, AArch32 (not supported)
    VECTOR_ENTRY 12     // Synchronous
    VECTOR_ENTRY 13     // IRQ
    VECTOR_ENTRY 14     // FIQ
    VECTOR_ENTRY 15     // SError

// ---------------------------------------------------------------------------
// Common exception handler — shared by all 16 vector entries.
//
// On entry: frame allocated, x0-x1 saved, source ID at [sp+280].
// Saves remaining GPRs + system registers + FP/SIMD, calls Rust, restores
// everything, and returns via eret.
// ---------------------------------------------------------------------------
__exception_common:
    // Save x2-x29 (x0-x1 already saved by vector entry).
    stp     x2, x3, [sp, #16 * 1]
    stp     x4, x5, [sp, #16 * 2]
    stp     x6, x7, [sp, #16 * 3]
    stp     x8, x9, [sp, #16 * 4]
    stp     x10, x11, [sp, #16 * 5]
    stp     x12, x13, [sp, #16 * 6]
    stp     x14, x15, [sp, #16 * 7]
    stp     x16, x17, [sp, #16 * 8]
    stp     x18, x19, [sp, #16 * 9]
    stp     x20, x21, [sp, #16 * 10]
    stp     x22, x23, [sp, #16 * 11]
    stp     x24, x25, [sp, #16 * 12]
    stp     x26, x27, [sp, #16 * 13]
    stp     x28, x29, [sp, #16 * 14]

    // Save x30 and ELR_EL1. Using x9 as scratch (already saved above).
    mrs     x9, elr_el1
    stp     x30, x9, [sp, #16 * 15]

    // Save SPSR_EL1 and ESR_EL1.
    mrs     x9, spsr_el1
    mrs     x10, esr_el1
    stp     x9, x10, [sp, #16 * 16]

    // Save FAR_EL1.
    mrs     x9, far_el1
    str     x9, [sp, #272]

    // Save FP/SIMD registers q0-q31 (128-bit each).
    stp     q0, q1, [sp, #288]
    stp     q2, q3, [sp, #320]
    stp     q4, q5, [sp, #352]
    stp     q6, q7, [sp, #384]
    stp     q8, q9, [sp, #416]
    stp     q10, q11, [sp, #448]
    stp     q12, q13, [sp, #480]
    stp     q14, q15, [sp, #512]
    stp     q16, q17, [sp, #544]
    stp     q18, q19, [sp, #576]
    stp     q20, q21, [sp, #608]
    stp     q22, q23, [sp, #640]
    stp     q24, q25, [sp, #672]
    stp     q26, q27, [sp, #704]
    stp     q28, q29, [sp, #736]
    stp     q30, q31, [sp, #768]

    // Save FPCR and FPSR.
    mrs     x9, fpcr
    str     x9, [sp, #800]
    mrs     x9, fpsr
    str     x9, [sp, #808]

    // Call Rust: exception_handler(frame: &mut TrapFrame, source: u64)
    mov     x0, sp
    ldr     x1, [sp, #280]
    bl      exception_handler

    // Restore FPCR and FPSR.
    ldr     x9, [sp, #800]
    msr     fpcr, x9
    ldr     x9, [sp, #808]
    msr     fpsr, x9

    // Restore FP/SIMD registers q0-q31.
    ldp     q0, q1, [sp, #288]
    ldp     q2, q3, [sp, #320]
    ldp     q4, q5, [sp, #352]
    ldp     q6, q7, [sp, #384]
    ldp     q8, q9, [sp, #416]
    ldp     q10, q11, [sp, #448]
    ldp     q12, q13, [sp, #480]
    ldp     q14, q15, [sp, #512]
    ldp     q16, q17, [sp, #544]
    ldp     q18, q19, [sp, #576]
    ldp     q20, q21, [sp, #608]
    ldp     q22, q23, [sp, #640]
    ldp     q24, q25, [sp, #672]
    ldp     q26, q27, [sp, #704]
    ldp     q28, q29, [sp, #736]
    ldp     q30, q31, [sp, #768]

    // Restore ELR_EL1 and SPSR_EL1 (using x9 as scratch — not yet restored).
    ldr     x9, [sp, #248]
    msr     elr_el1, x9
    ldr     x9, [sp, #256]
    msr     spsr_el1, x9

    // Restore all GPRs.
    ldp     x0, x1, [sp, #16 * 0]
    ldp     x2, x3, [sp, #16 * 1]
    ldp     x4, x5, [sp, #16 * 2]
    ldp     x6, x7, [sp, #16 * 3]
    ldp     x8, x9, [sp, #16 * 4]
    ldp     x10, x11, [sp, #16 * 5]
    ldp     x12, x13, [sp, #16 * 6]
    ldp     x14, x15, [sp, #16 * 7]
    ldp     x16, x17, [sp, #16 * 8]
    ldp     x18, x19, [sp, #16 * 9]
    ldp     x20, x21, [sp, #16 * 10]
    ldp     x22, x23, [sp, #16 * 11]
    ldp     x24, x25, [sp, #16 * 12]
    ldp     x26, x27, [sp, #16 * 13]
    ldp     x28, x29, [sp, #16 * 14]
    ldr     x30, [sp, #240]

    // Deallocate frame and return from exception.
    add     sp, sp, #816
    eret
