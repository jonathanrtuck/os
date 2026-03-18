//! Virgil3D render service — thick GPU driver.
//!
//! Initializes virtio-gpu in 3D mode (VIRTIO_GPU_F_VIRGL), creates a virgl
//! rendering context, sets up the Gallium3D pipeline (blend, DSA, rasterizer,
//! shaders, surface), and clears the screen to a solid color.
//!
//! Replaces the 2D virtio-gpu driver as a drop-in. Init spawns this for the
//! GPU device. Participates in the same IPC handshake (MSG_DEVICE_CONFIG,
//! MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_FB_PA_CHUNK, MSG_GPU_READY).
//!
//! The scene graph is the only interface — all rendering complexity is
//! internal to this driver (leaf node behind a simple boundary).

#![no_std]
#![no_main]

extern crate alloc;
extern crate scene;

use alloc::boxed::Box;

use protocol::{
    compose::{CompositorConfig, MSG_COMPOSITOR_CONFIG},
    device::{DeviceConfig, MSG_DEVICE_CONFIG},
    gpu::{
        DisplayInfoMsg, FbPaChunk, GpuConfig, MSG_DISPLAY_INFO, MSG_FB_PA_CHUNK, MSG_GPU_CONFIG,
        MSG_GPU_READY,
    },
    virgl::{
        self, PIPE_BUFFER, PIPE_PRIM_TRIANGLES, PIPE_SHADER_FRAGMENT, PIPE_SHADER_VERTEX,
        VIRGL_FORMAT_B8G8R8A8_UNORM, VIRGL_OBJECT_BLEND, VIRGL_OBJECT_DSA, VIRGL_OBJECT_RASTERIZER,
        VIRGL_OBJECT_VERTEX_ELEMENTS, VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE,
        VIRTIO_GPU_CMD_CTX_CREATE, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
        VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_CREATE_3D,
        VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_SUBMIT_3D,
        VIRTIO_GPU_RESP_OK_DISPLAY_INFO, VIRTIO_GPU_RESP_OK_NODATA,
    },
};

#[path = "scene_walk.rs"]
mod scene_walk;
#[path = "shaders.rs"]
mod shaders;

// ── Constants ────────────────────────────────────────────────────────────

/// Control virtqueue index.
const VIRTQ_CONTROL: u32 = 0;

/// VIRTIO_GPU_F_VIRGL feature bit (bit 0 of device features).
const VIRTIO_GPU_F_VIRGL: u64 = 1 << 0;

/// Resource IDs and context IDs (arbitrary nonzero).
const VIRGL_CTX_ID: u32 = 1;
const RT_RESOURCE_ID: u32 = 1;

/// Scanout index (first/only display).
const SCANOUT_ID: u32 = 0;

/// Handle indices for IPC channels.
const INIT_HANDLE: u8 = 0;
/// Handle for the core→virgil-render scene update channel.
const SCENE_HANDLE: u8 = 1;

/// Virgl object handles (assigned by us, must be nonzero).
const HANDLE_BLEND: u32 = 1;
const HANDLE_DSA: u32 = 2;
const HANDLE_RASTERIZER: u32 = 3;
const HANDLE_SURFACE: u32 = 4;
const HANDLE_VS: u32 = 5;
const HANDLE_FS: u32 = 6;
const HANDLE_VE: u32 = 7; // vertex elements layout
/// Resource ID for the vertex buffer (PIPE_BUFFER).
const VB_RESOURCE_ID: u32 = 2;

// ── Wire-format structs ──────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct CtrlHeader {
    cmd_type: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    _padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayInfo {
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    enabled: u32,
    flags: u32,
}

#[repr(C)]
struct CtxCreate {
    hdr: CtrlHeader,
    nlen: u32,
    context_init: u32,
    debug_name: [u8; 64],
}

#[repr(C)]
struct ResourceCreate3d {
    hdr: CtrlHeader,
    resource_id: u32,
    target: u32,
    format: u32,
    bind: u32,
    width: u32,
    height: u32,
    depth: u32,
    array_size: u32,
    last_level: u32,
    nr_samples: u32,
    flags: u32,
    _pad: u32,
}

