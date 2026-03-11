//! Echo process — IPC ping-pong responder.
//!
//! Waits for init's signal, reads "ping" from shared memory,
//! writes "pong" back, and signals init. Demonstrates the other
//! side of shared-memory IPC.

#![no_std]
#![no_main]

const SHM: *mut u8 = 0x4000_0000 as *mut u8; // must match kernel paging::CHANNEL_SHM_BASE

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Wait for init's message.
    let _ = sys::wait(&[0], u64::MAX);
    // Read message from shared memory (incoming region: offset 0).
    let msg = unsafe { core::slice::from_raw_parts(SHM, 4) };

    sys::print(b"echo recv: ");
    sys::print(msg);
    sys::print(b"\n");

    // Write "pong" to outgoing region (offset 128), then signal init.
    let reply = b"pong";

    unsafe {
        core::ptr::copy_nonoverlapping(reply.as_ptr(), SHM.add(128), reply.len());
    }

    let _ = sys::channel_signal(0);

    sys::exit();
}
