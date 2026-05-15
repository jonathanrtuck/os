//! Hello service — minimal test service for Phase 1.3 verification.
//!
//! Calls a syscall to prove it's running in EL0, then exits cleanly.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let _ = abi::system::info(abi::system::INFO_PAGE_SIZE);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