#[repr(C)]
struct AttachBacking {
    hdr: CtrlHeader,
    resource_id: u32,
    nr_entries: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    _padding: u32,
}

#[repr(C)]
struct CtxResource {
    hdr: CtrlHeader,
    resource_id: u32,
    _pad: [u32; 3],
}

#[repr(C)]
struct SetScanout {
    hdr: CtrlHeader,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
struct ResourceFlush {
    hdr: CtrlHeader,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    resource_id: u32,
    _padding: u32,
}

#[repr(C)]
struct Submit3dHeader {
    hdr: CtrlHeader,
    size: u32,
    _pad: u32,
}

// ── DMA buffer helper ────────────────────────────────────────────────────

struct DmaBuf {
    va: usize,
    pa: u64,
    order: u32,
}

impl DmaBuf {
    fn alloc(order: u32) -> DmaBuf {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
            sys::print(b"virgil-render: dma_alloc failed\n");
            sys::exit();
        });
        // SAFETY: va is valid DMA memory of (1 << order) pages, freshly allocated.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * 4096) };
        DmaBuf { va, pa, order }
    }

    fn free(self) {
        let _ = sys::dma_free(self.va as u64, self.order);
    }
}

// ── Helper functions ─────────────────────────────────────────────────────

fn ctrl_header(cmd_type: u32) -> CtrlHeader {
    CtrlHeader {
        cmd_type,
        flags: 0,
        fence_id: 0,
        ctx_id: 0,
        _padding: 0,
    }
}

fn ctrl_header_ctx(cmd_type: u32) -> CtrlHeader {
    CtrlHeader {
        cmd_type,
        flags: 0,
        fence_id: 0,
        ctx_id: VIRGL_CTX_ID,
        _padding: 0,
    }
}

/// Compute the base VA of channel N's shared pages.
fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

