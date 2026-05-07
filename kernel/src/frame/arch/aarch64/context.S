// context.S — Thread context switch and userspace entry.
//
// RegisterState layout (from register_state.rs):
//   gprs[0..31]  at offset 0     (31 * 8 = 248 bytes)
//   sp           at offset 248   (SP_EL0)
//   pc           at offset 256   (ELR_EL1)
//   pstate       at offset 264   (SPSR_EL1)
//   tpidr        at offset 272   (TPIDR_EL0)
//   fp_regs      at offset 280   (32 * 16 = 512 bytes)
//   fpcr         at offset 792
//   fpsr         at offset 800
//   kernel_sp    at offset 816   (SP_EL1 — kernel stack pointer)

.section .text

// ---------------------------------------------------------------------------
// context_switch(old: *mut RegisterState, new: *const RegisterState)
//
// Save callee-saved registers (x19-x30, sp) to `old`, load from `new`.
// Caller-saved registers (x0-x18) are saved/restored by the compiler.
//
// This function handles BOTH voluntary and involuntary switches.
// Voluntary: thread calls event_wait, recv, etc. Compiler saves caller-saved
//   via calling convention. We save callee-saved. Resume returns into Rust
//   which eventually returns through syscall exit → eret.
// Involuntary: timer preemption. The exception vector saved the full TrapFrame
//   on the thread's kernel stack. IRQ handler calls scheduler which calls us.
//   Resume returns into the IRQ handler → exception_common → eret.
// Both paths are identical from our perspective.
// ---------------------------------------------------------------------------
.global __context_switch
__context_switch:
    // Save callee-saved GPRs to old state.
    stp     x19, x20, [x0, #(19*8)]
    stp     x21, x22, [x0, #(21*8)]
    stp     x23, x24, [x0, #(23*8)]
    stp     x25, x26, [x0, #(25*8)]
    stp     x27, x28, [x0, #(27*8)]
    stp     x29, x30, [x0, #(29*8)]

    // Save SP_EL0, TPIDR_EL0, and kernel SP (SP_EL1).
    mrs     x9, sp_el0
    str     x9, [x0, #248]          // RegisterState.sp
    mrs     x9, tpidr_el0
    str     x9, [x0, #272]          // RegisterState.tpidr
    mov     x9, sp
    str     x9, [x0, #816]          // RegisterState.kernel_sp

    // Load callee-saved GPRs from new state.
    ldp     x19, x20, [x1, #(19*8)]
    ldp     x21, x22, [x1, #(21*8)]
    ldp     x23, x24, [x1, #(23*8)]
    ldp     x25, x26, [x1, #(25*8)]
    ldp     x27, x28, [x1, #(27*8)]
    ldp     x29, x30, [x1, #(29*8)]

    // Restore SP_EL0, TPIDR_EL0, and kernel SP.
    ldr     x9, [x1, #248]
    msr     sp_el0, x9
    ldr     x9, [x1, #272]
    msr     tpidr_el0, x9
    ldr     x9, [x1, #816]          // RegisterState.kernel_sp
    mov     sp, x9

    ret

// ---------------------------------------------------------------------------
// enter_userspace(state: *const RegisterState) -> !
//
// Load all registers from a RegisterState and eret to EL0.
// Used for initial thread launch — never returns.
// ---------------------------------------------------------------------------
.global __enter_userspace
__enter_userspace:
    // Mask IRQs — an interrupt between MSR ELR_EL1 and ERET would
    // overwrite ELR with the interrupted kernel PC, then ERET would
    // send the thread to EL0 at a kernel address.
    msr     daifset, #2

    // Set ELR_EL1 (return address) from RegisterState.pc.
    ldr     x9, [x0, #256]
    msr     elr_el1, x9

    // Set SPSR_EL1 from RegisterState.pstate.
    ldr     x9, [x0, #264]
    msr     spsr_el1, x9

    // Set SP_EL0 from RegisterState.sp.
    ldr     x9, [x0, #248]
    msr     sp_el0, x9

    // Set TPIDR_EL0 from RegisterState.tpidr.
    ldr     x9, [x0, #272]
    msr     tpidr_el0, x9

    // Load all GPRs (x0-x30).
    ldp     x2, x3, [x0, #(2*8)]
    ldp     x4, x5, [x0, #(4*8)]
    ldp     x6, x7, [x0, #(6*8)]
    ldp     x8, x9, [x0, #(8*8)]
    ldp     x10, x11, [x0, #(10*8)]
    ldp     x12, x13, [x0, #(12*8)]
    ldp     x14, x15, [x0, #(14*8)]
    ldp     x16, x17, [x0, #(16*8)]
    ldp     x18, x19, [x0, #(18*8)]
    ldp     x20, x21, [x0, #(20*8)]
    ldp     x22, x23, [x0, #(22*8)]
    ldp     x24, x25, [x0, #(24*8)]
    ldp     x26, x27, [x0, #(26*8)]
    ldp     x28, x29, [x0, #(28*8)]
    ldr     x30, [x0, #(30*8)]

    // Load x0 and x1 last (x0 was the state pointer).
    ldp     x0, x1, [x0, #0]

    eret

// ---------------------------------------------------------------------------
// park_thread(thread_rs: *mut RegisterState, idle_sp: u64, core_id: u64)
//
// Save callee-saved registers to thread_rs (for SMP safety — another core
// can direct_switch to this thread at any time after the save), switch SP
// to the idle stack, and tail-call park_loop(core_id). Never returns to the
// caller directly — park_loop resumes a thread via restore_context, which
// restores the saved callee-saved regs + kernel_sp and `ret`s to the
// save-point return address.
// ---------------------------------------------------------------------------
.global __park_thread
__park_thread:
    // Save callee-saved GPRs to thread_rs.
    stp     x19, x20, [x0, #(19*8)]
    stp     x21, x22, [x0, #(21*8)]
    stp     x23, x24, [x0, #(23*8)]
    stp     x25, x26, [x0, #(25*8)]
    stp     x27, x28, [x0, #(27*8)]
    stp     x29, x30, [x0, #(29*8)]

    // Save SP_EL0 and TPIDR_EL0.
    mrs     x9, sp_el0
    str     x9, [x0, #248]
    mrs     x9, tpidr_el0
    str     x9, [x0, #272]

    // Save kernel SP (current thread's stack).
    mov     x9, sp
    str     x9, [x0, #816]

    // Switch to idle stack and tail-call park_loop(core_id).
    mov     sp, x1
    mov     x0, x2
    b       park_loop

// ---------------------------------------------------------------------------
// restore_context(rs: *const RegisterState) -> !
//
// Load callee-saved registers from rs and resume execution at the saved
// return address (x30). Kernel SP is restored, so this effectively
// "becomes" the saved context — like longjmp. Used by park_loop to
// dispatch a thread without the overhead of saving idle state.
// ---------------------------------------------------------------------------
.global __restore_context
__restore_context:
    ldp     x19, x20, [x0, #(19*8)]
    ldp     x21, x22, [x0, #(21*8)]
    ldp     x23, x24, [x0, #(23*8)]
    ldp     x25, x26, [x0, #(25*8)]
    ldp     x27, x28, [x0, #(27*8)]
    ldp     x29, x30, [x0, #(29*8)]

    ldr     x9, [x0, #248]
    msr     sp_el0, x9
    ldr     x9, [x0, #272]
    msr     tpidr_el0, x9
    ldr     x9, [x0, #816]
    mov     sp, x9

    ret
