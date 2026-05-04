//! Init — the first userspace process.
//!
//! Launched by the kernel bootstrap. Receives bootstrap handles at
//! well-known indices and launches services from the embedded manifest.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    let page_size = libsys::system::info(libsys::system::INFO_PAGE_SIZE).unwrap_or(0);
    libsys::thread::exit(page_size as u32);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    libsys::thread::exit(0xDEAD);
}
