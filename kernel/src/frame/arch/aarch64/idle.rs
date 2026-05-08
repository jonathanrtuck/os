//! Per-core idle context — saves a blocked thread's RegisterState via
//! context_switch so that other cores can safely direct_switch to it.
//!
//! Without this, `switch_away` returning with no context switch leaves the
//! thread on the CPU with stale RegisterState. Any core loading that stale
//! state via direct_switch would run the same thread on two cores.

#[cfg(target_os = "none")]
use core::cell::UnsafeCell;

#[cfg(target_os = "none")]
use super::register_state::RegisterState;
#[cfg(target_os = "none")]
use crate::config;

#[cfg(target_os = "none")]
const IDLE_STACK_SIZE: usize = 4096;

#[cfg(target_os = "none")]
#[repr(C, align(16))]
struct IdleStack(UnsafeCell<[u8; IDLE_STACK_SIZE]>);

// SAFETY: Each core exclusively owns its idle slot, indexed by core_id.
// No cross-core idle state access occurs.
#[cfg(target_os = "none")]
unsafe impl Sync for IdleStack {}

#[cfg(target_os = "none")]
struct IdleStates([UnsafeCell<RegisterState>; config::MAX_CORES]);

#[cfg(target_os = "none")]
unsafe impl Sync for IdleStates {}

#[cfg(target_os = "none")]
static IDLE_STATES: IdleStates =
    IdleStates([const { UnsafeCell::new(RegisterState::ZEROED) }; config::MAX_CORES]);

#[cfg(target_os = "none")]
static IDLE_STACKS: [IdleStack; config::MAX_CORES] =
    [const { IdleStack(UnsafeCell::new([0; IDLE_STACK_SIZE])) }; config::MAX_CORES];

/// Initialize the idle context for a core. Sets kernel_sp and x30 so
/// that the first `switch_to_idle` enters the idle loop.
#[cfg(target_os = "none")]
pub fn init(core_id: usize) {
    // SAFETY: Called once per core during boot, before any context switch.
    let rs = unsafe { &mut *IDLE_STATES.0[core_id].get() };
    let stack_top = IDLE_STACKS[core_id].0.get() as usize + IDLE_STACK_SIZE;

    rs.kernel_sp = stack_top as u64;
    rs.gprs[30] = idle_entry as *const () as u64;
}

#[cfg(not(target_os = "none"))]
pub fn init(_core_id: usize) {}

/// Context switch from a thread to this core's idle context.
#[cfg(target_os = "none")]
pub fn switch_to_idle(thread_idx: u32, core_id: usize) {
    let thread_rs: *mut RegisterState;

    {
        let mut t = crate::frame::state::threads()
            .write(thread_idx)
            .expect("switch_to_idle: thread must exist");

        thread_rs = t.init_register_state() as *mut _;
    }

    let idle_rs = IDLE_STATES.0[core_id].get().cast_const();

    // SAFETY: thread_rs is owned by the global thread table (alive).
    // idle_rs is a static per-core slot. Both are valid for the switch.
    unsafe { super::context::context_switch(&mut *thread_rs, &*idle_rs) }
}

/// Context switch from this core's idle context to a thread.
#[cfg(target_os = "none")]
pub fn switch_from_idle(core_id: usize, thread_idx: u32) {
    let idle_rs = IDLE_STATES.0[core_id].get();
    let thread_rs: *const RegisterState;

    {
        let t = crate::frame::state::threads()
            .read(thread_idx)
            .expect("switch_from_idle: thread must exist");

        thread_rs = t
            .register_state()
            .expect("switch_from_idle: no RegisterState") as *const _;
    }

    // SAFETY: Both pointers valid — idle is static, thread is in global table.
    unsafe { super::context::context_switch(&mut *idle_rs, &*thread_rs) }
}

