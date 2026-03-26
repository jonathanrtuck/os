//! Document service — metadata-aware document store over virtio-blk.
//!
//! Replaces the filesystem service. Owns the virtio-blk device directly
//! (maps MMIO, does DMA I/O) and builds a COW filesystem + store layer
//! on top. The store library handles all document/metadata logic; this
//! service is a thin IPC translator.
//!
//! IPC loop handles:
//!   - MSG_DOC_COMMIT:   read doc buffer, write to file, commit
//!   - MSG_DOC_QUERY:    run store query, return matching file IDs
//!   - MSG_DOC_READ:     read file content to shared memory
//!   - MSG_DOC_SNAPSHOT: create snapshot of specified files
//!   - MSG_DOC_RESTORE:  restore a previous snapshot

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::cell::RefCell;

use protocol::document::*;

mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

const PAGE_SIZE: usize = system_config::PAGE_SIZE as usize;

const SECTOR_SIZE: usize = 512;

/// Counter frequency in Hz, set once at boot for timestamp computation.
static COUNTER_FREQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Clock source for fs library timestamps (nanos since boot).
/// Uses the ARM generic timer (CNTVCT_EL0 / CNTFRQ_EL0).
fn clock_nanos() -> u64 {
    let freq = COUNTER_FREQ.load(core::sync::atomic::Ordering::Relaxed);
    if freq == 0 {
        return 0;
    }
    let count = sys::counter();
    // nanos = count * 1_000_000_000 / freq. Use 128-bit to avoid overflow.
    ((count as u128 * 1_000_000_000) / freq as u128) as u64
}

// virtio-blk request types.
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;

// virtio-blk feature bits.
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

// virtio-blk status codes.
const VIRTIO_BLK_S_OK: u8 = 0;

const VIRTQ_REQUEST: u32 = 0;
const DATA_OFFSET: usize = 16;

/// Block request header (16 bytes, device-readable).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

// ── VirtioBlockDevice ────────────────────────────────────────────────

/// Mutable I/O state — borrowed via `RefCell` for interior mutability.
struct IoState {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    buf_va: usize,
    buf_pa: u64,
}

/// `BlockDevice` implementation over virtio-blk transport.
struct VirtioBlockDevice {
    io: RefCell<IoState>,
    capacity_sectors: u64,
    has_flush: bool,
}

impl IoState {
    /// Submit a virtio-blk request and wait for completion.
    fn submit(&mut self, req_type: u32, sector: u64, data_bytes: u32) -> u8 {
        let buf_ptr = self.buf_va as *mut u8;

        // SAFETY: buf_va points to DMA allocation with at least 16 bytes for header.
        unsafe {
            let header = buf_ptr as *mut BlkReqHeader;
            (*header).req_type = req_type;
            (*header).reserved = 0;
            (*header).sector = sector;
        }

        let header_pa = self.buf_pa;
        let status_offset = DATA_OFFSET + data_bytes as usize;
        let status_pa = self.buf_pa + status_offset as u64;

        // SAFETY: status_offset is within the DMA buffer.
        unsafe { *buf_ptr.add(status_offset) = 0xFF };

        if data_bytes == 0 {
            self.vq
                .push_chain(&[(header_pa, 16, false), (status_pa, 1, true)]);
        } else {
            let data_pa = self.buf_pa + DATA_OFFSET as u64;
            let data_writable = req_type == VIRTIO_BLK_T_IN;
            self.vq.push_chain(&[
                (header_pa, 16, false),
                (data_pa, data_bytes, data_writable),
                (status_pa, 1, true),
            ]);
        }

        self.device.notify(VIRTQ_REQUEST);
        let _ = sys::wait(&[self.irq_handle.0], u64::MAX);
        self.device.ack_interrupt();
        self.vq.pop_used();
        let _ = sys::interrupt_ack(self.irq_handle);

        // SAFETY: device has written the status byte.
        unsafe { *buf_ptr.add(status_offset) }
    }

    /// Pointer to the data area within the DMA buffer.
    fn data_ptr(&self) -> *mut u8 {
        (self.buf_va + DATA_OFFSET) as *mut u8
    }
}

