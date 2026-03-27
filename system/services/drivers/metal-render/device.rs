//! Phase A-C: device initialization, display handshake, and render config.

use protocol::{
    compose::MSG_COMPOSITOR_CONFIG,
    gpu::{DisplayInfoMsg, MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY},
};

use crate::{
    print_hex_u32, print_u32, virtio_helpers::alloc_virtqueue, INIT_HANDLE, VIRTQ_RENDER,
    VIRTQ_SETUP,
};

pub(crate) struct DisplayConfig {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) refresh_rate: u32,
}

pub(crate) struct RenderConfig {
    pub(crate) scene_va: u64,
    pub(crate) content_va: u64,
    pub(crate) content_size: u32,
    pub(crate) scale_factor: f32,
    pub(crate) pointer_state_va: u64,
    pub(crate) font_size_cfg: u16,
    pub(crate) frame_rate_cfg: u32,
}

pub(crate) struct DeviceState {
    pub(crate) device: virtio::Device,
    pub(crate) setup_vq: virtio::Virtqueue,
    pub(crate) render_vq: virtio::Virtqueue,
    pub(crate) irq_handle: sys::InterruptHandle,
}

/// Phase A: Receive device config from init, init virtio device.
pub(crate) fn phase_a(ch: &ipc::Channel) -> (virtio::Device, sys::InterruptHandle) {
    let mut msg = ipc::Message::new(0);
    if !ch.try_recv(&mut msg) || msg.msg_type != protocol::device::MSG_DEVICE_CONFIG {
        sys::print(b"metal-render: no device config message\n");
        sys::exit();
    }
    let dev_config = if let Some(protocol::device::Message::DeviceConfig(c)) =
        protocol::device::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"metal-render: bad device config\n");
        sys::exit();
    };

    // Map MMIO region.
    let page_offset = dev_config.mmio_pa & (ipc::PAGE_SIZE as u64 - 1);
    let page_pa = dev_config.mmio_pa & !(ipc::PAGE_SIZE as u64 - 1);
    let page_va = sys::device_map(page_pa, ipc::PAGE_SIZE as u64).unwrap_or_else(|_| {
        sys::print(b"metal-render: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Feature negotiation — accept VIRTIO_F_VERSION_1 only.
    device.reset();
    device.set_status(1); // ACKNOWLEDGE
    device.set_status(1 | 2); // ACKNOWLEDGE | DRIVER
    let _dev_features = device.read_device_features();
    device.write_driver_features(1u64 << 32); // VIRTIO_F_VERSION_1
    device.set_status(1 | 2 | 8); // FEATURES_OK
    if device.read_status() & 8 == 0 {
        sys::print(b"metal-render: FEATURES_OK not set\n");
        sys::exit();
    }

    // Register IRQ.
    let irq_handle: sys::InterruptHandle =
        sys::interrupt_register(dev_config.irq).unwrap_or_else(|_| {
            sys::print(b"metal-render: interrupt_register failed\n");
            sys::exit();
        });

    (device, irq_handle)
}

/// Set up virtqueues and mark device as ready.
pub(crate) fn setup_virtqueues(device: &virtio::Device) -> (virtio::Virtqueue, virtio::Virtqueue) {
    let setup_vq_size = core::cmp::min(
        device.queue_max_size(VIRTQ_SETUP),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let render_vq_size = core::cmp::min(
        device.queue_max_size(VIRTQ_RENDER),
        virtio::DEFAULT_QUEUE_SIZE,
    );

    let setup_vq = alloc_virtqueue(device, VIRTQ_SETUP, setup_vq_size);
    let render_vq = alloc_virtqueue(device, VIRTQ_RENDER, render_vq_size);

    device.driver_ok();
    sys::print(b"  \xF0\x9F\x94\xB1 metal-render: virtio device ready (2 queues)\n");

    (setup_vq, render_vq)
}

/// Phase B: Display query + init handshake.
pub(crate) fn phase_b(device: &virtio::Device, ch: &ipc::Channel) -> DisplayConfig {
    let disp_w = device.config_read32(0x00);
    let disp_h = device.config_read32(0x04);
    let disp_refresh = device.config_read32(0x08);
    let width = if disp_w > 0 { disp_w } else { 1024 };
    let height = if disp_h > 0 { disp_h } else { 768 };
    let refresh_rate = disp_refresh;

    sys::print(b"     display ");
    print_u32(width);
    sys::print(b"x");
    print_u32(height);
    sys::print(b"@");
    print_u32(refresh_rate);
    sys::print(b"Hz\n");

    // Send display info back to init.
    let info_msg = unsafe {
        ipc::Message::from_payload(
            MSG_DISPLAY_INFO,
            &DisplayInfoMsg {
                width,
                height,
                refresh_rate,
            },
        )
    };
    ch.send(&info_msg);
    let _ = sys::channel_signal(sys::ChannelHandle(INIT_HANDLE));

    // Wait for GPU config from init.
    sys::print(b"     waiting for gpu config\n");
    let mut msg = ipc::Message::new(0);
    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_GPU_CONFIG {
            break;
        }
    }
    // We use display dimensions from config space; decode to consume the message type safely.
    let _ = protocol::gpu::decode(msg.msg_type, &msg.payload);

    // Signal init that we're ready.
    sys::print(b"     handshake complete, sending GPU_READY\n");
    let ready_msg = ipc::Message::new(MSG_GPU_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(sys::ChannelHandle(INIT_HANDLE));

    DisplayConfig {
        width,
        height,
        refresh_rate,
    }
}

/// Phase C: Receive render config (compositor config).
pub(crate) fn phase_c(ch: &ipc::Channel) -> RenderConfig {
    sys::print(b"     waiting for render config\n");
    let mut scene_va: u64 = 0;
    let mut content_va: u64 = 0;
    let mut content_size: u32 = 0;
    let mut scale_factor: f32 = 1.0;
    let mut pointer_state_va: u64 = 0;
    let mut font_size_cfg: u16 = 18;
    let mut frame_rate_cfg: u32 = 60;

    let mut msg = ipc::Message::new(0);
    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_COMPOSITOR_CONFIG {
            if let Some(protocol::compose::Message::CompositorConfig(config)) =
                protocol::compose::decode(msg.msg_type, &msg.payload)
            {
                scene_va = config.scene_va;
                content_va = config.content_va;
                content_size = config.content_size;
                scale_factor = config.scale_factor;
                pointer_state_va = config.pointer_state_va;
                font_size_cfg = config.font_size;
                frame_rate_cfg = if config.frame_rate > 0 {
                    config.frame_rate as u32
                } else {
                    60
                };
                break;
            }
        }
    }

    sys::print(b"     render config: scene_va=");
    print_hex_u32((scene_va >> 32) as u32);
    print_hex_u32(scene_va as u32);
    sys::print(b" content_size=");
    print_u32(content_size);
    sys::print(b"\n");

    if scene_va == 0 {
        sys::print(b"metal-render: no scene_va, idling\n");
        loop {
            let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        }
    }

    RenderConfig {
        scene_va,
        content_va,
        content_size,
        scale_factor,
        pointer_state_va,
        font_size_cfg,
        frame_rate_cfg,
    }
}
