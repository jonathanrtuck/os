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
    // after context_switch on the same core, well after boot. We extract a
    // raw pointer to RegisterState and drop the slot guard before entering
    // userspace (which never returns).
    let rs_ptr = unsafe {
        let pc = super::cpu::percpu();
        let thread = crate::frame::state::threads()
            .read(pc.current_thread)
            .expect("new_thread_enter: current thread not found");

        thread
            .register_state()
            .expect("new_thread_enter: no RegisterState") as *const _
    };

    // SAFETY: RegisterState is owned by the Thread in the global table.
    // The thread won't be deallocated — it was just context-switched to.
    unsafe { enter_userspace(&*rs_ptr) }
}

/// Context switch between two threads identified by table index.
///
/// Extracts raw pointers to RegisterState from the global thread table,
/// releases the slot locks, then performs the switch. The locks MUST be
/// released before the switch — the new thread resumes on this core and
/// must not inherit held slot locks from the old thread.
#[cfg(target_os = "none")]
pub fn switch_threads(old_idx: u32, new_idx: u32) {
    let old_rs_ptr: *mut RegisterState;
    let new_rs_ptr: *const RegisterState;

    {
        let mut old_thread = crate::frame::state::threads()
            .write(old_idx)
            .expect("context switch: old thread must exist");

        old_rs_ptr = old_thread.init_register_state() as *mut _;
    }
    {
        let new_thread = crate::frame::state::threads()
            .read(new_idx)
            .expect("context switch: new thread must exist");

        new_rs_ptr = new_thread
            .register_state()
            .expect("new thread has no RegisterState") as *const _;
    }

    // SAFETY: Both pointers are valid for the duration of the context switch.
    // The thread objects remain allocated (they are in the global table and
    // haven't been deallocated). The slot locks are released above, but the
    // RegisterState memory is owned by the Thread objects which remain live.
    unsafe {
        context_switch(&mut *old_rs_ptr, &*new_rs_ptr);
    }
}

/// Enter userspace for a thread identified by table index.
///
/// Extracts the RegisterState pointer, releases the slot lock, then loads
/// all registers and erets to EL0. Never returns.
#[cfg(target_os = "none")]
pub fn enter_userspace_by_id(thread_idx: u32) -> ! {
    let rs_ptr: *const RegisterState;

    {
        let thread = crate::frame::state::threads()
            .read(thread_idx)
            .expect("enter_userspace: thread must exist");

        rs_ptr = thread
            .register_state()
            .expect("enter_userspace: no RegisterState") as *const _;
    }

    // SAFETY: RegisterState is owned by the Thread in the global table.
    // The thread won't be deallocated — it was just scheduled to run.
    unsafe { enter_userspace(&*rs_ptr) }
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
