//! Virtio-snd driver — audio output via virtio sound device.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!   Handle 5: service endpoint (pre-registered by init as "snd")
//!
//! Probes the virtio MMIO region for a sound device (device ID 25).
//! Configures output stream 0 for S16LE stereo 48 kHz, then enters
//! an IPC serve loop accepting PCM write requests from the audio
//! mixer service.

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

const CONTROLQ: u32 = 0;
const TXQUEUE: u32 = 2;

const VIRTIO_SND_R_PCM_SET_PARAMS: u32 = 0x0200;
const VIRTIO_SND_R_PCM_PREPARE: u32 = 0x0300;
const _VIRTIO_SND_R_PCM_RELEASE: u32 = 0x0400;
const VIRTIO_SND_R_PCM_START: u32 = 0x0500;
const _VIRTIO_SND_R_PCM_STOP: u32 = 0x0600;

const VIRTIO_SND_S_OK: u32 = 0x8000;

const VIRTIO_SND_PCM_FMT_S16: u8 = 2;
const VIRTIO_SND_PCM_RATE_48000: u8 = 6;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;
const FRAME_BYTES: usize = 4;

struct SndDevice {
    device: virtio::Device,
    ctrl_vq: virtio::Virtqueue,
    tx_vq: virtio::Virtqueue,
    irq_event: Handle,
    ctrl_dma: init::DmaBuf,
    tx_dma: [init::DmaBuf; 2],
    tx_active: usize,
    tx_pending: bool,
    started: bool,
    stopped: bool,
    tx_status_offset: [usize; 2],
}

impl SndDevice {
    fn ctrl_request(&mut self, request: &[u8]) -> u32 {
        let buf = self.ctrl_dma.va as *mut u8;
        let req_len = request.len();

        // SAFETY: ctrl_dma is a PAGE_SIZE DMA buffer, request fits.
        unsafe {
            core::ptr::copy_nonoverlapping(request.as_ptr(), buf, req_len);
            core::ptr::write_bytes(buf.add(req_len), 0, 4);
        }

        let req_pa = self.ctrl_dma.pa;
        let resp_pa = self.ctrl_dma.pa + req_len as u64;

        self.ctrl_vq
            .push_chain(&[(req_pa, req_len as u32, false), (resp_pa, 4, true)]);

        self.device.notify(CONTROLQ);

        let _ = abi::event::wait(&[(self.irq_event, 0x1)]);

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.ctrl_vq.pop_used();

        // SAFETY: device has written 4-byte response at resp_pa.
        unsafe {
            let resp = (self.ctrl_dma.va + req_len) as *const u32;

            core::ptr::read_volatile(resp)
        }
    }

    fn set_params(&mut self) -> bool {
        #[repr(C)]
        struct SetParams {
            code: u32,
            stream_id: u32,
            buffer_bytes: u32,
            period_bytes: u32,
            features: u32,
            channels: u8,
            format: u8,
            rate: u8,
            _padding: u8,
        }

        let params = SetParams {
            code: VIRTIO_SND_R_PCM_SET_PARAMS,
            stream_id: 0,
            buffer_bytes: PAGE_SIZE as u32,
            period_bytes: PAGE_SIZE as u32 / 4,
            features: 0,
            channels: CHANNELS as u8,
            format: VIRTIO_SND_PCM_FMT_S16,
            rate: VIRTIO_SND_PCM_RATE_48000,
            _padding: 0,
        };
        let bytes = unsafe {
            core::slice::from_raw_parts(
                &params as *const SetParams as *const u8,
                core::mem::size_of::<SetParams>(),
            )
        };

        self.ctrl_request(bytes) == VIRTIO_SND_S_OK
    }

    fn prepare(&mut self) -> bool {
        let mut req = [0u8; 8];

        req[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_PREPARE.to_le_bytes());

        self.ctrl_request(&req) == VIRTIO_SND_S_OK
    }

    fn start(&mut self) -> bool {
        let mut req = [0u8; 8];

        req[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_START.to_le_bytes());

        let ok = self.ctrl_request(&req) == VIRTIO_SND_S_OK;

        if ok {
            self.started = true;
            self.stopped = false;
        }

        ok
    }

