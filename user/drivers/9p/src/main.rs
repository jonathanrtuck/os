//! virtio-9p driver — host filesystem access via 9P2000.L over virtio.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!   Handle 5: service endpoint (pre-registered by init as "9p")
//!
//! Probes the virtio MMIO region for a 9P device (device ID 9).
//! Negotiates the MOUNT_TAG feature, sets up virtqueue and DMA buffers,
//! runs the 9P2000.L handshake (version + attach), then enters an IPC
//! serve loop accepting file read and stat requests.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);
const HANDLE_SVC_EP: Handle = Handle(5);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;

const VIRTIO_9P_F_MOUNT_TAG: u64 = 1;

const MSIZE: u32 = 32768;
const NOFID: u32 = 0xFFFF_FFFF;
const ROOT_FID: u32 = 0;
const FILE_FID: u32 = 1;
const VIRTQ_REQUEST: u32 = 0;

// 9P2000.L message types.
const P9_RLERROR: u8 = 7;
const P9_TLOPEN: u8 = 12;
const P9_RLOPEN: u8 = 13;
const P9_TVERSION: u8 = 100;
const P9_RVERSION: u8 = 101;
const P9_TATTACH: u8 = 104;
const P9_RATTACH: u8 = 105;
const P9_TWALK: u8 = 110;
const P9_RWALK: u8 = 111;
const P9_TREAD: u8 = 116;
const P9_RREAD: u8 = 117;
const P9_TCLUNK: u8 = 120;
const _P9_RCLUNK: u8 = 121;

struct MsgReader {
    buf: *const u8,
    pos: usize,
}

struct MsgWriter {
    buf: *mut u8,
    pos: usize,
}

struct P9Client {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_event: Handle,
    msg_dma: init::DmaBuf,
}

impl MsgReader {
    fn new(buf: *const u8) -> Self {
        Self { buf, pos: 4 }
    }

    fn data_ptr(&self) -> *const u8 {
        // SAFETY: buf points to a valid DMA buffer; pos is within bounds.
        unsafe { self.buf.add(self.pos) }
    }

    fn get_u8(&mut self) -> u8 {
        // SAFETY: pos is within the DMA R-message buffer (MSIZE bytes).
        let v = unsafe { *self.buf.add(self.pos) };

        self.pos += 1;

        v
    }

    fn get_u16(&mut self) -> u16 {
        // SAFETY: pos + 2 is within the DMA R-message buffer.
        let v = unsafe { core::ptr::read_unaligned(self.buf.add(self.pos) as *const u16) };

        self.pos += 2;

        v
    }

    fn get_u32(&mut self) -> u32 {
        // SAFETY: pos + 4 is within the DMA R-message buffer.
        let v = unsafe { core::ptr::read_unaligned(self.buf.add(self.pos) as *const u32) };

        self.pos += 4;

        v
    }
}

impl MsgWriter {
    fn new(buf: *mut u8) -> Self {
        Self { buf, pos: 4 }
    }

    fn finish(&mut self) -> u32 {
        let size = self.pos as u32;

        // SAFETY: buf points to a valid DMA buffer; writing the 4-byte size header.
        unsafe { core::ptr::write_unaligned(self.buf as *mut u32, size) };

        size
    }

    fn put_u8(&mut self, v: u8) {
        // SAFETY: pos is within the DMA T-message buffer (MSIZE bytes).
        unsafe { *self.buf.add(self.pos) = v };

        self.pos += 1;
    }

    fn put_u16(&mut self, v: u16) {
        // SAFETY: pos + 2 is within the DMA T-message buffer.
        unsafe { core::ptr::write_unaligned(self.buf.add(self.pos) as *mut u16, v) };

        self.pos += 2;
    }

    fn put_u32(&mut self, v: u32) {
        // SAFETY: pos + 4 is within the DMA T-message buffer.
        unsafe { core::ptr::write_unaligned(self.buf.add(self.pos) as *mut u32, v) };

        self.pos += 4;
    }

