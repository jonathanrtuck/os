//! Virgil3D render service — thick GPU driver.
//!
//! Initializes virtio-gpu in 3D mode (VIRTIO_GPU_F_VIRGL), creates a virgl
//! rendering context, sets up the Gallium3D pipeline (blend, DSA, rasterizer,
//! shaders, surface), and clears the screen to a solid color.
//!
//! Replaces the 2D virtio-gpu driver as a drop-in. Init spawns this for the
//! GPU device. Participates in the same IPC handshake (MSG_DEVICE_CONFIG,
//! MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY).
//!
//! The scene graph is the only interface — all rendering complexity is
//! internal to this driver (leaf node behind a simple boundary).

#![no_std]
#![no_main]

extern crate alloc;
extern crate fonts;
extern crate scene;

use alloc::boxed::Box;

use protocol::{
    compose::{CompositorConfig, MSG_COMPOSITOR_CONFIG},
    device::{DeviceConfig, MSG_DEVICE_CONFIG},
    gpu::{DisplayInfoMsg, GpuConfig, MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY},
    virgl::{
        self, PIPE_BUFFER, PIPE_PRIM_TRIANGLES, PIPE_SHADER_FRAGMENT, PIPE_SHADER_VERTEX,
        PIPE_TEXTURE_2D, VIRGL_FORMAT_B8G8R8A8_UNORM, VIRGL_FORMAT_R8_UNORM, VIRGL_FORMAT_S8_UINT,
        VIRGL_OBJECT_BLEND, VIRGL_OBJECT_DSA, VIRGL_OBJECT_RASTERIZER,
        VIRGL_OBJECT_VERTEX_ELEMENTS, VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE,
        VIRTIO_GPU_CMD_CTX_CREATE, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
        VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_CREATE_3D,
        VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_SUBMIT_3D,
        VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D, VIRTIO_GPU_RESP_OK_DISPLAY_INFO,
        VIRTIO_GPU_RESP_OK_NODATA,
    },
};

#[path = "atlas.rs"]
mod atlas;
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
const HANDLE_VE: u32 = 7; // vertex elements layout (color)
/// Textured pipeline handles (for glyph rendering).
const HANDLE_VE_TEXTURED: u32 = 8;
const HANDLE_VS_TEXTURED: u32 = 9;
const HANDLE_FS_GLYPH: u32 = 10;
const HANDLE_SAMPLER: u32 = 11;
const HANDLE_SAMPLER_VIEW: u32 = 12;
/// Image pipeline handles.
const HANDLE_FS_IMAGE: u32 = 13;
const HANDLE_SAMPLER_VIEW_IMG: u32 = 14;
/// Stencil-then-cover pipeline handles.
const HANDLE_DSA_STENCIL_WRITE: u32 = 15;
const HANDLE_DSA_STENCIL_TEST: u32 = 16;
const HANDLE_BLEND_NO_COLOR: u32 = 17;
const HANDLE_STENCIL_SURFACE: u32 = 18;

/// Resource ID for the vertex buffer (PIPE_BUFFER).
const VB_RESOURCE_ID: u32 = 2;
/// Resource ID for the glyph atlas texture (R8_UNORM).
const ATLAS_RESOURCE_ID: u32 = 3;
/// Resource ID for the textured vertex buffer (PIPE_BUFFER).
const TEXT_VB_RESOURCE_ID: u32 = 4;
/// Resource ID for the image texture (B8G8R8A8_UNORM).
const IMG_RESOURCE_ID: u32 = 5;
/// Resource ID for the depth/stencil surface (Z24_S8).
const STENCIL_RESOURCE_ID: u32 = 6;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Heap-allocate a zeroed `T` via `alloc_zeroed`, aborting on null.
///
/// Many types in this driver exceed the 16 KiB user stack, so `Box::new()`
/// cannot be used.  `alloc_zeroed` produces valid initial state (all zeros)
/// and this helper adds the null check that bare `Box::from_raw` would skip.
fn box_zeroed<T>() -> Box<T> {
    unsafe {
        let ptr = alloc::alloc::alloc_zeroed(alloc::alloc::Layout::new::<T>());
        if ptr.is_null() {
            sys::print(b"FATAL: alloc_zeroed returned null\n");
            sys::exit();
        }
        Box::from_raw(ptr as *mut T)
    }
}

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
struct TransferToHost3d {
    hdr: CtrlHeader,
    box_x: u32,
    box_y: u32,
    box_z: u32,
    box_w: u32,
    box_h: u32,
    box_d: u32,
    offset: u64,
    resource_id: u32,
    level: u32,
    stride: u32,
    layer_stride: u32,
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

    fn free(&mut self) {
        if self.va != 0 {
            let _ = sys::dma_free(self.va as u64, self.order);
            self.va = 0;
        }
    }
}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        self.free();
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