    fn wait_tx_complete(&mut self) {
        if !self.tx_pending {
            return;
        }

        let now = abi::system::clock_read().unwrap_or(0);
        let deadline = now + 500_000_000;

        if abi::event::wait_deadline(self.irq_event, 0x1, deadline).is_err() {
            self.tx_pending = false;
            self.stopped = true;
            self.started = false;

            return;
        }

        self.device.ack_interrupt();

        let _ = abi::event::clear(self.irq_event, 0x1);
        let _ = self.tx_vq.pop_used();

        self.tx_pending = false;

        let prev = 1 - self.tx_active;
        let status_va = self.tx_dma[prev].va + self.tx_status_offset[prev];
        // SAFETY: status_va points to the 8-byte status within the completed TX DMA buffer.
        let status = unsafe { core::ptr::read_volatile(status_va as *const u32) };

        if status != VIRTIO_SND_S_OK {
            self.stopped = true;
            self.started = false;
        }
    }

    fn fill_tx_buf(buf: &init::DmaBuf, f32_data: &[f32]) -> usize {
        let frame_count = f32_data.len() / 2;
        let s16_bytes = frame_count * FRAME_BYTES;

        // SAFETY: buf.va is a PAGE_SIZE DMA buffer.
        unsafe {
            core::ptr::write(buf.va as *mut u32, 0u32);
        }

        let s16_start = buf.va + 4;

        for (i, &val) in f32_data.iter().enumerate() {
            let sample = (val * 32767.0).clamp(-32768.0, 32767.0) as i16;

            // SAFETY: s16_start + i*2 is within the DMA buffer.
            unsafe {
                core::ptr::write((s16_start + i * 2) as *mut i16, sample);
            }
        }

        let status_offset = 4 + s16_bytes;

        // SAFETY: status_offset is within the DMA buffer.
        unsafe { core::ptr::write_bytes((buf.va + status_offset) as *mut u8, 0, 8) };

        s16_bytes
    }

    fn submit_tx_buf(&mut self, s16_bytes: usize) {
        self.tx_status_offset[self.tx_active] = 4 + s16_bytes;

        let buf = &self.tx_dma[self.tx_active];
        let header_pa = buf.pa;
        let data_pa = buf.pa + 4;
        let status_pa = buf.pa + (4 + s16_bytes) as u64;

        self.tx_vq.push_chain(&[
            (header_pa, 4, false),
            (data_pa, s16_bytes as u32, false),
            (status_pa, 8, true),
        ]);

        self.device.notify(TXQUEUE);

        self.tx_pending = true;
        self.tx_active ^= 1;
    }

    fn write_pcm(&mut self, f32_data: &[f32]) -> bool {
        let frame_count = f32_data.len() / 2;
        let s16_bytes = frame_count * FRAME_BYTES;

        if s16_bytes > PAGE_SIZE - 12 {
            return true;
        }

        let s16_bytes = Self::fill_tx_buf(&self.tx_dma[self.tx_active], f32_data);

        self.wait_tx_complete();

        if self.stopped {
            return false;
        }

        self.submit_tx_buf(s16_bytes);

        true
    }
}

struct SndServer {
    snd: SndDevice,
    shared_va: usize,
    shared_len: usize,
}

