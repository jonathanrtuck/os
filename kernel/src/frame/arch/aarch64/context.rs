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

/// Trampoline for newly created threads entering userspace after context_switch.
///
/// When `context_switch` restores a new thread's callee-saved registers and
/// `ret`s to x30, this function is the target. It reads the current thread
/// from per-CPU data and enters userspace with the thread's full RegisterState.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
extern "C" fn new_thread_enter() -> ! {
    // SAFETY: percpu() requires init_percpu to have been called. This runs
    // after context_switch on the same core, well after boot. kernel_ptr
    // was set during boot via set_kernel_ptr.
    let rs = unsafe {
        let pc = super::cpu::percpu();
        let kernel = &*(pc.kernel_ptr as *const crate::syscall::Kernel);
        let thread = kernel
            .threads
            .get(pc.current_thread)
            .expect("new_thread_enter: current thread not found");

        thread
            .register_state()
            .expect("new_thread_enter: no RegisterState")
    };

    enter_userspace(rs)
}

/// Address of the new-thread trampoline, for use in `init_thread_registers`.
#[cfg(target_os = "none")]
pub fn new_thread_trampoline() -> usize {
    new_thread_enter as *const () as usize
}

/// Allocate a per-thread kernel stack and return (base_va, top_va).
///
/// Each thread gets its own kernel stack so that blocking context switches
/// don't corrupt other threads' saved frames on a shared stack.
#[cfg(target_os = "none")]
pub fn alloc_kernel_stack() -> Option<(usize, usize)> {
    let pages = crate::config::KERNEL_STACK_PAGES;
    let base_pa = super::page_alloc::alloc_contiguous(pages)?;

    for i in 0..pages {
        crate::frame::user_mem::zero_phys(
            base_pa.as_usize() + i * crate::config::PAGE_SIZE,
            crate::config::PAGE_SIZE,
        );
    }

    let base_va = super::platform::phys_to_virt(base_pa.as_usize());
    let top_va = base_va + pages * crate::config::PAGE_SIZE;

    Some((base_va, top_va))
}