fn print_u32(n: u32) {
    sys::print_u32(n);
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
    // Signal init that we're ready.
    sys::print(b"     handshake complete, sending GPU_READY\n");
    let ready_msg = ipc::Message::new(MSG_GPU_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(INIT_HANDLE);

    (cfg_width, cfg_height)
}

// ── Phase C: Virgl 3D initialization ─────────────────────────────────────

fn ctx_create(device: &virtio::Device, vq: &mut virtio::Virtqueue, irq_handle: u8) {
    let mut cmd = DmaBuf::alloc(0);
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
    let mut cmd = DmaBuf::alloc(0);
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
    let mut cmd = DmaBuf::alloc(cmd_order);

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
        let mut rb = DmaBuf::alloc(0);
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

    if let Some(mut rb) = resp_buf {
        rb.free();
    }
    cmd.free();

    sys::print(b"     backing attached (");
    print_u32(chunks_needed as u32);
    sys::print(b" chunks)\n");
}

fn ctx_attach_resource(device: &virtio::Device, vq: &mut virtio::Virtqueue, irq_handle: u8) {
    let mut cmd = DmaBuf::alloc(0);
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
    let mut cmd = DmaBuf::alloc(0);
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
    let mut cmd = DmaBuf::alloc(0);
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
/// Returns (va, pa, order) of the backing DMA pages for direct writes.
fn attach_backing_vbo(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    size_bytes: u32,
) -> (usize, u64, u32) {
    let mut cmd = DmaBuf::alloc(0);
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
    (vbo_va, vbo_pa, vbo_order)
}

/// Transfer vertex data from guest DMA memory to host resource.
fn transfer_vbo_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    data_bytes: u32,
) {
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut TransferToHost3d,
            TransferToHost3d {
                hdr: ctrl_header_ctx(protocol::virgl::VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D),
                box_x: 0,
                box_y: 0,
                box_z: 0,
                box_w: data_bytes,
                box_h: 1,
                box_d: 1,
                offset: 0,
                resource_id: VB_RESOURCE_ID,
                level: 0,
                stride: 0,
                layer_stride: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<TransferToHost3d>() as u32,
    ) {
        sys::print(b"virgil-render: TRANSFER_TO_HOST_3D (VBO) failed\n");
    }
    cmd.free();
}

/// Attach VBO resource to the virgl context.
fn ctx_attach_vbo(device: &virtio::Device, vq: &mut virtio::Virtqueue, irq_handle: u8) {
    let mut cmd = DmaBuf::alloc(0);
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

/// Create a 3D resource with given parameters.
fn resource_create_3d_generic(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    target: u32,
    format: u32,
    bind: u32,
    width: u32,
    height: u32,
) {
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page, writing ResourceCreate3d.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceCreate3d,
            ResourceCreate3d {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_CREATE_3D),
                resource_id,
                target,
                format,
                bind,
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
        sys::print(b"virgil-render: RESOURCE_CREATE_3D (generic) failed\n");
        sys::exit();
    }
    cmd.free();
}

/// Attach backing memory and context-attach a resource. Returns (va, pa, order).
fn attach_and_ctx_resource(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    size_bytes: u32,
) -> (usize, u64, u32) {
    let mut cmd = DmaBuf::alloc(0);
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();

    let pages = ((size_bytes as usize) + 4095) / 4096;
    let order = (pages.next_power_of_two().trailing_zeros()) as u32;
    let mut pa: u64 = 0;
    let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
        sys::print(b"virgil-render: dma_alloc (resource backing) failed\n");
        sys::exit();
    });
    // SAFETY: va is valid DMA memory of (1 << order) pages.
    unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * 4096) };

    let ptr = cmd.va as *mut u8;
    // SAFETY: writing into zeroed DMA page at correct offsets.
    unsafe {
        core::ptr::write(
            ptr as *mut AttachBacking,
            AttachBacking {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
                resource_id,
                nr_entries: 1,
            },
        );
        core::ptr::write(
            ptr.add(header_size) as *mut MemEntry,
            MemEntry {
                addr: pa,
                length: (1u32 << order) * 4096,
                _padding: 0,
            },
        );
    }
    let total_cmd_bytes = header_size + entry_size;
    if !gpu_cmd_ok(device, vq, irq_handle, &cmd, total_cmd_bytes as u32) {
        sys::print(b"virgil-render: RESOURCE_ATTACH_BACKING (generic) failed\n");
        sys::exit();
    }
    cmd.free();

    // Context attach.
    let mut cmd2 = DmaBuf::alloc(0);
    // SAFETY: cmd2.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd2.va as *mut CtxResource,
            CtxResource {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE),
                resource_id,
                _pad: [0; 3],
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd2,
        core::mem::size_of::<CtxResource>() as u32,
    ) {
        sys::print(b"virgil-render: CTX_ATTACH_RESOURCE (generic) failed\n");
        sys::exit();
    }
    cmd2.free();
    (va, pa, order)
}