/// Save the current thread's RegisterState for SMP safety, switch to the
/// per-core idle stack, and enter a bounded WFE spin. If a runnable thread
/// appears quickly (common in IPC — the reply arrives within hundreds of
/// cycles), restores it directly with a single register load. Falls through
/// to the full idle loop only after the spin expires.
///
/// Replaces the old switch_to_idle path which did two full context switches
/// (thread→idle + idle→thread). This path does one save + one restore,
/// eliminating ~500-1000 cycles of redundant idle-state load/save and the
/// associated cache-line traffic.
#[cfg(target_os = "none")]
pub fn park_and_wait(thread_idx: u32, core_id: usize) {
    let thread_rs: *mut super::register_state::RegisterState;

    {
        let mut t = crate::frame::state::threads()
            .write(thread_idx)
            .expect("park_and_wait: thread must exist");

        thread_rs = t.init_register_state() as *mut _;
    }

    let idle_sp = IDLE_STACKS[core_id].0.get() as u64 + IDLE_STACK_SIZE as u64;

    // SAFETY: thread_rs is owned by the global thread table (alive).
    // idle_sp is a static per-core stack, 16-byte aligned. park_thread
    // saves callee-saved registers, switches SP, and tail-calls park_loop.
    // Execution resumes here when restore_context reloads the saved state.
    super::context::park_thread(unsafe { &mut *thread_rs }, idle_sp, core_id as u64);
}

/// WFE spin on the idle stack — called via tail-call from __park_thread.
///
/// Checks the run queue immediately (catches same-core enqueue race),
/// then does a bounded WFE spin for cross-core IPI wakeups. On finding
/// a thread, restores it directly via __restore_context (single register
/// load, no idle-state save). Falls through to idle_loop_ctx if no
/// wakeup arrives.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
extern "C" fn park_loop(core_id: usize) -> ! {
    super::sysreg::enable_irqs();

    drain_deferred_timer_wake(core_id);

    if let Some(tid) = pick_and_setup(core_id) {
        resume_thread(tid);
    }

    for _ in 0..2 {
        // SAFETY: wfe is a hint with no side effects.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };

        drain_deferred_timer_wake(core_id);

        if let Some(tid) = pick_and_setup(core_id) {
            resume_thread(tid);
        }
    }

    // No wakeup arrived — commit to the full idle loop.
    super::cpu::idle_loop_ctx(core_id);
}

/// Drain any deferred timer wakeup into the run queue. The timer ISR
/// cannot safely enqueue (scheduler lock may be held by the idle loop),
/// so it stores the wakeup here for the idle loop to drain.
#[cfg(target_os = "none")]
pub fn drain_deferred_timer_wake(core_id: usize) {
    if let Some((tid, pri)) = super::timer::take_deferred_wake(core_id) {
        crate::println!(
            "drain_timer: enqueue tid={} pri={pri} on core {core_id}",
            tid.0
        );

        crate::frame::state::schedulers()
            .core(core_id)
            .lock()
            .enqueue(tid, crate::types::Priority(pri));
    }
}

/// Pick the next runnable thread and set it up as Running + current.
#[cfg(target_os = "none")]
fn pick_and_setup(core_id: usize) -> Option<crate::types::ThreadId> {
    let tid = {
        let mut sched = crate::frame::state::schedulers().core(core_id).lock();

        sched.pick_next()?
    };

    crate::frame::state::threads()
        .write(tid.0)
        .unwrap()
        .set_state(crate::thread::ThreadRunState::Running);
    crate::frame::state::schedulers()
        .core(core_id)
        .lock()
        .set_current(Some(tid));

    super::cpu::set_current_thread(tid.0);

    let (pt_root, asid) = {
        let t = crate::frame::state::threads().read(tid.0).unwrap();
        let space_id = t.address_space().unwrap();

        drop(t);

        let space = crate::frame::state::spaces().read(space_id.0).unwrap();

        (space.page_table_root(), space.asid())
    };

    super::page_table::switch_table_if_needed(
        super::page_alloc::PhysAddr(pt_root),
        super::page_table::Asid(asid),
    );

    Some(tid)
}

/// Load a thread's saved RegisterState and resume it. Never returns.
#[cfg(target_os = "none")]
fn resume_thread(tid: crate::types::ThreadId) -> ! {
    let rs_ptr: *const super::register_state::RegisterState;

    {
        let thread = crate::frame::state::threads()
            .read(tid.0)
            .expect("resume_thread: thread must exist");

        rs_ptr = thread
            .register_state()
            .expect("resume_thread: no RegisterState") as *const _;
    }

    // SAFETY: rs_ptr is owned by the Thread in the global table. The thread
    // was just set to Running and won't be deallocated.
    unsafe { super::context::restore_context(rs_ptr) }
}

/// Entry point reached via context_switch when x30 = this address.
#[cfg(target_os = "none")]
extern "C" fn idle_entry() -> ! {
    // SAFETY: percpu() requires init_percpu, which completed before any
    // thread ran on this core.
    let core_id = unsafe { super::cpu::percpu().core_id as usize };

    super::sysreg::enable_irqs();
    super::cpu::idle_loop_ctx(core_id);
}
