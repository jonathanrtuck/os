//! ARM generic timer (EL1 physical) and userspace timer objects.
//!
//! Two concerns in one module:
//!
//! 1. **Hardware timer** — fixed-interval 250 Hz tick. Simple and predictable.
//!    A tickless design (next-event programming) would eliminate idle wakeups
//!    but adds complexity; fixed tick is the right starting point.
//!
//! 2. **Timer objects** — one-shot deadline handles for userspace. Created via
//!    `timer_create(timeout_ns)`, waited on via `wait`. The hardware tick checks
//!    all active timers and wakes blocked threads when deadlines expire.
//!    Level-triggered: once fired, the timer is permanently "ready" until closed.

use super::handle::HandleObject;
use super::interrupt_controller;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use core::sync::atomic::{AtomicU64, Ordering};

/// A one-shot timer kernel object.
///
/// Created with an absolute deadline (computed from relative timeout at
/// creation time). Becomes permanently "ready" when the deadline passes.
struct Timer {
    /// Absolute deadline in counter ticks (not nanoseconds — avoids repeated
    /// ns↔ticks conversion on every tick check).
    deadline_ticks: u64,
    /// True once the deadline has passed. Level-triggered: stays true forever.
    fired: bool,
    /// Thread currently waiting on this timer via `wait`. Set by
    /// `register_waiter`, cleared on fire or explicit unregister.
    waiter: Option<ThreadId>,
}
struct TimerTable {
    slots: [Option<Timer>; MAX_TIMERS],
}

/// Opaque timer identifier. Index into the global timer table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerId(pub u8);

/// Maximum concurrent timer objects across all processes.
const MAX_TIMERS: usize = 32;
/// Timer fires 250 times/sec (4ms). Responsive enough for interactive use
/// without excessive overhead. SMP-safe: each core has its own timer PPI.
const TICKS_PER_SEC: u64 = 250;

/// Physical timer PPI interrupt ID.
pub const IRQ_ID: u32 = 30;

static CNTFRQ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static TIMERS: IrqMutex<TimerTable> = IrqMutex::new(TimerTable {
    slots: [const { None }; MAX_TIMERS],
});

/// Set CNTP_TVAL_EL0 so the timer fires after one interval.
///
/// `freq` is CNTFRQ_EL0 (counter ticks per second), so `freq / TICKS_PER_SEC`
/// gives the number of counter ticks per timer interval. Writing TVAL also
/// clears the timer condition (de-asserts the interrupt).
fn reprogram(freq: u64) {
    let tval = freq / TICKS_PER_SEC;

    unsafe {
        core::arch::asm!(
            "msr cntp_tval_el0, {tval}",
            "isb",
            tval = in(reg) tval,
            options(nostack, nomem)
        );
    }
}

