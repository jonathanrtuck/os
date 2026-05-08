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

#[cfg(miri)]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(not(miri))]
use super::sysreg;

/// Timer frequency in Hz. ARM generic timer on Apple HVF and QEMU virt
/// runs at 24 MHz. Using a const lets the compiler replace udiv with
/// multiply-shift (saves ~24 cycles per clock_read).
pub const TIMER_FREQ_HZ: u64 = 24_000_000;

/// Per-core deadline-expired flags. The virtual timer (INTID 27) is a PPI —
/// each core has its own instance. The expired flag must be per-core to
/// prevent one core from stealing another's timer expiry.
static DEADLINE_EXPIRED: [AtomicBool; crate::config::MAX_CORES] =
    [const { AtomicBool::new(false) }; crate::config::MAX_CORES];

/// Per-core tracking of which thread armed the deadline timer.
/// `u32::MAX` means no thread. Set by `set_deadline_thread`, consumed by
/// `take_deadline_thread` in the timer ISR to know who to wake on timeout.
static DEADLINE_THREAD: [AtomicU32; crate::config::MAX_CORES] =
    [const { AtomicU32::new(u32::MAX) }; crate::config::MAX_CORES];

/// Monotonic counter for Miri — increments on each `now()` call so clock
/// reads are always non-decreasing.
#[cfg(miri)]
static MIRI_COUNTER: AtomicU64 = AtomicU64::new(1000);

/// Ensure the timer starts in a known disarmed state.
///
/// The GIC must be initialized before calling this — INTID 27 (virtual timer
/// PPI) must be enabled in the redistributor so that future [`set_deadline`]
/// calls can deliver interrupts.
#[cfg(not(miri))]
pub fn init() {
    sysreg::set_cntv_ctl_el0(0);

    let hw_freq = sysreg::cntfrq_el0();

    assert!(
        hw_freq == TIMER_FREQ_HZ,
        "CNTFRQ_EL0 mismatch: expected {TIMER_FREQ_HZ}, got {hw_freq}",
    );

    // Enable EL0 access to the virtual counter (CNTVCT_EL0). Bit 1
    // (EL0VCTEN) allows userspace to read the timer without trapping.
    let cntkctl = sysreg::cntkctl_el1();

    sysreg::set_cntkctl_el1(cntkctl | (1 << 1));
    sysreg::isb();
}

#[cfg(miri)]
pub fn init() {}

/// Read the current monotonic counter value.
///
/// Maps to the spec's `clock_read()` syscall. Returns a value in timer
/// ticks — divide by [`frequency`] to get seconds.
#[inline]
pub fn now() -> u64 {
    #[cfg(not(miri))]
    {
        sysreg::cntvct_el0()
    }
    #[cfg(miri)]
    {
        MIRI_COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}

/// Return the timer frequency in Hz.
#[inline]
pub const fn frequency() -> u64 {
    TIMER_FREQ_HZ
}

/// Arm a one-shot deadline `ticks_from_now` timer ticks in the future.
///
/// The timer will fire exactly once (INTID 27), calling [`handle_deadline`]
/// through the IRQ handler. After firing, the timer remains disarmed until
/// explicitly re-armed.
pub fn set_deadline(core_id: usize, _ticks_from_now: u64) {
    DEADLINE_EXPIRED[core_id].store(false, Ordering::Relaxed);

    #[cfg(not(miri))]
    {
        sysreg::set_cntv_tval_el0(_ticks_from_now);
        sysreg::set_cntv_ctl_el0(1); // ENABLE=1, IMASK=0 — TVAL write updates CVAL for HVF detection
        sysreg::isb();
    }
}

/// Disarm the timer by pushing CVAL far into the future. Keeps CTL
/// enabled (avoids HVF re-arm issues with CTL 0→1 transitions).
pub fn clear_deadline() {
    #[cfg(not(miri))]
    {
        sysreg::set_cntv_tval_el0(TIMER_FREQ_HZ * 3600);
        sysreg::isb();
    }
}

/// Handle a timer deadline expiry. Called from the IRQ handler on INTID 27.
///
/// Masks the timer interrupt (one-shot: does not re-arm) and sets the
/// expired flag for the scheduler to consume via [`deadline_elapsed`].
pub fn handle_deadline(core_id: usize) {
    #[cfg(not(miri))]
    {
        sysreg::set_cntv_ctl_el0(0b11); // ENABLE=1, IMASK=1 (HVF detects re-arm via CVAL change)
        sysreg::isb();
    }

    DEADLINE_EXPIRED[core_id].store(true, Ordering::Release);
}

/// Check and clear the deadline-expired flag for this core.
///
/// Returns `true` exactly once after each deadline expiry. The scheduler
/// calls this to decide whether to preempt or wake a timed-out thread.
pub fn deadline_elapsed(core_id: usize) -> bool {
    DEADLINE_EXPIRED[core_id].swap(false, Ordering::Acquire)
}

/// Record which thread armed the timer on this core. Called by
/// `block_with_deadline` before arming the timer.
pub fn set_deadline_thread(core_id: usize, thread_id: crate::types::ThreadId) {
    DEADLINE_THREAD[core_id].store(thread_id.0, Ordering::Release);
}

/// Atomically take the deadline thread for this core. Returns `Some` exactly
/// once per `set_deadline_thread` call. The timer ISR calls this to find
/// which thread to wake on timeout.
pub fn take_deadline_thread(core_id: usize) -> Option<crate::types::ThreadId> {
    let id = DEADLINE_THREAD[core_id].swap(u32::MAX, Ordering::Acquire);

    if id == u32::MAX {
        None
    } else {
        Some(crate::types::ThreadId(id))
    }
}

/// Clear the deadline thread for this core without returning it. Called
/// when a thread wakes normally (event signal) to prevent the stale timer
/// from waking the wrong thread.
pub fn clear_deadline_thread(core_id: usize) {
    DEADLINE_THREAD[core_id].store(u32::MAX, Ordering::Release);
}

/// Per-core deferred timer wakeup: thread ID + priority to enqueue.
/// The ISR cannot safely call `sched::wake` (scheduler lock may be held
/// by the idle loop on the same core). Instead, the ISR stores the wakeup
/// here, and the idle loop / park_loop drains it before pick_next.
static TIMER_WAKE_TID: [AtomicU32; crate::config::MAX_CORES] =
    [const { AtomicU32::new(u32::MAX) }; crate::config::MAX_CORES];
static TIMER_WAKE_PRI: [AtomicU32; crate::config::MAX_CORES] =
    [const { AtomicU32::new(0) }; crate::config::MAX_CORES];

pub fn set_deferred_wake(core_id: usize, tid: crate::types::ThreadId, priority: u8) {
    TIMER_WAKE_PRI[core_id].store(priority as u32, Ordering::Relaxed);
    TIMER_WAKE_TID[core_id].store(tid.0, Ordering::Release);
}

pub fn take_deferred_wake(core_id: usize) -> Option<(crate::types::ThreadId, u8)> {
    let tid = TIMER_WAKE_TID[core_id].swap(u32::MAX, Ordering::Acquire);

    if tid == u32::MAX {
        None
    } else {
        let pri = TIMER_WAKE_PRI[core_id].load(Ordering::Relaxed);

        Some((crate::types::ThreadId(tid), pri as u8))
    }
}
