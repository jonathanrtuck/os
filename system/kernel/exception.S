// ==========================================================================
// exception.S — Exception vectors, context switch, fatal handler.
//
// Lives in .text (upper VA, TTBR1). Linked after the kernel transitions
// to TTBR1. All addresses here are kernel virtual addresses.
//
// Rust entry points:
//   irq_handler(*mut Context) -> *const Context
//   svc_handler(*mut Context) -> *const Context
//
// AUDIT: 2026-03-11 — Comprehensive review of exception handling.
//   Vector table: .align 11 (2 KiB, correct for VBAR_EL1). 16 entries ×
//   0x80 bytes. All entry types covered (Sync/IRQ/FIQ/SError × 4 groups). ✓
//   save_context: x9/x10 spilled to stack before use as temps. All 31 GPRs
//   saved. SP restored via add sp, #0x10 (compensates for temp spill).
//   ELR, SPSR, SP_EL0, TPIDR_EL0 saved. All 32 NEON q-regs + FPCR/FPSR
//   saved. Context pointer returned in x0. ✓
//   restore_context_and_eret: SP, system regs, NEON, GPRs restored. x19
//   loaded last (self-referential — base register). x16 loaded separately
//   (not paired) — valid, no alignment issue. ERET at end. ✓
//   SVC handler: ESR_EL1 EC field extracted correctly (>> 26, cmp 0x15).
//   x10 spill/restore around check prevents register corruption. ✓
//   Fatal handler: Emergency stack switch with per-core calculation
//   ((core_id+1) * 4096). Diagnostic state (SP, LR, TPIDR) captured
//   before stack switch. ✓
//   exc_stacks: .bss.exc_stacks section, .align 12 (4 KiB), 8 cores. ✓
//   Context offsets: compile-time assertions in context.rs enforce match. ✓
//   No bugs found.
// ==========================================================================

// --------------------------------------------------------------------------
// Context struct field offsets (must match main.rs Context).
// --------------------------------------------------------------------------

.section .text

.equ CTX_X,         0x000
.equ CTX_SP,        0x0F8
.equ CTX_ELR,       0x100
.equ CTX_SPSR,      0x108
.equ CTX_SP_EL0,    0x110
.equ CTX_TPIDR_EL0, 0x118
.equ CTX_Q,         0x120
.equ CTX_FPCR,      0x320
.equ CTX_FPSR,      0x328
.equ CTX_SIZE,      0x330
// --------------------------------------------------------------------------
// UART helpers (PL011 at kernel VA = phys + KERNEL_VA_OFFSET)
// --------------------------------------------------------------------------
.equ UART0_BASE_VA, 0xFFFF000009000000
.equ UART0_DR,   0x00
.equ UART0_FR,   0x18
.equ UART0_TXFF, (1 << 5)

