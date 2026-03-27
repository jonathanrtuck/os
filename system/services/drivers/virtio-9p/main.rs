//! Userspace virtio 9P filesystem driver.
//!
//! Provides host filesystem passthrough via the 9P2000.L protocol over
//! virtio transport. QEMU serves a host directory; this driver reads files
//! from it on behalf of init (and later, other processes).
//!
//! Phase 1: single-shot file reads requested by init via IPC.

#![no_std]
#![no_main]

use protocol::{
    device::MSG_DEVICE_CONFIG,
    fs::{MSG_FS_READ_REQUEST, MSG_FS_READ_RESPONSE},
};

const FILE_FID: u32 = 1;
const MSIZE: u32 = 32768;
const NOFID: u32 = 0xFFFF_FFFF;
const ROOT_FID: u32 = 0;
const TAG_NOTAG: u16 = 0xFFFF;
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
const P9_RCLUNK: u8 = 121;

struct MsgReader {
    buf: *const u8,
    pos: usize,
}
struct MsgWriter {
    buf: *mut u8,
    pos: usize,
}
/// Shared state for 9P request/response exchange.
struct P9Client {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    t_va: usize,
    t_pa: u64,
    r_va: usize,
    r_pa: u64,
}

impl MsgReader {
    fn new(buf: *const u8) -> Self {
        Self { buf, pos: 4 } // skip size field
    }

    fn data_ptr(&self) -> *const u8 {
        unsafe { self.buf.add(self.pos) }
    }
    fn get_u8(&mut self) -> u8 {
        let v = unsafe { *self.buf.add(self.pos) };

        self.pos += 1;

        v
    }
    fn get_u16(&mut self) -> u16 {
        let v = unsafe { core::ptr::read_unaligned(self.buf.add(self.pos) as *const u16) };

        self.pos += 2;

        v
    }
    fn get_u32(&mut self) -> u32 {
        let v = unsafe { core::ptr::read_unaligned(self.buf.add(self.pos) as *const u32) };

        self.pos += 4;

        v
    }
}
impl MsgWriter {
    fn new(buf: *mut u8) -> Self {
        Self { buf, pos: 4 } // skip size field
    }

    fn finish(&mut self) -> u32 {
        let size = self.pos as u32;

        unsafe { core::ptr::write_unaligned(self.buf as *mut u32, size) };

        size
    }
    fn put_u8(&mut self, v: u8) {
        unsafe { *self.buf.add(self.pos) = v };

        self.pos += 1;
    }
    fn put_u16(&mut self, v: u16) {
        unsafe { core::ptr::write_unaligned(self.buf.add(self.pos) as *mut u16, v) };

        self.pos += 2;
    }
    fn put_u32(&mut self, v: u32) {
        unsafe { core::ptr::write_unaligned(self.buf.add(self.pos) as *mut u32, v) };

        self.pos += 4;
    }
    fn put_u64(&mut self, v: u64) {
        unsafe { core::ptr::write_unaligned(self.buf.add(self.pos) as *mut u64, v) };

        self.pos += 8;
    }
    fn put_str(&mut self, s: &[u8]) {
        self.put_u16(s.len() as u16);

        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), self.buf.add(self.pos), s.len()) };

        self.pos += s.len();
    }
}

