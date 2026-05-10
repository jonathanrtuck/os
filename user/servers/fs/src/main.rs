//! Filesystem service — unified file access over store and virtio-9p.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered as "fs")
//!
//! Looks up the "9p" driver via the name service. On READ_FILE, reads
//! the file via 9p into a new VMO and returns it to the caller. On STAT,
//! forwards to 9p for file size.
//!
//! Currently routes all requests to the 9p backend (host filesystem).
//! Future: route to store service for the document database based on
//! path prefix or file ID.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_SVC_EP: Handle = Handle(3);

const PAGE_SIZE: usize = 16384;

const SHARED_VMO_SIZE: usize = PAGE_SIZE * 256;

struct FsServer {
    ninep_ep: Handle,
    ninep_shared_va: usize,
}

impl Dispatch for FsServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            fs_service::READ_FILE => self.handle_read_file(msg),
            fs_service::STAT => self.handle_stat(msg),
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

impl FsServer {
    fn handle_read_file(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < fs_service::ReadFileRequest::SIZE {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let req = fs_service::ReadFileRequest::read_from(msg.payload);
        let path = req.path_bytes();

        if path.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let max_len = SHARED_VMO_SIZE as u32;
        let nine_req = virtio_9p::ReadFileRequest::new(path, 0, max_len);
        let mut call_buf = [0u8; ipc::message::MSG_SIZE];
        let mut nine_data = [0u8; virtio_9p::ReadFileRequest::SIZE];

        nine_req.write_to(&mut nine_data);

        let reply = match ipc::client::call(
            self.ninep_ep,
            virtio_9p::READ_FILE,
            &nine_data,
            &[],
            &mut [],
            &mut call_buf,
        ) {
            Ok(r) => r,
            Err(_) => {
                let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                return;
            }
        };

        if reply.is_error() {
            let _ = msg.reply_error(reply.status);

            return;
        }

        if reply.payload.len() < virtio_9p::ReadFileReply::SIZE {
            let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

            return;
        }

        let nine_reply = virtio_9p::ReadFileReply::read_from(reply.payload);
        let bytes_read = nine_reply.bytes_read as usize;

        if bytes_read == 0 {
            let reply = fs_service::ReadFileReply { bytes_read: 0 };
            let mut data = [0u8; fs_service::ReadFileReply::SIZE];

            reply.write_to(&mut data);

            let _ = msg.reply_ok(&data, &[]);

            return;
        }

        let vmo_size = bytes_read.next_multiple_of(PAGE_SIZE);
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let data_vmo = match abi::vmo::create(vmo_size, 0) {
            Ok(h) => h,
            Err(_) => {
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };
        let data_va = match abi::vmo::map(data_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => {
                let _ = abi::handle::close(data_vmo);
                let _ = msg.reply_error(ipc::STATUS_NO_SPACE);

                return;
            }
        };

        // SAFETY: ninep_shared_va and data_va are valid mappings of
        // at least bytes_read bytes each.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.ninep_shared_va as *const u8,
                data_va as *mut u8,
                bytes_read,
            );
        }

        let _ = abi::vmo::unmap(data_va);
        let reply = fs_service::ReadFileReply {
            bytes_read: bytes_read as u32,
        };
        let mut data = [0u8; fs_service::ReadFileReply::SIZE];

        reply.write_to(&mut data);

        let _ = msg.reply_ok(&data, &[data_vmo.0]);
    }

    fn handle_stat(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < fs_service::StatRequest::SIZE {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let req = fs_service::StatRequest::read_from(msg.payload);
        let path = req.path_bytes();
        let nine_req = virtio_9p::StatRequest::new(path);
        let mut call_buf = [0u8; ipc::message::MSG_SIZE];
        let mut nine_data = [0u8; virtio_9p::StatRequest::SIZE];

        nine_req.write_to(&mut nine_data);

        let reply = match ipc::client::call(
            self.ninep_ep,
            virtio_9p::STAT,
            &nine_data,
            &[],
            &mut [],
            &mut call_buf,
        ) {
            Ok(r) => r,
            Err(_) => {
                let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                return;
            }
        };

        if reply.is_error() {
            let _ = msg.reply_error(reply.status);

            return;
        }

        if reply.payload.len() < virtio_9p::StatReply::SIZE {
            let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

            return;
        }

        let nine_reply = virtio_9p::StatReply::read_from(reply.payload);
        let fs_reply = fs_service::StatReply {
            size: nine_reply.size,
            exists: nine_reply.exists,
        };
        let mut data = [0u8; fs_service::StatReply::SIZE];

        fs_reply.write_to(&mut data);

        let _ = msg.reply_ok(&data, &[]);
    }
}

fn setup_9p_shared_vmo(ninep_ep: Handle) -> Option<(usize, Handle)> {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let vmo = match abi::vmo::create(SHARED_VMO_SIZE, 0) {
        Ok(h) => h,
        Err(_) => return None,
    };

    let va = match abi::vmo::map(vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::handle::close(vmo);

            return None;
        }
    };

    let dup = match abi::handle::dup(vmo, Rights::ALL) {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::vmo::unmap(va);
            let _ = abi::handle::close(vmo);

            return None;
        }
    };

    let mut buf = [0u8; ipc::message::MSG_SIZE];
    let result = ipc::client::call(ninep_ep, virtio_9p::SETUP, &[], &[dup.0], &mut [], &mut buf);

    match result {
        Ok(r) if !r.is_error() => Some((va, vmo)),
        _ => {
            let _ = abi::vmo::unmap(va);
            let _ = abi::handle::close(vmo);

            None
        }
    }
}

fn lookup_with_timeout(ns_ep: Handle, svc_name: &[u8], max_attempts: u32) -> Option<Handle> {
    for _ in 0..max_attempts {
        if let Ok(h) = name::lookup(ns_ep, svc_name) {
            return Some(h);
        }

        abi::thread::yield_now().ok();
    }

    None
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE001),
    };

    console::write(console_ep, b"  fs: starting\n");

    let ninep_ep = match lookup_with_timeout(HANDLE_NS_EP, b"9p", 50) {
        Some(h) => h,
        None => {
            console::write(console_ep, b"  fs: no 9p driver, exiting\n");
            abi::thread::exit(0);
        }
    };
    let (shared_va, _shared_vmo) = match setup_9p_shared_vmo(ninep_ep) {
        Some(v) => v,
        None => {
            console::write(console_ep, b"  fs: 9p setup failed\n");
            abi::thread::exit(0xE003);
        }
    };
    console::write(console_ep, b"  fs: ready\n");

    let mut server = FsServer {
        ninep_ep,
        ninep_shared_va: shared_va,
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
