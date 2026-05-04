//! Init — the first userspace process.
//!
//! Launched by the kernel bootstrap. Receives bootstrap handles at
//! well-known indices and launches services from the embedded manifest.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

fn syscall(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> (u64, u64) {
    let error: u64;
    let value: u64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            inout("x8") num => _,
            inout("x0") a0 => error,
            inout("x1") a1 => value,
            inout("x2") a2 => _,
            inout("x3") a3 => _,
            inout("x4") a4 => _,
            inout("x5") a5 => _,
            options(nostack),
        );
    }
    (error, value)
}

const SYSCALL_SYSTEM_INFO: u64 = 27;
const SYSCALL_THREAD_EXIT: u64 = 18;

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    // Prove the syscall path works: read page size.
    let (_err, page_size) = syscall(SYSCALL_SYSTEM_INFO, 0, 0, 0, 0, 0, 0);

    // Exit with page_size as the exit code (should be 16384 = 0x4000).
    loop {
        syscall(SYSCALL_THREAD_EXIT, page_size, 0, 0, 0, 0, 0);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
