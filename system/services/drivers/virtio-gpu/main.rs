//! Userspace virtio-gpu 2D driver.
//!
//! Receives device info (MMIO PA, IRQ) and framebuffer info (PA, dimensions)
//! from init via channel shared memory. Initializes the GPU device, binds
//! the framebuffer as a 2D resource, and presents it to the display.
//!
//! # Present loop
//!
//! After initial device setup, enters an event loop: waits for MSG_PRESENT
//! messages from the compositor on channel 1, then transfers the framebuffer
//! to the host and flushes.
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
//! Receives device and framebuffer config via IPC ring buffer from init.

#![no_std]
#![no_main]

/// Channel shared memory base (first channel in our address space).
const CHANNEL_SHM_BASE: usize = 0x4000_0000;
// Protocol message types (must match init/compositor definitions).
const MSG_GPU_CONFIG: u32 = 2;
const MSG_PRESENT: u32 = 20;
/// Control virtqueue index.
const VIRTQ_CONTROL: u32 = 0;
/// Resource ID for our framebuffer (arbitrary nonzero).
const FB_RESOURCE_ID: u32 = 1;
/// Scanout index (first/only display).
const SCANOUT_ID: u32 = 0;
/// Bytes per pixel (BGRA8888).
const FB_BPP: u32 = 4;
// virtio-gpu command types (enum auto-increments from 0x0100).
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
// virtio-gpu response types.
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
// virtio-gpu pixel format (B8G8R8A8_UNORM).
const FORMAT_B8G8R8A8_UNORM: u32 = 1;
// Handle indices:
// Handle 0: init config channel
// Handle 1: compositor present channel
// Handle 2: IRQ handle (allocated by interrupt_register)
const PRESENT_HANDLE: u8 = 1;

