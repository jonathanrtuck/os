//! Name service — flat namespace for service discovery.
//!
//! Bootstrap handle layout:
//!   Handle 0: code VMO (refcount anchor)
//!   Handle 1: stack VMO (refcount anchor)
//!   Handle 2: endpoint to recv on (created by init)
//!
//! Protocol: Register, Lookup, Unregister, Watch (see name::lib).

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use alloc::vec::Vec;
use core::panic::PanicInfo;

use abi::types::Handle;
use ipc::server::{self, Dispatch, Incoming};

const HANDLE_ENDPOINT: Handle = Handle(2);
const NAME_LEN: usize = 32;

struct Entry {
    name: [u8; NAME_LEN],
    endpoint_handle: u32,
}

struct Watcher {
    name: [u8; NAME_LEN],
    reply_cap: u32,
}

struct NameTable {
    entries: Vec<Entry>,
    watchers: Vec<Watcher>,
}

impl NameTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            watchers: Vec::new(),
        }
    }

    fn register(&mut self, name: &[u8; NAME_LEN], ep_handle: u32) -> Result<(), u16> {
        if self.entries.iter().any(|e| e.name == *name) {
            return Err(ipc::STATUS_ALREADY_EXISTS);
        }

        self.entries.push(Entry {
            name: *name,
            endpoint_handle: ep_handle,
        });

        Ok(())
    }

    fn lookup(&self, name: &[u8; NAME_LEN]) -> Option<u32> {
        self.entries
            .iter()
            .find(|e| e.name == *name)
            .map(|e| e.endpoint_handle)
    }

    fn unregister(&mut self, name: &[u8; NAME_LEN]) -> bool {
        if let Some(pos) = self.entries.iter().position(|e| e.name == *name) {
            let entry = self.entries.swap_remove(pos);

            let _ = abi::handle::close(Handle(entry.endpoint_handle));

            true
        } else {
            false
        }
    }

    fn add_watcher(&mut self, name: &[u8; NAME_LEN], reply_cap: u32) {
        self.watchers.push(Watcher {
            name: *name,
            reply_cap,
        });
    }

    fn notify_watchers(&mut self, name: &[u8; NAME_LEN], ep_handle: u32) {
        let mut i = 0;

        while i < self.watchers.len() {
            if self.watchers[i].name == *name {
                let w = self.watchers.swap_remove(i);
                let dup = abi::handle::dup(Handle(ep_handle), abi::types::Rights::ALL);

                if let Ok(h) = dup {
                    let mut buf = [0u8; ipc::message::MSG_SIZE];

                    ipc::message::write_reply(&mut buf, name::WATCH, &[]);

                    let _ = abi::ipc::reply(
                        HANDLE_ENDPOINT,
                        w.reply_cap,
                        &buf[..ipc::message::HEADER_SIZE],
                        &[h.0],
                    );
                }
            } else {
                i += 1;
            }
        }
    }
}

impl Dispatch for NameTable {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            name::REGISTER => {
                if msg.payload.len() < name::NameRequest::SIZE || msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = name::NameRequest::read_from(msg.payload);
                let ep_handle = msg.handles[0];

                match self.register(&req.name, ep_handle) {
                    Ok(()) => {
                        self.notify_watchers(&req.name, ep_handle);

                        let _ = msg.reply_empty();
                    }
                    Err(status) => {
                        let _ = msg.reply_error(status);
                    }
                }
            }
            name::LOOKUP => {
                if msg.payload.len() < name::NameRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = name::NameRequest::read_from(msg.payload);

                match self.lookup(&req.name) {
                    Some(ep_handle) => {
                        let dup = abi::handle::dup(Handle(ep_handle), abi::types::Rights::ALL);

                        match dup {
                            Ok(h) => {
                                let _ = msg.reply_ok(&[], &[h.0]);
                            }
                            Err(_) => {
                                let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                            }
                        }
                    }
                    None => {
                        let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                    }
                }
            }
            name::UNREGISTER => {
                if msg.payload.len() < name::NameRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = name::NameRequest::read_from(msg.payload);

                if self.unregister(&req.name) {
                    let _ = msg.reply_empty();
                } else {
                    let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                }
            }
            name::WATCH => {
                if msg.payload.len() < name::NameRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = name::NameRequest::read_from(msg.payload);

                match self.lookup(&req.name) {
                    Some(ep_handle) => {
                        let dup = abi::handle::dup(Handle(ep_handle), abi::types::Rights::ALL);

                        match dup {
                            Ok(h) => {
                                let _ = msg.reply_ok(&[], &[h.0]);
                            }
                            Err(_) => {
                                let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                            }
                        }
                    }
                    None => {
                        let deferred = msg.defer();

                        self.add_watcher(&req.name, deferred.reply_cap);
                    }
                }
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
    let mut table = NameTable::new();

    server::serve(HANDLE_ENDPOINT, &mut table);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
