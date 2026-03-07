//! Echo process — IPC ping-pong responder.
//!
//! Waits for init's signal, reads "ping" from shared memory,
//! writes "pong" back, and signals init. Demonstrates the other
//! side of shared-memory IPC.

#![no_std]
#![no_main]

const SHM: *mut u8 = 0x4000_0000 as *mut u8;

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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Wait for init's message.
    sys_channel_wait(0);

    // Read message from shared memory (incoming region: offset 0).
    let msg = unsafe { core::slice::from_raw_parts(SHM, 4) };

    sys_write(b"echo recv: ");
    sys_write(msg);
    sys_write(b"\n");

    // Write "pong" to outgoing region (offset 128), then signal init.
    let reply = b"pong";

    unsafe {
        core::ptr::copy_nonoverlapping(reply.as_ptr(), SHM.add(128), reply.len());
    }

    sys_channel_signal(0);
    sys_exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
