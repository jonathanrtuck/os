#![no_std]

use abi::types::Handle;

pub const METHOD_WRITE: u32 = 1;

pub fn write(console_ep: Handle, text: &[u8]) {
    let mut buf = [0u8; ipc::message::MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, METHOD_WRITE, text);
    let _ = abi::ipc::call(console_ep, &mut buf, total, &[], &mut []);
}

pub fn write_u32(console_ep: Handle, prefix: &[u8], n: u32) {
    let mut text = [0u8; 80];
    let plen = prefix.len().min(60);

    text[..plen].copy_from_slice(&prefix[..plen]);

    let nlen = format_u32(n, &mut text[plen..]);

    text[plen + nlen] = b'\n';

    write(console_ep, &text[..plen + nlen + 1]);
}

pub fn format_u32(mut n: u32, buf: &mut [u8]) -> usize {
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
