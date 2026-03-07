// ==========================================================================
// boot.S — AArch64 boot, exception vectors, context switch, MMU setup.
//
// Provides the assembly surface for the kernel. Everything callable from
// Rust is .global. Rust entry points called from here:
//   kernel_main()                           — kernel entry (never returns)
//   irq_handler(*mut Context) -> *const Context — IRQ dispatch
//
// Exports to Rust:
//   enter_first_thread(ctx: *const Context)      — launch first thread via eret
//   enable_mmu(ttbr0: u64)               — configure and enable MMU
// ==========================================================================

// --------------------------------------------------------------------------
// Context struct field offsets.
//
// Must match Rust's #[repr(C)] Context in main.rs.
// Compile-time assertions in main.rs verify these at build time.
// --------------------------------------------------------------------------

.equ CTX_X,         0x000   // x: [u64; 31]        — GPRs x0..x30
.equ CTX_SP,        0x0F8   // sp: u64
.equ CTX_ELR,       0x100   // elr: u64
.equ CTX_SPSR,      0x108   // spsr: u64
.equ CTX_SP_EL0,    0x110   // sp_el0: u64          (reserved for EL0)
.equ CTX_TPIDR_EL0, 0x118   // tpidr_el0: u64       (reserved for EL0)
.equ CTX_Q,         0x120   // q: [u128; 32]        — FP/SIMD q0..q31
.equ CTX_FPCR,      0x320   // fpcr: u64            (low 32 bits)
.equ CTX_FPSR,      0x328   // fpsr: u64            (low 32 bits)
.equ CTX_SIZE,      0x330   // total struct size

// --------------------------------------------------------------------------
// Boot
// --------------------------------------------------------------------------

.section .text.boot
.global _start
_start:
  // Park secondary cores — only core 0 proceeds.
  mrs x0, mpidr_el1
  and x0, x0, #0xFF
  cbnz x0, park

  // Set stack.
  ldr x0, =__stack_top
  mov sp, x0

  // Zero BSS: [__bss_start, __bss_end)
  ldr x1, =__bss_start
  ldr x2, =__bss_end
1:
  cmp x1, x2
  b.ge 2f
  str xzr, [x1], #8
  b 1b
2:
  // If we entered at EL2 (typical for QEMU virt), drop to EL1.
  mrs x1, CurrentEL
  lsr x1, x1, #2
  and x1, x1, #3
  cmp x1, #2
  b.ne el1_start

  // --- EL2 configuration before dropping to EL1 ---

  // Install exception vectors at EL2 for debugging during transition.
  adr x0, vectors
  msr vbar_el2, x0
  isb

  // EL1 executes as AArch64.
  mov x0, #(1 << 31)           // HCR_EL2.RW = 1
  msr hcr_el2, x0

  // Allow EL1 access to physical timer and counter.
  mrs x0, cnthctl_el2
  orr x0, x0, #3               // EL1PCTEN | EL1PCEN
  msr cnthctl_el2, x0
  msr cntvoff_el2, xzr          // virtual offset = 0

  // Don't trap FP/ASIMD at EL2.
  mrs x0, cptr_el2
  bic x0, x0, #(1 << 10)        // TFP = 0
  msr cptr_el2, x0

  // Set EL1 stack pointer and configure eret to EL1h.
  ldr x0, =__stack_top
  msr sp_el1, x0
  mov x0, #(0b0101)             // EL1h
  msr spsr_el2, x0
  adr x0, el1_start
  msr elr_el2, x0
  isb
  eret

el1_start:
  // Ensure EL1h (use SP_EL1). The EL2 path erets into EL1h, but a
  // direct EL1 entry inherits whatever SPSel the firmware left.
  msr spsel, #1
  ldr x0, =__stack_top
  mov sp, x0

  // Install exception vector table.
  adr x0, vectors
  msr vbar_el1, x0
  isb

  // Enable FP/ASIMD at EL1.
  // Rust/LLVM may use NEON registers for struct copies even in early code.
  mrs x0, cpacr_el1
  orr x0, x0, #(3 << 20)        // FPEN = 0b11
  msr cpacr_el1, x0
  isb

  bl kernel_main

park:
  wfe
  b park

// --------------------------------------------------------------------------
// Exception vectors (AArch64)
//
// 16 entries, each 0x80 bytes apart (total 0x800 bytes).
// --------------------------------------------------------------------------

.align 11
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
  b exc_sync
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
// UART helpers (PL011 on QEMU virt at 0x0900_0000)
//
// Convention: x2 = UART base address, set once by caller.
// Leaf functions (uart_putc, hex_nibble) do not touch x30 or the stack.
// Non-leaf functions save/restore x30 so they return correctly.
// --------------------------------------------------------------------------

