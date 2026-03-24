//! Device initialization (Phase A) and display handshake (Phase B).
//!
//! Phase A: Maps MMIO, negotiates VIRTIO_GPU_F_VIRGL feature, sets up
//! the control virtqueue and registers the IRQ.
//!
//! Phase B: Queries display info from the device, exchanges handshake
//! messages with init (MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY).

use protocol::{
    gpu::{DisplayInfoMsg, GpuConfig, MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY},
    virgl::{VIRTIO_GPU_CMD_GET_DISPLAY_INFO, VIRTIO_GPU_RESP_OK_DISPLAY_INFO},
};

use crate::{
    wire::{ctrl_header, CtrlHeader, DisplayInfo, DmaBuf},
    INIT_HANDLE, VIRTIO_GPU_F_VIRGL, VIRTQ_CONTROL,
};

// ── Phase A: Device initialization ───────────────────────────────────────

pub(crate) fn init_device(mmio_pa: u64, irq: u32) -> (virtio::Device, virtio::Virtqueue, u8) {
    // Map the MMIO region.
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000).unwrap_or_else(|_| {
        sys::print(b"virgil-render: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Manual feature negotiation — we MUST set VIRTIO_GPU_F_VIRGL (bit 0).
    // Cannot use device.negotiate() because it accepts no features.
    device.reset();
    device.set_status(1); // ACKNOWLEDGE
    device.set_status(1 | 2); // ACKNOWLEDGE | DRIVER

    // Read device features and require VIRGL support.
    let dev_features = device.read_device_features();
    if dev_features & VIRTIO_GPU_F_VIRGL == 0 {
        sys::print(b"virgil-render: device does not support VIRTIO_GPU_F_VIRGL\n");
        sys::exit();
    }
    // Write driver features: enable VIRGL.
    device.write_driver_features(VIRTIO_GPU_F_VIRGL);

    device.set_status(1 | 2 | 8); // ACKNOWLEDGE | DRIVER | FEATURES_OK
    if device.read_status() & 8 == 0 {
        sys::print(b"virgil-render: FEATURES_OK not set by device\n");
        sys::exit();
    }

    // Register IRQ (handle slot 2: after init=0, present=1).
    let irq_handle = sys::interrupt_register(irq).unwrap_or_else(|_| {
        sys::print(b"virgil-render: interrupt_register failed\n");
        sys::exit();
    });

    // Setup control virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_CONTROL),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virgil-render: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_bytes = (1usize << vq_order) * 4096;
    // SAFETY: vq_va is valid DMA memory of vq_bytes size, freshly allocated.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };
    let mut vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);
    device.setup_queue(
        VIRTQ_CONTROL,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render: virtio-gpu 3D ready\n");
    (device, vq, irq_handle)
}

// ── Phase B: Display query + init handshake ──────────────────────────────

pub(crate) fn get_display_info(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
) -> (u32, u32) {
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page, writing CtrlHeader at start.
    unsafe {
        core::ptr::write(
            cmd.va as *mut CtrlHeader,
            ctrl_header(VIRTIO_GPU_CMD_GET_DISPLAY_INFO),
        );
    }
    let resp_offset = 256u64;
    let resp_pa = cmd.pa + resp_offset;
    let resp_va = cmd.va + resp_offset as usize;
    let resp_type = crate::resources::gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<CtrlHeader>() as u32,
        resp_pa,
        resp_va,
        24 + 16 * 24, // header + 16 display entries
    );
    let (width, height) = if resp_type == VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
        // SAFETY: Device wrote a valid display info response at resp_va.
        let info_ptr = (resp_va + core::mem::size_of::<CtrlHeader>()) as *const DisplayInfo;
        let info = unsafe { core::ptr::read_volatile(info_ptr) };
        if info.enabled != 0 {
            (info.rect_width, info.rect_height)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };
    cmd.free();
    (width, height)
}

pub(crate) fn init_handshake(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    ch: &ipc::Channel,
) -> (u32, u32) {
    // Query display dimensions.
    let (disp_w, disp_h) = get_display_info(device, vq, irq_handle);
    let width = if disp_w > 0 { disp_w } else { 1024 };
    let height = if disp_h > 0 { disp_h } else { 768 };

    {
        let mut buf = [0u8; 32];
        let prefix = b"     display ";
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        pos += sys::format_u32(width, &mut buf[pos..]);
        buf[pos] = b'x';
        pos += 1;
        pos += sys::format_u32(height, &mut buf[pos..]);
        buf[pos] = b'\n';
        pos += 1;
        sys::print(&buf[..pos]);
    }

    // Send display info back to init.
    let info_msg =
        // SAFETY: DisplayInfoMsg is repr(C) and fits in payload.
        unsafe { ipc::Message::from_payload(MSG_DISPLAY_INFO, &DisplayInfoMsg { width, height, refresh_rate: 0 }) };
    ch.send(&info_msg);
    let _ = sys::channel_signal(INIT_HANDLE);

    // Wait for GPU config from init.
    sys::print(b"     waiting for gpu config\n");
    let mut msg = ipc::Message::new(0);
    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_GPU_CONFIG {
            break;
        }
    }
    // SAFETY: msg payload contains a valid GpuConfig written by init.
    let config: GpuConfig = unsafe { msg.payload_as() };
    let cfg_width = config.fb_width;
    let cfg_height = config.fb_height;
    // Signal init that we're ready.
    sys::print(b"     handshake complete, sending GPU_READY\n");
    let ready_msg = ipc::Message::new(MSG_GPU_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(INIT_HANDLE);

    (cfg_width, cfg_height)
}
