// AUDIT: 2026-03-11 — 0 unsafe blocks. 6-category checklist applied. No bugs
// found. Counter overflow: AtomicU64::fetch_add wraps at u64::MAX (per Rust
// spec). At 1 billion increments/sec, wraps in ~584 years — not a practical
// concern for diagnostic counters. Per-core isolation: each core writes to
// METRICS[core_id()], no cross-core contention. Relaxed ordering appropriate
// for monotonic diagnostics. panic_dump bypasses UART lock (safe for panic).

//! Per-core kernel event counters.
//!
//! Lightweight instrumentation for debugging SMP timing issues. Each core
//! increments its own `AtomicU64` counters using `Relaxed` ordering — these
//! are monotonic diagnostics, not synchronization primitives.
//!
//! Counters are printed on panic (bypassing locks) to provide post-mortem data.

use super::per_core::{self, MAX_CORES};
use super::serial;
use core::sync::atomic::{AtomicU64, Ordering};

static METRICS: [CoreMetrics; MAX_CORES] = {
    const INIT: CoreMetrics = CoreMetrics {
        context_switches: AtomicU64::new(0),
        syscalls: AtomicU64::new(0),
        page_faults: AtomicU64::new(0),
        timer_ticks: AtomicU64::new(0),
        lock_spins: AtomicU64::new(0),
    };
    [INIT; MAX_CORES]
};

pub struct CoreMetrics {
    pub context_switches: AtomicU64,
    pub syscalls: AtomicU64,
    pub page_faults: AtomicU64,
    pub timer_ticks: AtomicU64,
    pub lock_spins: AtomicU64,
}

#[inline(always)]
fn current() -> &'static CoreMetrics {
    &METRICS[per_core::core_id() as usize]
}
#[inline(always)]
pub fn inc_timer_ticks() {
    current().timer_ticks.fetch_add(1, Ordering::Relaxed);
}
/// Print all counters for all online cores. Uses panic-safe serial output
/// (bypasses UART lock). Safe to call from the panic handler.
pub fn panic_dump() {
    serial::panic_puts("📊 kernel metrics\n");

    for core in 0..MAX_CORES {
        if !per_core::is_online(core as u32) {
            continue;
        }

        let m = &METRICS[core];

        serial::panic_puts("  core ");
        serial::panic_put_u32(core as u32);
        serial::panic_puts(": ctx_sw=");

        panic_put_u64(m.context_switches.load(Ordering::Relaxed));

        serial::panic_puts(" syscall=");

        panic_put_u64(m.syscalls.load(Ordering::Relaxed));

        serial::panic_puts(" pgfault=");

        panic_put_u64(m.page_faults.load(Ordering::Relaxed));

        serial::panic_puts(" tick=");

        panic_put_u64(m.timer_ticks.load(Ordering::Relaxed));

        serial::panic_puts(" spin=");

        panic_put_u64(m.lock_spins.load(Ordering::Relaxed));

        serial::panic_puts("\n");
    }
}

/// Panic-safe decimal u64 printer — bypasses the UART lock.
fn panic_put_u64(mut n: u64) {
    if n == 0 {
        serial::panic_putc(b'0');

        return;
    }

    let mut buf = [0u8; 20];
    let mut i = buf.len();

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    for &byte in &buf[i..] {
        serial::panic_putc(byte);
    }
}

#[inline(always)]
pub fn inc_context_switches() {
    current().context_switches.fetch_add(1, Ordering::Relaxed);
}
#[inline(always)]
pub fn inc_lock_spins() {
    current().lock_spins.fetch_add(1, Ordering::Relaxed);
}
#[inline(always)]
pub fn inc_page_faults() {
    current().page_faults.fetch_add(1, Ordering::Relaxed);
}
#[inline(always)]
pub fn inc_syscalls() {
    current().syscalls.fetch_add(1, Ordering::Relaxed);
}