    fn put_u64(&mut self, v: u64) {
        // SAFETY: pos + 8 is within the DMA T-message buffer.
        unsafe { core::ptr::write_unaligned(self.buf.add(self.pos) as *mut u64, v) };

        self.pos += 8;
    }

    fn put_str(&mut self, s: &[u8]) {
        self.put_u16(s.len() as u16);

        // SAFETY: pos + s.len() is within the DMA T-message buffer.
        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), self.buf.add(self.pos), s.len()) };

        self.pos += s.len();
    }
}

impl P9Client {
    fn transact(&mut self, t_size: u32) -> MsgReader {
        // SAFETY: r_va points to a valid DMA allocation of at least MSIZE bytes.
        unsafe {
            core::ptr::write_bytes(
                (self.msg_dma.va + MSIZE as usize) as *mut u8,
                0,
                MSIZE as usize,
            )
        };

        self.vq.push_chain(&[
            (self.msg_dma.pa, t_size, false),
            ((self.msg_dma.pa + MSIZE as u64), MSIZE, true),
        ]);
        self.device.notify(VIRTQ_REQUEST);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);

        self.vq.pop_used();

        MsgReader::new((self.msg_dma.va + MSIZE as usize) as *const u8)
    }

    fn version(&mut self) -> bool {
        let mut w = MsgWriter::new(self.msg_dma.va as *mut u8);

        w.put_u8(P9_TVERSION);
        w.put_u16(0xFFFF);
        w.put_u32(MSIZE);
        w.put_str(b"9P2000.L");

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();
        let _tag = r.get_u16();

        if msg_type == P9_RLERROR || msg_type != P9_RVERSION {
            return false;
        }

        let _msize = r.get_u32();

        true
    }

    fn attach(&mut self) -> bool {
        let mut w = MsgWriter::new(self.msg_dma.va as *mut u8);

        w.put_u8(P9_TATTACH);
        w.put_u16(0);
        w.put_u32(ROOT_FID);
        w.put_u32(NOFID);
        w.put_str(b"");
        w.put_str(b"");
        w.put_u32(0);

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();

        msg_type == P9_RATTACH
    }

    fn walk(&mut self, fid: u32, newfid: u32, path: &[u8]) -> bool {
        let mut components = [[0u8; 64]; 8];
        let mut comp_lens = [0usize; 8];
        let mut nwname = 0u16;
        let mut start = 0;

        for i in 0..path.len() {
            if path[i] == b'/' {
                if i > start && nwname < 8 {
                    let len = (i - start).min(64);

                    components[nwname as usize][..len].copy_from_slice(&path[start..start + len]);
                    comp_lens[nwname as usize] = len;
                    nwname += 1;
                }

                start = i + 1;
            }
        }

        if start < path.len() && nwname < 8 {
            let len = (path.len() - start).min(64);

            components[nwname as usize][..len].copy_from_slice(&path[start..start + len]);
            comp_lens[nwname as usize] = len;
            nwname += 1;
        }

        if nwname == 0 {
            return false;
        }

        let mut w = MsgWriter::new(self.msg_dma.va as *mut u8);

        w.put_u8(P9_TWALK);
        w.put_u16(0);
        w.put_u32(fid);
        w.put_u32(newfid);
        w.put_u16(nwname);

        for i in 0..nwname as usize {
            w.put_str(&components[i][..comp_lens[i]]);
        }

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();

        msg_type == P9_RWALK
    }

    fn lopen(&mut self, fid: u32) -> bool {
        let mut w = MsgWriter::new(self.msg_dma.va as *mut u8);

        w.put_u8(P9_TLOPEN);
        w.put_u16(0);
        w.put_u32(fid);
        w.put_u32(0);

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();

        msg_type == P9_RLOPEN
    }

    fn read_file(&mut self, fid: u32, target: *mut u8, capacity: u32) -> u32 {
        let mut offset: u64 = 0;
        let max_chunk = MSIZE - 11;

        loop {
            let remaining = capacity as u64 - offset;

            if remaining == 0 {
                break;
            }

            let count = (remaining as u32).min(max_chunk);
            let mut w = MsgWriter::new(self.msg_dma.va as *mut u8);

            w.put_u8(P9_TREAD);
            w.put_u16(0);
            w.put_u32(fid);
            w.put_u64(offset);
            w.put_u32(count);

            let size = w.finish();
            let mut r = self.transact(size);
            let msg_type = r.get_u8();
            let _tag = r.get_u16();

            if msg_type != P9_RREAD {
                break;
            }

            let got = r.get_u32();

            if got == 0 {
                break;
            }

            // SAFETY: target + offset is within the caller's buffer (bounded
            // by capacity). r.data_ptr() points to got bytes of response data
            // within the DMA R-message buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    r.data_ptr(),
                    target.add(offset as usize),
                    got as usize,
                );
            }

            offset += got as u64;
        }

        offset as u32
    }

    fn clunk(&mut self, fid: u32) {
        let mut w = MsgWriter::new(self.msg_dma.va as *mut u8);

        w.put_u8(P9_TCLUNK);
        w.put_u16(0);
        w.put_u32(fid);

        let size = w.finish();
        let mut r = self.transact(size);
        let _msg_type = r.get_u8();
    }

    fn open_and_read(&mut self, path: &[u8], target: *mut u8, capacity: u32) -> Option<u32> {
        if !self.walk(ROOT_FID, FILE_FID, path) {
            return None;
        }

        if !self.lopen(FILE_FID) {
            self.clunk(FILE_FID);

            return None;
        }

        let bytes = self.read_file(FILE_FID, target, capacity);

        self.clunk(FILE_FID);

        Some(bytes)
    }
}

