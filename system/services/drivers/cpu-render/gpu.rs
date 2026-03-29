//! Virtio-GPU 2D command layer.
//!
//! Protocol structs and command functions for the virtio-gpu 2D protocol.
//! Extracted from the standalone virtio-gpu driver for use by cpu-render.
//!
//! All commands go through the control virtqueue as request/response pairs:
//! driver writes a command header + payload, device writes a response.

/// Control virtqueue index.
pub const VIRTQ_CONTROL: u32 = 0;
/// Resource ID for the framebuffer (arbitrary nonzero).
pub const FB_RESOURCE_ID: u32 = 1;
/// Scanout index (first/only display).
pub const SCANOUT_ID: u32 = 0;
/// Bytes per pixel (BGRA8888).
pub const FB_BPP: u32 = 4;

// virtio-gpu command and response types — imported from protocol crate.
use protocol::metal::virgl::{
    VIRGL_FORMAT_B8G8R8A8_UNORM as FORMAT_B8G8R8A8_UNORM,
    VIRTIO_GPU_CMD_GET_DISPLAY_INFO as CMD_GET_DISPLAY_INFO,
    VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING as CMD_RESOURCE_ATTACH_BACKING,
    VIRTIO_GPU_CMD_RESOURCE_CREATE_2D as CMD_RESOURCE_CREATE_2D,
    VIRTIO_GPU_CMD_RESOURCE_FLUSH as CMD_RESOURCE_FLUSH,
    VIRTIO_GPU_CMD_SET_SCANOUT as CMD_SET_SCANOUT,
    VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D as CMD_TRANSFER_TO_HOST_2D,
    VIRTIO_GPU_RESP_OK_DISPLAY_INFO as RESP_OK_DISPLAY_INFO,
    VIRTIO_GPU_RESP_OK_NODATA as RESP_OK_NODATA,
};

// ── Protocol structs ─────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CtrlHeader {
    pub cmd_type: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub _padding: u32,
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

pub struct DmaBuf {
    pub va: usize,
    pub pa: u64,
    pub order: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    _padding: u32,
}

#[repr(C)]
struct AttachBacking {
    header: CtrlHeader,
    resource_id: u32,
    nr_entries: u32,
}

#[repr(C)]
struct ResourceCreate2d {
    header: CtrlHeader,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct ResourceFlush {
    header: CtrlHeader,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    resource_id: u32,
    _padding: u32,
}

#[repr(C)]
struct SetScanout {
    header: CtrlHeader,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
struct TransferToHost2d {
    header: CtrlHeader,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    offset: u64,
    resource_id: u32,
    _padding: u32,
}

// ── DmaBuf ───────────────────────────────────────────────────────────────

impl DmaBuf {
    pub fn alloc(order: u32) -> DmaBuf {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
            sys::print(b"cpu-render: dma_alloc failed\n");
            sys::exit();
        });
        // SAFETY: va is a valid DMA allocation of (1 << order) pages.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * ipc::PAGE_SIZE) };
        DmaBuf { va, pa, order }
    }