impl fs::BlockDevice for VirtioBlockDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), fs::FsError> {
        let mut io = self.io.borrow_mut();
        let sectors_per_block = fs::BLOCK_SIZE / SECTOR_SIZE as u32;
        let sector = index as u64 * sectors_per_block as u64;
        let status = io.submit(VIRTIO_BLK_T_IN, sector, fs::BLOCK_SIZE);
        if status != VIRTIO_BLK_S_OK {
            return Err(fs::FsError::Io);
        }

        // SAFETY: data area has fs::BLOCK_SIZE bytes after a successful read.
        unsafe {
            core::ptr::copy_nonoverlapping(
                io.data_ptr(),
                buf.as_mut_ptr(),
                fs::BLOCK_SIZE as usize,
            );
        }
        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), fs::FsError> {
        let mut io = self.io.borrow_mut();
        let sectors_per_block = fs::BLOCK_SIZE / SECTOR_SIZE as u32;
        let sector = index as u64 * sectors_per_block as u64;

        // SAFETY: data area has space for fs::BLOCK_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), io.data_ptr(), fs::BLOCK_SIZE as usize);
        }

        let status = io.submit(VIRTIO_BLK_T_OUT, sector, fs::BLOCK_SIZE);
        if status != VIRTIO_BLK_S_OK {
            return Err(fs::FsError::Io);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), fs::FsError> {
        if !self.has_flush {
            return Ok(());
        }
        let mut io = self.io.borrow_mut();
        let status = io.submit(VIRTIO_BLK_T_FLUSH, 0, 0);
        if status != VIRTIO_BLK_S_OK {
            return Err(fs::FsError::Io);
        }
        Ok(())
    }

    fn block_count(&self) -> u32 {
        let sectors_per_block = fs::BLOCK_SIZE / SECTOR_SIZE as u32;
        (self.capacity_sectors / sectors_per_block as u64) as u32
    }
}

