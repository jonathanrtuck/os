//! Userspace virtio-gpu 2D driver.
//!
//! Receives device info (MMIO PA, IRQ) from the kernel via channel shared
//! memory, initializes the GPU device, creates a framebuffer, and draws a
//! test pattern to prove the rendering pipeline works.
//!
//! # virtio-gpu 2D protocol
//!
//! All commands go through the control virtqueue (queue 0) as request/response
//! pairs: driver writes a command header + payload, device writes a response.
//! The six core 2D commands:
//!
//! 1. GET_DISPLAY_INFO — query scanout dimensions
//! 2. RESOURCE_CREATE_2D — allocate a host-side 2D resource
//! 3. RESOURCE_ATTACH_BACKING — attach guest physical pages to the resource
//! 4. SET_SCANOUT — bind resource to a display output
//! 5. TRANSFER_TO_HOST_2D — copy rectangle from guest to host resource
//! 6. RESOURCE_FLUSH — present the resource on screen
//!
//! # Shared memory layout (written by kernel before start)
//!
//! ```text
//! offset 0:  mmio_pa (u64) — physical address of the MMIO region
//! offset 8:  irq     (u32) — GIC IRQ number
//! ```

#![no_std]
#![no_main]

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Channel shared memory base (must match kernel paging::CHANNEL_SHM_BASE).
const SHM: *const u8 = 0x4000_0000 as *const u8;

/// Control virtqueue index.
const VIRTQ_CONTROL: u32 = 0;

/// Resource ID for our framebuffer (arbitrary nonzero).
const FB_RESOURCE_ID: u32 = 1;

/// Scanout index (first/only display).
const SCANOUT_ID: u32 = 0;

/// Framebuffer dimensions — 1024x768 matches QEMU's default virtio-gpu scanout.
const FB_WIDTH: u32 = 1024;
const FB_HEIGHT: u32 = 768;
const FB_BPP: u32 = 4; // BGRA8888, 4 bytes per pixel
const FB_STRIDE: u32 = FB_WIDTH * FB_BPP;
const FB_SIZE: u32 = FB_STRIDE * FB_HEIGHT;

// virtio-gpu command types (enum auto-increments from 0x0100).
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
// 0x0102 = RESOURCE_UNREF (not used)
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
// 0x0107 = RESOURCE_DETACH_BACKING (not used)

// virtio-gpu response types.
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

// virtio-gpu pixel format (B8G8R8A8_UNORM).
const FORMAT_B8G8R8A8_UNORM: u32 = 1;

// ---------------------------------------------------------------------------
// Command structures (all repr(C) for hardware layout)
// ---------------------------------------------------------------------------

/// Control command header — prefixes every request and response.
#[repr(C)]
#[derive(Clone, Copy)]
struct CtrlHeader {
    cmd_type: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    _padding: u32,
}

/// Response to GET_DISPLAY_INFO — one entry per scanout (max 16).
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

/// RESOURCE_CREATE_2D request (follows header).
#[repr(C)]
struct ResourceCreate2d {
    header: CtrlHeader,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

/// Memory entry for RESOURCE_ATTACH_BACKING.
#[repr(C)]
#[derive(Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    _padding: u32,
}

/// RESOURCE_ATTACH_BACKING request (followed by MemEntry array).
#[repr(C)]
struct AttachBacking {
    header: CtrlHeader,
    resource_id: u32,
    nr_entries: u32,
    // MemEntry[nr_entries] follows immediately in the same descriptor.
}

/// SET_SCANOUT request.
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

/// TRANSFER_TO_HOST_2D request.
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