    pub fn free(self) {
        let _ = sys::dma_free(self.va as u64, self.order);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn ctrl_header(cmd_type: u32) -> CtrlHeader {
    CtrlHeader {
        cmd_type,
        flags: 0,
        fence_id: 0,
        ctx_id: 0,
        _padding: 0,
    }
}

/// Send a command through the control virtqueue and wait for the response.
fn gpu_command(
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
    // SAFETY: resp_va points to a CtrlHeader written by the device.
    unsafe { core::ptr::read_volatile(resp_va as *const u32) }
}

fn print_u32(n: u32) {
    sys::print_u32(n);
}

// ── Device initialization ────────────────────────────────────────────────

/// Initialize a virtio-gpu device: map MMIO, negotiate features, register
/// IRQ, and set up the control virtqueue.
///
/// Returns `(device, virtqueue, irq_handle)`.
pub fn init_device(
    mmio_pa: u64,
    irq: u32,
) -> (virtio::Device, virtio::Virtqueue, sys::InterruptHandle) {
    let page_offset = mmio_pa & (ipc::PAGE_SIZE as u64 - 1);
    let page_pa = mmio_pa & !(ipc::PAGE_SIZE as u64 - 1);
    let page_va = sys::device_map(page_pa, ipc::PAGE_SIZE as u64).unwrap_or_else(|_| {
        sys::print(b"cpu-render: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);
    if !device.negotiate() {
        sys::print(b"cpu-render: negotiate failed\n");
        sys::exit();
    }
    let irq_handle = sys::interrupt_register(irq).unwrap_or_else(|_| {
        sys::print(b"cpu-render: interrupt_register failed\n");
        sys::exit();
    });
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_CONTROL),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"cpu-render: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_bytes = (1usize << vq_order) * ipc::PAGE_SIZE;
    // SAFETY: vq_va is a valid DMA allocation of vq_bytes.
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
    (device, vq, irq_handle)
}

// ── GPU commands ─────────────────────────────────────────────────────────

/// Query display dimensions from the virtual display.
pub fn get_display_info(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
) -> (u32, u32) {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va is a valid DMA page, CtrlHeader fits at offset 0.
    unsafe { core::ptr::write(cmd.va as *mut CtrlHeader, ctrl_header(CMD_GET_DISPLAY_INFO)) };
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
        24 + 16 * 24,
    );
    let (width, height) = if resp_type == RESP_OK_DISPLAY_INFO {
        let info_ptr = (resp_va + core::mem::size_of::<CtrlHeader>()) as *const DisplayInfo;
        // SAFETY: device wrote a valid DisplayInfo after the response header.
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

pub fn resource_create_2d(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    resource_id: u32,
    width: u32,
    height: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va is a valid DMA page, ResourceCreate2d fits.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceCreate2d,
            ResourceCreate2d {
                header: ctrl_header(CMD_RESOURCE_CREATE_2D),
                resource_id,
                format: FORMAT_B8G8R8A8_UNORM,
                width,
                height,
            },
        );
    }
    let resp_pa = cmd.pa + 512;
    let resp_va = cmd.va + 512;
    let resp_type = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<ResourceCreate2d>() as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
    let ok = resp_type == RESP_OK_NODATA;
    cmd.free();
    ok
}

pub fn attach_backing_sg(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    resource_id: u32,
    pa_table: &[u64],
    chunk_bytes: u32,
) -> bool {
    let nr_entries = pa_table.len() as u32;
    let header_size = core::mem::size_of::<AttachBacking>();
    let entry_size = core::mem::size_of::<MemEntry>();
    let total_bytes = header_size + (nr_entries as usize) * entry_size;
    let cmd_pages = (total_bytes + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE;
    let cmd_order = (cmd_pages.next_power_of_two().trailing_zeros()) as u32;
    let cmd = DmaBuf::alloc(cmd_order);
    let ptr = cmd.va as *mut u8;
    // SAFETY: cmd.va has enough space for header + entries.
    unsafe {
        core::ptr::write(
            ptr as *mut AttachBacking,
            AttachBacking {
                header: ctrl_header(CMD_RESOURCE_ATTACH_BACKING),
                resource_id,
                nr_entries,
            },
        );
    }
    for (i, &pa) in pa_table.iter().enumerate() {
        // SAFETY: writing MemEntry at the correct offset within the DMA buffer.
        unsafe {
            core::ptr::write(
                ptr.add(header_size + i * entry_size) as *mut MemEntry,
                MemEntry {
                    addr: pa,
                    length: chunk_bytes,
                    _padding: 0,
                },
            );
        }
    }
    let resp_offset = ((total_bytes + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE) * ipc::PAGE_SIZE;
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
        total_bytes as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
    let ok = resp_type == RESP_OK_NODATA;
    if let Some(rb) = resp_buf {
        rb.free();
    }
    cmd.free();
    ok
}

pub fn set_scanout(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    scanout_id: u32,
    resource_id: u32,
    width: u32,
    height: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va is a valid DMA page, SetScanout fits.
    unsafe {
        core::ptr::write(
            cmd.va as *mut SetScanout,
            SetScanout {
                header: ctrl_header(CMD_SET_SCANOUT),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                scanout_id,
                resource_id,
            },
        );
    }
    let resp_pa = cmd.pa + 512;
    let resp_va = cmd.va + 512;
    let resp_type = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<SetScanout>() as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
    let ok = resp_type == RESP_OK_NODATA;
    cmd.free();
    ok
}

/// Transfer framebuffer to host using a pre-allocated DMA buffer.
/// `base_offset` is the byte offset to the start of the buffer (for double-buffering).
pub fn transfer_to_host_reuse(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    cmd: &DmaBuf,
    resource_id: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
    base_offset: u64,
    stride: u32,
) {
    let ptr = cmd.va as *mut u8;
    let pixel_offset = (rect_y as u64) * (stride as u64) + (rect_x as u64) * (FB_BPP as u64);
    let offset = base_offset + pixel_offset;
    // SAFETY: cmd.va is a valid DMA page, TransferToHost2d fits in 512 bytes.
    unsafe {
        core::ptr::write_bytes(ptr, 0, 512);
        core::ptr::write(
            ptr as *mut TransferToHost2d,
            TransferToHost2d {
                header: ctrl_header(CMD_TRANSFER_TO_HOST_2D),
                rect_x,
                rect_y,
                rect_width: rect_w,
                rect_height: rect_h,
                offset,
                resource_id,
                _padding: 0,
            },
        );
    }
    let resp = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<TransferToHost2d>() as u32,
        cmd.pa + 512,
        cmd.va + 512,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
    if resp != RESP_OK_NODATA {
        sys::print(b"gpu: unexpected transfer_to_host response\n");
    }
}

/// Flush resource to display using a pre-allocated DMA buffer.
pub fn resource_flush_reuse(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    cmd: &DmaBuf,
    resource_id: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
) {
    let ptr = cmd.va as *mut u8;
    // SAFETY: cmd.va is a valid DMA page, ResourceFlush fits in 512 bytes.
    unsafe {
        core::ptr::write_bytes(ptr, 0, 512);
        core::ptr::write(
            ptr as *mut ResourceFlush,
            ResourceFlush {
                header: ctrl_header(CMD_RESOURCE_FLUSH),
                rect_x,
                rect_y,
                rect_width: rect_w,
                rect_height: rect_h,
                resource_id,
                _padding: 0,
            },
        );
    }
    let resp = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<ResourceFlush>() as u32,
        cmd.pa + 512,
        cmd.va + 512,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
    if resp != RESP_OK_NODATA {
        sys::print(b"gpu: unexpected resource_flush response\n");
    }
}