/// Send a GPU command via the control virtqueue and wait for the response.
/// Returns the response cmd_type.
fn gpu_command(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    cmd_pa: u64,
    cmd_len: u32,
    resp_pa: u64,
    resp_va: usize,
    resp_len: u32,
) -> u32 {
    vq.push_chain(&[(cmd_pa, cmd_len, false), (resp_pa, resp_len, true)]);
    device.notify(VIRTQ_CONTROL);
    let _ = sys::wait(&[irq_handle], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    let _ = sys::interrupt_ack(irq_handle);
    // SAFETY: resp_va points to valid DMA memory containing a CtrlHeader written by device.
    let resp_header = resp_va as *const CtrlHeader;
    unsafe { core::ptr::read_volatile(&(*resp_header).cmd_type as *const u32) }
}

/// Send a simple command (header only, no extra payload) and check for RESP_OK_NODATA.
fn gpu_cmd_ok(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    cmd: &DmaBuf,
    cmd_len: u32,
) -> bool {
    let resp_pa = cmd.pa + 2048;
    let resp_va = cmd.va + 2048;
    let resp_type = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        cmd_len,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
    resp_type == VIRTIO_GPU_RESP_OK_NODATA
}

fn format_u32(mut n: u32, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut i = 10;
    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let len = 10 - i;
    buf[..len].copy_from_slice(&tmp[i..]);
    len
}

fn print_u32(mut n: u32) {
    if n == 0 {
        sys::print(b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 10;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::print(&buf[i..]);
}

fn print_hex_u32(val: u32) {
    let mut buf = [0u8; 10];
    buf[0] = b'0';
    buf[1] = b'x';
    let hex = b"0123456789abcdef";
    for i in 0..8 {
        let nibble = ((val >> (28 - i * 4)) & 0xF) as usize;
        buf[2 + i] = hex[nibble];
    }
    sys::print(&buf[..10]);
}

// ── Phase A: Device initialization ───────────────────────────────────────

fn init_device(mmio_pa: u64, irq: u32) -> (virtio::Device, virtio::Virtqueue, u8) {
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

fn get_display_info(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
) -> (u32, u32) {
    let cmd = DmaBuf::alloc(0);
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
    let resp_type = gpu_command(
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

fn init_handshake(
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
        pos += format_u32(width, &mut buf[pos..]);
        buf[pos] = b'x';
        pos += 1;
        pos += format_u32(height, &mut buf[pos..]);
        buf[pos] = b'\n';
        pos += 1;
        sys::print(&buf[..pos]);
    }

    // Send display info back to init.
    let info_msg =
        // SAFETY: DisplayInfoMsg is repr(C) and fits in payload.
        unsafe { ipc::Message::from_payload(MSG_DISPLAY_INFO, &DisplayInfoMsg { width, height }) };
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
    let chunks_per_buf = config.chunks_per_buf as usize;

    // Drain FB PA chunk messages (we don't use them for virgl, but init sends them).
    let total_entries = chunks_per_buf * 2;
    let mut received = 0;
    while received < total_entries {
        let mut got_any = false;
        while received < total_entries && ch.try_recv(&mut msg) {
            got_any = true;
            if msg.msg_type == MSG_FB_PA_CHUNK {
                // SAFETY: msg payload contains a valid FbPaChunk.
                let chunk: FbPaChunk = unsafe { msg.payload_as() };
                let count = (chunk.count as usize).min(6);
                received += count;
            }
        }
        if received < total_entries && !got_any {
            let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        }
    }

    // Signal init that we're ready.
    sys::print(b"     handshake complete, sending GPU_READY\n");
    let ready_msg = ipc::Message::new(MSG_GPU_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(INIT_HANDLE);

    (cfg_width, cfg_height)
}

// ── Phase C: Virgl 3D initialization ─────────────────────────────────────

fn ctx_create(device: &virtio::Device, vq: &mut virtio::Virtqueue, irq_handle: u8) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page, writing CtxCreate at start.
    unsafe {
        core::ptr::write(
            cmd.va as *mut CtxCreate,
            CtxCreate {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_CTX_CREATE),
                nlen: 0,
                context_init: 0,
                debug_name: [0u8; 64],
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<CtxCreate>() as u32,
    ) {
        sys::print(b"virgil-render: CTX_CREATE failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     virgl context created\n");
}

fn resource_create_3d(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page, writing ResourceCreate3d.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceCreate3d,
            ResourceCreate3d {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_CREATE_3D),
                resource_id: RT_RESOURCE_ID,
                target: 2, // PIPE_TEXTURE_2D
                format: VIRGL_FORMAT_B8G8R8A8_UNORM,
                bind: 2, // PIPE_BIND_RENDER_TARGET
                width,
                height,
                depth: 1,
                array_size: 1,
                last_level: 0,
                nr_samples: 0,
                flags: 0,
                _pad: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<ResourceCreate3d>() as u32,
    ) {
        sys::print(b"virgil-render: RESOURCE_CREATE_3D failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     3D render target created\n");
}

fn attach_backing(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    let fb_bytes = (width as usize) * (height as usize) * 4;
    // Allocate backing pages as individual order-0 DMA pages.
    // We use a single scatter-gather list.
    // For simplicity, allocate in 256 KiB chunks (order 6 = 64 pages).
    const CHUNK_ORDER: u32 = 6;
    const CHUNK_PAGES: usize = 1 << CHUNK_ORDER;
    const CHUNK_BYTES: usize = CHUNK_PAGES * 4096;
    let chunks_needed = (fb_bytes + CHUNK_BYTES - 1) / CHUNK_BYTES;

    // Allocate the command DMA buffer (needs space for header + entries).
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();
    let total_cmd_bytes = header_size + chunks_needed * entry_size;
    let cmd_pages = (total_cmd_bytes + 4095) / 4096;
    let cmd_order = (cmd_pages.next_power_of_two().trailing_zeros()) as u32;
    let cmd = DmaBuf::alloc(cmd_order);

    // Allocate backing DMA memory and build scatter-gather entries.
    let ptr = cmd.va as *mut u8;
    for i in 0..chunks_needed {
        let mut chunk_pa: u64 = 0;
        let chunk_va = sys::dma_alloc(CHUNK_ORDER, &mut chunk_pa).unwrap_or_else(|_| {
            sys::print(b"virgil-render: dma_alloc (backing) failed\n");
            sys::exit();
        });
        // SAFETY: chunk_va is valid DMA of CHUNK_BYTES, zero it.
        unsafe { core::ptr::write_bytes(chunk_va as *mut u8, 0, CHUNK_BYTES) };
        // SAFETY: writing MemEntry into the command buffer at the right offset.
        unsafe {
            core::ptr::write(
                ptr.add(header_size + i * entry_size) as *mut MemEntry,
                MemEntry {
                    addr: chunk_pa,
                    length: CHUNK_BYTES as u32,
                    _padding: 0,
                },
            );
        }
    }

    // Write the attach backing header.
    // SAFETY: ptr points to start of zeroed DMA buffer, writing AttachBacking.
    unsafe {
        core::ptr::write(
            ptr as *mut AttachBacking,
            AttachBacking {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
                resource_id: RT_RESOURCE_ID,
                nr_entries: chunks_needed as u32,
            },
        );
    }

    // Response goes after command data, page-aligned.
    let resp_offset = ((total_cmd_bytes + 4095) / 4096) * 4096;
    let (resp_pa, resp_va, resp_buf) = if resp_offset + 64 <= (1 << cmd_order) * 4096 {
        (cmd.pa + resp_offset as u64, cmd.va + resp_offset, None)
    } else {
        let rb = DmaBuf::alloc(0);
        (rb.pa, rb.va, Some(rb))
    };

    let resp_type = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        total_cmd_bytes as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );

    if resp_type != VIRTIO_GPU_RESP_OK_NODATA {
        sys::print(b"virgil-render: RESOURCE_ATTACH_BACKING failed (resp=");
        print_hex_u32(resp_type);
        sys::print(b")\n");
        sys::exit();
    }

    if let Some(rb) = resp_buf {
        rb.free();
    }
    cmd.free();

    sys::print(b"     backing attached (");
    print_u32(chunks_needed as u32);
    sys::print(b" chunks)\n");
}

fn ctx_attach_resource(device: &virtio::Device, vq: &mut virtio::Virtqueue, irq_handle: u8) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut CtxResource,
            CtxResource {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE),
                resource_id: RT_RESOURCE_ID,
                _pad: [0; 3],
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<CtxResource>() as u32,
    ) {
        sys::print(b"virgil-render: CTX_ATTACH_RESOURCE failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     resource attached to context\n");
}

fn set_scanout(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut SetScanout,
            SetScanout {
                hdr: ctrl_header(VIRTIO_GPU_CMD_SET_SCANOUT),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                scanout_id: SCANOUT_ID,
                resource_id: RT_RESOURCE_ID,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<SetScanout>() as u32,
    ) {
        sys::print(b"virgil-render: SET_SCANOUT failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     scanout bound to render target\n");
}

// ── Vertex buffer resource ────────────────────────────────────────────────

/// Create a PIPE_BUFFER resource for vertex data.
fn resource_create_vbo(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    size_bytes: u32,
) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page, writing ResourceCreate3d.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceCreate3d,
            ResourceCreate3d {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_CREATE_3D),
                resource_id: VB_RESOURCE_ID,
                target: PIPE_BUFFER,
                format: VIRGL_FORMAT_B8G8R8A8_UNORM, // format doesn't matter for buffers
                bind: 0x10,                          // PIPE_BIND_VERTEX_BUFFER
                width: size_bytes,
                height: 1,
                depth: 1,
                array_size: 1,
                last_level: 0,
                nr_samples: 0,
                flags: 0,
                _pad: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<ResourceCreate3d>() as u32,
    ) {
        sys::print(b"virgil-render: RESOURCE_CREATE_3D (VBO) failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     vertex buffer resource created\n");
}

/// Attach backing memory for the vertex buffer resource.
fn attach_backing_vbo(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    size_bytes: u32,
) {
    let cmd = DmaBuf::alloc(0);
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();

    // Allocate DMA pages for VBO backing.
    let vbo_pages = ((size_bytes as usize) + 4095) / 4096;
    let vbo_order = (vbo_pages.next_power_of_two().trailing_zeros()) as u32;
    let mut vbo_pa: u64 = 0;
    let vbo_va = sys::dma_alloc(vbo_order, &mut vbo_pa).unwrap_or_else(|_| {
        sys::print(b"virgil-render: dma_alloc (vbo backing) failed\n");
        sys::exit();
    });
    // SAFETY: vbo_va is valid DMA memory, zero it.
    unsafe { core::ptr::write_bytes(vbo_va as *mut u8, 0, (1usize << vbo_order) * 4096) };

    // Write attach backing header + one entry.
    let ptr = cmd.va as *mut u8;
    // SAFETY: writing into zeroed DMA page at correct offsets.
    unsafe {
        core::ptr::write(
            ptr as *mut AttachBacking,
            AttachBacking {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
                resource_id: VB_RESOURCE_ID,
                nr_entries: 1,
            },
        );
        core::ptr::write(
            ptr.add(header_size) as *mut MemEntry,
            MemEntry {
                addr: vbo_pa,
                length: (1u32 << vbo_order) * 4096,
                _padding: 0,
            },
        );
    }

    let total_cmd_bytes = header_size + entry_size;
    if !gpu_cmd_ok(device, vq, irq_handle, &cmd, total_cmd_bytes as u32) {
        sys::print(b"virgil-render: RESOURCE_ATTACH_BACKING (VBO) failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     VBO backing attached\n");
}

/// Attach VBO resource to the virgl context.
fn ctx_attach_vbo(device: &virtio::Device, vq: &mut virtio::Virtqueue, irq_handle: u8) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut CtxResource,
            CtxResource {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE),
                resource_id: VB_RESOURCE_ID,
                _pad: [0; 3],
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<CtxResource>() as u32,
    ) {
        sys::print(b"virgil-render: CTX_ATTACH_RESOURCE (VBO) failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     VBO attached to context\n");
}

/// Flush the render target to the display.
fn flush_resource(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceFlush,
            ResourceFlush {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_FLUSH),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                resource_id: RT_RESOURCE_ID,
                _padding: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<ResourceFlush>() as u32,
    ) {
        sys::print(b"virgil-render: RESOURCE_FLUSH failed\n");
    }
    cmd.free();
}

// ── Phase D: GPU pipeline setup via CMD_SUBMIT_3D ────────────────────────

fn submit_3d(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    cmdbuf: &virgl::CommandBuffer,
) -> bool {
    let data = cmdbuf.as_dwords();
    let data_bytes = cmdbuf.size_bytes();
    let header_size = core::mem::size_of::<Submit3dHeader>();
    let total_cmd_bytes = header_size + data_bytes as usize;

    // Allocate DMA buffer large enough for header + command data + response.
    let total_with_resp = total_cmd_bytes + 4096; // leave room for response
    let cmd_pages = (total_with_resp + 4095) / 4096;
    let cmd_order = (cmd_pages.next_power_of_two().trailing_zeros()) as u32;
    let cmd = DmaBuf::alloc(cmd_order);

    // Write Submit3dHeader.
    // SAFETY: cmd.va points to zeroed DMA memory, writing header at start.
    unsafe {
        core::ptr::write(
            cmd.va as *mut Submit3dHeader,
            Submit3dHeader {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_SUBMIT_3D),
                size: data_bytes,
                _pad: 0,
            },
        );
    }

    // Copy command buffer data after the header.
    // SAFETY: data is a valid u32 slice, destination is within DMA allocation.
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr() as *const u8,
            (cmd.va + header_size) as *mut u8,
            data_bytes as usize,
        );
    }

    // Response at page-aligned offset after command data.
    let resp_offset = ((total_cmd_bytes + 4095) / 4096) * 4096;
    let resp_pa = cmd.pa + resp_offset as u64;
    let resp_va = cmd.va + resp_offset;

    let resp_type = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        total_cmd_bytes as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );

    let ok = resp_type == VIRTIO_GPU_RESP_OK_NODATA;
    if !ok {
        sys::print(b"virgil-render: SUBMIT_3D failed (resp=");
        print_hex_u32(resp_type);
        sys::print(b")\n");
    }
    cmd.free();
    ok
}