/// RESOURCE_FLUSH request.
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
///
/// `cmd_pa` / `cmd_len`: physical address and length of the request buffer.
/// `resp_pa` / `resp_len`: physical address and length of the response buffer.
/// `resp_va`: virtual address of the response buffer (for CPU access).
///
/// Returns the response command type (e.g. RESP_OK_NODATA).
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
    // 2-descriptor chain: command (device-readable) → response (device-writable).
    vq.push_chain(&[(cmd_pa, cmd_len, false), (resp_pa, resp_len, true)]);
    device.notify(VIRTQ_CONTROL);

    // Block until the device completes the command.
    sys::wait(&[irq_handle], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    sys::interrupt_ack(irq_handle);

    // Read the response header's cmd_type field (must use VA, not PA).
    let resp_header = resp_va as *const CtrlHeader;
    unsafe { core::ptr::read_volatile(&(*resp_header).cmd_type as *const u32) }
}

/// Print a u32 in decimal.
fn print_u32(mut n: u32) {
    if n == 0 {
        sys::write(b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 10;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::write(&buf[i..]);
}

// ---------------------------------------------------------------------------
// DMA buffer management
// ---------------------------------------------------------------------------

/// A DMA buffer with both VA and PA tracked.
struct DmaBuf {
    va: usize,
    pa: u64,
    order: u32,
}

impl DmaBuf {
    fn alloc(order: u32) -> DmaBuf {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa);
        if va < 0 {
            sys::write(b"virtio-gpu: dma_alloc failed\n");
            sys::exit();
        }
        let va = va as usize;
        // Zero the buffer.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * 4096) };
        DmaBuf { va, pa, order }
    }

    fn free(self) {
        sys::dma_free(self.va as u64, self.order);
    }
}

// ---------------------------------------------------------------------------
// GPU initialization and drawing
// ---------------------------------------------------------------------------

/// Query display info and return (width, height) of scanout 0.
fn get_display_info(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
) -> (u32, u32) {
    // Command buffer: header only (24 bytes).
    // Response buffer: header (24 bytes) + 16 DisplayInfo entries (384 bytes) = 408 bytes.
    let cmd = DmaBuf::alloc(0); // one page is plenty

    let cmd_ptr = cmd.va as *mut CtrlHeader;
    unsafe { core::ptr::write(cmd_ptr, ctrl_header(CMD_GET_DISPLAY_INFO)) };

    let resp_offset = 256u64; // put response in same page, offset 256
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
        24 + 16 * 24, // header + 16 DisplayInfo entries
    );

    let (width, height) = if resp_type == RESP_OK_DISPLAY_INFO {
        // First DisplayInfo entry starts after the response header.
        let info_ptr = (resp_va + core::mem::size_of::<CtrlHeader>()) as *const DisplayInfo;
        let info = unsafe { core::ptr::read_volatile(info_ptr) };
        if info.enabled != 0 {
            (info.rect_width, info.rect_height)
        } else {
            (FB_WIDTH, FB_HEIGHT) // fallback
        }
    } else {
        (FB_WIDTH, FB_HEIGHT) // fallback
    };

    cmd.free();
    (width, height)
}

