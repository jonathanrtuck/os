//! Store service — COW filesystem over the block device driver.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (unused — blk driver owns hardware)
//!   Handle 4: init endpoint (for DMA allocation — forwarded but unused)
//!
//! Boots, looks up "blk" from name service, establishes a shared VMO
//! for block I/O, mounts the COW filesystem, opens the document store,
//! registers as "store", and enters an IPC serve loop.

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use alloc::boxed::Box;
use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use fs::BlockDevice;
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);

const PAGE_SIZE: usize = 16384;
const BLOCK_SIZE: usize = fs::BLOCK_SIZE as usize;

const SHARED_VMO_SIZE: usize = BLOCK_SIZE * 4;

// ── IPC block device ────────────────────────────────────────────────

struct IpcBlockDevice {
    blk_ep: Handle,
    shared_va: usize,
    capacity_blocks: u32,
    has_flush: bool,
}

impl IpcBlockDevice {
    fn connect(blk_ep: Handle) -> Self {
        let vmo = abi::vmo::create(SHARED_VMO_SIZE, 0).unwrap_or_else(|_| {
            abi::thread::exit(0xE010);
        });
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let shared_va = abi::vmo::map(vmo, 0, rw).unwrap_or_else(|_| {
            abi::thread::exit(0xE011);
        });
        let dup = abi::handle::dup(vmo, Rights::ALL).unwrap_or_else(|_| {
            abi::thread::exit(0xE012);
        });
        let mut buf = [0u8; ipc::message::MSG_SIZE];

        ipc::message::write_request(&mut buf, blk::SETUP, &[]);

        let mut recv_handles = [0u32; 4];
        let _ = abi::ipc::call(
            blk_ep,
            &mut buf,
            ipc::message::HEADER_SIZE,
            &[dup.0],
            &mut recv_handles,
        );
        let mut info_buf = [0u8; ipc::message::MSG_SIZE];
        let total = ipc::message::write_request(&mut info_buf, blk::GET_INFO, &[]);
        let _ = abi::ipc::call(blk_ep, &mut info_buf, total, &[], &mut []);
        let header = ipc::message::Header::read_from(&info_buf);
        let (capacity_blocks, has_flush) = if header.len >= blk::InfoReply::SIZE as u16 {
            let reply = blk::InfoReply::read_from(ipc::message::payload(&info_buf));

            (reply.capacity_blocks, reply.has_flush != 0)
        } else {
            (0, false)
        };

        Self {
            blk_ep,
            shared_va,
            capacity_blocks,
            has_flush,
        }
    }
}

impl fs::BlockDevice for IpcBlockDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), fs::FsError> {
        let req = blk::BlockRequest {
            block_index: index,
            vmo_offset: 0,
        };
        let mut data = [0u8; blk::BlockRequest::SIZE];

        req.write_to(&mut data);

        let (status, _) = ipc::client::call_simple(self.blk_ep, blk::READ_BLOCK, &data)
            .map_err(|_| fs::FsError::Io)?;

        if status != 0 {
            return Err(fs::FsError::Io);
        }

        // SAFETY: shared_va is a valid mapping of SHARED_VMO_SIZE bytes.
        // The blk driver has written BLOCK_SIZE bytes at vmo_offset 0.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.shared_va as *const u8,
                buf.as_mut_ptr(),
                BLOCK_SIZE,
            );
        }

        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), fs::FsError> {
        // SAFETY: shared_va is a valid mapping of SHARED_VMO_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.shared_va as *mut u8, BLOCK_SIZE);
        }

        let req = blk::BlockRequest {
            block_index: index,
            vmo_offset: 0,
        };
        let mut req_data = [0u8; blk::BlockRequest::SIZE];

        req.write_to(&mut req_data);

        let (status, _) = ipc::client::call_simple(self.blk_ep, blk::WRITE_BLOCK, &req_data)
            .map_err(|_| fs::FsError::Io)?;

        if status != 0 {
            return Err(fs::FsError::Io);
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), fs::FsError> {
        if !self.has_flush {
            return Ok(());
        }

        let (status, _) =
            ipc::client::call_simple(self.blk_ep, blk::FLUSH, &[]).map_err(|_| fs::FsError::Io)?;

        if status != 0 && status != ipc::STATUS_UNSUPPORTED {
            return Err(fs::FsError::Io);
        }

        Ok(())
    }

    fn block_count(&self) -> u32 {
        self.capacity_blocks
    }
}

