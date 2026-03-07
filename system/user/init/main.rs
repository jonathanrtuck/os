//! Init process — the first userspace program loaded by the kernel.
//!
//! Prints a greeting via the write syscall, then exits.

#![no_std]
#![no_main]

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
    sys_write(b"hello from init\n");
    sys_exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
