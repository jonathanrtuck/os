//! Saved register context for Observer context switches.
//!
//! Distinct from [`super::exception::TrapFrame`] which captures
//! exception-specific registers (ESR, FAR) not part of the Observer's
//! persistent identity.

/// Full saved register context for an Observer (AArch64).
///
/// Lives in the consumed Space's structural backing (D35). The Observer
/// metadata struct holds a pointer to this (D43: too large for root-Space
/// metadata per D32).
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
}

const _: () = {
    assert!(core::mem::size_of::<RegisterState>() == 816);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_state_size() {
        assert_eq!(core::mem::size_of::<RegisterState>(), 816);
    }
}