#[repr(C)]
struct AttachBacking {
    header: CtrlHeader,
    resource_id: u32,
    nr_entries: u32,
}
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
struct DmaBuf {
    va: usize,
    pa: u64,
    order: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct GpuConfig {
    mmio_pa: u64,
    irq: u32,
    _pad: u32,
    fb_pa: u64,
    fb_width: u32,
    fb_height: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    _padding: u32,
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

impl DmaBuf {
    fn alloc(order: u32) -> DmaBuf {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
            sys::print(b"virtio-gpu: dma_alloc failed\n");
            sys::exit();
        });

        unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * 4096) };

        DmaBuf { va, pa, order }
    }
    fn free(self) {
        let _ = sys::dma_free(self.va as u64, self.order);
    }
}

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
fn ctrl_header(cmd_type: u32) -> CtrlHeader {
    CtrlHeader {
        cmd_type,
        flags: 0,
        fence_id: 0,
        ctx_id: 0,
        _padding: 0,
    }
}
/// Compute the base VA of channel N's shared pages.
fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}
fn get_display_info(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
) -> (u32, u32) {
    let cmd = DmaBuf::alloc(0);
    let cmd_ptr = cmd.va as *mut CtrlHeader;

    unsafe { core::ptr::write(cmd_ptr, ctrl_header(CMD_GET_DISPLAY_INFO)) };

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
/// Send a command through the control virtqueue and wait for the response.
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
    let resp_header = resp_va as *const CtrlHeader;

    unsafe { core::ptr::read_volatile(&(*resp_header).cmd_type as *const u32) }
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
fn resource_flush(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
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
                rect_x: 0,
                rect_y: 0,
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
/// Flush resource to display using a pre-allocated DMA buffer.
fn resource_flush_reuse(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    cmd: &DmaBuf,
    resource_id: u32,
    width: u32,
    height: u32,
) {
    let ptr = cmd.va as *mut u8;

    unsafe {
        core::ptr::write_bytes(ptr, 0, 512);
        core::ptr::write(
            ptr as *mut ResourceFlush,
            ResourceFlush {
                header: ctrl_header(CMD_RESOURCE_FLUSH),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                resource_id,
                _padding: 0,
            },
        );
    }

    gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<ResourceFlush>() as u32,
        cmd.pa + 512,
        cmd.va + 512,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
}
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
fn transfer_to_host(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    resource_id: u32,
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
                rect_x: 0,
                rect_y: 0,
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
/// Transfer framebuffer to host using a pre-allocated DMA buffer.
fn transfer_to_host_reuse(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: u8,
    cmd: &DmaBuf,
    resource_id: u32,
    width: u32,
    height: u32,
) {
    let ptr = cmd.va as *mut u8;

    unsafe {
        core::ptr::write_bytes(ptr, 0, 512);
        core::ptr::write(
            ptr as *mut TransferToHost2d,
            TransferToHost2d {
                header: ctrl_header(CMD_TRANSFER_TO_HOST_2D),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                offset: 0,
                resource_id,
                _padding: 0,
            },
        );
    }

    gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        core::mem::size_of::<TransferToHost2d>() as u32,
        cmd.pa + 512,
        cmd.va + 512,
        core::mem::size_of::<CtrlHeader>() as u32,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read GPU config from ring buffer (first message, sent by init).
    let ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_GPU_CONFIG {
        sys::print(b"virtio-gpu: no config message\n");
        sys::exit();
    }

    let config: GpuConfig = unsafe { msg.payload_as() };
    let mmio_pa = config.mmio_pa;
    let irq = config.irq;
    let fb_pa = config.fb_pa;
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;
    let fb_size = fb_width * fb_height * FB_BPP;
    // Map the MMIO region (sub-page alignment for virtio-mmio).
    let page_offset = mmio_pa & 0xFFF;
    let page_pa = mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000).unwrap_or_else(|_| {
        sys::print(b"virtio-gpu: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    if !device.negotiate() {
        sys::print(b"virtio-gpu: negotiate failed\n");
        sys::exit();
    }

    // IRQ handle goes into slot 2 (after init channel=0, present channel=1).
    let irq_handle = sys::interrupt_register(irq).unwrap_or_else(|_| {
        sys::print(b"virtio-gpu: interrupt_register failed\n");
        sys::exit();
    });
    // Setup the control virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_CONTROL),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-gpu: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_bytes = (1usize << vq_order) * 4096;

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

    sys::print(b"  \xF0\x9F\x96\xA5\xEF\xB8\x8F  virtio-gpu ready\n");

    // Query actual display dimensions (informational).
    let (disp_w, disp_h) = get_display_info(&device, &mut vq, irq_handle);

    sys::print(b"     display ");

    print_u32(if disp_w > 0 { disp_w } else { fb_width });

    sys::print(b"x");

    print_u32(if disp_h > 0 { disp_h } else { fb_height });

    sys::print(b"\n");

    // Use init-provided dimensions for the resource (matches framebuffer).
    let width = fb_width;
    let height = fb_height;

    // -----------------------------------------------------------------------
    // One-time device setup: create resource, attach backing, set scanout.
    // -----------------------------------------------------------------------
    if !resource_create_2d(&device, &mut vq, irq_handle, FB_RESOURCE_ID, width, height) {
        sys::print(b"virtio-gpu: resource_create_2d failed\n");
        sys::exit();
    }
    if !attach_backing(&device, &mut vq, irq_handle, FB_RESOURCE_ID, fb_pa, fb_size) {
        sys::print(b"virtio-gpu: attach_backing failed\n");
        sys::exit();
    }
    if !set_scanout(
        &device,
        &mut vq,
        irq_handle,
        SCANOUT_ID,
        FB_RESOURCE_ID,
        width,
        height,
    ) {
        sys::print(b"virtio-gpu: set_scanout failed\n");
        sys::exit();
    }

    // Pre-allocate a DMA page for the present loop commands. Reusing one
    // page eliminates 4 syscalls (2 alloc + 2 free) per frame.
    let present_cmd = DmaBuf::alloc(0);

    sys::print(b"     device setup complete, entering present loop\n");

    // Channel 1: compositor present commands (endpoint 1 = receive side).
    let present_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    // -----------------------------------------------------------------------
    // Present loop: wait for compositor → transfer → flush → repeat
    // -----------------------------------------------------------------------
    loop {
        // Wait for a present command from the compositor.
        let _ = sys::wait(&[PRESENT_HANDLE], u64::MAX);

        // Drain all pending present messages (coalesce multiple into one
        // transfer+flush — the framebuffer already has the latest contents).
        while present_ch.try_recv(&mut msg) {
            // Consume but don't need to inspect — MSG_PRESENT has no payload.
        }

        // Transfer and flush using pre-allocated command buffer.
        transfer_to_host_reuse(
            &device,
            &mut vq,
            irq_handle,
            &present_cmd,
            FB_RESOURCE_ID,
            width,
            height,
        );
        resource_flush_reuse(
            &device,
            &mut vq,
            irq_handle,
            &present_cmd,
            FB_RESOURCE_ID,
            width,
            height,
        );
    }
}