struct NinePServer {
    client: P9Client,
    shared_va: usize,
    shared_len: usize,
}

impl Dispatch for NinePServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            virtio_9p::SETUP => self.handle_setup(msg),
            virtio_9p::READ_FILE => self.handle_read_file(msg),
            virtio_9p::STAT => self.handle_stat(msg),
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

impl NinePServer {
    fn handle_setup(&mut self, msg: Incoming<'_>) {
        if msg.handles.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let vmo = Handle(msg.handles[0]);
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

        match abi::vmo::map(vmo, 0, rw) {
            Ok(va) => {
                let size = abi::vmo::info(vmo).unwrap_or(PAGE_SIZE);

                self.shared_va = va;
                self.shared_len = size;

                let _ = msg.reply_empty();
            }
            Err(_) => {
                let _ = msg.reply_error(ipc::STATUS_INVALID);
            }
        }
    }

    fn handle_read_file(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < virtio_9p::ReadFileRequest::SIZE {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let req = virtio_9p::ReadFileRequest::read_from(msg.payload);
        let path = req.path_bytes();

        if path.is_empty() {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let offset = req.vmo_offset as usize;
        let max_len = req.max_len as usize;

        if self.shared_va == 0 || offset + max_len > self.shared_len {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let target = (self.shared_va + offset) as *mut u8;

        match self.client.open_and_read(path, target, max_len as u32) {
            Some(bytes_read) => {
                let reply = virtio_9p::ReadFileReply { bytes_read };
                let mut data = [0u8; virtio_9p::ReadFileReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            None => {
                let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
            }
        }
    }

    fn handle_stat(&mut self, msg: Incoming<'_>) {
        if msg.payload.len() < virtio_9p::StatRequest::SIZE {
            let _ = msg.reply_error(ipc::STATUS_INVALID);

            return;
        }

        let req = virtio_9p::StatRequest::read_from(msg.payload);
        let path = req.path_bytes();

        if path.is_empty() {
            let reply = virtio_9p::StatReply { size: 0, exists: 0 };
            let mut data = [0u8; virtio_9p::StatReply::SIZE];

            reply.write_to(&mut data);

            let _ = msg.reply_ok(&data, &[]);

            return;
        }

        if !self.client.walk(ROOT_FID, FILE_FID, path) {
            let reply = virtio_9p::StatReply { size: 0, exists: 0 };
            let mut data = [0u8; virtio_9p::StatReply::SIZE];

            reply.write_to(&mut data);

            let _ = msg.reply_ok(&data, &[]);

            return;
        }

        if !self.client.lopen(FILE_FID) {
            self.client.clunk(FILE_FID);

            let reply = virtio_9p::StatReply { size: 0, exists: 0 };
            let mut data = [0u8; virtio_9p::StatReply::SIZE];

            reply.write_to(&mut data);

            let _ = msg.reply_ok(&data, &[]);

            return;
        }

        let mut total: u64 = 0;
        let max_chunk = MSIZE - 11;

        loop {
            let mut w = MsgWriter::new(self.client.msg_dma.va as *mut u8);

            w.put_u8(P9_TREAD);
            w.put_u16(0);
            w.put_u32(FILE_FID);
            w.put_u64(total);
            w.put_u32(max_chunk);

            let size = w.finish();
            let mut r = self.client.transact(size);
            let msg_type = r.get_u8();
            let _tag = r.get_u16();

            if msg_type != P9_RREAD {
                break;
            }

            let got = r.get_u32();

            if got == 0 {
                break;
            }

            total += got as u64;
        }

        self.client.clunk(FILE_FID);

        let reply = virtio_9p::StatReply {
            size: total,
            exists: 1,
        };
        let mut data = [0u8; virtio_9p::StatReply::SIZE];

        reply.write_to(&mut data);

        let _ = msg.reply_ok(&data, &[]);
    }
}

struct StubServer;

impl Dispatch for StubServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
    }
}

fn serve_stub() -> ! {
    let mut stub = StubServer;

    ipc::server::serve(HANDLE_SVC_EP, &mut stub);

    abi::thread::exit(0);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
    let (device, slot) = match virtio::find_device(virtio_va, virtio::DEVICE_9P) {
        Some(d) => d,
        None => serve_stub(),
    };
    let (ok, _accepted) = device.negotiate_features(VIRTIO_9P_F_MOUNT_TAG);

    if !ok {
        abi::thread::exit(3);
    }

    let queue_size = device
        .queue_max_size(VIRTQ_REQUEST)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let vq_bytes = virtio::Virtqueue::total_bytes(queue_size);
    let vq_alloc = vq_bytes.next_multiple_of(PAGE_SIZE);
    let _vq_dma = match init::request_dma(HANDLE_INIT_EP, vq_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(4),
    };

    // SAFETY: _vq_dma.va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(_vq_dma.va as *mut u8, 0, vq_alloc) };

    let vq = virtio::Virtqueue::new(queue_size, _vq_dma.va, _vq_dma.pa);

    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    let msg_alloc = (MSIZE as usize * 2).next_multiple_of(PAGE_SIZE);
    let msg_dma = match init::request_dma(HANDLE_INIT_EP, msg_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: msg_dma.va is a valid DMA allocation; zeroing before use.
    unsafe { core::ptr::write_bytes(msg_dma.va as *mut u8, 0, msg_alloc) };

    device.driver_ok();

    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(6),
    };
    let irq_num = virtio::SPI_BASE_INTID + slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(7);
    }

    let mut client = P9Client {
        device,
        vq,
        irq_event,
        msg_dma,
    };

    if !client.version() {
        abi::thread::exit(8);
    }

    if !client.attach() {
        abi::thread::exit(9);
    }

    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE001),
    };

    console::write(console_ep, b"  9p: 9P2000.L attached\n");

    console::write(console_ep, b"  9p: ready\n");

    let mut server = NinePServer {
        client,
        shared_va: 0,
        shared_len: 0,
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
