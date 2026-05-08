//! Test service B — looks up test-a via the name service, calls it,
//! verifies the reply. Uses WATCH to block until test-a has registered.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::Handle;

const HANDLE_NAME_SVC: Handle = Handle(2);
const MAGIC_REQUEST: u64 = 0xDEAD_BEEF;
const MAGIC_REPLY: u64 = 0xC0FF_EE42;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let test_a_ep = match name::watch(HANDLE_NAME_SVC, b"test-a") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(21),
    };
    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let payload = MAGIC_REQUEST.to_le_bytes();

    call_buf[..8].copy_from_slice(&payload);

    if abi::ipc::call(test_a_ep, &mut call_buf, 8, &[], &mut []).is_err() {
        abi::thread::exit(22);
    }

    let reply_val = u64::from_le_bytes(call_buf[..8].try_into().unwrap());

    if reply_val != MAGIC_REPLY {
        abi::thread::exit(23);
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
