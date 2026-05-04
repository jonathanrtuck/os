//! Context switch and userspace entry — safe wrappers around assembly.

#[cfg(target_os = "none")]
use super::register_state::RegisterState;

#[cfg(target_os = "none")]
core::arch::global_asm!(include_str!("context.S"));

#[cfg(target_os = "none")]
unsafe extern "C" {
    fn __context_switch(old: *mut RegisterState, new: *const RegisterState);
    fn __enter_userspace(state: *const RegisterState) -> !;
}

/// Save current thread's register state to `old`, load `new`'s state, and
/// continue execution at `new`'s saved PC.
#[cfg(target_os = "none")]
pub fn context_switch(old: &mut RegisterState, new: &RegisterState) {
    // SAFETY: Both pointers are valid RegisterState references. The assembly
    // saves only callee-saved registers; the caller-saved set is handled by
    // the Rust calling convention. No memory corruption possible.
    unsafe { __context_switch(old as *mut _, new as *const _) };
}

/// Load all registers from `state` and eret to EL0. Never returns.
/// Used for initial thread launch.
#[cfg(target_os = "none")]
pub fn enter_userspace(state: &RegisterState) -> ! {
    // SAFETY: state contains a valid ELR (entry point), SPSR (EL0 mode),
    // and SP (user stack). The assembly loads all registers and issues eret.
    unsafe { __enter_userspace(state as *const _) }
}
