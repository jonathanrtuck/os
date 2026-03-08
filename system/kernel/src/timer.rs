//! ARM generic timer (EL1 physical).
//!
//! Uses a fixed-interval tick: the timer fires, we handle it, reprogram it for
//! the same interval, repeat. Simple and predictable, but the CPU wakes on every
//! tick even when idle. A tickless design (program the timer for "next event"
//! instead of a fixed interval) would eliminate idle wakeups but adds complexity
//! — we'd need a sorted event queue and careful reprogramming on insert/cancel.
//! Fixed tick is the right starting point; tickless is an optimization for later.

use super::gic;
use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);
static CNTFRQ: AtomicU64 = AtomicU64::new(0);

/// Physical timer PPI interrupt ID.
pub const IRQ_ID: u32 = 30;

/// Timer fires 250 times/sec (4ms). Responsive enough for interactive use
/// without excessive overhead. SMP-safe: each core has its own timer PPI.
const TICKS_PER_SEC: u64 = 250;

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
            tval = in(reg) tval,
            options(nostack, nomem)
        );
    }
}

/// Handle a timer interrupt: increment tick count and reprogram for next interval.
pub fn handle_irq() {
    TICKS.fetch_add(1, Ordering::Relaxed);

    reprogram(CNTFRQ.load(Ordering::Relaxed));
}
/// Initialize the timer. Call after `gic::init()`.
pub fn init() {
    gic::enable_irq(IRQ_ID);

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
/// Monotonic tick count since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
