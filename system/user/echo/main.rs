//! Echo process — IPC ping-pong responder.
//!
//! Waits for init's signal, reads "ping" from shared memory,
//! writes "pong" back, and signals init. Demonstrates the other
//! side of shared-memory IPC.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

const SHM: *mut u8 = protocol::CHANNEL_SHM_BASE as *mut u8;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Wait for init's message.
    let _ = sys::wait(&[0], u64::MAX);
    // Read message from shared memory (incoming region: offset 0).
    let msg = unsafe { core::slice::from_raw_parts(SHM, 4) };

    sys::print(b"echo recv: ");
    sys::print(msg);
    sys::print(b"\n");

    // Smoke test: dynamic allocation via Vec (uses memory_alloc under the hood).
    let mut v: Vec<u8> = Vec::new();

    v.push(b'h');
    v.push(b'e');
    v.push(b'a');
    v.push(b'p');

    sys::print(b"  heap ok: ");
    sys::print(&v);
    sys::print(b"\n");

    // Write "pong" to outgoing region (offset 128), then signal init.
    let reply = b"pong";

    unsafe {
        core::ptr::copy_nonoverlapping(reply.as_ptr(), SHM.add(128), reply.len());
    }

    let _ = sys::channel_signal(0);

    sys::exit();
}