fn setup_pipeline(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    // Heap-allocate the CommandBuffer (16 KiB — must not go on 16 KiB stack).
    let mut cmdbuf = Box::new(virgl::CommandBuffer::new());

    // Create pipeline state objects.
    cmdbuf.cmd_create_blend(HANDLE_BLEND);
    cmdbuf.cmd_create_dsa(HANDLE_DSA);
    cmdbuf.cmd_create_rasterizer(HANDLE_RASTERIZER, true);

    // Create surface wrapping our render target resource.
    cmdbuf.cmd_create_surface(HANDLE_SURFACE, RT_RESOURCE_ID, VIRGL_FORMAT_B8G8R8A8_UNORM);

    // Shaders and vertex elements are not needed for scissor+clear rendering.
    // They will be added when VBO-based triangle rendering is implemented.

    // Bind pipeline state.
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_RASTERIZER, HANDLE_RASTERIZER);

    // Set framebuffer and viewport.
    cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, 0);
    cmdbuf.cmd_set_viewport(width as f32, height as f32);

    if cmdbuf.overflowed() {
        sys::print(b"virgil-render: pipeline command buffer overflowed!\n");
        sys::exit();
    }

    sys::print(b"     submitting pipeline setup (");
    print_u32(cmdbuf.size_bytes());
    sys::print(b" bytes)\n");

    if !submit_3d(device, vq, irq_handle, &cmdbuf) {
        sys::print(b"virgil-render: pipeline setup SUBMIT_3D failed\n");
        sys::exit();
    }
    sys::print(b"     pipeline setup complete\n");
}

