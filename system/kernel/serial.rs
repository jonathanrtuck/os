// AUDIT: 2026-03-11 — 0 unsafe blocks (MMIO via memory_mapped_io module).
// 6-category checklist applied. No bugs found. SMP safety: IrqMutex guards
// all non-panic output. Panic variants deliberately bypass lock (deadlock
// avoidance, documented). MMIO addresses use KERNEL_VA_OFFSET. TX timeout
// prevents infinite spin on stuck FIFO.

//! PL011 UART driver (TX only) with SMP-safe locking.
//!
//! All public output functions acquire `LOCK` to prevent interleaved output
//! from concurrent cores. The `panic_` variants bypass the lock for use in
//! the panic handler (where the lock may already be held).

use super::memory::KERNEL_VA_OFFSET;
use super::memory_mapped_io;
use super::sync::IrqMutex;

const TXFF: u32 = 1 << 5;
const UART0_BASE: usize = 0x0900_0000 + KERNEL_VA_OFFSET;
const UART0_DR: usize = UART0_BASE;
const UART0_FR: usize = UART0_BASE + 0x18;
/// Maximum iterations to wait for UART TXFF to clear. If the FIFO is
/// stuck, we write anyway (lossy output > dead kernel).
const TX_TIMEOUT: u32 = 1_000_000;

static LOCK: IrqMutex<()> = IrqMutex::new(());

/// Raw character output — no lock. For internal use and panic handler.
fn raw_putc(c: u8) {
    let mut timeout = TX_TIMEOUT;

    while memory_mapped_io::read32(UART0_FR) & TXFF != 0 {
        timeout -= 1;

        if timeout == 0 {
            break;
        }
    }

    memory_mapped_io::write32(UART0_DR, c as u32);
}
fn raw_puts(s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            raw_putc(b'\r');
        }
        raw_putc(byte);
    }
}

/// Panic-safe put_hex — bypasses the lock.
pub fn panic_put_hex(v: u64) {
    let mut buf = [0u8; 16];

    for i in 0..16 {
        let shift = 60 - (i * 4);
        let nib = ((v >> shift) & 0xF) as u8;

        buf[i] = match nib {
            0..=9 => b'0' + nib,
            _ => b'A' + (nib - 10),
        };
    }

    for b in buf {
        raw_putc(b);
    }
}
/// Panic-safe put_u32 — bypasses the lock.
pub fn panic_put_u32(mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();

    if n == 0 {
        raw_putc(b'0');

        return;
    }

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    for &byte in &buf[i..] {
        raw_putc(byte);
    }
}
/// Panic-safe putc — bypasses the lock.
pub fn panic_putc(c: u8) {
    raw_putc(c);
}
/// Panic-safe puts — bypasses the lock. Only for the panic handler where
/// the lock may already be held (deadlock avoidance).
pub fn panic_puts(s: &str) {
    raw_puts(s);
}
pub fn put_hex(v: u64) {
    let _guard = LOCK.lock();

    panic_put_hex(v);
}
pub fn put_u32(n: u32) {
    let _guard = LOCK.lock();

    panic_put_u32(n);
}
pub fn put_u64(mut n: u64) {
    let _guard = LOCK.lock();
    let mut buf = [0u8; 20];
    let mut i = buf.len();

    if n == 0 {
        raw_putc(b'0');

        return;
    }

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    for &byte in &buf[i..] {
        raw_putc(byte);
    }
}
pub fn putc(c: u8) {
    let _guard = LOCK.lock();

    raw_putc(c);
}
pub fn puts(s: &str) {
    let _guard = LOCK.lock();

    raw_puts(s);
}
