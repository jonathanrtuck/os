// AUDIT: 2026-03-11 — 5 unsafe blocks verified, 6-category checklist applied.
// Findings: (1) timer::create() used wrapping addition for deadline computation;
// if `now + delta` overflowed u64, the timer would fire immediately instead of
// far in the future — fixed with saturating_add. (2) nomem correctly omitted
// from CNTP_TVAL, CNTP_CTL, CNTPCT, DAIFCLR writes (all have side effects).
// (3) nomem correctly present on CNTFRQ read (immutable firmware value).
// (4) counter_to_ns uses u128 intermediate — no overflow. (5) Two-phase wake
// in check_expired maintains lock ordering (timer → scheduler). (6) SAFETY
// comments added to all 5 blocks.
//
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
//!
//! Waiter registration and readiness tracking are delegated to `WaitableRegistry`.

use super::handle::HandleObject;
use super::interrupt_controller;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use super::waitable::{WaitableId, WaitableRegistry};
use core::sync::atomic::{AtomicU64, Ordering};

/// Maximum concurrent timer objects across all processes.
const MAX_TIMERS: usize = 32;
/// Timer fires 250 times/sec (4ms). Responsive enough for interactive use
/// without excessive overhead. SMP-safe: each core has its own timer PPI.
const TICKS_PER_SEC: u64 = 250;

/// Physical timer PPI interrupt ID.
pub const IRQ_ID: u32 = 30;

struct TimerTable {
    /// Deadline in counter ticks. Slot index = TimerId. `None` = free slot.
    slots: [Option<u64>; MAX_TIMERS],
    /// Readiness + waiter tracking for each timer.
    waiters: WaitableRegistry<TimerId>,
}

/// Opaque timer identifier. Index into the global timer table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerId(pub u8);

static CNTFRQ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static TIMERS: IrqMutex<TimerTable> = IrqMutex::new(TimerTable {
    slots: [const { None }; MAX_TIMERS],
    waiters: WaitableRegistry::new(),
});

impl WaitableId for TimerId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Set CNTP_TVAL_EL0 so the timer fires after one interval.
///
/// `freq` is CNTFRQ_EL0 (counter ticks per second), so `freq / TICKS_PER_SEC`
/// gives the number of counter ticks per timer interval. Writing TVAL also
/// clears the timer condition (de-asserts the interrupt).
fn reprogram(freq: u64) {
    let tval = freq / TICKS_PER_SEC;

    // SAFETY: Writing CNTP_TVAL_EL0 reprograms the timer countdown and
    // de-asserts the interrupt line — a hardware side effect. The ISB
    // ensures the new countdown is committed before returning. `nomem` is
    // intentionally omitted: the write has observable side effects that
    // LLVM must not reorder past surrounding memory operations (e.g.
    // check_expired's timer table reads). `nostack` is correct — MSR and
    // ISB do not touch the stack. `tval` is passed via in(reg).
    unsafe {
        core::arch::asm!(
            "msr cntp_tval_el0, {tval}",
            "isb",
            tval = in(reg) tval,
            options(nostack)
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

        for i in 0..MAX_TIMERS {
            if let Some(&deadline_ticks) = table.slots[i].as_ref() {
                if now >= deadline_ticks {
                    let id = TimerId(i as u8);

                    if let Some(waiter) = table.waiters.notify(id) {
                        to_wake[wake_count] = (id, waiter);
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
    TIMERS.lock().waiters.check_ready(id)
}
/// Read the hardware counter (CNTPCT_EL0). Monotonic, sub-tick precision.
pub fn counter() -> u64 {
    let cnt: u64;

    // SAFETY: Reading CNTPCT_EL0 (physical counter). The counter is
    // monotonically increasing hardware state. `nomem` is intentionally
    // omitted so LLVM cannot CSE or hoist repeated reads, which would
    // return stale values. `nostack` is correct — MRS does not touch the
    // stack. The result is written to `cnt` via out(reg).
    unsafe {
        core::arch::asm!("mrs {0}, cntpct_el0", out(reg) cnt, options(nostack));
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
    // overflow (freq ~62.5 MHz, timeout could be seconds). saturating_add
    // prevents wrap-around: if now + delta > u64::MAX, clamp to u64::MAX.
    // Without saturation, a large timeout near counter wrap would produce a
    // small deadline, causing the timer to fire immediately.
    let deadline_ticks = if timeout_ns == 0 {
        now // Already expired — will fire on next check.
    } else {
        let delta = (timeout_ns as u128 * freq as u128 / 1_000_000_000) as u64;
        now.saturating_add(delta)
    };
    let mut table = TIMERS.lock();

    for i in 0..MAX_TIMERS {
        if table.slots[i].is_none() {
            let id = TimerId(i as u8);

            table.slots[i] = Some(deadline_ticks);
            table.waiters.create(id);

            return Some(id);
        }
    }

    None
}
/// Destroy a timer object (called from `handle_close`).
///
/// Wakes any thread blocked in `sys_wait` on this timer — closing a handle
/// must not leave threads stuck forever.
pub fn destroy(id: TimerId) {
    let waiter = {
        let mut table = TIMERS.lock();
        table.slots[id.0 as usize] = None;
        table.waiters.destroy(id)
    };

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Timer(id);
        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
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

    // SAFETY: Reading CNTFRQ_EL0 — the counter frequency set by firmware
    // at boot. Read-only, immutable after firmware init. `nomem` is correct
    // here (unlike CNTPCT_EL0) because the value never changes — LLVM may
    // freely CSE/hoist this read. `nostack` is correct — MRS does not touch
    // the stack. Result written to `freq` via out(reg).
    unsafe {
        core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nostack, nomem));
    }

    CNTFRQ.store(freq, Ordering::Relaxed);

    // Program first interval and enable the timer.
    reprogram(freq);

    // SAFETY: Writing CNTP_CTL_EL0 with ENABLE=1, IMASK=0 starts generating
    // timer IRQs — a hardware side effect. `nomem` is intentionally omitted
    // because enabling the timer has observable effects that LLVM must not
    // reorder past the preceding reprogram() call. `nostack` is correct —
    // MOV and MSR do not touch the stack. x0 is clobbered and declared via
    // out("x0").
    unsafe {
        core::arch::asm!(
            "mov x0, #1",
            "msr cntp_ctl_el0, x0",       // ENABLE=1, IMASK=0
            out("x0") _,
            options(nostack)
        );
    }
    // SAFETY: Writing DAIFCLR with bit 1 clears DAIF.I, unmasking IRQs at
    // the CPU level. GIC routing is already configured; this is the final
    // gate. `nostack` is correct — MSR DAIFCLR does not touch the stack.
    // `nomem` is intentionally omitted: unmasking IRQs is a side effect
    // that affects control flow (IRQs may fire immediately after this
    // instruction).
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack));
    }
}
/// Register a thread as the waiter for this timer.
///
/// Called by `sys_wait` before checking readiness. If the timer fires
/// between registration and blocking, the wake is delivered correctly.
pub fn register_waiter(id: TimerId, thread: ThreadId) {
    TIMERS.lock().waiters.register_waiter(id, thread);
}
/// Monotonic tick count since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
/// Unregister a thread from a timer (cleanup when `wait` returns).
///
/// Safe to call even if the waiter was already cleared by the fire path.
pub fn unregister_waiter(id: TimerId) {
    TIMERS.lock().waiters.unregister_waiter(id);
}