/// Create a 2D resource on the host.
fn resource_create_2d(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    width: u32,
    height: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);

    let req = cmd.va as *mut ResourceCreate2d;
    unsafe {
        core::ptr::write(
            req,
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

/// Attach guest physical memory to a resource.
fn attach_backing(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    fb_pa: u64,
    fb_size: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);
    let ptr = cmd.va as *mut u8;

    // Write AttachBacking header + MemEntry contiguously.
    unsafe {
        core::ptr::write(
            ptr as *mut AttachBacking,
            AttachBacking {
                header: ctrl_header(CMD_RESOURCE_ATTACH_BACKING),
                resource_id,
                nr_entries: 1,
            },
        );
    }

    let entry_offset = core::mem::size_of::<AttachBacking>();
    unsafe {
        core::ptr::write(
            ptr.add(entry_offset) as *mut MemEntry,
            MemEntry {
                addr: fb_pa,
                length: fb_size,
                _padding: 0,
            },
        );
    }

    let cmd_len = (entry_offset + core::mem::size_of::<MemEntry>()) as u32;
    let resp_pa = cmd.pa + 512;
    let resp_va = cmd.va + 512;
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

    let ok = resp_type == RESP_OK_NODATA;
    cmd.free();
    ok
}

/// Bind a resource to a scanout.
fn set_scanout(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    scanout_id: u32,
    resource_id: u32,
    width: u32,
    height: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);

    unsafe {
        let req = cmd.va as *mut SetScanout;
        core::ptr::write(
            req,
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

/// Transfer a rectangle from guest memory to the host resource.
fn transfer_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);

    unsafe {
        let req = cmd.va as *mut TransferToHost2d;
        core::ptr::write(
            req,
            TransferToHost2d {
                header: ctrl_header(CMD_TRANSFER_TO_HOST_2D),
                rect_x: x,
                rect_y: y,
                rect_width: width,
                rect_height: height,
                offset: 0,
                resource_id,
                _padding: 0,
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
        core::mem::size_of::<TransferToHost2d>() as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );

    let ok = resp_type == RESP_OK_NODATA;
    cmd.free();
    ok
}

/// Flush a rectangle to the display.
fn resource_flush(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> bool {
    let cmd = DmaBuf::alloc(0);

    unsafe {
        let req = cmd.va as *mut ResourceFlush;
        core::ptr::write(
            req,
            ResourceFlush {
                header: ctrl_header(CMD_RESOURCE_FLUSH),
                rect_x: x,
                rect_y: y,
                rect_width: width,
                rect_height: height,
                resource_id,
                _padding: 0,
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
        core::mem::size_of::<ResourceFlush>() as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );

    let ok = resp_type == RESP_OK_NODATA;
    cmd.free();
    ok
}

/// Draw a test pattern into the framebuffer: colored rectangles with a
/// white border, demonstrating that the full rendering pipeline works.
fn draw_test_pattern(fb_ptr: *mut u8, width: u32, height: u32) {
    let stride = width * FB_BPP;

    // Fill with dark background (BGRA: 30, 30, 30, 255).
    for y in 0..height {
        for x in 0..width {
            let offset = (y * stride + x * FB_BPP) as usize;
            unsafe {
                *fb_ptr.add(offset) = 30; // B
                *fb_ptr.add(offset + 1) = 30; // G
                *fb_ptr.add(offset + 2) = 30; // R
                *fb_ptr.add(offset + 3) = 255; // A
            }
        }
    }

    // Draw colored rectangles.
    let colors: &[(u8, u8, u8)] = &[
        (80, 80, 220),   // blue (BGR)
        (80, 180, 80),   // green
        (80, 80, 220),   // red — wait, BGR: (B=80, G=80, R=220) = red
        (0, 200, 220),   // yellow (B=0, G=200, R=220)
        (200, 100, 50),  // teal-ish
        (180, 50, 200),  // magenta-ish
    ];

    let box_w = width / 4;
    let box_h = height / 4;
    let margin = 20u32;

    for (i, &(b, g, r)) in colors.iter().enumerate() {
        let col = (i % 3) as u32;
        let row = (i / 3) as u32;
        let x0 = margin + col * (box_w + margin);
        let y0 = margin + row * (box_h + margin);

        for y in y0..core::cmp::min(y0 + box_h, height) {
            for x in x0..core::cmp::min(x0 + box_w, width) {
                let offset = (y * stride + x * FB_BPP) as usize;
                unsafe {
                    *fb_ptr.add(offset) = b;
                    *fb_ptr.add(offset + 1) = g;
                    *fb_ptr.add(offset + 2) = r;
                    *fb_ptr.add(offset + 3) = 255;
                }
            }
        }
    }

    // Draw a 2px white border around the entire framebuffer.
    let white = [255u8, 255, 255, 255]; // BGRA
    for y in 0..height {
        for x in 0..width {
            if x < 2 || x >= width - 2 || y < 2 || y >= height - 2 {
                let offset = (y * stride + x * FB_BPP) as usize;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        white.as_ptr(),
                        fb_ptr.add(offset),
                        4,
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read device info from shared memory.
    let mmio_pa = unsafe { core::ptr::read_volatile(SHM as *const u64) };
    let irq = unsafe { core::ptr::read_volatile(SHM.add(8) as *const u32) };

    // Map the MMIO region (same sub-page alignment as virtio-blk).
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000);
    if page_va < 0 {
        sys::write(b"virtio-gpu: device_map failed\n");
        sys::exit();
    }

    let device = virtio::Device::new(page_va as usize + page_offset as usize);

    // Negotiate features (accept none for 2D).
    if !device.negotiate() {
        sys::write(b"virtio-gpu: negotiate failed\n");
        sys::exit();
    }

    // Register for interrupts.
    let irq_handle = sys::interrupt_register(irq);
    if irq_handle < 0 {
        sys::write(b"virtio-gpu: interrupt_register failed\n");
        sys::exit();
    }
    let irq_handle = irq_handle as u8;

    // Setup the control virtqueue (queue 0).
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_CONTROL),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa);
    if vq_va < 0 {
        sys::write(b"virtio-gpu: dma_alloc (vq) failed\n");
        sys::exit();
    }
    let vq_bytes = (1usize << vq_order) * 4096;
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };

    let mut vq = virtio::Virtqueue::new(queue_size, vq_va as usize, vq_pa);
    device.setup_queue(
        VIRTQ_CONTROL,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    sys::write(b"  \xF0\x9F\x96\xA5\xEF\xB8\x8F  virtio - gpu ready\n");

    // Query display dimensions.
    let (width, height) = get_display_info(&device, &mut vq, irq_handle);
    sys::write(b"     display ");
    print_u32(width);
    sys::write(b"x");
    print_u32(height);
    sys::write(b"\n");

    // Allocate framebuffer in DMA memory.
    let fb_bytes = (width * height * FB_BPP) as usize;
    let fb_pages = (fb_bytes + 4095) / 4096;
    let fb_order = (fb_pages.next_power_of_two().trailing_zeros()) as u32;
    let mut fb_pa: u64 = 0;
    let fb_va = sys::dma_alloc(fb_order, &mut fb_pa);
    if fb_va < 0 {
        sys::write(b"virtio-gpu: dma_alloc (fb) failed\n");
        sys::exit();
    }
    let fb_alloc_bytes = (1usize << fb_order) * 4096;
    unsafe { core::ptr::write_bytes(fb_va as *mut u8, 0, fb_alloc_bytes) };

    // Create a 2D resource.
    if !resource_create_2d(&device, &mut vq, irq_handle, FB_RESOURCE_ID, width, height) {
        sys::write(b"virtio-gpu: resource_create_2d failed\n");
        sys::exit();
    }

    // Attach guest memory to the resource.
    if !attach_backing(
        &device,
        &mut vq,
        irq_handle,
        FB_RESOURCE_ID,
        fb_pa,
        fb_bytes as u32,
    ) {
        sys::write(b"virtio-gpu: attach_backing failed\n");
        sys::exit();
    }

    // Bind resource to scanout 0.
    if !set_scanout(
        &device,
        &mut vq,
        irq_handle,
        SCANOUT_ID,
        FB_RESOURCE_ID,
        width,
        height,
    ) {
        sys::write(b"virtio-gpu: set_scanout failed\n");
        sys::exit();
    }

    // Draw a test pattern into the framebuffer.
    draw_test_pattern(fb_va as *mut u8, width, height);

    // Transfer the framebuffer to host and flush to display.
    if !transfer_to_host(
        &device,
        &mut vq,
        irq_handle,
        FB_RESOURCE_ID,
        0,
        0,
        width,
        height,
    ) {
        sys::write(b"virtio-gpu: transfer_to_host failed\n");
        sys::exit();
    }

    if !resource_flush(
        &device,
        &mut vq,
        irq_handle,
        FB_RESOURCE_ID,
        0,
        0,
        width,
        height,
    ) {
        sys::write(b"virtio-gpu: resource_flush failed\n");
        sys::exit();
    }

    sys::write(b"     test pattern displayed\n");

    // Signal completion and exit. The framebuffer remains visible because
    // the host resource persists until the guest is shut down.
    sys::channel_signal(0);
    sys::exit();
}
