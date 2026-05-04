//! Init — the first userspace process.
//!
//! Launched by the kernel bootstrap. Receives bootstrap handles at
//! well-known indices and launches services from the embedded manifest.

#![no_std]
#![no_main]

mod manifest;

use core::panic::PanicInfo;

use libsys::types::Handle;

const STACK_SIZE: usize = 16384 * 4;

fn spawn_service(entry: &manifest::ServiceEntry) -> Result<Handle, libsys::types::SyscallError> {
    let code_vmo = Handle(entry.code_vmo_handle_index);

    let space = libsys::space::create()?;
    let stack_vmo = libsys::vmo::create(STACK_SIZE, 0)?;

    let bootstrap_handles = [code_vmo.0, stack_vmo.0];
    libsys::thread::create_in(
        space,
        0x0020_0000,
        0x4000_0000 + STACK_SIZE,
        0,
        &bootstrap_handles,
    )
}

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    let page_size = libsys::system::info(libsys::system::INFO_PAGE_SIZE).unwrap_or(0);

    for service in manifest::SERVICES {
        let _ = spawn_service(service);
    }

    libsys::thread::exit(page_size as u32);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    libsys::thread::exit(0xDEAD);
}