impl P9Client {
    fn attach(&mut self) -> bool {
        let mut w = MsgWriter::new(self.t_va as *mut u8);

        w.put_u8(P9_TATTACH);
        w.put_u16(0);
        w.put_u32(ROOT_FID);
        w.put_u32(NOFID); // afid (no auth)
        w.put_str(b""); // uname
        w.put_str(b""); // aname
        w.put_u32(0); // n_uname (9P2000.L extension)

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();
        let _tag = r.get_u16();

        if msg_type == P9_RLERROR {
            let ecode = r.get_u32();

            {
                let mut buf = [0u8; 32];
                let prefix = b"9p: attach error ";

                buf[..prefix.len()].copy_from_slice(prefix);

                let mut pos = prefix.len();

                pos += sys::format_u32(ecode, &mut buf[pos..]);
                buf[pos] = b'\n';
                pos += 1;

                sys::print(&buf[..pos]);
            }

            return false;
        }
        if msg_type != P9_RATTACH {
            sys::print(b"9p: unexpected attach response\n");

            return false;
        }

        sys::print(b"     attached to root\n");

        true
    }
    fn clunk(&mut self, fid: u32) {
        let mut w = MsgWriter::new(self.t_va as *mut u8);

        w.put_u8(P9_TCLUNK);
        w.put_u16(0);
        w.put_u32(fid);

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();

        if msg_type == P9_RLERROR {
            sys::print(b"9p: clunk error\n");
        }
    }
    fn lopen(&mut self, fid: u32) -> bool {
        let mut w = MsgWriter::new(self.t_va as *mut u8);

        w.put_u8(P9_TLOPEN);
        w.put_u16(0);
        w.put_u32(fid);
        w.put_u32(0); // flags = O_RDONLY

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();
        let _tag = r.get_u16();

        if msg_type == P9_RLERROR {
            let ecode = r.get_u32();

            {
                let mut buf = [0u8; 32];
                let prefix = b"9p: lopen error ";

                buf[..prefix.len()].copy_from_slice(prefix);

                let mut pos = prefix.len();

                pos += sys::format_u32(ecode, &mut buf[pos..]);
                buf[pos] = b'\n';
                pos += 1;

                sys::print(&buf[..pos]);
            }

            return false;
        }

        msg_type == P9_RLOPEN
    }
    /// Read file data into the target buffer. Returns total bytes read.
    fn read_file(&mut self, fid: u32, target: *mut u8, capacity: u32) -> u32 {
        let mut offset: u64 = 0;
        let max_chunk = MSIZE - 11; // Rread header: size(4)+type(1)+tag(2)+count(4)

        loop {
            let remaining = capacity as u64 - offset;

            if remaining == 0 {
                break;
            }

            let count = core::cmp::min(remaining, max_chunk as u64) as u32;
            let mut w = MsgWriter::new(self.t_va as *mut u8);

            w.put_u8(P9_TREAD);
            w.put_u16(0);
            w.put_u32(fid);
            w.put_u64(offset);
            w.put_u32(count);

            let size = w.finish();
            let mut r = self.transact(size);
            let msg_type = r.get_u8();
            let _tag = r.get_u16();

            if msg_type == P9_RLERROR {
                sys::print(b"9p: read error\n");

                break;
            }
            if msg_type != P9_RREAD {
                sys::print(b"9p: unexpected read response\n");

                break;
            }

            let got = r.get_u32();

            if got == 0 {
                break; // EOF
            }

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
    fn transact(&mut self, t_size: u32) -> MsgReader {
        // Zero the response buffer.
        unsafe { core::ptr::write_bytes(self.r_va as *mut u8, 0, MSIZE as usize) };

        // 2-descriptor chain: T-message (readable) → R-message (writable).
        self.vq
            .push_chain(&[(self.t_pa, t_size, false), (self.r_pa, MSIZE, true)]);
        self.device.notify(VIRTQ_REQUEST);

        let _ = sys::wait(&[self.irq_handle.0], u64::MAX);

        self.device.ack_interrupt();
        self.vq.pop_used();

        let _ = sys::interrupt_ack(self.irq_handle);

        MsgReader::new(self.r_va as *const u8)
    }
    fn version(&mut self) -> bool {
        let mut w = MsgWriter::new(self.t_va as *mut u8);

        w.put_u8(P9_TVERSION);
        w.put_u16(TAG_NOTAG);
        w.put_u32(MSIZE);
        w.put_str(b"9P2000.L");

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();
        let _tag = r.get_u16();

        if msg_type == P9_RLERROR {
            sys::print(b"9p: version error\n");

            return false;
        }
        if msg_type != P9_RVERSION {
            sys::print(b"9p: unexpected version response\n");

            return false;
        }

        let _msize = r.get_u32();

        sys::print(b"     9P2000.L negotiated\n");

        true
    }
    fn walk(&mut self, fid: u32, newfid: u32, name: &[u8]) -> bool {
        let mut w = MsgWriter::new(self.t_va as *mut u8);

        w.put_u8(P9_TWALK);
        w.put_u16(0);
        w.put_u32(fid);
        w.put_u32(newfid);
        w.put_u16(1); // nwname
        w.put_str(name);

        let size = w.finish();
        let mut r = self.transact(size);
        let msg_type = r.get_u8();
        let _tag = r.get_u16();

        if msg_type == P9_RLERROR {
            let ecode = r.get_u32();

            {
                let mut buf = [0u8; 32];
                let prefix = b"9p: walk error ";

                buf[..prefix.len()].copy_from_slice(prefix);

                let mut pos = prefix.len();

                pos += sys::format_u32(ecode, &mut buf[pos..]);
                buf[pos] = b'\n';
                pos += 1;

                sys::print(&buf[..pos]);
            }

            return false;
        }

        msg_type == P9_RWALK
    }
}

fn print_u32(n: u32) {
    sys::print_u32(n);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x82 virtio-9p - starting\n");

    // Read device config from ring buffer (channel 0 = init).
    let ch = unsafe { ipc::Channel::from_base(protocol::CHANNEL_SHM_BASE, ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virtio-9p: no config message\n");
        sys::exit();
    }

    let config = if let Some(protocol::device::Message::DeviceConfig(c)) =
        protocol::device::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"virtio-9p: bad device config\n");
        sys::exit();
    };
    let mmio_pa = config.mmio_pa;
    let irq = config.irq;
    // Map MMIO region.
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000).unwrap_or_else(|_| {
        sys::print(b"virtio-9p: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    if !device.negotiate() {
        sys::print(b"virtio-9p: negotiate failed\n");
        sys::exit();
    }

    let irq_handle: sys::InterruptHandle = sys::interrupt_register(irq).unwrap_or_else(|_| {
        sys::print(b"virtio-9p: interrupt_register failed\n");
        sys::exit();
    });
    // Allocate virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_REQUEST),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-9p: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_bytes = (1usize << order) * ipc::PAGE_SIZE;

    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };

    let mut vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    // Allocate T/R message buffers (16 pages = 64 KiB: T at offset 0, R at MSIZE).
    let msg_order = 4; // 2^4 = 16 pages = 64 KiB
    let mut msg_pa: u64 = 0;
    let msg_va = sys::dma_alloc(msg_order, &mut msg_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-9p: dma_alloc (msg) failed\n");
        sys::exit();
    });

