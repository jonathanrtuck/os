// AUDIT: 2026-03-11 — 5 unsafe blocks verified, 6-category checklist applied.
// Findings: (1) timer::create() used wrapping addition for deadline computation;
// if `now + delta` overflowed u64, the timer would fire immediately instead of
// far in the future — fixed with saturating_add. (2) nomem correctly omitted
// from CNTV_TVAL, CNTV_CTL, CNTVCT, DAIFCLR writes (all have side effects).
// (3) nomem correctly present on CNTFRQ read (immutable firmware value).
// (4) counter_to_ns uses u128 intermediate — no overflow. (5) Two-phase wake
// in check_expired maintains lock ordering (timer → scheduler). (6) SAFETY
// comments added to all 5 blocks.
//
// HVF compatibility (2026-03-17): switched from physical timer (CNTP_*) to
// virtual timer (CNTV_*). Under HVF (Apple Hypervisor.framework), the physical
// timer is managed by the hypervisor for VM scheduling. Guest access to
// CNTP_TVAL_EL0 / CNTP_CTL_EL0 traps with EC=0 (uncategorized). The virtual
// timer is the standard guest timer for both bare-metal and HVF. CNTVOFF_EL2
// is set to 0 by both our boot.S (bare-metal) and QEMU HVF, so CNTVCT_EL0
// equals CNTPCT_EL0. Virtual timer PPI = IRQ 27 (vs physical PPI = IRQ 30).
//
//! ARM generic timer (EL1 virtual) and userspace timer objects.
//!
//! Two concerns in one module:
//!
//! 1. **Hardware timer** — tickless (next-event programming). Each core
//!    programs CNTV_TVAL_EL0 to fire at the earliest deadline across: timer
//!    objects, scheduler quantum expiry, and scheduling context replenishment.
//!    Cores with no deadlines enter WFI and wake only on IPI or device IRQ.
//!
//! 2. **Timer objects** — one-shot deadline handles for userspace. Created via
//!    `timer_create(timeout_ns)`, waited on via `wait`. The timer IRQ handler
//!    checks all active timers and wakes blocked threads when deadlines expire.
//!    Level-triggered: once fired, the timer is permanently "ready" until closed.
//!
//! Waiter registration and readiness tracking are delegated to `WaitableRegistry`.

use core::sync::atomic::{AtomicU64, Ordering};

use super::{
    handle::HandleObject,
    interrupt_controller::{self, InterruptController},
    scheduler,
    sync::IrqMutex,
    thread::ThreadId,
    waitable::{WaitableId, WaitableRegistry},
};

/// Maximum concurrent timer objects across all processes.
const MAX_TIMERS: usize = 32;

/// Virtual timer PPI interrupt ID (27). Physical timer PPI (30) is reserved
/// by the hypervisor under HVF.
pub const IRQ_ID: u32 = 27;

static CNTFRQ: AtomicU64 = AtomicU64::new(0);
/// Cached earliest timer deadline in counter ticks. Updated on timer create,
/// destroy, and check_expired. 0 means "no timers" (sentinel). Avoids acquiring
/// the TIMERS lock on every schedule_inner call.
static EARLIEST_DEADLINE: AtomicU64 = AtomicU64::new(0);
static TIMERS: IrqMutex<TimerTable> = IrqMutex::new(TimerTable {
    slots: [const { None }; MAX_TIMERS],
    waiters: WaitableRegistry::new(),
});

struct TimerTable {
    /// Deadline in counter ticks. Slot index = TimerId. `None` = free slot.
    slots: [Option<u64>; MAX_TIMERS],
    /// Readiness + waiter tracking for each timer.
    waiters: WaitableRegistry<TimerId>,
}

/// Opaque timer identifier. Index into the global timer table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerId(pub u8);

