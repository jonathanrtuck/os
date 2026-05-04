//! ARM generic timer — one-shot deadline support.
//!
//! Provides deadline-based timing for the kernel. The timer fires only when
//! explicitly armed with [`set_deadline`] and does not re-arm itself — this
//! matches the spec's event-driven scheduler model where `event_wait`
//! timeouts and preemption quanta are one-shot deadlines, not periodic ticks.
//!
//! ## HVF interaction
//!
//! When the virtual timer fires, Apple's Hypervisor.framework masks the timer
//! (sets IMASK in CNTV_CTL_EL0) and injects an IRQ to the guest. Writing
//! CNTV_TVAL_EL0 via [`set_deadline`] re-arms the timer — HVF detects the
//! CNTV_CVAL change and unmasks automatically.

use core::sync::atomic::{AtomicBool, Ordering};

use super::sysreg;

/// Per-core deadline-expired flags. The virtual timer (INTID 27) is a PPI —
/// each core has its own instance. The expired flag must be per-core to
/// prevent one core from stealing another's timer expiry.
static DEADLINE_EXPIRED: [AtomicBool; crate::config::MAX_CORES] =
    [const { AtomicBool::new(false) }; crate::config::MAX_CORES];

/// Ensure the timer starts in a known disarmed state.
///
/// The GIC must be initialized before calling this — INTID 27 (virtual timer
/// PPI) must be enabled in the redistributor so that future [`set_deadline`]
/// calls can deliver interrupts.
pub fn init() {
    sysreg::set_cntv_ctl_el0(0);
    sysreg::isb();
}

/// Read the current monotonic counter value.
///
/// Maps to the spec's `clock_read()` syscall. Returns a value in timer
/// ticks — divide by [`frequency`] to get seconds.
#[inline]
pub fn now() -> u64 {
    sysreg::cntvct_el0()
}

/// Return the timer frequency in Hz.
#[inline]
pub fn frequency() -> u64 {
    sysreg::cntfrq_el0()
}

/// Arm a one-shot deadline `ticks_from_now` timer ticks in the future.
///
/// The timer will fire exactly once (INTID 27), calling [`handle_deadline`]
/// through the IRQ handler. After firing, the timer remains disarmed until
/// explicitly re-armed.
pub fn set_deadline(core_id: usize, ticks_from_now: u64) {
    DEADLINE_EXPIRED[core_id].store(false, Ordering::Relaxed);

    sysreg::set_cntv_tval_el0(ticks_from_now);
    sysreg::set_cntv_ctl_el0(1); // ENABLE=1, IMASK=0
    sysreg::isb();
}

/// Disarm the timer. No interrupt will fire until the next [`set_deadline`].
pub fn clear_deadline() {
    sysreg::set_cntv_ctl_el0(0);
    sysreg::isb();
}

/// Handle a timer deadline expiry. Called from the IRQ handler on INTID 27.
///
/// Masks the timer interrupt (one-shot: does not re-arm) and sets the
/// expired flag for the scheduler to consume via [`deadline_elapsed`].
pub fn handle_deadline(core_id: usize) {
    sysreg::set_cntv_ctl_el0(0b11); // ENABLE=1, IMASK=1
    sysreg::isb();

    DEADLINE_EXPIRED[core_id].store(true, Ordering::Release);
}

/// Check and clear the deadline-expired flag for this core.
///
/// Returns `true` exactly once after each deadline expiry. The scheduler
/// calls this to decide whether to preempt or wake a timed-out thread.
pub fn deadline_elapsed(core_id: usize) -> bool {
    DEADLINE_EXPIRED[core_id].swap(false, Ordering::Acquire)
}
