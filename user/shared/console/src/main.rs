//! PL011 UART console driver.
//!
//! Bootstrap handle layout:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: (reserved)
//!   Handle 3: UART MMIO VMO (device-backed, one page)
//!   Handle 4: service endpoint (pre-registered by init as "console")
//!
//! Maps the UART MMIO region and serves write requests. Each request
//! contains raw bytes in the IPC payload, written directly to the UART
//! TX register.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{self, Dispatch, Incoming};

const HANDLE_UART_VMO: Handle = Handle(3);
const HANDLE_SVC_EP: Handle = Handle(4);

const PL011_DR: usize = 0x00;
const PL011_FR: usize = 0x18;
const PL011_TXFF: u32 = 1 << 5;

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
            console::METHOD_WRITE => {
                self.write_bytes(msg.payload);

                let _ = msg.reply_empty();
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let uart_va = match abi::vmo::map(HANDLE_UART_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
    let mut console = Console { uart_base: uart_va };

    console.write_bytes(b"console: ready\n");

    server::serve(HANDLE_SVC_EP, &mut console);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
