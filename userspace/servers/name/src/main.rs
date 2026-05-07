//! Name service — flat namespace for service discovery.
//!
//! Bootstrap handle layout:
//!   Handle 0: code VMO (refcount anchor)
//!   Handle 1: stack VMO (refcount anchor)
//!   Handle 2: endpoint to recv on (created by init)
//!
//! Protocol: Register, Lookup, Unregister (see protocol::name_service).

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::Handle;
use ipc::server::{self, Dispatch, Incoming};
use protocol::name_service;

const HANDLE_ENDPOINT: Handle = Handle(2);
const MAX_ENTRIES: usize = 16;
const NAME_LEN: usize = 32;

struct Entry {
    name: [u8; NAME_LEN],
    endpoint_handle: u32,
    occupied: bool,
}

struct NameTable {
    entries: [Entry; MAX_ENTRIES],
    count: usize,
}

impl NameTable {
    const fn new() -> Self {
        const EMPTY: Entry = Entry {
            name: [0; NAME_LEN],
            endpoint_handle: 0,
            occupied: false,
        };

        Self {
            entries: [EMPTY; MAX_ENTRIES],
            count: 0,
        }
    }

    fn register(&mut self, name: &[u8; NAME_LEN], ep_handle: u32) -> Result<(), u16> {
        for entry in &self.entries {
            if entry.occupied && entry.name == *name {
                return Err(protocol::STATUS_ALREADY_EXISTS);
            }
        }

        for entry in &mut self.entries {
            if !entry.occupied {
                entry.name = *name;
                entry.endpoint_handle = ep_handle;
                entry.occupied = true;
                self.count += 1;

                return Ok(());
            }
        }

        Err(protocol::STATUS_NO_SPACE)
    }

    fn lookup(&self, name: &[u8; NAME_LEN]) -> Option<u32> {
        for entry in &self.entries {
            if entry.occupied && entry.name == *name {
                return Some(entry.endpoint_handle);
            }
        }

        None
    }

    fn unregister(&mut self, name: &[u8; NAME_LEN]) -> bool {
        for entry in &mut self.entries {
            if entry.occupied && entry.name == *name {
                entry.occupied = false;
                self.count -= 1;

                return true;
            }
        }

        false
    }
}

impl Dispatch for NameTable {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            name_service::REGISTER => {
                if msg.payload.len() < name_service::NameRequest::SIZE || msg.handles.is_empty() {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let req = name_service::NameRequest::read_from(msg.payload);
                let ep_handle = msg.handles[0];

                match self.register(&req.name, ep_handle) {
                    Ok(()) => {
                        let _ = msg.reply_empty();
                    }
                    Err(status) => {
                        let _ = msg.reply_error(status);
                    }
                }
            }

            name_service::LOOKUP => {
                if msg.payload.len() < name_service::NameRequest::SIZE {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let req = name_service::NameRequest::read_from(msg.payload);

                match self.lookup(&req.name) {
                    Some(ep_handle) => {
                        // Dup the stored handle so we keep our copy.
                        let dup = abi::handle::dup(Handle(ep_handle), abi::types::Rights::ALL);

                        match dup {
                            Ok(h) => {
                                let _ = msg.reply_ok(&[], &[h.0]);
                            }
                            Err(_) => {
                                let _ = msg.reply_error(protocol::STATUS_NOT_FOUND);
                            }
                        }
                    }
                    None => {
                        let _ = msg.reply_error(protocol::STATUS_NOT_FOUND);
                    }
                }
            }

            name_service::UNREGISTER => {
                if msg.payload.len() < name_service::NameRequest::SIZE {
                    let _ = msg.reply_error(protocol::STATUS_INVALID);

                    return;
                }

                let req = name_service::NameRequest::read_from(msg.payload);

                if self.unregister(&req.name) {
                    let _ = msg.reply_empty();
                } else {
                    let _ = msg.reply_error(protocol::STATUS_NOT_FOUND);
                }
            }

            _ => {
                let _ = msg.reply_error(protocol::STATUS_UNSUPPORTED);
            }
        }
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let mut table = NameTable::new();

    server::serve(HANDLE_ENDPOINT, &mut table);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