/// Check all timers for expiry. Called from the timer IRQ handler.
///
/// Two-phase design: collect fired timers under the timer lock, then wake
/// threads after releasing it. Maintains lock ordering: timer → scheduler.
pub fn check_expired() {
    let now = counter();
    // Phase 1: collect fired timers under lock.
    let mut to_wake: [(TimerId, ThreadId); MAX_TIMERS] = [(TimerId(0), ThreadId(0)); MAX_TIMERS];
    let mut wake_count = 0;

    {
        let mut table = TIMERS.lock();

        for (i, slot) in table.slots.iter_mut().enumerate() {
            if let Some(timer) = slot {
                if !timer.fired && now >= timer.deadline_ticks {
                    timer.fired = true;

                    if let Some(waiter) = timer.waiter.take() {
                        to_wake[wake_count] = (TimerId(i as u8), waiter);
                        wake_count += 1;
                    }
                }
            }
        }
    }

    // Phase 2: wake threads (acquires scheduler lock).
    for &(timer_id, thread_id) in &to_wake[..wake_count] {
        if !scheduler::try_wake_for_handle(thread_id, HandleObject::Timer(timer_id)) {
            scheduler::set_wake_pending_for_handle(thread_id, HandleObject::Timer(timer_id));
        }
    }
}
/// Check whether a timer has fired (for `sys_wait` readiness check).
///
/// Level-triggered: returns `true` every time after the deadline passes.
/// Does not consume the signal (unlike channels).
pub fn check_fired(id: TimerId) -> bool {
    let table = TIMERS.lock();

    table.slots[id.0 as usize].as_ref().is_some_and(|t| t.fired)
}
/// Read the hardware counter (CNTPCT_EL0). Monotonic, sub-tick precision.
pub fn counter() -> u64 {
    let cnt: u64;

    unsafe {
        core::arch::asm!("mrs {0}, cntpct_el0", out(reg) cnt, options(nostack, nomem));
    }

    cnt
}
/// Counter frequency in Hz (cached from CNTFRQ_EL0).
pub fn counter_freq() -> u64 {
    CNTFRQ.load(Ordering::Relaxed)
}
/// Convert hardware counter ticks to nanoseconds.
/// Uses u128 intermediate to avoid overflow (freq can be ~62.5 MHz).
pub fn counter_to_ns(ticks: u64) -> u64 {
    let freq = counter_freq();

    if freq == 0 {
        return 0;
    }

    (ticks as u128 * 1_000_000_000 / freq as u128) as u64
}
/// Create a one-shot timer that fires after `timeout_ns` nanoseconds.
///
/// Returns the timer ID on success, or `None` if the table is full.
pub fn create(timeout_ns: u64) -> Option<TimerId> {
    let now = counter();
    let freq = counter_freq();
    // Convert timeout from nanoseconds to counter ticks. Use u128 to avoid
    // overflow (freq ~62.5 MHz, timeout could be seconds).
    let deadline_ticks = if timeout_ns == 0 {
        now // Already expired — will fire on next check.
    } else {
        now + (timeout_ns as u128 * freq as u128 / 1_000_000_000) as u64
    };
    let mut table = TIMERS.lock();

    for (i, slot) in table.slots.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(Timer {
                deadline_ticks,
                fired: false,
                waiter: None,
            });

            return Some(TimerId(i as u8));
        }
    }

    None
}
/// Destroy a timer object (called from `handle_close`).
pub fn destroy(id: TimerId) {
    let mut table = TIMERS.lock();

    table.slots[id.0 as usize] = None;
}
/// Handle a timer interrupt: increment tick count, check timer objects, reprogram.
pub fn handle_irq() {
    TICKS.fetch_add(1, Ordering::Relaxed);

    check_expired();
    reprogram(CNTFRQ.load(Ordering::Relaxed));
}
/// Initialize the timer. Call after `interrupt_controller::init()`.
pub fn init() {
    interrupt_controller::enable_irq(IRQ_ID);

    // CNTFRQ_EL0: counter frequency in Hz, set by firmware (e.g. 62.5 MHz on QEMU)
    let freq: u64;

    unsafe {
        core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nostack, nomem));
    }

    CNTFRQ.store(freq, Ordering::Relaxed);

    // Program first interval and enable the timer.
    reprogram(freq);

    unsafe {
        core::arch::asm!(
            "mov x0, #1",
            "msr cntp_ctl_el0, x0",       // ENABLE=1, IMASK=0
            out("x0") _,
            options(nostack, nomem)
        );
    }
    // Clear DAIF.I to unmask IRQs at the CPU level.
    // GIC routing is already configured; this is the final gate.
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack, nomem));
    }
}
/// Register a thread as the waiter for this timer.
///
/// Called by `sys_wait` before checking readiness. If the timer fires
/// between registration and blocking, the wake is delivered correctly.
pub fn register_waiter(id: TimerId, thread: ThreadId) {
    let mut table = TIMERS.lock();

    if let Some(timer) = &mut table.slots[id.0 as usize] {
        timer.waiter = Some(thread);
    }
}
/// Monotonic tick count since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
/// Unregister a thread from a timer (cleanup when `wait` returns).
///
/// Safe to call even if the waiter was already cleared by the fire path.
pub fn unregister_waiter(id: TimerId) {
    let mut table = TIMERS.lock();

    if let Some(timer) = &mut table.slots[id.0 as usize] {
        timer.waiter = None;
    }
}
