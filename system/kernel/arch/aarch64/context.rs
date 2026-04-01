// AUDIT: 2026-03-11 — No unsafe blocks (pure data struct + const assertions).
// Register save/restore completeness verified against exception.S:
// x0-x30 (31 GPRs), SP (EL1), ELR_EL1, SPSR_EL1 (includes PSTATE/DAIF/flags),
// SP_EL0 (user stack), TPIDR_EL0 (user TLS), q0-q31 (32 NEON/FP regs), FPCR,
// FPSR. Total 0x330 bytes. All offsets match exception.S CTX_* equates (enforced
// by compile-time assertions). TPIDR_EL1 not in Context — it points TO the
// Context/Thread, set on context switch. No bugs found.

//! CPU context saved/restored across exception boundaries.
//!
//! Layout must match the CTX_* offsets in exception.S exactly.
//! The const assertions below enforce this at compile time.
//!
//! The struct is fully arch-defined (the register file IS the architecture).
//! Generic kernel code accesses it via the accessor methods below, never
//! touching aarch64 fields directly.

/// CPU register state saved on exception entry and restored on return.
///
/// Each thread embeds a `Context` at offset 0. `TPIDR_EL1` always points
/// at the current thread, and exception.S saves/restores registers at the
/// offsets defined here.
#[repr(C)]
pub struct Context {
    pub x: [u64; 31], // x0..x30
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    pub sp_el0: u64,
    pub tpidr_el0: u64,
    pub q: [u128; 32],
    pub fpcr: u64,
    pub fpsr: u64,
}

const _: () = {
    assert!(core::mem::offset_of!(Context, x) == 0x000);
    assert!(core::mem::offset_of!(Context, sp) == 0x0F8);
    assert!(core::mem::offset_of!(Context, elr) == 0x100);
    assert!(core::mem::offset_of!(Context, spsr) == 0x108);
    assert!(core::mem::offset_of!(Context, sp_el0) == 0x110);
    assert!(core::mem::offset_of!(Context, tpidr_el0) == 0x118);
    assert!(core::mem::offset_of!(Context, q) == 0x120);
    assert!(core::mem::offset_of!(Context, fpcr) == 0x320);
    assert!(core::mem::offset_of!(Context, fpsr) == 0x328);
    assert!(core::mem::size_of::<Context>() == 0x330);
};

// ---------------------------------------------------------------------------
// Generic accessor methods — the arch interface for Context
// ---------------------------------------------------------------------------

impl Context {
    /// Create a zero-initialized context.
    pub const fn new() -> Self {
        Self {
            x: [0; 31],
            sp: 0,
            elr: 0,
            spsr: 0,
            sp_el0: 0,
            tpidr_el0: 0,
            q: [0; 32],
            fpcr: 0,
            fpsr: 0,
        }
    }

    /// Program counter (ELR_EL1 on aarch64).
    #[inline(always)]
    pub fn pc(&self) -> u64 {
        self.elr
    }

    /// Set the program counter.
    #[inline(always)]
    pub fn set_pc(&mut self, pc: u64) {
        self.elr = pc;
    }

    /// Kernel stack pointer (SP_EL1 on aarch64).
    #[inline(always)]
    pub fn sp(&self) -> u64 {
        self.sp
    }

    /// Set the kernel stack pointer.
    #[inline(always)]
    pub fn set_sp(&mut self, sp: u64) {
        self.sp = sp;
    }

    /// User stack pointer (SP_EL0 on aarch64).
    #[inline(always)]
    pub fn user_sp(&self) -> u64 {
        self.sp_el0
    }

    /// Set the user stack pointer.
    #[inline(always)]
    pub fn set_user_sp(&mut self, sp: u64) {
        self.sp_el0 = sp;
    }

    /// Read argument register n (x0-x5 on aarch64).
    ///
    /// Panics if n >= 6.
    #[inline(always)]
    pub fn arg(&self, n: usize) -> u64 {
        assert!(n < 6, "arg index out of range");

        self.x[n]
    }

    /// Set argument register n (x0-x5 on aarch64).
    ///
    /// Panics if n >= 6.
    #[inline(always)]
    pub fn set_arg(&mut self, n: usize, val: u64) {
        assert!(n < 6, "arg index out of range");

        self.x[n] = val;
    }

    /// Configure for user-mode execution (EL0t on aarch64).
    ///
    /// Clears the exception level bits in SPSR to EL0t (0x0).
    /// Other SPSR bits (DAIF, NZCV) are preserved if set.
    #[inline(always)]
    pub fn set_user_mode(&mut self) {
        // Clear bits [3:0] (M field) — EL0t = 0b0000.
        self.spsr &= !0xF;
    }

    /// User TLS pointer (TPIDR_EL0 on aarch64).
    #[inline(always)]
    pub fn user_tls(&self) -> u64 {
        self.tpidr_el0
    }

    /// Set the user TLS pointer.
    #[inline(always)]
    pub fn set_user_tls(&mut self, tls: u64) {
        self.tpidr_el0 = tls;
    }
}