impl WaitableId for TimerId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Recompute and cache the earliest timer deadline from the TIMERS table.
/// Must be called with the TIMERS lock held (not public).
fn update_earliest_deadline_locked(table: &TimerTable) {
    let mut earliest: u64 = 0; // 0 = sentinel for "no timers"

    for (i, slot) in table.slots.iter().enumerate() {
        if let Some(&deadline) = slot.as_ref() {
            // Skip timers that have already fired (ready=true). Their expired
            // deadlines are in the past and would poison EARLIEST_DEADLINE,
            // causing reprogram_next_deadline to program TVAL=1 repeatedly
            // (timer IRQ storm). Fired timers stay in the slot until the
            // owning thread closes the handle; they don't need hardware timer
            // wakeups — they're already level-triggered ready.
            let id = TimerId(i as u8);

            if table.waiters.check_ready(id) {
                continue;
            }

            if earliest == 0 || deadline < earliest {
                earliest = deadline;
            }
        }
    }

    EARLIEST_DEADLINE.store(earliest, Ordering::Release);
}

/// Return the cached earliest timer object deadline, or None if no timers.
/// Lock-free: reads an atomic that is updated on timer create/destroy/expire.
fn earliest_timer_deadline() -> Option<u64> {
    let cached = EARLIEST_DEADLINE.load(Ordering::Acquire);

    if cached == 0 {
        None
    } else {
        Some(cached)
    }
}

/// Program the hardware timer with the given number of counter ticks.
///
/// Writing TVAL also clears the timer condition (de-asserts the interrupt).
fn program_tval(tval: u64) {
    super::arch::timer::program_tval(tval);
}

