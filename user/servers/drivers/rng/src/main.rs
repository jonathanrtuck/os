//! Virtio-rng driver — hardware random number generator.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!   Handle 5: service endpoint (pre-registered by init as "rng")
//!
//! Probes the virtio MMIO region for an RNG device (device ID 4).
//! Allocates a DMA buffer, runs a self-test, then enters an IPC serve
//! loop returning random bytes to callers.

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
const VIRTQ_REQUEST: u32 = 0;

struct RngDevice {
    device: virtio::Device,
    vq: virtio::Virtqueue,
    irq_event: Handle,
    buf_va: usize,
    buf_pa: u64,
}

impl RngDevice {
    fn fill(&mut self, size: usize) -> &[u8] {
        let clamped = size.min(PAGE_SIZE);

        self.vq.push_chain(&[(self.buf_pa, clamped as u32, true)]);
        self.device.notify(VIRTQ_REQUEST);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let used_len = match self.vq.pop_used() {
            Some(elem) => (elem.len as usize).min(clamped),
            None => 0,
        };

        // SAFETY: buf_va points to a DMA allocation of PAGE_SIZE bytes;
        // the device has written used_len bytes.
        unsafe { core::slice::from_raw_parts(self.buf_va as *const u8, used_len) }
    }
}

struct RngServer {
    rng: RngDevice,
}

impl Dispatch for RngServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            rng::FILL => {
                if msg.payload.len() < rng::FillRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = rng::FillRequest::read_from(msg.payload);
                let size = (req.size as usize).min(ipc::MAX_PAYLOAD);
                let bytes = self.rng.fill(size);
                let _ = msg.reply_ok(bytes, &[]);
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

fn self_test(rng: &mut RngDevice, console_ep: Handle) {
    let bytes = rng.fill(8);

    if bytes.len() < 8 {
        console::write(console_ep, b"rng: FAIL short read\n");

        return;
    }

    let val = u64::from_le_bytes(bytes[..8].try_into().unwrap());

    if val == 0 {
        console::write(console_ep, b"rng: WARN all zeros\n");
    } else {
        console::write(console_ep, b"rng: self-test OK\n");
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
    let (device, rng_slot) = match virtio::find_device(virtio_va, virtio::DEVICE_RNG) {
        Some(d) => d,
        None => abi::thread::exit(0),
    };
    let (ok, _accepted) = device.negotiate_features(0);

    if !ok {
        abi::thread::exit(3);
    }

    let queue_size = device
        .queue_max_size(VIRTQ_REQUEST)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let vq_bytes = virtio::Virtqueue::total_bytes(queue_size);
    let vq_alloc = vq_bytes.next_multiple_of(PAGE_SIZE);
    let vq_dma = match init::request_dma(HANDLE_INIT_EP, vq_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(4),
    };
    let vq_va = vq_dma.va;

    // SAFETY: vq_va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_alloc) };

    let vq_pa = vq_va as u64;
    let vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        VIRTQ_REQUEST,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    let buf_dma = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };
    let buf_va = buf_dma.va;

    // SAFETY: buf_va is a valid DMA allocation of 1 page; zeroing before use.
    unsafe { core::ptr::write_bytes(buf_va as *mut u8, 0, PAGE_SIZE) };

    let buf_pa = buf_va as u64;

    device.driver_ok();

    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(6),
    };
    let irq_num = virtio::SPI_BASE_INTID + rng_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(7);
    }

    let mut rng_dev = RngDevice {
        device,
        vq,
        irq_event,
        buf_va,
        buf_pa,
    };
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    self_test(&mut rng_dev, console_ep);

    console::write(console_ep, b"rng: ready\n");

    let mut server = RngServer { rng: rng_dev };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
