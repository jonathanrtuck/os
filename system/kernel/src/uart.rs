//! PL011 UART driver (TX only).

use super::mmio;

const UART0_BASE: usize = 0x0900_0000;
const UART0_DR: usize = UART0_BASE;
const UART0_FR: usize = UART0_BASE + 0x18;
const TXFF: u32 = 1 << 5;

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
