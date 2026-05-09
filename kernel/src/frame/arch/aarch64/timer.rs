//! ARM generic timer — one-shot deadline support with per-core queue.
//!
//! Multiple threads per core can have concurrent deadlines. The queue
//! stores up to [`MAX_DEADLINES`] entries per core. The hardware timer
//! is always armed to the earliest pending deadline. When it fires,
//! [`drain_expired`] returns all entries whose deadlines have passed
//! and rearms the timer for the next.
//!
//! ## HVF interaction
//!
//! When the virtual timer fires, Apple's Hypervisor.framework masks the timer
//! (sets IMASK in CNTV_CTL_EL0) and injects an IRQ to the guest. Writing
//! CNTV_TVAL_EL0 via [`rearm_earliest`] re-arms the timer — HVF detects the
//! CNTV_CVAL change and unmasks automatically.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(not(miri))]
use super::sysreg;

/// Timer frequency in Hz. ARM generic timer on Apple HVF and QEMU virt
/// runs at 24 MHz. Using a const lets the compiler replace udiv with
/// multiply-shift (saves ~24 cycles per clock_read).
pub const TIMER_FREQ_HZ: u64 = 24_000_000;

const MAX_DEADLINES: usize = 4;

/// Per-core deadline queue — thread IDs. `u32::MAX` marks an empty slot.
static DQ_TID: [[AtomicU32; MAX_DEADLINES]; crate::config::MAX_CORES] =
    [const { [const { AtomicU32::new(u32::MAX) }; MAX_DEADLINES] }; crate::config::MAX_CORES];

/// Per-core deadline queue — absolute tick values (paired with DQ_TID).
static DQ_TICK: [[AtomicU64; MAX_DEADLINES]; crate::config::MAX_CORES] =
    [const { [const { AtomicU64::new(u64::MAX) }; MAX_DEADLINES] }; crate::config::MAX_CORES];

/// Monotonic counter for Miri — increments on each `now()` call so clock
/// reads are always non-decreasing.
#[cfg(miri)]
static MIRI_COUNTER: AtomicU64 = AtomicU64::new(1000);

/// Ensure the timer starts in a known disarmed state.
///
/// The GIC must be initialized before calling this — INTID 27 (virtual timer
/// PPI) must be enabled in the redistributor so that future timer arms
/// can deliver interrupts.
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

// ── Deadline queue ──────────────────────────────────────────────────

/// Insert a deadline for a thread on this core. Arms the hardware timer
/// to the earliest pending deadline (which may be this one or an existing
/// earlier entry).
pub fn insert_deadline(core_id: usize, thread_id: crate::types::ThreadId, deadline_tick: u64) {
    let q = &DQ_TID[core_id];
    let t = &DQ_TICK[core_id];

    for i in 0..MAX_DEADLINES {
        if q[i].load(Ordering::Relaxed) == u32::MAX {
            t[i].store(deadline_tick, Ordering::Relaxed);
            q[i].store(thread_id.0, Ordering::Release);
            rearm_earliest(core_id);

            return;
        }
    }
}

/// Remove a thread's deadline entry. Called when a thread is woken by
/// something other than the timer (IPC message, event signal, etc.)
/// so the stale entry doesn't fire later.
pub fn remove_deadline(core_id: usize, thread_id: crate::types::ThreadId) {
    let q = &DQ_TID[core_id];

    for slot in q.iter() {
        if slot.load(Ordering::Relaxed) == thread_id.0 {
            slot.store(u32::MAX, Ordering::Release);

            return;
        }
    }
}

/// Drain all expired deadline entries from this core's queue. Returns
/// up to `MAX_DEADLINES` thread IDs whose deadlines have passed.
/// Rearms the hardware timer for the next pending deadline (if any).
pub fn drain_expired(core_id: usize) -> [Option<crate::types::ThreadId>; MAX_DEADLINES] {
    let current = now();
    let q = &DQ_TID[core_id];
    let t = &DQ_TICK[core_id];
    let mut result = [None; MAX_DEADLINES];
    let mut count = 0;

    for i in 0..MAX_DEADLINES {
        let tid = q[i].load(Ordering::Acquire);

        if tid != u32::MAX && t[i].load(Ordering::Relaxed) <= current {
            q[i].store(u32::MAX, Ordering::Release);
            result[count] = Some(crate::types::ThreadId(tid));
            count += 1;
        }
    }

    rearm_earliest(core_id);

    result
}

/// Rearm the hardware timer to the earliest pending deadline on this
/// core, or push it far into the future if no deadlines remain.
fn rearm_earliest(core_id: usize) {
    let q = &DQ_TID[core_id];
    let t = &DQ_TICK[core_id];
    let mut earliest = u64::MAX;

    for i in 0..MAX_DEADLINES {
        if q[i].load(Ordering::Relaxed) != u32::MAX {
            let tick = t[i].load(Ordering::Relaxed);

            if tick < earliest {
                earliest = tick;
            }
        }
    }

    if earliest == u64::MAX {
        clear_deadline();
    } else {
        let delta = earliest.saturating_sub(now()).max(1);

        arm_timer(delta);
    }
}

// ── Hardware timer control ──────────────────────────────────────────

/// Arm the hardware timer to fire `ticks_from_now` ticks in the future.
fn arm_timer(_ticks_from_now: u64) {
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
/// Masks the timer interrupt (one-shot: does not re-arm). The caller
/// must follow with [`drain_expired`] to process the queue and rearm.
pub fn handle_deadline(_core_id: usize) {
    #[cfg(not(miri))]
    {
        sysreg::set_cntv_ctl_el0(0b11); // ENABLE=1, IMASK=1 (HVF detects re-arm via CVAL change)
        sysreg::isb();
    }
}

// ── Deferred timer wakeup (ISR → idle loop handoff) ─────────────────

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