/// Reprogram the hardware timer for the earliest deadline across all sources.
///
/// Sources:
/// 1. Timer objects: scanned from the global TIMERS table.
/// 2. Scheduler deadline: quantum expiry or context replenishment (passed by caller).
///
/// The scheduler deadline is passed as a parameter because this function is
/// called from `schedule_inner` (which holds the scheduler lock — we can't
/// re-acquire it). When called from `timer::create`, pass `None` for the
/// scheduler deadline — `schedule_inner` will reprogram with full info on
/// the next schedule.
///
/// If the minimum deadline is in the past, sets CNTV_TVAL to 1 (fire immediately).
/// If no deadlines exist, programs the maximum interval (u32::MAX ticks ≈ 68s).
pub fn reprogram_next_deadline(scheduler_deadline_ticks: Option<u64>) {
    let now = counter();
    // Source 1: earliest timer object deadline.
    let timer_deadline = earliest_timer_deadline();
    // Collect all deadline sources and find the minimum.
    let mut earliest: Option<u64> = timer_deadline;

    if let Some(sched) = scheduler_deadline_ticks {
        earliest = Some(earliest.map_or(sched, |e| e.min(sched)));
    }

    match earliest {
        None => {
            // No deadlines — program a very long interval. u32::MAX ticks
            // ≈ 68 seconds at 62.5 MHz. The core will be woken by IPI or
            // device IRQ if work arrives before this expires.
            program_tval(u32::MAX as u64);
        }
        Some(deadline) if deadline <= now => {
            // Deadline already passed — fire immediately.
            program_tval(1);
        }
        Some(deadline) => {
            let delta = deadline - now;
            // Cap to u32::MAX (CNTV_TVAL is 32-bit).
            let tval = delta.min(u32::MAX as u64);

            program_tval(tval);
        }
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
    let mut any_expired = false;

    {
        let mut table = TIMERS.lock();

        for i in 0..MAX_TIMERS {
            if let Some(&deadline_ticks) = table.slots[i].as_ref() {
                if now >= deadline_ticks {
                    any_expired = true;

                    let id = TimerId(i as u8);

                    if let Some(waiter) = table.waiters.notify(id) {
                        to_wake[wake_count] = (id, waiter);
                        wake_count += 1;
                    }
                }
            }
        }

        // Update cached earliest deadline whenever expired timers exist.
        // Previously guarded by `wake_count > 0`, which missed the case
        // where a timer expired but had no waiter (already notified, not
        // yet destroyed). The stale expired deadline in EARLIEST_DEADLINE
        // caused reprogram_next_deadline to program TVAL=1 repeatedly,
        // creating a timer IRQ storm that starved the display pipeline.
        if any_expired {
            update_earliest_deadline_locked(&table);
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
/// Read the hardware counter. Monotonic, sub-tick precision.
pub fn counter() -> u64 {
    super::arch::timer::counter()
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
    let result = {
        let mut table = TIMERS.lock();
        let mut found = None;

        for i in 0..MAX_TIMERS {
            if table.slots[i].is_none() {
                let id = TimerId(i as u8);

                table.slots[i] = Some(deadline_ticks);

                table.waiters.create(id);

                found = Some(id);

                break;
            }
        }

        // Update the cached earliest deadline while we hold the lock.
        if found.is_some() {
            update_earliest_deadline_locked(&table);
        }

        found
    };

    // Reprogram the hardware timer so the new deadline takes effect.
    // Only reprogram if the new timer's deadline is meaningfully in the future
    // (more than 100µs). Very short timers (including already-expired ones)
    // will be caught by check_fired() on the next poll or check_expired() on
    // the next schedule event. This prevents IRQ storms from rapid
    // create-poll-destroy patterns (e.g., stress test timer worker).
    if result.is_some() {
        let now = counter();

        if deadline_ticks > now + (counter_freq() / 10_000) {
            // Timer is > 100µs in the future — reprogram to catch it.
            reprogram_next_deadline(None);
        }
    }

    result
}
/// Destroy a timer object (called from `handle_close`).
///
/// Wakes any thread blocked in `sys_wait` on this timer — closing a handle
/// must not leave threads stuck forever.
pub fn destroy(id: TimerId) {
    let waiter = {
        let mut table = TIMERS.lock();
        table.slots[id.0 as usize] = None;
        update_earliest_deadline_locked(&table);
        table.waiters.destroy(id)
    };

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Timer(id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Handle a timer interrupt: check timer objects for expiry.
///
/// Note: `reprogram_next_deadline` is NOT called here. The caller (irq_handler)
/// calls `scheduler::schedule()` next, and `schedule_inner` calls
/// `reprogram_next_deadline` with the full scheduler deadline (quantum +
/// replenishment). This avoids double-reprogramming and ensures the scheduler's
/// deadline is always included.
pub fn handle_irq() {
    check_expired();
}
/// Initialize the timer. Call after `interrupt_controller::init()`.
///
/// Tickless: does NOT program a fixed 250 Hz interval. Instead, programs
/// the timer for a long sleep (u32::MAX ticks). The first `schedule_inner`
/// call will reprogram with the actual deadline.
pub fn init() {
    interrupt_controller::GIC.enable_irq(IRQ_ID);

    let freq = super::arch::timer::read_frequency();

    CNTFRQ.store(freq, Ordering::Relaxed);

    super::arch::timer::enable_el0_counter();

    // Tickless: program a long initial interval. No fixed 250 Hz tick.
    // The first schedule_inner call will reprogram with the actual deadline.
    program_tval(u32::MAX as u64);

    super::arch::timer::enable_virtual_timer();
    super::arch::timer::unmask_irqs();
}
/// Register a thread as the waiter for this timer.
///
/// Called by `sys_wait` before checking readiness. If the timer fires
/// between registration and blocking, the wake is delivered correctly.
pub fn register_waiter(id: TimerId, thread: ThreadId) {
    TIMERS.lock().waiters.register_waiter(id, thread);
}
/// Unregister a thread from a timer (cleanup when `wait` returns).
///
/// Safe to call even if the waiter was already cleared by the fire path.
pub fn unregister_waiter(id: TimerId) {
    TIMERS.lock().waiters.unregister_waiter(id);
}
