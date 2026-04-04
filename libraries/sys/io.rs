//! I/O and formatting utilities.

use crate::{
    asm::{nr, result, syscall2},
    types::SyscallResult,
};

/// Write `buf` to the kernel console (UART).
///
/// Returns the number of bytes written. For fire-and-forget logging, use
/// `print` instead.
pub fn write(buf: &[u8]) -> SyscallResult<usize> {
    let raw = unsafe { syscall2(nr::WRITE, buf.as_ptr() as u64, buf.len() as u64) as i64 };

    result(raw).map(|v| v as usize)
}

/// Write to the kernel console, ignoring errors.
///
/// Convenience wrapper around `write` for debug logging where the caller
/// doesn't need (or want) to handle UART failures.
pub fn print(buf: &[u8]) {
    let _ = write(buf);
}

/// Format a `u32` as decimal ASCII into `buf`, returning bytes written.
/// Returns 0 if `buf` is empty.
pub fn format_u32(mut n: u32, buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut i = 10;
    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let len = 10 - i;
    buf[..len].copy_from_slice(&tmp[i..]);
    len
}

/// Print a `u32` as decimal to the kernel console.
pub fn print_u32(mut n: u32) {
    if n == 0 {
        print(b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 10;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    print(&buf[i..]);
}