/// Transfer a texture region to host (for atlas uploads).
fn transfer_texture_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    width: u32,
    height: u32,
    stride: u32,
) {
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut TransferToHost3d,
            TransferToHost3d {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D),
                box_x: 0,
                box_y: 0,
                box_z: 0,
                box_w: width,
                box_h: height,
                box_d: 1,
                offset: 0,
                resource_id,
                level: 0,
                stride,
                layer_stride: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<TransferToHost3d>() as u32,
    ) {
        sys::print(b"virgil-render: TRANSFER_TO_HOST_3D (texture) failed\n");
    }
    cmd.free();
}

/// Transfer buffer data to host (for VBO uploads).
fn transfer_buffer_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    data_bytes: u32,
) {
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut TransferToHost3d,
            TransferToHost3d {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D),
                box_x: 0,
                box_y: 0,
                box_z: 0,
                box_w: data_bytes,
                box_h: 1,
                box_d: 1,
                offset: 0,
                resource_id,
                level: 0,
                stride: 0,
                layer_stride: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<TransferToHost3d>() as u32,
    ) {
        sys::print(b"virgil-render: TRANSFER_TO_HOST_3D (buffer) failed\n");
    }
    cmd.free();
}

/// Flush the render target to the display.
fn flush_resource(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) {
    let mut cmd = DmaBuf::alloc(0);
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
    let mut cmd = DmaBuf::alloc(cmd_order);

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

/// Set up the GPU pipeline. Returns true if stencil-then-cover is available.
fn setup_pipeline(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    width: u32,
    height: u32,
) -> bool {
    // Heap-allocate the CommandBuffer (16 KiB — same size as user stack).
    let mut cmdbuf: Box<virgl::CommandBuffer> = box_zeroed();

    // Create pipeline state objects.
    cmdbuf.cmd_create_blend(HANDLE_BLEND);
    cmdbuf.cmd_create_dsa(HANDLE_DSA);
    cmdbuf.cmd_create_rasterizer(HANDLE_RASTERIZER, true);

    // Create surface wrapping our render target resource.
    cmdbuf.cmd_create_surface(HANDLE_SURFACE, RT_RESOURCE_ID, VIRGL_FORMAT_B8G8R8A8_UNORM);

    // Create vertex elements layout (position float2 + color float4).
    cmdbuf.cmd_create_vertex_elements_color(HANDLE_VE);

    // Create shaders from TGSI text.
    cmdbuf.cmd_create_shader_text(HANDLE_VS, PIPE_SHADER_VERTEX, shaders::COLOR_VS);
    cmdbuf.cmd_create_shader_text(HANDLE_FS, PIPE_SHADER_FRAGMENT, shaders::COLOR_FS);

    // Textured pipeline objects (for glyph rendering).
    cmdbuf.cmd_create_vertex_elements_textured(HANDLE_VE_TEXTURED);
    cmdbuf.cmd_create_shader_text(HANDLE_VS_TEXTURED, PIPE_SHADER_VERTEX, shaders::TEXTURED_VS);
    cmdbuf.cmd_create_shader_text(HANDLE_FS_GLYPH, PIPE_SHADER_FRAGMENT, shaders::GLYPH_FS);
    cmdbuf.cmd_create_sampler_state(HANDLE_SAMPLER);
    cmdbuf.cmd_create_sampler_view(
        HANDLE_SAMPLER_VIEW,
        ATLAS_RESOURCE_ID,
        VIRGL_FORMAT_R8_UNORM,
    );

    // Image pipeline: full-color fragment shader (TEXTURED_FS) + sampler view.
    cmdbuf.cmd_create_shader_text(HANDLE_FS_IMAGE, PIPE_SHADER_FRAGMENT, shaders::TEXTURED_FS);
    cmdbuf.cmd_create_sampler_view(
        HANDLE_SAMPLER_VIEW_IMG,
        IMG_RESOURCE_ID,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
    );

    // Bind color pipeline state (initial state for background rendering).
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_RASTERIZER, HANDLE_RASTERIZER);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE);
    cmdbuf.cmd_bind_shader(HANDLE_VS, PIPE_SHADER_VERTEX);
    cmdbuf.cmd_bind_shader(HANDLE_FS, PIPE_SHADER_FRAGMENT);

    // Set framebuffer (color only — stencil attached separately if available).
    cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, 0);
    cmdbuf.cmd_set_viewport(width as f32, height as f32);
    cmdbuf.cmd_set_vertex_buffers(scene_walk::VERTEX_STRIDE, 0, VB_RESOURCE_ID);

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

    // ── Stencil-then-cover setup (each object in its own submission) ──
    cmdbuf.clear();
    cmdbuf.cmd_create_dsa_stencil_write(HANDLE_DSA_STENCIL_WRITE);
    cmdbuf.cmd_create_dsa_stencil_test(HANDLE_DSA_STENCIL_TEST);
    cmdbuf.cmd_create_blend_no_color(HANDLE_BLEND_NO_COLOR);
    cmdbuf.cmd_create_surface(
        HANDLE_STENCIL_SURFACE,
        STENCIL_RESOURCE_ID,
        126, // Z32_FLOAT_S8X24_UINT (depth32f + stencil8) (Apple Silicon compatible)
    );
    cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, HANDLE_STENCIL_SURFACE);
    let stencil_ok = submit_3d(device, vq, irq_handle, &cmdbuf);

    if stencil_ok {
        sys::print(b"     stencil pipeline ready\n");
    } else {
        sys::print(b"     stencil pipeline FAILED - recovering\n");
        cmdbuf.clear();
        cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, 0);
        cmdbuf.cmd_set_viewport(width as f32, height as f32);
        let _ = submit_3d(device, vq, irq_handle, &cmdbuf);
    }

    stencil_ok
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
    let mut cmdbuf: Box<virgl::CommandBuffer> = box_zeroed();
    cmdbuf.cmd_clear(0.13, 0.13, 0.16, 1.0);

    if !submit_3d(device, vq, irq_handle, &cmdbuf) {
        sys::print(b"virgil-render: clear SUBMIT_3D failed\n");
        sys::exit();
    }
    sys::print(b"     clear submitted\n");

    // Flush the render target to display.
    let mut cmd = DmaBuf::alloc(0);
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

    // Create color vertex buffer resource.
    let vbo_size = scene_walk::MAX_VERTEX_BYTES as u32;
    resource_create_vbo(&device, &mut vq, irq_handle, vbo_size);
    let (vbo_va, _vbo_pa, _vbo_order) = attach_backing_vbo(&device, &mut vq, irq_handle, vbo_size);
    ctx_attach_vbo(&device, &mut vq, irq_handle);

    // Create glyph atlas texture resource (R8_UNORM, 512×512).
    resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        ATLAS_RESOURCE_ID,
        PIPE_TEXTURE_2D,
        VIRGL_FORMAT_R8_UNORM,
        virgl::PIPE_BIND_SAMPLER_VIEW,
        atlas::ATLAS_WIDTH,
        atlas::ATLAS_HEIGHT,
    );
    let (atlas_va, _atlas_pa, _atlas_order) = attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        ATLAS_RESOURCE_ID,
        atlas::ATLAS_BYTES as u32,
    );
    sys::print(b"     glyph atlas texture created\n");

    // Create textured vertex buffer resource. Sized for image quads + glyphs.
    let text_vbo_size = scene_walk::TOTAL_TEXTURED_VBO_BYTES as u32;
    resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        TEXT_VB_RESOURCE_ID,
        PIPE_BUFFER,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
        virgl::PIPE_BIND_VERTEX_BUFFER,
        text_vbo_size,
        1,
    );
    let (text_vbo_va, _text_vbo_pa, _text_vbo_order) = attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        TEXT_VB_RESOURCE_ID,
        text_vbo_size,
    );
    sys::print(b"     textured VBO created\n");

    // Create depth/stencil surface resource (Z32_FLOAT, same size as render target).
    resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        STENCIL_RESOURCE_ID,
        PIPE_TEXTURE_2D,
        126, // Z32_FLOAT_S8X24_UINT (depth32f + stencil8) (Apple Silicon; D24_S8 is Intel-only)
        virgl::VIRGL_BIND_DEPTH_STENCIL,
        width,
        height,
    );
    let (_stencil_va, _stencil_pa, _stencil_order) = attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        STENCIL_RESOURCE_ID,
        width * height * 8, // Z32F_S8X24 = 8 bytes/pixel
    );
    sys::print(b"     stencil surface created\n");

    // Image texture will be created lazily on first image frame.
    // Pre-allocate a DMA buffer for the max image size we support (64×64 BGRA).
    let max_img_bytes: u32 = 64 * 64 * 4; // 16 KiB for a 64×64 BGRA image.
    resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        IMG_RESOURCE_ID,
        PIPE_TEXTURE_2D,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
        virgl::PIPE_BIND_SAMPLER_VIEW,
        64, // Max supported width — matches DMA backing size.
        64, // Max supported height.
    );
    let (img_dma_va, _img_dma_pa, _img_dma_order) =
        attach_and_ctx_resource(&device, &mut vq, irq_handle, IMG_RESOURCE_ID, max_img_bytes);
    sys::print(b"     image texture created (64x64)\n");

    // ── Phase D: GPU pipeline setup ──────────────────────────────────────
    let stencil_available = setup_pipeline(&device, &mut vq, irq_handle, width, height);

    // ── Phase E: Clear screen + flush ────────────────────────────────────
    clear_screen(&device, &mut vq, irq_handle, width, height);

    // ── Phase F: Receive render config, init glyph atlas, render loop ────
    sys::print(b"     waiting for render config\n");

    let mut scene_va: u64 = 0;
    let mut font_va: u64 = 0;
    let mut font_len: u32 = 0;
    let mut scale_factor: f32 = 1.0;

    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_COMPOSITOR_CONFIG {
            // SAFETY: msg payload is a valid CompositorConfig from init.
            let config: CompositorConfig = unsafe { msg.payload_as() };
            scene_va = config.scene_va;
            font_va = config.mono_font_va;
            font_len = config.mono_font_len;
            scale_factor = config.scale_factor;

            sys::print(b"     render config: scene_va=");
            print_hex_u32((scene_va >> 32) as u32);
            print_hex_u32(scene_va as u32);
            sys::print(b" font_len=");
            print_u32(font_len);
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

    // ── Glyph atlas initialization ───────────────────────────────────────
    //
    // Rasterize ASCII glyphs on the CPU via GlyphCache, pack into atlas
    // DMA backing memory, then transfer to GPU texture.
    // Heap-allocate atlas (~24 KiB) directly — cannot use Box::new() because
    // the struct exceeds the 16 KiB stack. alloc_zeroed produces valid initial
    // state (all entries empty, cursors at 0), then we set dma_va.
    let mut glyph_atlas: Box<atlas::GlyphAtlas> = box_zeroed();
    glyph_atlas.set_dma_va(atlas_va);
    let mut font_ascent: u32 = 14;

    if font_va != 0 && font_len > 0 {
        sys::print(b"     initializing glyph atlas via HarfBuzz shaping\n");

        // SAFETY: font_va is mapped read-only into our address space by init.
        let font_data =
            unsafe { core::slice::from_raw_parts(font_va as *const u8, font_len as usize) };

        // Font size = core's FONT_SIZE (18px) in logical pixels.
        // The scene graph x_advance/x_offset are in logical pixels at this size.
        // Rasterize at the LOGICAL size — the scene_walk applies * scale for NDC.
        let font_size_px: u32 = 18; // must match core/main.rs FONT_SIZE

        // Axes must match core's shaping axes (MONO=1.0).
        let mono_axes = [fonts::rasterize::AxisValue {
            tag: *b"MONO",
            value: 1.0,
        }];

        // Get font metrics for ascent.
        // scale_fu_ceil(val, size, upem) = (val * size + upem - 1) / upem
        if let Some(metrics) = fonts::rasterize::font_metrics(font_data) {
            let upem = metrics.units_per_em as i32;
            let asc = metrics.ascent as i32;
            let size = font_size_px as i32;
            font_ascent = ((asc * size + upem - 1) / upem) as u32;

            sys::print(b"     font ascent=");
            print_u32(font_ascent);
            sys::print(b" size=");
            print_u32(font_size_px);
            sys::print(b"\n");
        }

        // Shape all printable ASCII through HarfBuzz to get real glyph IDs
        // (including GSUB substitutions like Recursive's MONO alternates).
        // Then rasterize each unique glyph ID directly into the atlas.
        let ascii: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
        let shaped = fonts::shape_with_variations(font_data, ascii, &[], &mono_axes);

        // Heap-allocate rasterization scratch space (~39 KiB).
        let mut scratch: Box<fonts::rasterize::RasterScratch> = box_zeroed();

        // Raster buffer for individual glyph rasterization (50×50 max).
        let mut raster_buf = [0u8; 50 * 50];

        let mut packed = 0u32;
        for sg in &shaped {
            if glyph_atlas.lookup(sg.glyph_id).is_some() {
                continue; // Already packed.
            }
            let mut rb = fonts::rasterize::RasterBuffer {
                data: &mut raster_buf,
                width: 50,
                height: 50,
            };
            if let Some(m) = fonts::rasterize::rasterize_with_axes(
                font_data,
                sg.glyph_id,
                font_size_px as u16,
                &mut rb,
                &mut scratch,
                &mono_axes,
            ) {
                if m.width > 0 && m.height > 0 {
                    let coverage = &raster_buf[..(m.width * m.height) as usize];
                    glyph_atlas.pack_glyph(
                        sg.glyph_id,
                        m.width,
                        m.height,
                        m.bearing_x,
                        m.bearing_y,
                        coverage,
                    );
                    packed += 1;
                }
            }
        }

        sys::print(b"     atlas packed ");
        print_u32(packed);
        sys::print(b" glyphs (");
        print_u32(shaped.len() as u32);
        sys::print(b" shaped)\n");

        // Transfer atlas texture to GPU.
        transfer_texture_to_host(
            &device,
            &mut vq,
            irq_handle,
            ATLAS_RESOURCE_ID,
            atlas::ATLAS_WIDTH,
            atlas::ATLAS_HEIGHT,
            atlas::ATLAS_WIDTH, // stride = width for R8
        );
        sys::print(b"     glyph atlas uploaded to GPU\n");
    } else {
        sys::print(b"     no font data, text rendering disabled\n");
    }

    // Scene graph shared memory.
    let scene_buf = unsafe {
        // SAFETY: scene_va is mapped into our address space by init via
        // memory_share before process start. Size is TRIPLE_SCENE_SIZE.
        core::slice::from_raw_parts(scene_va as *const u8, scene::TRIPLE_SCENE_SIZE)
    };

    // Heap-allocate batches and command buffer directly (all are zero-valid).
    // Cannot use Box::new() — TexturedBatch (~96 KiB) exceeds 16 KiB stack.
    let mut batch: Box<scene_walk::QuadBatch> = box_zeroed();
    let mut text_batch: Box<scene_walk::TexturedBatch> = box_zeroed();
    let mut image_batch: Box<scene_walk::ImageBatch> = box_zeroed();
    let mut path_batch: Box<scene_walk::PathBatch> = box_zeroed();
    let mut cmdbuf: Box<virgl::CommandBuffer> = box_zeroed();

    let mut last_gen: u32 = 0;
    let mut frame_count: u32 = 0;

    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render: render loop starting\n");

    // SAFETY: Channel 1 shared memory was set up by init before start.
    let scene_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    loop {
        let _ = sys::wait(&[SCENE_HANDLE], u64::MAX);

        // Drain scene update messages.
        {
            let mut drain_msg = ipc::Message::new(0);
            while scene_ch.try_recv(&mut drain_msg) {}
        }

        // Read the latest scene graph.
        let reader = scene::TripleReader::new(scene_buf);
        let nodes = reader.front_nodes();
        let gen = reader.front_generation();

        if gen == last_gen && frame_count > 0 {
            reader.finish_read(gen);
            continue;
        }
        last_gen = gen;

        // Walk scene tree: accumulate colored quads (backgrounds) and
        // textured quads (glyphs) in a single pass.
        let root = reader.front_root();
        let data_buf = reader.front_data_buf();
        scene_walk::walk_scene(
            nodes,
            root,
            scale_factor,
            width,
            height,
            &mut batch,
            &mut text_batch,
            &mut image_batch,
            &mut path_batch,
            data_buf,
            &glyph_atlas,
            font_ascent,
        );

        if frame_count < 3 {
            sys::print(b"     frame ");
            print_u32(frame_count);
            sys::print(b": bg=");
            print_u32(batch.vertex_count);
            sys::print(b" img=");
            print_u32(image_batch.count as u32);
            sys::print(b" path=");
            print_u32(path_batch.fan_vertex_count);
            sys::print(b" text=");
            print_u32(text_batch.vertex_count);
            sys::print(b"\n");
        }

        // ── Color VBO: pack backgrounds + path fan + path cover at offsets ─
        let color_data = batch.as_vertex_data();
        let color_dwords = color_data.len();
        let color_bytes = color_dwords * 4;

        let has_paths = path_batch.fan_vertex_count > 0 && stencil_available;
        let fan_data = path_batch.as_fan_data();
        let fan_dwords = fan_data.len();
        let fan_bytes = fan_dwords * 4;
        let cover_data = path_batch.as_cover_data();
        let cover_dwords = cover_data.len();
        let cover_bytes = cover_dwords * 4;

        let fan_vbo_offset = color_bytes;
        let cover_vbo_offset = fan_vbo_offset + fan_bytes;
        let total_color_bytes = cover_vbo_offset + cover_bytes;

        // Upload all color vertex data in one transfer.
        if total_color_bytes > 0 {
            if color_bytes > 0 {
                // SAFETY: vbo_va is valid DMA of MAX_VERTEX_BYTES size.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        color_data.as_ptr(),
                        vbo_va as *mut u32,
                        color_dwords,
                    );
                }
            }
            if has_paths && fan_bytes > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        fan_data.as_ptr(),
                        (vbo_va + fan_vbo_offset) as *mut u32,
                        fan_dwords,
                    );
                }
            }
            if has_paths && cover_bytes > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        cover_data.as_ptr(),
                        (vbo_va + cover_vbo_offset) as *mut u32,
                        cover_dwords,
                    );
                }
            }
            transfer_vbo_to_host(&device, &mut vq, irq_handle, total_color_bytes as u32);
        }

        // Build GPU commands (single cmdbuf for entire frame).
        // Re-set framebuffer state each frame so the render loop is
        // self-contained — doesn't depend on GPU state from prior submits
        // (e.g. the image loop's mid-frame submit/clear cycle).
        cmdbuf.clear();
        let zsurf = if stencil_available {
            HANDLE_STENCIL_SURFACE
        } else {
            0
        };
        cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, zsurf);
        cmdbuf.cmd_set_viewport(width as f32, height as f32);
        cmdbuf.cmd_clear(0.13, 0.13, 0.16, 1.0);
        if has_paths {
            cmdbuf.cmd_clear_stencil();
        }

        // Draw backgrounds (color pipeline, VBO offset 0).
        if batch.vertex_count > 0 {
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE);
            cmdbuf.cmd_bind_shader(HANDLE_VS, PIPE_SHADER_VERTEX);
            cmdbuf.cmd_bind_shader(HANDLE_FS, PIPE_SHADER_FRAGMENT);
            cmdbuf.cmd_set_vertex_buffers(scene_walk::VERTEX_STRIDE, 0, VB_RESOURCE_ID);
            cmdbuf.cmd_draw_vbo(0, batch.vertex_count, PIPE_PRIM_TRIANGLES, false);
        }

        // Stencil-then-cover paths (VBO offsets for fan + cover).
        if has_paths {
            // Pass A: stencil write (fan triangles, no color).
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND_NO_COLOR);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA_STENCIL_WRITE);
            cmdbuf.cmd_set_vertex_buffers(
                scene_walk::VERTEX_STRIDE,
                fan_vbo_offset as u32,
                VB_RESOURCE_ID,
            );
            cmdbuf.cmd_set_stencil_ref(0, 0);
            cmdbuf.cmd_draw_vbo(0, path_batch.fan_vertex_count, PIPE_PRIM_TRIANGLES, false);

            // Pass B: stencil test + cover (colored quads where stencil != 0).
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA_STENCIL_TEST);
            cmdbuf.cmd_set_vertex_buffers(
                scene_walk::VERTEX_STRIDE,
                cover_vbo_offset as u32,
                VB_RESOURCE_ID,
            );
            cmdbuf.cmd_draw_vbo(0, path_batch.cover_vertex_count, PIPE_PRIM_TRIANGLES, false);

            // Restore normal DSA.
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
        }

        // ── Pass 3: Upload + draw images (TEXTURED_FS) ──────────────────
        // Each image shares a single GPU texture resource, so we must
        // upload, transfer, and draw each image individually.  Vertices
        // are written sequentially into the text VBO (image 0 at offset
        // 0, image 1 at offset 192, etc.) and each image is drawn
        // immediately after its texture transfer.
        let mut images_drawn: usize = 0;
        {
            let vw = width as f32;
            let vh = height as f32;
            let white = 1.0f32.to_bits();

            for idx in 0..image_batch.count {
                let img = match image_batch.get(idx) {
                    Some(i) => i,
                    None => break,
                };
                let img_pixels = img.src_width as u32 * img.src_height as u32 * 4;
                let src_offset = img.data_offset as usize;
                let src_end = src_offset + img_pixels as usize;

                if src_end > data_buf.len() || img_pixels > max_img_bytes {
                    continue;
                }

                // Copy image pixel data to DMA backing.
                // SAFETY: img_dma_va is valid DMA of max_img_bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        data_buf[src_offset..src_end].as_ptr(),
                        img_dma_va as *mut u8,
                        img_pixels as usize,
                    );
                }
                // Transfer image texture to GPU.
                transfer_texture_to_host(
                    &device,
                    &mut vq,
                    irq_handle,
                    IMG_RESOURCE_ID,
                    img.src_width as u32,
                    img.src_height as u32,
                    img.src_width as u32 * 4, // BGRA stride
                );

                // Build textured quad vertices for the image.
                let x0 = img.x / vw * 2.0 - 1.0;
                let y0 = 1.0 - img.y / vh * 2.0;
                let x1 = (img.x + img.w) / vw * 2.0 - 1.0;
                let y1 = 1.0 - (img.y + img.h) / vh * 2.0;

                // 6 vertices x 8 floats = 48 dwords.
                let dwords = scene_walk::DWORDS_PER_IMAGE_QUAD;
                let mut img_verts = [0u32; 48];
                // pos(x,y) + texcoord(u,v) + color(r,g,b,a)
                let verts: [(f32, f32, f32, f32); 6] = [
                    (x0, y0, 0.0, 0.0), // top-left
                    (x0, y1, 0.0, 1.0), // bottom-left
                    (x1, y0, 1.0, 0.0), // top-right
                    (x1, y0, 1.0, 0.0), // top-right
                    (x0, y1, 0.0, 1.0), // bottom-left
                    (x1, y1, 1.0, 1.0), // bottom-right
                ];
                for (i, &(px, py, u, v)) in verts.iter().enumerate() {
                    let base = i * 8;
                    img_verts[base] = px.to_bits();
                    img_verts[base + 1] = py.to_bits();
                    img_verts[base + 2] = u.to_bits();
                    img_verts[base + 3] = v.to_bits();
                    img_verts[base + 4] = white; // r
                    img_verts[base + 5] = white; // g
                    img_verts[base + 6] = white; // b
                    img_verts[base + 7] = white; // a
                }

                // Write this image's vertices at its slot in the text VBO.
                let vbo_dword_offset = images_drawn * dwords;
                // SAFETY: text_vbo_va is valid DMA of TOTAL_TEXTURED_VBO_BYTES;
                // vbo_dword_offset is bounded by MAX_IMAGES * dwords.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        img_verts.as_ptr(),
                        (text_vbo_va as *mut u32).add(vbo_dword_offset),
                        dwords,
                    );
                }

                // Upload this image's vertices to the GPU immediately.
                let vbo_byte_offset = (vbo_dword_offset * 4) as u32;
                let vbo_byte_len = (dwords * 4) as u32;
                transfer_buffer_to_host(
                    &device,
                    &mut vq,
                    irq_handle,
                    TEXT_VB_RESOURCE_ID,
                    vbo_byte_offset + vbo_byte_len,
                );

                // Draw this image's quad immediately (texture will be
                // overwritten by the next image's upload).
                cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE_TEXTURED);
                cmdbuf.cmd_bind_shader(HANDLE_VS_TEXTURED, PIPE_SHADER_VERTEX);
                cmdbuf.cmd_bind_shader(HANDLE_FS_IMAGE, PIPE_SHADER_FRAGMENT);
                cmdbuf.cmd_set_vertex_buffers(
                    scene_walk::TEXTURED_VERTEX_STRIDE,
                    vbo_byte_offset,
                    TEXT_VB_RESOURCE_ID,
                );
                cmdbuf.cmd_bind_sampler_states(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER);
                cmdbuf.cmd_set_sampler_views(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER_VIEW_IMG);
                cmdbuf.cmd_draw_vbo(0, 6, PIPE_PRIM_TRIANGLES, false);

                // Submit + flush between images so the GPU consumes the
                // texture before we overwrite it with the next image.
                if !cmdbuf.overflowed() {
                    submit_3d(&device, &mut vq, irq_handle, &cmdbuf);
                }
                cmdbuf.clear();
                let zsurf = if stencil_available {
                    HANDLE_STENCIL_SURFACE
                } else {
                    0
                };
                cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, zsurf);
                cmdbuf.cmd_set_viewport(width as f32, height as f32);

                images_drawn += 1;
            }
        }

        // ── Pass 4: Upload glyph vertices to text VBO and draw.
        //
        // Layout: [image vertices (MAX_IMAGES * 192 bytes)] [glyph vertices]
        // Glyph draw uses VBO offset after all image data.
        let text_data = text_batch.as_vertex_data();
        let text_dwords = text_data.len();
        let text_bytes = text_dwords * 4;

        // Reserve space for MAX_IMAGES image quads so glyph offset is stable.
        let img_vbo_bytes: usize = scene_walk::MAX_IMAGES * scene_walk::DWORDS_PER_IMAGE_QUAD * 4;
        let glyph_vbo_offset = img_vbo_bytes; // glyphs start after all image slots

        if text_bytes > 0 {
            // Copy glyph data after image region in DMA buffer.
            // SAFETY: text_vbo_va is valid DMA of TOTAL_TEXTURED_VBO_BYTES.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    text_data.as_ptr(),
                    (text_vbo_va + img_vbo_bytes) as *mut u32,
                    text_dwords,
                );
            }

            let total_upload = img_vbo_bytes + text_bytes;
            transfer_buffer_to_host(
                &device,
                &mut vq,
                irq_handle,
                TEXT_VB_RESOURCE_ID,
                total_upload as u32,
            );

            if text_batch.vertex_count > 0 {
                cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE_TEXTURED);
                cmdbuf.cmd_bind_shader(HANDLE_VS_TEXTURED, PIPE_SHADER_VERTEX);
                cmdbuf.cmd_bind_shader(HANDLE_FS_GLYPH, PIPE_SHADER_FRAGMENT);
                cmdbuf.cmd_set_vertex_buffers(
                    scene_walk::TEXTURED_VERTEX_STRIDE,
                    glyph_vbo_offset as u32, // glyphs start after image data
                    TEXT_VB_RESOURCE_ID,
                );
                cmdbuf.cmd_bind_sampler_states(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER);
                cmdbuf.cmd_set_sampler_views(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER_VIEW);
                cmdbuf.cmd_draw_vbo(0, text_batch.vertex_count, PIPE_PRIM_TRIANGLES, false);
            }
        }

        if cmdbuf.overflowed() {
            sys::print(b"virgil-render: command buffer overflow!\n");
        } else {
            submit_3d(&device, &mut vq, irq_handle, &cmdbuf);
            flush_resource(&device, &mut vq, irq_handle, width, height);
        }

        reader.finish_read(gen);
        frame_count = frame_count.wrapping_add(1);
    }
}
