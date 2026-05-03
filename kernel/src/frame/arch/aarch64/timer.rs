//! ARM generic timer — virtual timer (CNTV_*) for periodic ticks.
//!
//! Configures the EL1 virtual timer to fire at a fixed interval (10 ms by
//! default). Each tick increments a monotonic counter. The timer interrupt
//! (INTID 27) is routed through the GIC and handled in the exception handler.
//!
//! ## HVF interaction
//!
//! When the virtual timer fires, Apple's Hypervisor.framework masks the timer
//! (sets IMASK in CNTV_CTL_EL0) and injects an IRQ to the guest. Writing
//! CNTV_TVAL_EL0 in the handler re-arms the timer — HVF detects the
//! CNTV_CVAL change and unmasks automatically.

use core::sync::atomic::{AtomicU64, Ordering};

use super::sysreg;

/// Tick rate in Hz (100 Hz = 10 ms interval).
const TICK_HZ: u64 = 100;

/// Monotonic tick counter, incremented by each timer interrupt.
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Compute the timer interval in counter ticks from the hardware frequency.
#[inline]
fn interval() -> u64 {
    sysreg::cntfrq_el0() / TICK_HZ
}

/// Configure the virtual timer and unmask IRQs.
///
/// The GIC must be initialized before calling this — INTID 27 (virtual timer
/// PPI) must be enabled in the redistributor.
pub fn init() {
    // Arm the timer: fire in `interval` ticks from now.
    sysreg::set_cntv_tval_el0(interval());

    // Enable the timer, unmask its interrupt.
    // Bit 0 (ENABLE) = 1, Bit 1 (IMASK) = 0.
    sysreg::set_cntv_ctl_el0(1);

    // Ensure the timer is fully configured before unmasking IRQs.
    sysreg::isb();

    // Unmask IRQs in PSTATE. From this point, the CPU will take IRQ
    // exceptions when the timer (or any enabled interrupt) fires.
    sysreg::enable_irqs();
}

/// Handle a timer tick. Called from the IRQ handler on INTID 27.
///
/// Re-arms the timer for the next interval. Writing CNTV_TVAL_EL0
/// automatically clears IMASK and ISTATUS. ISB ensures the re-arm is
/// committed before returning to the exception handler.
pub fn tick() {
    TICK_COUNT.fetch_add(1, Ordering::Relaxed);

    sysreg::set_cntv_tval_el0(interval());
    sysreg::isb();
}

/// Return the number of timer ticks since boot.
pub fn tick_count() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}
