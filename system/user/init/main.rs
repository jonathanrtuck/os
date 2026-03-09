//! Init process — IPC ping-pong initiator.
//!
//! Writes "ping" to shared memory, signals echo, waits for reply,
//! reads "pong" back. Demonstrates shared-memory IPC with signal/wait.

#![no_std]
#![no_main]

const SHM: *mut u8 = 0x4000_0000 as *mut u8;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Write "ping" to shared memory (outgoing region: offset 0).
    let msg = b"ping";

    unsafe {
        core::ptr::copy_nonoverlapping(msg.as_ptr(), SHM, msg.len());
    }

    // Signal echo that data is ready, then wait for reply.
    sys::channel_signal(0);
    sys::wait(&[0], u64::MAX);

    // Read reply from incoming region (offset 128).
    let reply = unsafe { core::slice::from_raw_parts(SHM.add(128), 4) };

    sys::write(b"init recv: ");
    sys::write(reply);
    sys::write(b"\n");
    sys::exit();
}
