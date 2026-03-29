//! Virgl 3D resource management (Phase C) and GPU command helpers.
//!
//! Creates the rendering context, render target, backing memory, vertex
//! buffers, texture resources, and provides low-level GPU command submission
//! (gpu_command, gpu_cmd_ok) used by all phases.

use protocol::metal::virgl::{
    VIRGL_FORMAT_B8G8R8A8_UNORM, VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE, VIRTIO_GPU_CMD_CTX_CREATE,
    VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_CREATE_3D,
    VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D,
    VIRTIO_GPU_RESP_OK_NODATA,
};

use crate::{
    wire::{
        ctrl_header, ctrl_header_ctx, AttachBacking, CtrlHeader, CtxCreate, CtxResource, DmaBuf,
        MemEntry, ResourceCreate3d, ResourceFlush, SetScanout, TransferToHost3d,
    },
    RT_RESOURCE_ID, SCANOUT_ID, VB_RESOURCE_ID, VIRGL_CTX_ID, VIRTQ_CONTROL,
};

// ── GPU command helpers ──────────────────────────────────────────────────

/// Send a GPU command via the control virtqueue and wait for the response.
/// Returns the response cmd_type.
pub(crate) fn gpu_command(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    cmd_pa: u64,
    cmd_len: u32,
    resp_pa: u64,
    resp_va: usize,
    resp_len: u32,
) -> u32 {
    vq.push_chain(&[(cmd_pa, cmd_len, false), (resp_pa, resp_len, true)]);
    device.notify(VIRTQ_CONTROL);
    let _ = sys::wait(&[irq_handle.0], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    let _ = sys::interrupt_ack(irq_handle);
    // SAFETY: resp_va points to valid DMA memory containing a CtrlHeader written by device.
    let resp_header = resp_va as *const CtrlHeader;
    unsafe { core::ptr::read_volatile(&(*resp_header).cmd_type as *const u32) }
}

/// Send a simple command (header only, no extra payload) and check for RESP_OK_NODATA.
pub(crate) fn gpu_cmd_ok(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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

/// Compute the base VA of channel N's shared pages.
pub(crate) fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

pub(crate) fn print_u32(n: u32) {
    sys::print_u32(n);
}

pub(crate) fn print_hex_u32(val: u32) {
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

// ── Phase C: Virgl 3D initialization ─────────────────────────────────────

pub(crate) fn ctx_create(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
) {
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

pub(crate) fn resource_create_3d(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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

pub(crate) fn attach_backing(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    width: u32,
    height: u32,
) {
    let fb_bytes = (width as usize) * (height as usize) * 4;
    // Allocate backing pages as individual order-0 DMA pages.
    // We use a single scatter-gather list.
    // For simplicity, allocate in 256 KiB chunks (order 6 = 64 pages).
    const CHUNK_ORDER: u32 = 6;
    const CHUNK_PAGES: usize = 1 << CHUNK_ORDER;
    const CHUNK_BYTES: usize = CHUNK_PAGES * ipc::PAGE_SIZE;
    let chunks_needed = (fb_bytes + CHUNK_BYTES - 1) / CHUNK_BYTES;

    // Allocate the command DMA buffer (needs space for header + entries).
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();
    let total_cmd_bytes = header_size + chunks_needed * entry_size;
    let cmd_pages = (total_cmd_bytes + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE;
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
    let resp_offset = ((total_cmd_bytes + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE) * ipc::PAGE_SIZE;
    let (resp_pa, resp_va, resp_buf) = if resp_offset + 64 <= (1 << cmd_order) * ipc::PAGE_SIZE {
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

    if let Some(mut rb) = resp_buf {
        rb.free();
    }
    cmd.free();

    sys::print(b"     backing attached (");
    print_u32(chunks_needed as u32);
    sys::print(b" chunks)\n");
}

pub(crate) fn ctx_attach_resource(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
) {
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

pub(crate) fn set_scanout(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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
pub(crate) fn resource_create_vbo(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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
                target: protocol::metal::virgl::PIPE_BUFFER,
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
pub(crate) fn attach_backing_vbo(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    size_bytes: u32,
) -> (usize, u64, u32) {
    let mut cmd = DmaBuf::alloc(0);
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();

    // Allocate DMA pages for VBO backing.
    let vbo_pages = ((size_bytes as usize) + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE;
    let vbo_order = (vbo_pages.next_power_of_two().trailing_zeros()) as u32;
    let mut vbo_pa: u64 = 0;
    let vbo_va = sys::dma_alloc(vbo_order, &mut vbo_pa).unwrap_or_else(|_| {
        sys::print(b"virgil-render: dma_alloc (vbo backing) failed\n");
        sys::exit();
    });
    // SAFETY: vbo_va is valid DMA memory, zero it.
    unsafe { core::ptr::write_bytes(vbo_va as *mut u8, 0, (1usize << vbo_order) * ipc::PAGE_SIZE) };

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
                length: (1u32 << vbo_order) * ipc::PAGE_SIZE as u32,
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
pub(crate) fn transfer_vbo_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    data_bytes: u32,
) {
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut TransferToHost3d,
            TransferToHost3d {
                hdr: ctrl_header_ctx(protocol::metal::virgl::VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D),
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
pub(crate) fn ctx_attach_vbo(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
) {
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
pub(crate) fn resource_create_3d_generic(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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
pub(crate) fn attach_and_ctx_resource(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    resource_id: u32,
    size_bytes: u32,
) -> (usize, u64, u32) {
    let mut cmd = DmaBuf::alloc(0);
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();

    let pages = ((size_bytes as usize) + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE;
    let order = (pages.next_power_of_two().trailing_zeros()) as u32;
    let mut pa: u64 = 0;
    let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
        sys::print(b"virgil-render: dma_alloc (resource backing) failed\n");
        sys::exit();
    });
    // SAFETY: va is valid DMA memory of (1 << order) pages.
    unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * ipc::PAGE_SIZE) };

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
                length: (1u32 << order) * ipc::PAGE_SIZE as u32,
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
pub(crate) fn transfer_texture_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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
pub(crate) fn transfer_buffer_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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
pub(crate) fn flush_resource(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
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
