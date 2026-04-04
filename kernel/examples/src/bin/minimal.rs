//! Absolute minimum userspace program — raw syscall, no stack, no runtime.

#![no_std]
#![no_main]

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Write a single byte directly via syscall — no function calls, no stack use.
    unsafe {
        core::arch::asm!(
            "mov x0, {buf}",       // buf ptr
            "mov x1, #1",          // len = 1
            "mov x8, #1",          // WRITE syscall
            "svc #0",
            "mov x8, #0",          // EXIT syscall
            "svc #0",
            "b .",
            buf = in(reg) b"X".as_ptr(),
            options(noreturn),
        );
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
