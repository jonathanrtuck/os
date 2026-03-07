//! Echo process — a second userspace program for testing multi-process.
//!
//! Prints a message, yields, and repeats. Proves two EL0 processes
//! run concurrently under the preemptive scheduler with TTBR0 swap.

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
#[inline(always)]
fn sys_yield() {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") 2u64, // SYS_YIELD
            lateout("x0") _,
            options(nostack),
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    for _ in 0..3 {
        sys_write(b"hello from echo\n");
        sys_yield();
    }

    sys_exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