.equ UART0_BASE, 0x09000000
.equ UART0_DR,   0x00
.equ UART0_FR,   0x18
.equ UART0_TXFF, (1 << 5)

// uart_putc: write one byte to UART.
// w0 = byte, x2 = UART base. Clobbers: w1.
uart_putc:
1:
  ldr w1, [x2, #UART0_FR]
  tst w1, #UART0_TXFF
  b.ne 1b
  str w0, [x2, #UART0_DR]
  ret

// uart_puts: write a null-terminated string to UART. Maps \n to \r\n.
// x0 = string pointer, x2 = UART base. Clobbers: w0, w1, w3.
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

// hex_nibble: convert 0..15 to ASCII hex digit.
// w0 = nibble in, w0 = ASCII out. Clobbers: nothing else.
hex_nibble:
  cmp w0, #9
  ble 1f
  add w0, w0, #55   // 'A' - 10
  ret
1:
  add w0, w0, #48   // '0'
  ret

// uart_puthex32: print a 32-bit value as 8 hex digits.
// w0 = value, x2 = UART base. Clobbers: w0, w1, w4, w5.
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

// uart_puthex64: print a 64-bit value as 16 hex digits.
// x0 = value, x2 = UART base. Clobbers: w0, w1, w4, w5.
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
// Fatal exception handler
//
// Prints exception type, ESR (32-bit), ELR (64-bit), FAR (64-bit), then
// hangs. Works at both EL1 and EL2 (reads correct system registers).
//
// Output format: <T> E:<ESR> L:<ELR> F:<FAR>\r\n
//   T = S (sync), F (fiq), A (serror/async abort)
//
// Register plan (all survive UART calls):
//   x2  = UART base (never clobbered by any UART function)
//   x6  = ELR       (not clobbered by UART functions)
//   x7  = FAR       (not clobbered by UART functions)
//   x20 = type char  (not clobbered by UART functions)
//   w4 is clobbered by uart_puthex32, so ESR must be printed first.
// --------------------------------------------------------------------------

exc_sync:
  mov w20, #83          // 'S'
  b exc_fatal
exc_fiq:
  mov w20, #70          // 'F'
  b exc_fatal
exc_serr:
  mov w20, #65          // 'A'

exc_fatal:
  ldr x2, =UART0_BASE

  // Exception type character.
  mov w0, w20
  bl uart_putc
  mov w0, #32           // ' '
  bl uart_putc

  // Read ESR/ELR/FAR for whichever EL we're at.
  mrs x5, CurrentEL
  lsr x5, x5, #2
  and x5, x5, #3
  cmp x5, #2
  b.ne 7f
  mrs x4, esr_el2
  mrs x6, elr_el2
  mrs x7, far_el2
  b 8f
7:
  mrs x4, esr_el1
  mrs x6, elr_el1
  mrs x7, far_el1
8:
  // "E:" + ESR (32-bit). Print first — w4 is clobbered by uart_puthex32.
  mov w0, #69           // 'E'
  bl uart_putc
  mov w0, #58           // ':'
  bl uart_putc
  mov w0, w4
  bl uart_puthex32

  // " L:" + ELR (64-bit).
  mov w0, #32           // ' '
  bl uart_putc
  mov w0, #76           // 'L'
  bl uart_putc
  mov w0, #58           // ':'
  bl uart_putc
  mov x0, x6
  bl uart_puthex64

  // " F:" + FAR (64-bit).
  mov w0, #32           // ' '
  bl uart_putc
  mov w0, #70           // 'F'
  bl uart_putc
  mov w0, #58           // ':'
  bl uart_putc
  mov x0, x7
  bl uart_puthex64

  mov w0, #13           // '\r'
  bl uart_putc
  mov w0, #10           // '\n'
  bl uart_putc
  b .

// --------------------------------------------------------------------------
// Context switch
//
// TPIDR_EL1 always holds a pointer to the running thread's Context.
// Field offsets use the CTX_* constants defined at the top of this file.
//
// Macro: restore full context from [x19] and eret.
// x19 is the base pointer and is restored last.
// --------------------------------------------------------------------------

.macro restore_context_and_eret
  // SP, ELR, SPSR.
  ldr x1, [x19, #CTX_SP]
  mov sp, x1
  ldr x1, [x19, #CTX_ELR]
  msr elr_el1, x1
  ldr x1, [x19, #CTX_SPSR]
  msr spsr_el1, x1

  // FP/SIMD: q0..q31, FPCR, FPSR.
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

  // GPRs: x0..x30. x19 restored last (base pointer).
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
// exc_irq: save full context, call Rust, restore (possibly different) context.
//
// NOTE — IRQ stack model:
// Currently uses whatever SP was active when the IRQ fired. During early
// boot this is the kernel stack, which is fine. Once multiple threads exist,
// each thread's SP will be live here, meaning:
//   - Every thread stack must have headroom for the deepest IRQ call chain.
//   - A thread stack overflow could corrupt the IRQ handler.
// A dedicated per-CPU IRQ stack (swap SP on entry) is the standard fix.
// --------------------------------------------------------------------------

exc_irq:
  // Scratch: save x9/x10 to read TPIDR_EL1 without losing interrupted state.
  sub sp, sp, #0x10
  str x9,  [sp, #0x00]
  str x10, [sp, #0x08]

  // x9 = current Context*
  mrs x9, tpidr_el1

  // GPRs: x0..x8.
  stp x0,  x1,  [x9, #(CTX_X + 0*8)]
  stp x2,  x3,  [x9, #(CTX_X + 2*8)]
  stp x4,  x5,  [x9, #(CTX_X + 4*8)]
  stp x6,  x7,  [x9, #(CTX_X + 6*8)]
  str x8,        [x9, #(CTX_X + 8*8)]

  // Original x9/x10 from scratch area.
  ldr x0, [sp, #0x00]
  str x0, [x9, #(CTX_X +  9*8)]
  ldr x0, [sp, #0x08]
  str x0, [x9, #(CTX_X + 10*8)]

  // GPRs: x11..x30.
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

  // SP (pre-scratch), ELR, SPSR.
  add x0, sp, #0x10
  str x0, [x9, #CTX_SP]
  mrs x0, elr_el1
  str x0, [x9, #CTX_ELR]
  mrs x0, spsr_el1
  str x0, [x9, #CTX_SPSR]

  // FP/SIMD: q0..q31, FPCR, FPSR.
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

  // Call Rust: x0 = current Context*, returns next Context*.
  mov x0, x9
  bl irq_handler

  // Switch to returned context.
  msr tpidr_el1, x0
  mov x19, x0
  restore_context_and_eret

// --------------------------------------------------------------------------
// enter_first_thread(ctx: *const Context)
//
// Load a Context and eret into it. One-way — used to launch the first
// thread. Sets TPIDR_EL1 so subsequent IRQs can find the Context.
// --------------------------------------------------------------------------

.global enter_first_thread
enter_first_thread:
  msr tpidr_el1, x0
  mov x19, x0
  restore_context_and_eret

// --------------------------------------------------------------------------
// enable_mmu(ttbr0: u64)
//
// Configures MAIR_EL1, TCR_EL1, TTBR0_EL1, invalidates TLB, and sets
// SCTLR_EL1.M to enable the MMU. Caches are left OFF (C=0, I=0) so the
// caller can enable them separately after verifying page tables.
// --------------------------------------------------------------------------

.global enable_mmu
enable_mmu:
  // MAIR_EL1: AttrIdx0 = 0xFF (Normal WBWA), AttrIdx1 = 0x04 (Device-nGnRE)
  mov x1, #0xFF
  mov x2, #0x04
  lsl x2, x2, #8
  orr x1, x1, x2
  msr mair_el1, x1

  // TCR_EL1:
  //   T0SZ  = 16 (48-bit VA)
  //   TG0   = 4k (bits [15:14] = 0b00, default)
  //   SH0   = inner shareable
  //   ORGN0 = WBWA
  //   IRGN0 = WBWA
  //   EPD1  = 1 (disable TTBR1 walks)
  //   IPS   = 40-bit PA
  mov x1, #16                  // T0SZ
  mov x2, #(0b11)              // SH0
  lsl x2, x2, #12
  orr x1, x1, x2
  mov x2, #(0b01)              // ORGN0
  lsl x2, x2, #10
  orr x1, x1, x2
  mov x2, #(0b01)              // IRGN0
  lsl x2, x2, #8
  orr x1, x1, x2
  mov x2, #(1)                 // EPD1
  lsl x2, x2, #23
  orr x1, x1, x2
  mov x2, #(0b010)             // IPS = 40-bit
  lsl x2, x2, #32
  orr x1, x1, x2
  msr tcr_el1, x1

  msr ttbr0_el1, x0

  dsb sy
  isb
  tlbi vmalle1
  dsb ish
  isb

  mrs x1, sctlr_el1
  orr x1, x1, #1              // M = 1 (enable MMU)
  bic x1, x1, #(1 << 2)       // C = 0 (data cache off)
  bic x1, x1, #(1 << 12)      // I = 0 (instruction cache off)
  msr sctlr_el1, x1
  isb
  ret
