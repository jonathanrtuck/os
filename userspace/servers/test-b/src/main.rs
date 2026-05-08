//! Test service B — looks up test-a via the name service, calls it,
//! verifies the reply. Retries Lookup until test-a has registered.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::Handle;
use ipc::message::{self, MSG_SIZE};
use name as name_service;

const HANDLE_NAME_SVC: Handle = Handle(2);
const MAGIC_REQUEST: u64 = 0xDEAD_BEEF;
const MAGIC_REPLY: u64 = 0xC0FF_EE42;
const MAX_RETRIES: usize = 100;

fn lookup_with_retry() -> Option<Handle> {
    for _ in 0..MAX_RETRIES {
        let req = name_service::NameRequest::new(b"test-a");
        let mut buf = [0u8; MSG_SIZE];

        message::write_request(&mut buf, name_service::LOOKUP, &req.name);

        let mut recv_handles = [0u32; 4];

        if let Ok(result) = abi::ipc::call(HANDLE_NAME_SVC, &mut buf, 40, &[], &mut recv_handles) {
            let header = ipc::message::Header::read_from(&buf);

            if !header.is_error() && result.handle_count > 0 {
                return Some(Handle(recv_handles[0]));
            }
        }

        // Spin briefly to give test-a time to register.
        for _ in 0..10000 {
            core::hint::spin_loop();
        }
    }

    None
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let test_a_ep = match lookup_with_retry() {
        Some(h) => h,
        None => abi::thread::exit(21),
    };

    let mut call_buf = [0u8; MSG_SIZE];
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
