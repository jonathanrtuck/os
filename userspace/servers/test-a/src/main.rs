//! Test service A — registers with the name service, then serves one
//! IPC round-trip to prove Lookup-based discovery works.
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
use protocol::name_service;

const HANDLE_NAME_SVC: Handle = Handle(2);
const MAGIC_REPLY: u64 = 0xC0FF_EE42;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let my_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(10),
    };
    let my_ep_dup = match abi::handle::dup(my_ep, abi::types::Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(13),
    };

    let req = name_service::NameRequest::new(b"test-a");
    let mut buf = [0u8; MSG_SIZE];

    message::write_request(&mut buf, name_service::REGISTER, &req.name);

    if abi::ipc::call(HANDLE_NAME_SVC, &mut buf, 40, &[my_ep_dup.0], &mut []).is_err() {
        abi::thread::exit(11);
    }

    let mut msg_buf = [0u8; MSG_SIZE];
    let mut handle_buf = [0u32; 4];

    let recv = match abi::ipc::recv(my_ep, &mut msg_buf, &mut handle_buf) {
        Ok(r) => r,
        Err(e) => abi::thread::exit(100 + e as u32),
    };

    let reply = MAGIC_REPLY.to_le_bytes();
    let _ = abi::ipc::reply(my_ep, recv.reply_cap, &reply, &[]);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
