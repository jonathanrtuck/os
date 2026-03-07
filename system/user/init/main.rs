//! Init process — IPC ping-pong initiator.
//!
//! Writes "ping" to shared memory, signals echo, waits for reply,
//! reads "pong" back. Demonstrates shared-memory IPC with signal/wait.

#![no_std]
#![no_main]

const SHM: *mut u8 = 0x4000_0000 as *mut u8;

#[inline(always)]
fn sys_channel_signal(handle: u64) {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x0") handle,
            in("x8") 4u64, // SYS_CHANNEL_SIGNAL
            lateout("x0") _,
            options(nostack),
        );
    }
}
#[inline(always)]
fn sys_channel_wait(handle: u64) {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x0") handle,
            in("x8") 5u64, // SYS_CHANNEL_WAIT
            lateout("x0") _,
            options(nostack),
        );
    }
}
#[inline(always)]
fn sys_exit() -> ! {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") 0u64, // SYS_EXIT
            options(noreturn, nostack),
        );
    }
}
#[inline(always)]
fn sys_write(buf: &[u8]) -> u64 {
    let ret: u64;

    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x0") buf.as_ptr(),
            in("x1") buf.len(),
            in("x8") 1u64, // SYS_WRITE
            lateout("x0") ret,
            options(nostack),
        );
    }

    ret
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Write "ping" to shared memory (outgoing region: offset 0).
    let msg = b"ping";

    unsafe {
        core::ptr::copy_nonoverlapping(msg.as_ptr(), SHM, msg.len());
    }

    // Signal echo that data is ready, then wait for reply.
    sys_channel_signal(0);
    sys_channel_wait(0);

    // Read reply from incoming region (offset 128).
    let reply = unsafe { core::slice::from_raw_parts(SHM.add(128), 4) };

    sys_write(b"init recv: ");
    sys_write(reply);
    sys_write(b"\n");
    sys_exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