// ── Entry point ──────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x84 document - starting\n");

    // ── Phase 1: Receive config from init ────────────────────────────

    let ch = unsafe { ipc::Channel::from_base(protocol::CHANNEL_SHM_BASE, ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DOC_CONFIG {
        sys::print(b"document: no config message\n");
        sys::exit();
    }

    let config = if let Some(Message::DocConfig(c)) = decode(msg.msg_type, &msg.payload) {
        c
    } else {
        sys::print(b"document: bad config\n");
        sys::exit();
    };

    // ── Phase 2: Map MMIO, negotiate virtio, setup virtqueue ─────────

    let mmio_pa = config.mmio_pa;
    let page_offset = mmio_pa & (PAGE_SIZE as u64 - 1);
    let page_pa = mmio_pa & !(PAGE_SIZE as u64 - 1);
    let page_va = sys::device_map(page_pa, PAGE_SIZE as u64).unwrap_or_else(|_| {
        sys::print(b"document: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    let (ok, accepted) = device.negotiate_features(VIRTIO_BLK_F_FLUSH);
    if !ok {
        sys::print(b"document: negotiate failed\n");
        sys::exit();
    }
    let has_flush = accepted & VIRTIO_BLK_F_FLUSH != 0;

    let irq_handle: sys::InterruptHandle =
        sys::interrupt_register(config.irq).unwrap_or_else(|_| {
            sys::print(b"document: interrupt_register failed\n");
            sys::exit();
        });

    let capacity_sectors = device.config_read64(0);

    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_REQUEST),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"document: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_pages = 1usize << vq_order;
    // SAFETY: vq_va is a valid DMA allocation; zeroing before use.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_pages * PAGE_SIZE) };

    let vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);
    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    let mut buf_pa: u64 = 0;
    let buf_va = sys::dma_alloc(1, &mut buf_pa).unwrap_or_else(|_| {
        sys::print(b"document: dma_alloc (buf) failed\n");
        sys::exit();
    });
    // SAFETY: buf_va is a valid DMA allocation of 2 pages; zeroing before use.
    unsafe { core::ptr::write_bytes(buf_va as *mut u8, 0, 2 * PAGE_SIZE) };

    let blk = VirtioBlockDevice {
        io: RefCell::new(IoState {
            device,
            vq,
            irq_handle,
            buf_va,
            buf_pa,
        }),
        capacity_sectors,
        has_flush,
    };

    // ── Phase 3: Mount or format filesystem, open or init store ──────

    // Check if the disk has a valid filesystem by reading block 0's magic.
    // This avoids consuming the device in a failed mount attempt.
    let has_filesystem = {
        let mut hdr = alloc::vec![0u8; fs::BLOCK_SIZE as usize];
        fs::BlockDevice::read_block(&blk, 0, &mut hdr).is_ok()
            && u64::from_le_bytes(hdr[0..8].try_into().unwrap_or([0; 8]))
                == u64::from_le_bytes(*b"docOScow")
    };

    // Initialize the counter frequency for inode timestamps.
    COUNTER_FREQ.store(sys::counter_freq(), core::sync::atomic::Ordering::Relaxed);

    let (filesystem, needs_store_init) = if has_filesystem {
        sys::print(b"     mounting existing filesystem...\n");
        match fs::Filesystem::mount(blk) {
            Ok(f) => (f, false),
            Err(_) => {
                sys::print(b"document: mount failed\n");
                let _ = sys::channel_signal(sys::ChannelHandle(0));
                sys::exit();
            }
        }
    } else {
        sys::print(b"     formatting new filesystem...\n");
        match fs::Filesystem::format(blk) {
            Ok(f) => (f, true),
            Err(_) => {
                sys::print(b"document: format failed\n");
                let _ = sys::channel_signal(sys::ChannelHandle(0));
                sys::exit();
            }
        }
    };

    let mut filesystem = filesystem;
    filesystem.set_time_source(clock_nanos);
    let fs_box: Box<dyn fs::Files> = Box::new(filesystem);

    let mut store = if needs_store_init {
        sys::print(b"     initializing new store\n");
        match store::Store::init(fs_box) {
            Ok(s) => s,
            Err(_) => {
                sys::print(b"document: store init failed\n");
                let _ = sys::channel_signal(sys::ChannelHandle(0));
                sys::exit();
            }
        }
    } else {
        sys::print(b"     opening existing store\n");
        match store::Store::open(fs_box) {
            Ok(s) => s,
            Err(_) => {
                sys::print(b"document: store open failed\n");
                let _ = sys::channel_signal(sys::ChannelHandle(0));
                sys::exit();
            }
        }
    };

    // ── Phase 4: Signal ready ────────────────────────────────────────

    sys::print(b"     document service ready\n");

    let doc_va = config.doc_va as usize;
    let doc_capacity = config.doc_capacity as usize;
    let content_va = config.content_va as usize;

    // Signal init that we're ready by sending MSG_DOC_READY message.
    let ready_msg = ipc::Message::new(MSG_DOC_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(sys::ChannelHandle(0));

    // ── Phase 4.5: Boot-query loop (init channel) ────────────────────
    //
    // Init sends font queries and read requests on handle 0 during boot.
    // We handle them here until MSG_DOC_BOOT_DONE signals the end.

    if content_va != 0 {
        sys::print(b"     entering boot-query phase\n");

        loop {
            let _ = sys::wait(&[0], u64::MAX);

            let mut boot_done = false;

            while ch.try_recv(&mut msg) {
                match msg.msg_type {
                    MSG_DOC_QUERY => {
                        if let Some(Message::DocQuery(q)) = decode(msg.msg_type, &msg.payload) {
                            let query_str =
                                core::str::from_utf8(&q.data[..q.data_len as usize]).unwrap_or("");

                            let query = match q.query_type {
                                0 => {
                                    store::Query::MediaType(alloc::string::String::from(query_str))
                                }
                                1 => store::Query::Type(alloc::string::String::from(query_str)),
                                2 => {
                                    // Attribute query: data is "key\0value".
                                    if let Some(sep) = query_str.find('\0') {
                                        store::Query::Attribute {
                                            key: alloc::string::String::from(&query_str[..sep]),
                                            value: alloc::string::String::from(
                                                &query_str[sep + 1..],
                                            ),
                                        }
                                    } else {
                                        store::Query::MediaType(alloc::string::String::from(""))
                                    }
                                }
                                _ => store::Query::MediaType(alloc::string::String::from("")),
                            };

                            let results = store.query(&query);
                            let count = results.len().min(6) as u32;

                            let mut result = DocQueryResult {
                                count,
                                _pad: 0,
                                file_ids: [0u64; 6],
                            };
                            for (i, fid) in results.iter().take(6).enumerate() {
                                result.file_ids[i] = fid.0;
                            }

                            let reply = unsafe {
                                ipc::Message::from_payload(MSG_DOC_QUERY_RESULT, &result)
                            };
                            ch.send(&reply);
                            let _ = sys::channel_signal(sys::ChannelHandle(0));
                        }
                    }

                    MSG_DOC_READ => {
                        if let Some(Message::DocRead(r)) = decode(msg.msg_type, &msg.payload) {
                            let file_id = fs::FileId(r.file_id);
                            let target = r.target_va as usize;
                            let capacity = r.capacity as usize;

                            let mut done = DocReadDone {
                                file_id: r.file_id,
                                len: 0,
                                status: 1,
                            };

                            // SAFETY: target_va points to Content Region shared
                            // memory mapped by init before starting this process.
                            let buf = unsafe {
                                core::slice::from_raw_parts_mut(target as *mut u8, capacity)
                            };

                            match store.read(file_id, 0, buf) {
                                Ok(n) => {
                                    done.len = n as u32;
                                    done.status = 0;
                                }
                                Err(_) => {
                                    done.status = 1;
                                }
                            }

                            let reply =
                                unsafe { ipc::Message::from_payload(MSG_DOC_READ_DONE, &done) };
                            ch.send(&reply);
                            let _ = sys::channel_signal(sys::ChannelHandle(0));
                        }
                    }

                    MSG_DOC_BOOT_DONE => {
                        boot_done = true;
                        break;
                    }

                    _ => {}
                }
            }

            if boot_done {
                break;
            }
        }

        sys::print(b"     boot-query phase complete\n");
    }

    // ── Phase 5: IPC loop ────────────────────────────────────────────

    // Core channel: handle 1 (sent by init via handle_send).
    let core_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    loop {
        let _ = sys::wait(&[1], u64::MAX);

        while core_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_DOC_COMMIT => {
                    if let Some(Message::DocCommit(c)) = decode(msg.msg_type, &msg.payload) {
                        let file_id = fs::FileId(c.file_id);
                        // Read document content from shared buffer.
                        // Header: [0..8) = content_len (u64), [8..16) = cursor_pos,
                        //          [16..64) = reserved, [64..) = content
                        let content_len =
                            unsafe { core::ptr::read_volatile(doc_va as *const u64) as usize };

                        let actual_len = if content_len > doc_capacity {
                            doc_capacity
                        } else {
                            content_len
                        };

                        // SAFETY: doc_va + 64 points to content area in shared memory,
                        // mapped read-only by init. actual_len bounded by doc_capacity.
                        let content = unsafe {
                            core::slice::from_raw_parts((doc_va + 64) as *const u8, actual_len)
                        };

                        let _ = store.write(file_id, 0, content);
                        let _ = store.truncate(file_id, actual_len as u64);
                        let _ = store.commit();
                    }
                }

                MSG_DOC_QUERY => {
                    if let Some(Message::DocQuery(q)) = decode(msg.msg_type, &msg.payload) {
                        let query_str =
                            core::str::from_utf8(&q.data[..q.data_len as usize]).unwrap_or("");

                        let query = match q.query_type {
                            0 => store::Query::MediaType(alloc::string::String::from(query_str)),
                            1 => store::Query::Type(alloc::string::String::from(query_str)),
                            2 => {
                                if let Some(sep) = query_str.find('\0') {
                                    store::Query::Attribute {
                                        key: alloc::string::String::from(&query_str[..sep]),
                                        value: alloc::string::String::from(&query_str[sep + 1..]),
                                    }
                                } else {
                                    store::Query::MediaType(alloc::string::String::from(""))
                                }
                            }
                            _ => store::Query::MediaType(alloc::string::String::from("")),
                        };

                        let results = store.query(&query);
                        let count = results.len().min(6) as u32;

                        let mut result = DocQueryResult {
                            count,
                            _pad: 0,
                            file_ids: [0u64; 6],
                        };
                        for (i, fid) in results.iter().take(6).enumerate() {
                            result.file_ids[i] = fid.0;
                        }

                        let reply =
                            unsafe { ipc::Message::from_payload(MSG_DOC_QUERY_RESULT, &result) };
                        core_ch.send(&reply);
                        let _ = sys::channel_signal(sys::ChannelHandle(1));
                    }
                }

                MSG_DOC_READ => {
                    if let Some(Message::DocRead(r)) = decode(msg.msg_type, &msg.payload) {
                        let file_id = fs::FileId(r.file_id);
                        // target_va=0 means "write to the shared doc buffer".
                        let target = if r.target_va == 0 {
                            doc_va + 64 // DOC_HEADER_SIZE = 64
                        } else {
                            r.target_va as usize
                        };
                        let capacity = r.capacity as usize;

                        let mut done = DocReadDone {
                            file_id: r.file_id,
                            len: 0,
                            status: 1,
                        };

                        // SAFETY: target_va points to shared memory mapped by init,
                        // or doc_va+64 points to the doc buffer content area.
                        let buf =
                            unsafe { core::slice::from_raw_parts_mut(target as *mut u8, capacity) };

                        match store.read(file_id, 0, buf) {
                            Ok(n) => {
                                done.len = n as u32;
                                done.status = 0;
                            }
                            Err(_) => {
                                done.status = 1;
                            }
                        }

                        let reply = unsafe { ipc::Message::from_payload(MSG_DOC_READ_DONE, &done) };
                        core_ch.send(&reply);
                        let _ = sys::channel_signal(sys::ChannelHandle(1));
                    }
                }

                MSG_DOC_SNAPSHOT => {
                    if let Some(Message::DocSnapshot(s)) = decode(msg.msg_type, &msg.payload) {
                        let count = (s.file_count as usize).min(6);
                        let mut files = alloc::vec::Vec::with_capacity(count);
                        for i in 0..count {
                            files.push(fs::FileId(s.file_ids[i]));
                        }

                        let mut result = DocSnapshotResult {
                            snapshot_id: 0,
                            status: 1,
                            _pad: 0,
                        };

                        match store.snapshot(&files) {
                            Ok(sid) => {
                                result.snapshot_id = sid.0;
                                result.status = 0;
                            }
                            Err(_) => {
                                result.status = 1;
                            }
                        }

                        let reply =
                            unsafe { ipc::Message::from_payload(MSG_DOC_SNAPSHOT_RESULT, &result) };
                        core_ch.send(&reply);
                        let _ = sys::channel_signal(sys::ChannelHandle(1));
                    }
                }

                MSG_DOC_RESTORE => {
                    if let Some(Message::DocRestore(r)) = decode(msg.msg_type, &msg.payload) {
                        let mut result = DocRestoreResult { status: 1, _pad: 0 };

                        match store.restore(fs::SnapshotId(r.snapshot_id)) {
                            Ok(()) => {
                                result.status = 0;
                            }
                            Err(_) => {
                                result.status = 1;
                            }
                        }

                        let reply =
                            unsafe { ipc::Message::from_payload(MSG_DOC_RESTORE_RESULT, &result) };
                        core_ch.send(&reply);
                        let _ = sys::channel_signal(sys::ChannelHandle(1));
                    }
                }

                MSG_DOC_CREATE => {
                    if let Some(Message::DocCreate(c)) = decode(msg.msg_type, &msg.payload) {
                        let mt_len = (c.media_type_len as usize).min(c.media_type.len());
                        let media_type = core::str::from_utf8(&c.media_type[..mt_len])
                            .unwrap_or("application/octet-stream");

                        let mut result = DocCreateResult {
                            file_id: 0,
                            status: 1,
                            _pad: 0,
                        };

                        match store.create(media_type) {
                            Ok(fid) => {
                                let _ = store.commit();
                                result.file_id = fid.0;
                                result.status = 0;
                            }
                            Err(_) => {
                                result.status = 1;
                            }
                        }

                        let reply =
                            unsafe { ipc::Message::from_payload(MSG_DOC_CREATE_RESULT, &result) };
                        core_ch.send(&reply);
                        let _ = sys::channel_signal(sys::ChannelHandle(1));
                    }
                }

                MSG_DOC_DELETE_SNAPSHOT => {
                    if let Some(Message::DocDeleteSnapshot(d)) = decode(msg.msg_type, &msg.payload)
                    {
                        // Fire-and-forget: best-effort cleanup, no response.
                        let _ = store.delete_snapshot(fs::SnapshotId(d.snapshot_id));
                    }
                }

                _ => {}
            }
        }
    }
}