impl Dispatch for SndServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            snd::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let vmo = Handle(msg.handles[0]);
                let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

                match abi::vmo::map(vmo, 0, rw) {
                    Ok(va) => {
                        self.shared_va = va;
                        self.shared_len = PAGE_SIZE * 4;

                        let _ = msg.reply_empty();
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }
            snd::WRITE => {
                if msg.payload.len() < snd::WriteRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = snd::WriteRequest::read_from(msg.payload);
                let offset = req.offset as usize;
                let len = req.len as usize;

                if self.shared_va == 0 || offset + len > self.shared_len {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let f32_count = len / 4;
                let f32_data = unsafe {
                    core::slice::from_raw_parts((self.shared_va + offset) as *const f32, f32_count)
                };
                let chunk_frames = (PAGE_SIZE - 12) / FRAME_BYTES;
                let chunk_samples = chunk_frames * 2;
                let mut written = 0;

                if !self.snd.started && !self.snd.start() {
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                    return;
                }

                if self.snd.stopped {
                    self.snd.stopped = false;
                    self.snd.started = false;

                    if !self.snd.prepare() || !self.snd.start() {
                        let _ = msg.reply_error(ipc::STATUS_IO_ERROR);

                        return;
                    }
                }

                while written < f32_count {
                    let remaining = f32_count - written;
                    let chunk = remaining.min(chunk_samples);

                    if !self.snd.write_pcm(&f32_data[written..written + chunk]) {
                        break;
                    }

                    written += chunk;
                }

                self.snd.wait_tx_complete();

                if self.snd.stopped {
                    let _ = msg.reply_error(ipc::STATUS_IO_ERROR);
                } else {
                    let _ = msg.reply_empty();
                }
            }
            snd::GET_INFO => {
                let reply = snd::InfoReply {
                    sample_rate: SAMPLE_RATE,
                    channels: CHANNELS,
                    bits_per_sample: 16,
                };
                let mut data = [0u8; snd::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

fn setup_virtqueue(
    device: &virtio::Device,
    queue_idx: u32,
    init_ep: Handle,
) -> Option<(virtio::Virtqueue, init::DmaBuf)> {
    let queue_size = device
        .queue_max_size(queue_idx)
        .min(virtio::DEFAULT_QUEUE_SIZE);

    if queue_size == 0 {
        return None;
    }

    let vq_bytes = virtio::Virtqueue::total_bytes(queue_size);
    let vq_alloc = vq_bytes.next_multiple_of(PAGE_SIZE);
    let vq_dma = init::request_dma(init_ep, vq_alloc).ok()?;

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(vq_dma.va as *mut u8, 0, vq_alloc) };

    let vq = virtio::Virtqueue::new(queue_size, vq_dma.va, vq_dma.pa);

    device.setup_queue(
        queue_idx,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    Some((vq, vq_dma))
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
    let (device, snd_slot) = match virtio::find_device(virtio_va, virtio::DEVICE_SND) {
        Some(d) => d,
        None => abi::thread::exit(0),
    };
    let (ok, _) = device.negotiate_features(0);

    if !ok {
        abi::thread::exit(3);
    }

    let (ctrl_vq, _ctrl_vq_dma) = match setup_virtqueue(&device, CONTROLQ, HANDLE_INIT_EP) {
        Some(pair) => pair,
        None => abi::thread::exit(4),
    };
    let _event_vq = setup_virtqueue(&device, 1, HANDLE_INIT_EP);
    let (tx_vq, _tx_vq_dma) = match setup_virtqueue(&device, TXQUEUE, HANDLE_INIT_EP) {
        Some(pair) => pair,
        None => abi::thread::exit(4),
    };
    let _rx_vq = setup_virtqueue(&device, 3, HANDLE_INIT_EP);
    let ctrl_dma = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(ctrl_dma.va as *mut u8, 0, PAGE_SIZE) };

    let tx_dma_0 = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(tx_dma_0.va as *mut u8, 0, PAGE_SIZE) };

    let tx_dma_1 = match init::request_dma(HANDLE_INIT_EP, PAGE_SIZE) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: DMA allocation is valid; zeroing before use.
    unsafe { core::ptr::write_bytes(tx_dma_1.va as *mut u8, 0, PAGE_SIZE) };

    device.driver_ok();

    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(6),
    };
    let irq_num = virtio::SPI_BASE_INTID + snd_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(7);
    }

    let mut snd = SndDevice {
        device,
        ctrl_vq,
        tx_vq,
        irq_event,
        ctrl_dma,
        tx_dma: [tx_dma_0, tx_dma_1],
        tx_active: 0,
        tx_pending: false,
        started: false,
        stopped: false,
        tx_status_offset: [0; 2],
    };
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    if !snd.set_params() {
        console::write(console_ep, b"snd: FAIL set_params\n");

        abi::thread::exit(9);
    }
    if !snd.prepare() {
        console::write(console_ep, b"snd: FAIL prepare\n");

        abi::thread::exit(10);
    }

    console::write(console_ep, b"snd: ready (S16LE stereo 48kHz)\n");

    let mut server = SndServer {
        snd,
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
