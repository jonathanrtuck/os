//! PL011 UART driver (TX only).
//!
//! Writes directly to the physical UART address. Before the MMU is enabled,
//! this works because the hypervisor/QEMU maps device memory at the physical
//! address. After the MMU is enabled, the caller must ensure the UART PA is
//! mapped.
//!
//! All output goes through [`Writer`]'s [`core::fmt::Write`] implementation.
//!
//! The spinlock is only active after [`enable_lock`] is called (post-MMU,
//! pre-secondary-core activation). Before that, only core 0 runs, so no
//! lock is needed. Atomic operations require cacheable memory (MMU on).

use core::sync::atomic::{AtomicBool, Ordering};

use super::{mmio, platform};

const TX_TIMEOUT: u32 = 1_000_000;
const TXFF: u32 = 1 << 5;

static SERIAL_LOCK: AtomicBool = AtomicBool::new(false);
static LOCK_ENABLED: AtomicBool = AtomicBool::new(false);
static SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Enable the serial spinlock. Call after the MMU is enabled and before
/// secondary cores start printing.
pub fn enable_lock() {
    LOCK_ENABLED.store(true, Ordering::Release);
}

/// Suppress informational serial output. Call once userspace owns the UART
/// via the console service — avoids interleaved output from kernel and
/// userspace writing to the same UART register on different cores.
pub fn suppress() {
    SUPPRESSED.store(true, Ordering::Release);
}

/// Force-release the serial lock and clear suppression. Called from the
/// panic handler to ensure panic messages always reach the UART.
pub fn break_lock() {
    SUPPRESSED.store(false, Ordering::Release);
    SERIAL_LOCK.store(false, Ordering::Release);
}

/// RAII guard that releases the serial lock on drop, ensuring the lock is
/// freed even if the caller panics (e.g., during a panic message).
struct SerialGuard {
    held: bool,
}

impl SerialGuard {
    fn acquire() -> Self {
        if !LOCK_ENABLED.load(Ordering::Relaxed) {
            return Self { held: false };
        }

        loop {
            while SERIAL_LOCK.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }

            if SERIAL_LOCK
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Self { held: true };
            }
        }
    }
}

impl Drop for SerialGuard {
    fn drop(&mut self) {
        if self.held {
            SERIAL_LOCK.store(false, Ordering::Release);
        }
    }
}

/// PL011 UART output. Use with [`core::fmt::Write`]:
///
/// ```ignore
/// use core::fmt::Write;
/// writeln!(serial::Writer, "hello {}", 42).ok();
/// ```
pub struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                putc(b'\r');
            }

            putc(byte);
        }

        Ok(())
    }

    // Hold the lock across the entire formatted message. The default
    // write_fmt calls write_str per segment, which would interleave
    // output from concurrent cores.
    fn write_fmt(&mut self, args: core::fmt::Arguments<'_>) -> core::fmt::Result {
        if SUPPRESSED.load(Ordering::Relaxed) {
            return Ok(());
        }

        let _guard = SerialGuard::acquire();

        core::fmt::write(self, args)
    }
}

/// Write a single byte to the UART, waiting if the TX FIFO is full.
fn putc(c: u8) {
    let mut timeout = TX_TIMEOUT;

    while mmio::read32(uart0_fr()) & TXFF != 0 {
        timeout -= 1;

        if timeout == 0 {
            break;
        }
    }

    // Write even if the FIFO is full after timeout — losing a character
    // is better than hanging the kernel (this is often a panic dump path).
    mmio::write32(uart0_dr(), c as u32);
}

#[inline(always)]
fn uart0_dr() -> usize {
    platform::device_addr(platform::UART_BASE)
}

#[inline(always)]
fn uart0_fr() -> usize {
    platform::device_addr(platform::UART_BASE + 0x18)
}
