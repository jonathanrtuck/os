//! Kernel entry point.
//!
//! Boot sequence: exception vectors, platform discovery (DTB), MMU,
//! serial lock (SMP-safe), entropy, interrupts, timer, secondary cores.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use kernel::{frame::arch, println};

#[unsafe(no_mangle)]
extern "C" fn kernel_main(dtb_ptr: usize) -> ! {
    arch::exception::init();
    arch::platform::init(dtb_ptr);
    arch::mmu::init();
    arch::serial::enable_lock();
    arch::entropy::init();
    arch::interrupts::init();
    arch::timer::init();

    println!("alive");

    arch::cpu::activate_secondaries();

    loop {
        arch::halt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    arch::disable_interrupts();
    arch::serial::break_lock();

    println!();
    println!("panic: {info}");
    println!();

    arch::dump_panic_registers();
    arch::signal_panic();

    loop {
        arch::halt();
    }
}