    unsafe { core::ptr::write_bytes(msg_va as *mut u8, 0, (MSIZE as usize) * 2) };

    device.driver_ok();

    let mut client = P9Client {
        device,
        vq,
        irq_handle,
        t_va: msg_va,
        t_pa: msg_pa,
        r_va: msg_va + MSIZE as usize,
        r_pa: msg_pa + MSIZE as u64,
    };

    // 9P protocol init.
    if !client.version() {
        sys::exit();
    }
    if !client.attach() {
        sys::exit();
    }

    // Process file read requests from init.
    loop {
        let _ = sys::wait(&[0], u64::MAX);

        while ch.try_recv(&mut msg) {
            if msg.msg_type != MSG_FS_READ_REQUEST {
                continue;
            }

            // Read request fields manually to avoid payload_as alignment issues.
            let (va, capacity, name_bytes) = unsafe {
                let p = msg.payload.as_ptr();
                let va = core::ptr::read_unaligned(p as *const u64);
                let capacity = core::ptr::read_unaligned(p.add(8) as *const u32);
                let name_start = p.add(16);
                let mut name_len = 0usize;

                while name_len < 44 && *name_start.add(name_len) != 0 {
                    name_len += 1;
                }

                (
                    va,
                    capacity,
                    core::slice::from_raw_parts(name_start, name_len),
                )
            };

            {
                let prefix = b"     reading: ";
                let mut buf = [0u8; 80];
                let mut pos = prefix.len();

                buf[..pos].copy_from_slice(prefix);

                let name_len = if name_bytes.len() > 60 {
                    60
                } else {
                    name_bytes.len()
                };

                buf[pos..pos + name_len].copy_from_slice(&name_bytes[..name_len]);

                pos += name_len;
                buf[pos] = b'\n';
                pos += 1;

                sys::print(&buf[..pos]);
            }

            // Walk from root to file.
            if !client.walk(ROOT_FID, FILE_FID, name_bytes) {
                sys::print(b"     file not found\n");

                let mut resp_msg = ipc::Message::new(MSG_FS_READ_RESPONSE);

                unsafe {
                    let p = resp_msg.payload.as_mut_ptr();

                    core::ptr::write_unaligned(p as *mut u32, 0); // len
                    core::ptr::write_unaligned(p.add(4) as *mut u32, 1); // status
                }

                ch.send(&resp_msg);

                let _ = sys::channel_signal(sys::ChannelHandle(0));

                continue;
            }
            // Open for reading.
            if !client.lopen(FILE_FID) {
                client.clunk(FILE_FID);

                let mut resp_msg = ipc::Message::new(MSG_FS_READ_RESPONSE);

                unsafe {
                    let p = resp_msg.payload.as_mut_ptr();

                    core::ptr::write_unaligned(p as *mut u32, 0); // len
                    core::ptr::write_unaligned(p.add(4) as *mut u32, 2); // status
                }

                ch.send(&resp_msg);

                let _ = sys::channel_signal(sys::ChannelHandle(0));

                continue;
            }

            // Read into shared buffer.
            let len = client.read_file(FILE_FID, va as *mut u8, capacity);

            client.clunk(FILE_FID);

            {
                let mut buf = [0u8; 32];
                let prefix = b"     read ";

                buf[..prefix.len()].copy_from_slice(prefix);

                let mut pos = prefix.len();

                pos += sys::format_u32(len, &mut buf[pos..]);

                let suffix = b" bytes\n";

                buf[pos..pos + suffix.len()].copy_from_slice(suffix);

                pos += suffix.len();

                sys::print(&buf[..pos]);
            }

            let mut resp_msg = ipc::Message::new(MSG_FS_READ_RESPONSE);

            unsafe {
                let p = resp_msg.payload.as_mut_ptr();

                core::ptr::write_unaligned(p as *mut u32, len);
                core::ptr::write_unaligned(p.add(4) as *mut u32, 0); // status
            }

            ch.send(&resp_msg);

            let _ = sys::channel_signal(sys::ChannelHandle(0));
        }
    }
}
