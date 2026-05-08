//! PL011 UART console driver.
//!
//! Bootstrap handle layout:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint (for register/lookup)
//!   Handle 3: UART MMIO VMO (device-backed, one page)
//!
//! Maps the UART MMIO region, registers as "console" with the name service,
//! then serves write requests. Each request contains raw bytes in the IPC
//! payload, written directly to the UART TX register.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::{
    message::{self, MSG_SIZE},
    server::{self, Dispatch, Incoming},
};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_UART_VMO: Handle = Handle(3);

const PL011_DR: usize = 0x00;
const PL011_FR: usize = 0x18;
const PL011_TXFF: u32 = 1 << 5;

const METHOD_WRITE: u32 = 1;

struct Console {
    uart_base: usize,
}

impl Console {
    fn write_byte(&self, b: u8) {
        // SAFETY: uart_base points to a mapped PL011 MMIO region (Device-nGnRnE).
        // Volatile reads/writes are required for MMIO registers.
        unsafe {
            let fr = self.uart_base + PL011_FR;

            while (fr as *const u32).read_volatile() & PL011_TXFF != 0 {
                core::hint::spin_loop();
            }

            let dr = self.uart_base + PL011_DR;

            (dr as *mut u32).write_volatile(b as u32);
        }
    }

    fn write_bytes(&self, bytes: &[u8]) {
        for &b in bytes {
            self.write_byte(b);
        }
    }
}

impl Dispatch for Console {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            METHOD_WRITE => {
                self.write_bytes(msg.payload);

                let _ = msg.reply_empty();
            }
            _ => {
                let _ = msg.reply_error(protocol::STATUS_UNSUPPORTED);
            }
        }
    }
}

fn register_with_name_service(ns_ep: Handle, own_ep: Handle) {
    let dup = match abi::handle::dup(own_ep, abi::types::Rights::ALL) {
        Ok(h) => h,
        Err(_) => return,
    };
    let req = protocol::name_service::NameRequest::new(b"console");
    let mut buf = [0u8; MSG_SIZE];
    let total = message::write_request(&mut buf, protocol::name_service::REGISTER, &req.name);
    let _ = abi::ipc::call(ns_ep, &mut buf, total, &[dup.0], &mut []);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let uart_va = match abi::vmo::map(HANDLE_UART_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(2),
    };

    register_with_name_service(HANDLE_NS_EP, own_ep);

    let mut console = Console { uart_base: uart_va };

    console.write_bytes(b"console: ready\n");

    server::serve(own_ep, &mut console);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
