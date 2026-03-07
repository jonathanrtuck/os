#![no_std]
#![no_main]

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("boot.S"));

mod uart;

/// Thread context saved/restored across exception boundaries.
///
/// Layout must match the CTX_* offsets in boot.S exactly.
/// The const assertions below enforce this at compile time.
#[repr(C)]
pub struct Context {
    pub x: [u64; 31], // x0..x30
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    pub sp_el0: u64,    // reserved for EL0 support
    pub tpidr_el0: u64, // reserved for EL0 support
    pub q: [u128; 32],  // FP/SIMD q0..q31
    pub fpcr: u64,      // FP control register (low 32 bits)
    pub fpsr: u64,      // FP status register (low 32 bits)
}

// Verify Context layout matches boot.S CTX_* constants.
const _: () = {
    assert!(core::mem::offset_of!(Context, x) == 0x000);
    assert!(core::mem::offset_of!(Context, sp) == 0x0F8);
    assert!(core::mem::offset_of!(Context, elr) == 0x100);
    assert!(core::mem::offset_of!(Context, spsr) == 0x108);
    assert!(core::mem::offset_of!(Context, sp_el0) == 0x110);
    assert!(core::mem::offset_of!(Context, tpidr_el0) == 0x118);
    assert!(core::mem::offset_of!(Context, q) == 0x120);
    assert!(core::mem::offset_of!(Context, fpcr) == 0x320);
    assert!(core::mem::offset_of!(Context, fpsr) == 0x328);
    assert!(core::mem::size_of::<Context>() == 0x330);
};

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    uart::puts("hello, world\n");
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn irq_handler(current: *mut Context) -> *const Context {
    let next: *const Context = current;

    // @todo

    next
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    uart::puts("\n!!! PANIC !!!\n");
    if let Some(location) = info.location() {
        uart::puts(location.file());
        uart::puts(":");
        uart::put_u32(location.line());
        uart::puts("\n");
    }
    if let Some(msg) = info.message().as_str() {
        uart::puts(msg);
        uart::puts("\n");
    }
    loop {}
}
