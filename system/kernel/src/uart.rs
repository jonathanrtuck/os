//! PL011 UART driver (TX only).

use super::memory::KERNEL_VA_OFFSET;
use super::mmio;

const TXFF: u32 = 1 << 5;
const UART0_BASE: usize = 0x0900_0000 + KERNEL_VA_OFFSET;
const UART0_DR: usize = UART0_BASE;
const UART0_FR: usize = UART0_BASE + 0x18;

pub fn putc(c: u8) {
    while mmio::read32(UART0_FR) & TXFF != 0 {}
    mmio::write32(UART0_DR, c as u32);
}
pub fn puts(s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            putc(b'\r');
        }

        putc(byte);
    }
}
pub fn put_hex(v: u64) {
    let mut buf = [0u8; 16];
    let mut i = 0;

    while i < 16 {
        let shift = 60 - (i * 4);
        let nib = ((v >> shift) & 0xF) as u8;

        buf[i] = match nib {
            0..=9 => b'0' + nib,
            _ => b'A' + (nib - 10),
        };

        i += 1;
    }

    for b in buf {
        putc(b);
    }
}
pub fn put_u32(mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();

    if n == 0 {
        putc(b'0');

        return;
    }

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    for &byte in &buf[i..] {
        putc(byte);
    }
}
pub fn put_u64(mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();

    if n == 0 {
        putc(b'0');

        return;
    }

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    for &byte in &buf[i..] {
        putc(byte);
    }
}