// ── Store server ────────────────────────────────────────────────────

struct StoreServer {
    store: store::Store,
    client_shared_va: usize,
    client_shared_len: usize,
}

impl Dispatch for StoreServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            store_service::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let vmo = Handle(msg.handles[0]);
                let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                match abi::vmo::map(vmo, 0, rw) {
                    Ok(va) => {
                        self.client_shared_va = va;
                        self.client_shared_len = PAGE_SIZE * 4;

                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }

            store_service::CREATE => {
                if msg.payload.len() < store_service::CreateRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::CreateRequest::read_from(msg.payload);
                let mt_len = (req.media_type_len as usize)
                    .min(msg.payload.len() - store_service::CreateRequest::SIZE);
                let media_type = core::str::from_utf8(
                    &msg.payload[store_service::CreateRequest::SIZE
                        ..store_service::CreateRequest::SIZE + mt_len],
                )
                .unwrap_or("application/octet-stream");

                match self.store.create(media_type) {
                    Ok(file_id) => {
                        let reply = store_service::CreateReply { file_id: file_id.0 };
                        let mut data = [0u8; store_service::CreateReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                    }
                }
            }

            store_service::WRITE_DOC => {
                if msg.payload.len() < store_service::WriteRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::WriteRequest::read_from(msg.payload);

                if self.client_shared_va == 0 {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let offset = req.vmo_offset as usize;
                let len = req.len as usize;

                if offset + len > self.client_shared_len {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                // SAFETY: client_shared_va + offset..+len is within the mapped VMO.
                let data = unsafe {
                    core::slice::from_raw_parts((self.client_shared_va + offset) as *const u8, len)
                };

                let file_id = fs::FileId(req.file_id);

                match self.store.write(file_id, req.offset, data) {
                    Ok(()) => {
                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                    }
                }
            }

            store_service::READ_DOC => {
                if msg.payload.len() < store_service::ReadRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::ReadRequest::read_from(msg.payload);

                if self.client_shared_va == 0 {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let offset = req.vmo_offset as usize;
                let max_len = req.max_len as usize;

                if offset + max_len > self.client_shared_len {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let file_id = fs::FileId(req.file_id);
                // SAFETY: client_shared_va + offset..+max_len is within the mapped VMO.
                let buf = unsafe {
                    core::slice::from_raw_parts_mut(
                        (self.client_shared_va + offset) as *mut u8,
                        max_len,
                    )
                };

                match self.store.read(file_id, req.offset, buf) {
                    Ok(n) => {
                        let reply = store_service::ReadReply {
                            bytes_read: n as u32,
                        };
                        let mut data = [0u8; store_service::ReadReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                    }
                }
            }

            store_service::TRUNCATE => {
                if msg.payload.len() < store_service::TruncateRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::TruncateRequest::read_from(msg.payload);
                let file_id = fs::FileId(req.file_id);

                match self.store.truncate(file_id, req.len) {
                    Ok(()) => {
                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                    }
                }
            }

            store_service::COMMIT => match self.store.commit() {
                Ok(()) => {
                    let _ = msg.reply_empty();
                }
                Err(_) => {
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                }
            },

            store_service::SNAPSHOT => {
                if msg.payload.len() < store_service::SnapshotRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::SnapshotRequest::read_from(msg.payload);
                let file_id = fs::FileId(req.file_id);

                match self.store.snapshot(&[file_id]) {
                    Ok(snap_id) => {
                        let reply = store_service::SnapshotReply {
                            snapshot_id: snap_id.0,
                        };
                        let mut data = [0u8; store_service::SnapshotReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                    }
                }
            }

            store_service::RESTORE => {
                if msg.payload.len() < store_service::RestoreRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::RestoreRequest::read_from(msg.payload);

                match self.store.restore(fs::SnapshotId(req.snapshot_id)) {
                    Ok(()) => {
                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                    }
                }
            }

            store_service::DELETE_SNAPSHOT => {
                if msg.payload.len() < store_service::DeleteSnapshotRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = store_service::DeleteSnapshotRequest::read_from(msg.payload);
                let _ = self.store.delete_snapshot(fs::SnapshotId(req.snapshot_id));
                let _ = msg.reply_empty();
            }

            store_service::GET_INFO => {
                if msg.payload.len() < 8 {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let file_id = fs::FileId(u64::from_le_bytes(msg.payload[0..8].try_into().unwrap()));

                match self.store.read(file_id, 0, &mut []) {
                    Ok(_) => {
                        // Get the actual size via the filesystem.
                        let size = self.store.metadata(file_id).map(|m| m.size).unwrap_or(0);
                        let reply = store_service::InfoReply {
                            file_id: file_id.0,
                            size,
                        };
                        let mut data = [0u8; store_service::InfoReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                    }
                }
            }

            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

// ── Entry point ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::lookup_wait(HANDLE_NS_EP, b"console", 1000) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE001),
    };

    console::write(console_ep, b"store: starting\n");

    let blk_ep = match name::lookup_wait(HANDLE_NS_EP, b"blk", 5000) {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"store: blk not found\n");
            abi::thread::exit(0xE002);
        }
    };

    console::write(console_ep, b"store: connecting to blk\n");

    let blk_device = IpcBlockDevice::connect(blk_ep);

    console::write_u32(
        console_ep,
        b"store: blk capacity=",
        blk_device.capacity_blocks,
    );

    let has_filesystem = {
        let mut hdr = [0u8; BLOCK_SIZE];

        if blk_device.read_block(0, &mut hdr).is_ok() {
            u64::from_le_bytes(hdr[0..8].try_into().unwrap_or([0; 8]))
                == u64::from_le_bytes(*b"docOScow")
        } else {
            false
        }
    };

    console::write(console_ep, b"store: init fs\n");

    let filesystem = if has_filesystem {
        console::write(console_ep, b"store: mounting\n");

        match fs::Filesystem::mount(blk_device) {
            Ok(f) => f,
            Err(_) => {
                console::write(console_ep, b"store: mount FAIL\n");
                abi::thread::exit(0xE003);
            }
        }
    } else {
        console::write(console_ep, b"store: formatting\n");

        match fs::Filesystem::format(blk_device) {
            Ok(f) => f,
            Err(_) => {
                console::write(console_ep, b"store: format FAIL\n");
                abi::thread::exit(0xE004);
            }
        }
    };

    console::write(console_ep, b"store: fs OK\n");

    let needs_store_init = !has_filesystem;

    let fs_box: Box<dyn fs::Files> = Box::new(filesystem);

    let store = if needs_store_init {
        console::write(console_ep, b"store: initializing new store\n");

        match store::Store::init(fs_box) {
            Ok(s) => s,
            Err(_) => {
                console::write(console_ep, b"store: init failed\n");

                abi::thread::exit(0xE005);
            }
        }
    } else {
        console::write(console_ep, b"store: opening existing store\n");

        match store::Store::open(fs_box) {
            Ok(s) => s,
            Err(_) => {
                console::write(console_ep, b"store: open failed\n");

                abi::thread::exit(0xE006);
            }
        }
    };

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE007),
    };

    name::register(HANDLE_NS_EP, b"store", own_ep);

    console::write(console_ep, b"store: ready\n");

    let mut server = StoreServer {
        store,
        client_shared_va: 0,
        client_shared_len: 0,
    };

    ipc::server::serve(own_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
