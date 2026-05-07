//! Saved register context for thread context switches.
//!
//! Distinct from [`super::exception::TrapFrame`] which captures
//! exception-specific registers (ESR, FAR) not part of a thread's
//! persistent identity.

/// Full saved register context for a thread (AArch64).
///
/// Stored per-thread, referenced from the thread's kernel metadata.
/// Separate from the address space — threads sharing an address space
/// each have their own RegisterState.
#[repr(C)]
pub struct RegisterState {
    /// General-purpose registers x0–x30.
    pub gprs: [u64; 31],
    /// User stack pointer (SP_EL0).
    pub sp: u64,
    /// Program counter (ELR_EL1 — resume address).
    pub pc: u64,
    /// Saved processor state (SPSR_EL1).
    pub pstate: u64,
    /// Thread-local storage (TPIDR_EL0).
    pub tpidr: u64,
    /// FP/SIMD registers v0–v31 (128-bit each).
    pub fp_regs: [u128; 32],
    /// Floating-point control register.
    pub fpcr: u64,
    /// Floating-point status register.
    pub fpsr: u64,
    /// Kernel stack pointer (SP_EL1) — saved/restored during context_switch
    /// so each thread resumes at its correct kernel stack depth.
    pub kernel_sp: u64,
}

impl RegisterState {
    pub const ZEROED: Self = Self {
        gprs: [0; 31],
        sp: 0,
        pc: 0,
        pstate: 0,
        tpidr: 0,
        fp_regs: [0; 32],
        fpcr: 0,
        fpsr: 0,
        kernel_sp: 0,
    };

    pub fn zeroed() -> Self {
        Self::ZEROED
    }
}

const _: () = {
    assert!(core::mem::size_of::<RegisterState>() == 832);
    assert!(core::mem::offset_of!(RegisterState, fp_regs) == 288);
    assert!(core::mem::offset_of!(RegisterState, kernel_sp) == 816);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_state_size() {
        assert_eq!(core::mem::size_of::<RegisterState>(), 832);
    }
}
