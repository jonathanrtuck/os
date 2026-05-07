// exception.S — AArch64 exception vector table and context save/restore.
//
// The vector table has 16 entries (4 exception types x 4 source contexts),
// each 128 bytes (0x80). VBAR_EL1 must be 2KB-aligned.
//
// Each entry saves x0-x1 to get scratch registers, stashes the source ID
// in the TrapFrame's pad slot, and branches to a common handler that saves
// the remaining context and calls into Rust.
//
// TrapFrame layout (832 bytes, 16-byte aligned):
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
// FP/SIMD state: conditional save/restore. FP is saved only when CPACR_EL1
// has FP enabled at entry (the thread was actively using FP). When lazy FP
// trapping is active (CPACR.FPEN=0), the save is skipped at entry and the
// entry-time decision is recorded in fp_saved [sp+824]. The restore path
// checks fp_saved, NOT the live CPACR — handle_fp_trap may enable CPACR
// mid-exception without saving FP to the TrapFrame.
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
//   [x] SP_EL0 (user stack pointer — saved/restored for EL0 return)

// ---------------------------------------------------------------------------
// Vector entry macro — inlined at each of the 16 vector positions.
//
// Allocates the TrapFrame, saves x0-x1 as scratch, writes the source ID
// into the pad slot, and branches to the common handler. Five instructions
// (20 bytes), well within the 128-byte entry limit.
// ---------------------------------------------------------------------------
.macro VECTOR_ENTRY source
.align 7
    sub     sp, sp, #832
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
    // Source 8: Synchronous — SVC fast path or full save.
    // Check EC before committing to a save path. Save x0-x1 first
    // (needed to recover user args after the ESR check).
.align 7
    stp     x0, x1, [sp, #-16]!    // Save x0-x1 on kernel stack (temporary)
    mrs     x0, esr_el1
    lsr     x0, x0, #26            // EC field [31:26]
    cmp     x0, #0x15              // SVC from AArch64?
    b.ne    __el0_sync_slow_from_fast
    b       __svc_fast

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

    // Save SP_EL0 (user stack pointer) — needed for EL0 return.
    mrs     x9, sp_el0
    str     x9, [sp, #816]

    // Conditional FP save: skip if FP is disabled (lazy mode active).
    // Record the decision at [sp+824] (fp_saved) so the restore path uses
    // the entry-time state, not the live CPACR (which handle_fp_trap may
    // enable mid-exception).
    mrs     x9, cpacr_el1
    ubfx    x9, x9, #20, #2        // Extract FPEN field [21:20]
    str     x9, [sp, #824]          // fp_saved: non-zero if we save FP
    cbz     x9, .Lskip_fp_save     // If disabled, skip FP save

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

.Lskip_fp_save:

.ifdef PROFILE
    mrs     x9, cntvct_el0
    adrp    x10, __profile_buf
    add     x10, x10, :lo12:__profile_buf
    str     x9, [x10, #(0 * 8)]        // slot ASM_BEFORE_HANDLER
.endif

    // Call Rust: exception_handler(frame: &mut TrapFrame, source: u64)
    mov     x0, sp
    ldr     x1, [sp, #280]
    bl      exception_handler

.ifdef PROFILE
    mrs     x9, cntvct_el0
    adrp    x10, __profile_buf
    add     x10, x10, :lo12:__profile_buf
    str     x9, [x10, #(1 * 8)]        // slot ASM_AFTER_HANDLER
.endif

    // Conditional FP restore: use the entry-time decision, not live CPACR.
    // handle_fp_trap may have enabled CPACR mid-exception without saving
    // FP to the TrapFrame — restoring unsaved data would corrupt FP state.
    ldr     x9, [sp, #824]          // fp_saved: was FP saved at entry?
    cbz     x9, .Lskip_fp_restore

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

.Lskip_fp_restore:

    // Restore SP_EL0.
    ldr     x9, [sp, #816]
    msr     sp_el0, x9

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
    add     sp, sp, #832
    eret

// ---------------------------------------------------------------------------
// SVC fast path — minimal register save for syscalls.
//
// On entry: x0-x1 saved at [sp], ESR already checked (EC = SVC).
// User registers: x0-x5 = args (x0-x1 saved, need to reload), x8 = syscall num.
//
// Save only what's needed:
//   - ELR_EL1 + SPSR_EL1 (for ERET)
//   - SP_EL0 (for EL0 return)
//   - x29 + x30 (ABI: frame pointer + link register for bl)
//   - User x0-x1 (already on stack)
//
// Callee-saved x19-x28 are preserved by the Rust calling convention.
// ---------------------------------------------------------------------------
__svc_fast:
.ifdef PROFILE
    mrs     x9, cntvct_el0
    adrp    x10, __profile_buf
    add     x10, x10, :lo12:__profile_buf
    str     x9, [x10, #(2 * 8)]        // slot ASM_SVC_ENTRY
.endif

    // Allocate mini-frame: 48 bytes (already have 16 from x0-x1 push).
    sub     sp, sp, #48

    // Save ELR, SPSR.
    mrs     x0, elr_el1
    mrs     x1, spsr_el1
    stp     x0, x1, [sp, #0]       // [sp+0] = ELR, [sp+8] = SPSR

    // Save SP_EL0.
    mrs     x0, sp_el0
    str     x0, [sp, #16]          // [sp+16] = SP_EL0

    // Save x29, x30 (needed for bl to Rust).
    stp     x29, x30, [sp, #24]    // [sp+24] = x29, [sp+32] = x30

    // Reload user's x0-x1 from temporary save area.
    ldp     x0, x1, [sp, #48]      // x0-x1 were pushed at [sp+48]

    // Now: x0-x5 = user args, x8 = syscall number.
    // Call Rust: svc_fast_handler(a0, a1, a2, a3, a4, a5, syscall_num)
    mov     x6, x8                  // x6 = syscall number

.ifdef PROFILE
    mrs     x9, cntvct_el0
    adrp    x10, __profile_buf
    add     x10, x10, :lo12:__profile_buf
    str     x9, [x10, #(3 * 8)]        // slot ASM_SVC_BEFORE_BL
.endif

    bl      svc_fast_handler

.ifdef PROFILE
    mrs     x9, cntvct_el0
    adrp    x10, __profile_buf
    add     x10, x10, :lo12:__profile_buf
    str     x9, [x10, #(4 * 8)]        // slot ASM_SVC_AFTER_BL
.endif

    // x0 = error, x1 = value (returned from Rust).

    // Restore x29, x30.
    ldp     x29, x30, [sp, #24]

    // Restore SP_EL0.
    ldr     x9, [sp, #16]
    msr     sp_el0, x9

    // Mask IRQs before writing ELR/SPSR — an interrupt between
    // MSR ELR_EL1 and ERET would overwrite ELR with the interrupted
    // kernel PC, causing ERET to jump to a kernel address at EL0.
    msr     daifset, #2

    // Restore ELR, SPSR.
    ldp     x9, x10, [sp, #0]
    msr     elr_el1, x9
    msr     spsr_el1, x10

    // Deallocate mini-frame + temporary x0-x1 save.
    add     sp, sp, #64

    eret

// Slow path entry when EC != SVC. Undo the x0-x1 save and enter full path.
__el0_sync_slow_from_fast:
    ldp     x0, x1, [sp], #16      // Restore x0-x1
    sub     sp, sp, #832            // Allocate full TrapFrame
    stp     x0, x1, [sp, #0]       // Save x0-x1 into TrapFrame
    mov     x0, #8                  // source = 8 (EL0/64 Sync)
    str     x0, [sp, #280]
    b       __exception_common