uart_putc:
1:
  ldr w1, [x2, #UART0_FR]
  tst w1, #UART0_TXFF
  b.ne 1b
  str w0, [x2, #UART0_DR]
  ret

uart_puts:
  stp x19, x30, [sp, #-16]!
  mov x19, x0
1:
  ldrb w3, [x19], #1
  cbz w3, 2f
  cmp w3, #10
  b.ne 3f
  mov w0, #13
  bl uart_putc
3:
  mov w0, w3
  bl uart_putc
  b 1b
2:
  ldp x19, x30, [sp], #16
  ret

hex_nibble:
  cmp w0, #9
  ble 1f
  add w0, w0, #55
  ret
1:
  add w0, w0, #48
  ret

uart_puthex32:
  stp x29, x30, [sp, #-16]!
  mov w4, w0
  mov w5, #28
1:
  lsr w0, w4, w5
  and w0, w0, #0xF
  bl hex_nibble
  bl uart_putc
  subs w5, w5, #4
  b.ge 1b
  ldp x29, x30, [sp], #16
  ret

uart_puthex64:
  stp x19, x30, [sp, #-16]!
  mov x19, x0
  lsr x0, x19, #32
  bl uart_puthex32
  mov w0, w19
  bl uart_puthex32
  ldp x19, x30, [sp], #16
  ret

// --------------------------------------------------------------------------
// Exception vectors (AArch64): 16 entries, 0x80 bytes each.
// --------------------------------------------------------------------------

.align 11
.global vectors
vectors:
  // 0x000: Current EL, SP0, Sync
  b exc_sync
  .space 0x80 - 4
  // 0x080: Current EL, SP0, IRQ
  b exc_irq
  .space 0x80 - 4
  // 0x100: Current EL, SP0, FIQ
  b exc_fiq
  .space 0x80 - 4
  // 0x180: Current EL, SP0, SError
  b exc_serr
  .space 0x80 - 4

  // 0x200: Current EL, SPx, Sync
  b exc_sync
  .space 0x80 - 4
  // 0x280: Current EL, SPx, IRQ
  b exc_irq
  .space 0x80 - 4
  // 0x300: Current EL, SPx, FIQ
  b exc_fiq
  .space 0x80 - 4
  // 0x380: Current EL, SPx, SError
  b exc_serr
  .space 0x80 - 4

  // 0x400: Lower EL (AArch64), Sync
  b exc_lower_sync
  .space 0x80 - 4
  // 0x480: Lower EL (AArch64), IRQ
  b exc_irq
  .space 0x80 - 4
  // 0x500: Lower EL (AArch64), FIQ
  b exc_fiq
  .space 0x80 - 4
  // 0x580: Lower EL (AArch64), SError
  b exc_serr
  .space 0x80 - 4

  // 0x600: Lower EL (AArch32), Sync
  b exc_sync
  .space 0x80 - 4
  // 0x680: Lower EL (AArch32), IRQ
  b exc_irq
  .space 0x80 - 4
  // 0x700: Lower EL (AArch32), FIQ
  b exc_fiq
  .space 0x80 - 4
  // 0x780: Lower EL (AArch32), SError
  b exc_serr
  .space 0x80 - 4

// --------------------------------------------------------------------------
// Fatal exception handler (EL1 faults)
//
// Switches to a per-core emergency stack before calling Rust. The original
// SP may be corrupted (e.g. kernel stack overflow hit the guard page).
// --------------------------------------------------------------------------

exc_sync:
  mov w3, #0              // type = sync
  b exc_fatal
exc_fiq:
  mov w3, #1              // type = FIQ
  b exc_fatal
exc_serr:
  mov w3, #2              // type = SError

exc_fatal:
  // Capture diagnostic state before switching stacks.
  mov x6, sp
  mov x7, x30            // LR at fault point (who called address 0?)
  mrs x8, tpidr_el1      // current thread Context pointer

  // Switch to per-core emergency stack (4 KiB each, in .bss).
  mrs x4, mpidr_el1
  and x4, x4, #0xFF      // core_id
  add x4, x4, #1         // (core_id + 1) → stack top
  lsl x4, x4, #12        // × 4096
  ldr x5, =__exc_stacks
  add sp, x5, x4

  // Call Rust: kernel_fault_handler(esr, elr, far, type, sp, lr, tpidr) -> !
  mrs x0, esr_el1
  mrs x1, elr_el1
  mrs x2, far_el1
  // x3 already set (exception type)
  mov x4, x6             // original SP
  mov x5, x7             // original LR (x30)
  mov x6, x8             // TPIDR_EL1 (current thread)
  bl kernel_fault_handler
  b .                     // Unreachable (handler panics)

// --------------------------------------------------------------------------
// Context save/restore macros
// --------------------------------------------------------------------------

.macro save_context
  sub sp, sp, #0x10
  str x9,  [sp, #0x00]
  str x10, [sp, #0x08]

  mrs x9, tpidr_el1

  stp x0,  x1,  [x9, #(CTX_X + 0*8)]
  stp x2,  x3,  [x9, #(CTX_X + 2*8)]
  stp x4,  x5,  [x9, #(CTX_X + 4*8)]
  stp x6,  x7,  [x9, #(CTX_X + 6*8)]
  str x8,        [x9, #(CTX_X + 8*8)]

  ldr x0, [sp, #0x00]
  str x0, [x9, #(CTX_X +  9*8)]
  ldr x0, [sp, #0x08]
  str x0, [x9, #(CTX_X + 10*8)]

  stp x11, x12, [x9, #(CTX_X + 11*8)]
  stp x13, x14, [x9, #(CTX_X + 13*8)]
  stp x15, x16, [x9, #(CTX_X + 15*8)]
  stp x17, x18, [x9, #(CTX_X + 17*8)]
  stp x19, x20, [x9, #(CTX_X + 19*8)]
  stp x21, x22, [x9, #(CTX_X + 21*8)]
  stp x23, x24, [x9, #(CTX_X + 23*8)]
  stp x25, x26, [x9, #(CTX_X + 25*8)]
  stp x27, x28, [x9, #(CTX_X + 27*8)]
  stp x29, x30, [x9, #(CTX_X + 29*8)]

  add x0, sp, #0x10
  str x0, [x9, #CTX_SP]
  mrs x0, elr_el1
  str x0, [x9, #CTX_ELR]
  mrs x0, spsr_el1
  str x0, [x9, #CTX_SPSR]

  mrs x0, sp_el0
  str x0, [x9, #CTX_SP_EL0]
  mrs x0, tpidr_el0
  str x0, [x9, #CTX_TPIDR_EL0]

  stp q0,  q1,  [x9, #(CTX_Q +  0*16)]
  stp q2,  q3,  [x9, #(CTX_Q +  2*16)]
  stp q4,  q5,  [x9, #(CTX_Q +  4*16)]
  stp q6,  q7,  [x9, #(CTX_Q +  6*16)]
  stp q8,  q9,  [x9, #(CTX_Q +  8*16)]
  stp q10, q11, [x9, #(CTX_Q + 10*16)]
  stp q12, q13, [x9, #(CTX_Q + 12*16)]
  stp q14, q15, [x9, #(CTX_Q + 14*16)]
  stp q16, q17, [x9, #(CTX_Q + 16*16)]
  stp q18, q19, [x9, #(CTX_Q + 18*16)]
  stp q20, q21, [x9, #(CTX_Q + 20*16)]
  stp q22, q23, [x9, #(CTX_Q + 22*16)]
  stp q24, q25, [x9, #(CTX_Q + 24*16)]
  stp q26, q27, [x9, #(CTX_Q + 26*16)]
  stp q28, q29, [x9, #(CTX_Q + 28*16)]
  stp q30, q31, [x9, #(CTX_Q + 30*16)]
  mrs x0, fpcr
  str x0, [x9, #CTX_FPCR]
  mrs x0, fpsr
  str x0, [x9, #CTX_FPSR]

  mov x0, x9
.endm

.macro restore_context_and_eret
  ldr x1, [x19, #CTX_SP]
  mov sp, x1
  ldr x1, [x19, #CTX_ELR]
  msr elr_el1, x1
  ldr x1, [x19, #CTX_SPSR]
  msr spsr_el1, x1

  ldr x1, [x19, #CTX_SP_EL0]
  msr sp_el0, x1
  ldr x1, [x19, #CTX_TPIDR_EL0]
  msr tpidr_el0, x1

  ldp q0,  q1,  [x19, #(CTX_Q +  0*16)]
  ldp q2,  q3,  [x19, #(CTX_Q +  2*16)]
  ldp q4,  q5,  [x19, #(CTX_Q +  4*16)]
  ldp q6,  q7,  [x19, #(CTX_Q +  6*16)]
  ldp q8,  q9,  [x19, #(CTX_Q +  8*16)]
  ldp q10, q11, [x19, #(CTX_Q + 10*16)]
  ldp q12, q13, [x19, #(CTX_Q + 12*16)]
  ldp q14, q15, [x19, #(CTX_Q + 14*16)]
  ldp q16, q17, [x19, #(CTX_Q + 16*16)]
  ldp q18, q19, [x19, #(CTX_Q + 18*16)]
  ldp q20, q21, [x19, #(CTX_Q + 20*16)]
  ldp q22, q23, [x19, #(CTX_Q + 22*16)]
  ldp q24, q25, [x19, #(CTX_Q + 24*16)]
  ldp q26, q27, [x19, #(CTX_Q + 26*16)]
  ldp q28, q29, [x19, #(CTX_Q + 28*16)]
  ldp q30, q31, [x19, #(CTX_Q + 30*16)]
  ldr x1, [x19, #CTX_FPCR]
  msr fpcr, x1
  ldr x1, [x19, #CTX_FPSR]
  msr fpsr, x1

  ldp x0,  x1,  [x19, #(CTX_X +  0*8)]
  ldp x2,  x3,  [x19, #(CTX_X +  2*8)]
  ldp x4,  x5,  [x19, #(CTX_X +  4*8)]
  ldp x6,  x7,  [x19, #(CTX_X +  6*8)]
  ldp x8,  x9,  [x19, #(CTX_X +  8*8)]
  ldp x10, x11, [x19, #(CTX_X + 10*8)]
  ldp x12, x13, [x19, #(CTX_X + 12*8)]
  ldp x14, x15, [x19, #(CTX_X + 14*8)]
  ldr x16,       [x19, #(CTX_X + 16*8)]
  ldp x17, x18, [x19, #(CTX_X + 17*8)]
  ldp x20, x21, [x19, #(CTX_X + 20*8)]
  ldp x22, x23, [x19, #(CTX_X + 22*8)]
  ldp x24, x25, [x19, #(CTX_X + 24*8)]
  ldp x26, x27, [x19, #(CTX_X + 26*8)]
  ldp x28, x29, [x19, #(CTX_X + 28*8)]
  ldr x30,       [x19, #(CTX_X + 30*8)]
  ldr x19,       [x19, #(CTX_X + 19*8)]
  eret
.endm

// --------------------------------------------------------------------------
// IRQ handler
// --------------------------------------------------------------------------

exc_irq:
  save_context
  bl irq_handler
  cbz x0, handler_returned_null
  // TPIDR_EL1 already set by schedule_inner (before lock drop).
  // Redundant write here for defense-in-depth.
  msr tpidr_el1, x0
  mov x19, x0
  restore_context_and_eret

// --------------------------------------------------------------------------
// SVC handler (EL0 syscalls)
// --------------------------------------------------------------------------

exc_lower_sync:
  sub sp, sp, #0x10
  str x10, [sp]

  mrs x10, esr_el1
  lsr x10, x10, #26
  cmp x10, #0x15          // SVC from AArch64?

  ldr x10, [sp]
  add sp, sp, #0x10

  b.ne exc_user_fault      // Non-SVC user fault: terminate process, not kernel.
  save_context
  bl svc_handler
  cbz x0, handler_returned_null
  // TPIDR_EL1 already set by schedule_inner (before lock drop).
  // Redundant write here for defense-in-depth.
  msr tpidr_el1, x0
  mov x19, x0
  restore_context_and_eret

// --------------------------------------------------------------------------
// User fault handler (non-SVC synchronous exception from EL0)
//
// Data aborts, instruction aborts, alignment faults, undefined instructions,
// etc. from user processes. Save context, call Rust handler to log + terminate
// the faulting process, then restore the next thread's context.
// --------------------------------------------------------------------------

exc_user_fault:
  save_context
  bl user_fault_handler
  cbz x0, handler_returned_null
  // TPIDR_EL1 already set by schedule_inner (before lock drop).
  // Redundant write here for defense-in-depth.
  msr tpidr_el1, x0
  mov x19, x0
  restore_context_and_eret

// --------------------------------------------------------------------------
// Handler returned null context pointer — fatal bug.
// x30 = return address (which handler bl'd to), SP = save_context level.
// --------------------------------------------------------------------------

handler_returned_null:
  mov x6, sp
  mov x7, x30             // which handler returned null
  mrs x8, tpidr_el1

  mrs x4, mpidr_el1
  and x4, x4, #0xFF
  add x4, x4, #1
  lsl x4, x4, #12
  ldr x5, =__exc_stacks
  add sp, x5, x4

  // Print diagnostic.
  ldr x2, =UART0_BASE_VA
  adr x0, .Lnull_msg
  bl uart_puts
  mov x0, x7
  bl uart_puthex64
  adr x0, .Lsp_msg
  bl uart_puts
  mov x0, x6
  bl uart_puthex64
  adr x0, .Ltpidr_msg
  bl uart_puts
  mov x0, x8
  bl uart_puthex64
  adr x0, .Lnl
  bl uart_puts
  b .

.Lnull_msg:   .asciz "\nBUG: handler returned null! caller=0x"
.Lsp_msg:     .asciz " SP=0x"
.Ltpidr_msg:  .asciz " TPIDR=0x"
.Lnl:         .asciz "\n"
.align 2

// Force literal pool emission before switching sections.
.ltorg

// --------------------------------------------------------------------------
// Per-core emergency exception stacks (4 KiB × MAX_CORES).
//
// Used by exc_fatal so the EL1 fault handler works even when the original
// SP points into a guard page (kernel stack overflow).
// Must match per_core::MAX_CORES.
// --------------------------------------------------------------------------
.section .bss.exc_stacks
.align 12
.global __exc_stacks
__exc_stacks:
  .space 4096 * 8