// ── Phase E: Clear screen + flush ────────────────────────────────────────

fn clear_screen(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    // Clear to OS theme dark background.
    let mut cmdbuf = Box::new(virgl::CommandBuffer::new());
    cmdbuf.cmd_clear(0.13, 0.13, 0.16, 1.0);

    if !submit_3d(device, vq, irq_handle, &cmdbuf) {
        sys::print(b"virgil-render: clear SUBMIT_3D failed\n");
        sys::exit();
    }
    sys::print(b"     clear submitted\n");

    // Flush the render target to display.
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceFlush,
            ResourceFlush {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_FLUSH),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                resource_id: RT_RESOURCE_ID,
                _padding: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<ResourceFlush>() as u32,
    ) {
        sys::print(b"virgil-render: RESOURCE_FLUSH failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     flush complete \xe2\x80\x94 pixels on screen\n");
}

// ── Entry point ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render - starting\n");

    // ── Phase A: Receive device config from init, init virtio device ─────
    // SAFETY: Channel 0 shared memory is mapped by kernel before process start.
    let ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);
    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virgil-render: no device config message\n");
        sys::exit();
    }
    // SAFETY: msg payload contains a valid DeviceConfig from init.
    let dev_config: DeviceConfig = unsafe { msg.payload_as() };
    let (device, mut vq, irq_handle) = init_device(dev_config.mmio_pa, dev_config.irq);

    // ── Phase B: Display query + init handshake ──────────────────────────
    let (width, height) = init_handshake(&device, &mut vq, irq_handle, &ch);

    sys::print(b"     render target: ");
    print_u32(width);
    sys::print(b"x");
    print_u32(height);
    sys::print(b"\n");

    // ── Phase C: Virgl 3D initialization ─────────────────────────────────
    ctx_create(&device, &mut vq, irq_handle);
    resource_create_3d(&device, &mut vq, irq_handle, width, height);
    attach_backing(&device, &mut vq, irq_handle, width, height);
    ctx_attach_resource(&device, &mut vq, irq_handle);
    set_scanout(&device, &mut vq, irq_handle, width, height);

    // ── Phase D: GPU pipeline setup ──────────────────────────────────────
    // No VBO needed — we render rectangles using scissor+clear, which is
    // simpler and avoids RESOURCE_INLINE_WRITE issues with virglrenderer.
    setup_pipeline(&device, &mut vq, irq_handle, width, height);

    // ── Phase E: Clear screen + flush ────────────────────────────────────
    clear_screen(&device, &mut vq, irq_handle, width, height);

    // ── Phase F: Receive render config + scene graph render loop ─────────
    //
    // Read compositor config from init (sent on the init channel).
    // Contains scene_va, font_va, font_len, scale_factor.
    sys::print(b"     waiting for render config\n");

    let mut scene_va: u64 = 0;
    let mut scale_factor: f32 = 1.0;

    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_COMPOSITOR_CONFIG {
            // SAFETY: msg payload is a valid CompositorConfig from init.
            let config: CompositorConfig = unsafe { msg.payload_as() };
            scene_va = config.scene_va;
            scale_factor = config.scale_factor;

            sys::print(b"     render config: scene_va=");
            print_hex_u32((scene_va >> 32) as u32);
            print_hex_u32(scene_va as u32);
            sys::print(b" scale=");
            print_u32((scale_factor * 100.0) as u32);
            sys::print(b"%\n");
            break;
        }
    }

    if scene_va == 0 {
        sys::print(b"virgil-render: no scene_va in config, idling\n");
        loop {
            let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        }
    }

    // Scene graph shared memory: TripleReader reads from this.
    let scene_buf = unsafe {
        // SAFETY: scene_va is mapped into our address space by init via
        // memory_share before process start. Size is TRIPLE_SCENE_SIZE.
        core::slice::from_raw_parts(scene_va as *const u8, scene::TRIPLE_SCENE_SIZE)
    };

    // Heap-allocate the quad batch for scene walk results.
    let mut batch = Box::new(scene_walk::QuadBatch::new());
    // Heap-allocate the command buffer for per-frame rendering (16 KiB).
    let mut cmdbuf = Box::new(virgl::CommandBuffer::new());

    let mut last_gen: u32 = 0;
    let mut frame_count: u32 = 0;

    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render: render loop starting\n");

    loop {
        // Wait for scene update signal from core (handle 1).
        let _ = sys::wait(&[SCENE_HANDLE], u64::MAX);

        // Drain scene update messages (we only care that there's a new frame).
        {
            // SAFETY: Channel 1 shared memory was set up by init before start.
            let scene_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
            let mut drain_msg = ipc::Message::new(0);
            while scene_ch.try_recv(&mut drain_msg) {}
        }

        // Read the latest scene graph.
        let reader = scene::TripleReader::new(scene_buf);
        let nodes = reader.front_nodes();
        let gen = reader.front_generation();

        // Skip if generation hasn't changed.
        if gen == last_gen && frame_count > 0 {
            reader.finish_read(gen);
            continue;
        }
        last_gen = gen;

        // Walk scene tree and accumulate colored quads.
        let root = reader.front_root();
        scene_walk::walk_scene(nodes, root, scale_factor, width, height, &mut batch);

        // Debug: log frame info for first few frames.
        if frame_count < 3 {
            sys::print(b"     frame ");
            print_u32(frame_count);
            sys::print(b": gen=");
            print_u32(gen);
            sys::print(b" root=");
            print_u32(root as u32);
            sys::print(b" nodes=");
            print_u32(nodes.len() as u32);
            sys::print(b" quads=");
            print_u32(batch.quads().len() as u32);
            sys::print(b"\n");

            // Log first few quads' colors.
            for (i, q) in batch.quads().iter().take(5).enumerate() {
                sys::print(b"       q");
                print_u32(i as u32);
                sys::print(b": (");
                print_u32(q.x as u32);
                sys::print(b",");
                print_u32(q.y as u32);
                sys::print(b" ");
                print_u32(q.w as u32);
                sys::print(b"x");
                print_u32(q.h as u32);
                sys::print(b") rgba=(");
                print_u32((q.r * 255.0) as u32);
                sys::print(b",");
                print_u32((q.g * 255.0) as u32);
                sys::print(b",");
                print_u32((q.b * 255.0) as u32);
                sys::print(b",");
                print_u32((q.a * 255.0) as u32);
                sys::print(b")\n");
            }
        }

        // TODO: render quads via VBO triangle draw (scissor+clear doesn't work —
        // virglrenderer's CLEAR ignores scissor state).
        // For now, just clear to the scene's background color.
        cmdbuf.clear();
        if let Some(q) = batch.quads().first() {
            cmdbuf.cmd_clear(q.r, q.g, q.b, q.a);
        } else {
            cmdbuf.cmd_clear(0.13, 0.13, 0.16, 1.0);
        }

        if cmdbuf.overflowed() {
            sys::print(b"virgil-render: frame command buffer overflowed!\n");
        } else {
            submit_3d(&device, &mut vq, irq_handle, &cmdbuf);
            flush_resource(&device, &mut vq, irq_handle, width, height);
        }

        reader.finish_read(gen);
        frame_count += 1;
    }
}
