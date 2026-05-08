//! Metal render driver — GPU compositor for Metal-over-virtio.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!
//! Probes the virtio MMIO region for a Metal GPU device (device ID 22).
//! Sets up two virtqueues (setup + render), compiles shaders, creates
//! a render pipeline, and renders frames. Registers with the name
//! service as "render".
//!
//! Phase 2.4 scope: solid-color frame rendering verified via hypervisor
//! screenshot capture. Full scene graph rendering comes in Phase 3-4.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights, SyscallError};
use protocol::metal::{self, CommandWriter};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;
const MSG_SIZE: usize = 128;

// ── MSL shader source ───────────────────────────────────────────────

const MSL_SOLID_COLOR: &[u8] = b"
#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    float2 position [[attribute(0)]];
    float2 texCoord [[attribute(1)]];
    float4 color    [[attribute(2)]];
};

struct VertexOut {
    float4 position [[position]];
    float4 color;
};

vertex VertexOut vertex_main(VertexIn in [[stage_in]]) {
    VertexOut out;
    out.position = float4(in.position, 0.0, 1.0);
    out.color = in.color;
    return out;
}

fragment float4 fragment_main(VertexOut in [[stage_in]]) {
    return in.color;
}
";

// Guest-assigned Metal object handle IDs.
const COLOR_WRITE_ALL: u8 = 0xF;

const H_LIBRARY: u32 = 1;
const H_VERTEX_FN: u32 = 2;
const H_FRAGMENT_FN: u32 = 3;
const H_PIPELINE: u32 = 10;

// ── DMA buffer layout ───────────────────────────────────────────────

const SETUP_BUF_PAGES: usize = 2;
const RENDER_BUF_PAGES: usize = 1;

struct DmaBuf {
    va: usize,
    pa: u64,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn request_dma(init_ep: Handle, size: usize) -> Result<DmaBuf, SyscallError> {
    let mut msg = [0u8; MSG_SIZE];
    let method = protocol::bootstrap::DMA_ALLOC;

    msg[0..4].copy_from_slice(&method.to_le_bytes());

    let req = protocol::bootstrap::DmaAllocRequest { size: size as u32 };

    req.write_to(&mut msg[4..8]);

    let mut recv_handles = [0u32; 4];
    let result = abi::ipc::call(init_ep, &mut msg, 8, &[], &mut recv_handles)?;

    if result.handle_count == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let vmo = Handle(recv_handles[0]);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let va = abi::vmo::map(vmo, 0, rw)?;

    Ok(DmaBuf { va, pa: va as u64 })
}

fn register_with_name_service(ns_ep: Handle, name: &[u8], own_ep: Handle) {
    let dup = match abi::handle::dup(own_ep, abi::types::Rights::ALL) {
        Ok(h) => h,
        Err(_) => return,
    };
    let req = protocol::name_service::NameRequest::new(name);
    let mut buf = [0u8; MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, protocol::name_service::REGISTER, &req.name);
    let _ = abi::ipc::call(ns_ep, &mut buf, total, &[dup.0], &mut []);
}

fn lookup_service(ns_ep: Handle, name: &[u8]) -> Result<Handle, SyscallError> {
    let req = protocol::name_service::NameRequest::new(name);
    let mut buf = [0u8; MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, protocol::name_service::LOOKUP, &req.name);
    let mut recv_handles = [0u32; 4];
    let result = abi::ipc::call(ns_ep, &mut buf, total, &[], &mut recv_handles)?;

    if result.handle_count == 0 {
        return Err(SyscallError::NotFound);
    }

    Ok(Handle(recv_handles[0]))
}

fn console_write(console_ep: Handle, text: &[u8]) {
    let mut buf = [0u8; MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, 1, text);
    let _ = abi::ipc::call(console_ep, &mut buf, total, &[], &mut []);
}

fn console_write_u32(console_ep: Handle, prefix: &[u8], n: u32) {
    let mut text = [0u8; 80];
    let plen = prefix.len().min(60);

    text[..plen].copy_from_slice(&prefix[..plen]);

    let nlen = format_u32(n, &mut text[plen..]);

    text[plen + nlen] = b'\n';

    console_write(console_ep, &text[..plen + nlen + 1]);
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

// ── Virtqueue submission ────────────────────────────────────────────

fn submit_and_wait(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_event: Handle,
    queue_index: u32,
    dma_pa: u64,
    cmd_len: usize,
) {
    vq.push(dma_pa, cmd_len as u32, false);
    device.notify(queue_index);

    let _ = abi::event::wait(&[(irq_event, 0x1)]);
    let _ = abi::event::clear(irq_event, 0x1);

    device.ack_interrupt();
    vq.pop_used();
}

// ── GPU pipeline setup ──────────────────────────────────────────────

fn setup_pipeline(
    device: &virtio::Device,
    setup_vq: &mut virtio::Virtqueue,
    irq_event: Handle,
    setup_dma: &DmaBuf,
    buf_size: usize,
) {
    // SAFETY: setup_dma.va is a valid DMA allocation of buf_size bytes.
    let dma_buf = unsafe { core::slice::from_raw_parts_mut(setup_dma.va as *mut u8, buf_size) };
    let len = {
        let mut w = CommandWriter::new(dma_buf);

        w.compile_library(H_LIBRARY, MSL_SOLID_COLOR);
        w.get_function(H_VERTEX_FN, H_LIBRARY, b"vertex_main");
        w.get_function(H_FRAGMENT_FN, H_LIBRARY, b"fragment_main");
        w.create_render_pipeline(
            H_PIPELINE,
            H_VERTEX_FN,
            H_FRAGMENT_FN,
            false,
            COLOR_WRITE_ALL,
            false,
            1,
            metal::PIXEL_FORMAT_BGRA8_SRGB,
        );

        w.len()
    };

    submit_and_wait(
        device,
        setup_vq,
        irq_event,
        metal::VIRTQ_SETUP,
        setup_dma.pa,
        len,
    );
}

// ── Vertex data for a fullscreen quad ───────────────────────────────

#[repr(C)]
struct Vertex {
    position: [f32; 2],
    tex_coord: [f32; 2],
    color: [f32; 4],
}

const VERTEX_SIZE: usize = core::mem::size_of::<Vertex>();

fn fullscreen_vertices(r: f32, g: f32, b: f32, a: f32, buf: &mut [u8]) -> usize {
    let verts = [
        // Triangle 1: top-left, bottom-left, bottom-right
        Vertex {
            position: [-1.0, 1.0],
            tex_coord: [0.0, 0.0],
            color: [r, g, b, a],
        },
        Vertex {
            position: [-1.0, -1.0],
            tex_coord: [0.0, 1.0],
            color: [r, g, b, a],
        },
        Vertex {
            position: [1.0, -1.0],
            tex_coord: [1.0, 1.0],
            color: [r, g, b, a],
        },
        // Triangle 2: top-left, bottom-right, top-right
        Vertex {
            position: [-1.0, 1.0],
            tex_coord: [0.0, 0.0],
            color: [r, g, b, a],
        },
        Vertex {
            position: [1.0, -1.0],
            tex_coord: [1.0, 1.0],
            color: [r, g, b, a],
        },
        Vertex {
            position: [1.0, 1.0],
            tex_coord: [1.0, 0.0],
            color: [r, g, b, a],
        },
    ];

    let total = 6 * VERTEX_SIZE;

    // SAFETY: Vertex is #[repr(C)] with known size; total fits in buf.
    unsafe {
        let src = verts.as_ptr() as *const u8;
        core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), total);
    }

    total
}

// ── Render a single frame ───────────────────────────────────────────

fn render_frame(
    device: &virtio::Device,
    render_vq: &mut virtio::Virtqueue,
    irq_event: Handle,
    render_dma: &DmaBuf,
    buf_size: usize,
    frame_id: u32,
) {
    // SAFETY: render_dma.va is a valid DMA allocation of buf_size bytes.
    let dma_buf = unsafe { core::slice::from_raw_parts_mut(render_dma.va as *mut u8, buf_size) };
    let len = {
        let mut w = CommandWriter::new(dma_buf);

        w.begin_render_pass(
            metal::DRAWABLE_HANDLE,
            0,
            0,
            metal::LOAD_CLEAR,
            metal::STORE_STORE,
            0,
            0,
            0.13,
            0.13,
            0.14,
            1.0,
        );

        w.set_render_pipeline(H_PIPELINE);

        let mut verts = [0u8; 6 * VERTEX_SIZE];
        let vert_len = fullscreen_vertices(0.13, 0.13, 0.14, 1.0, &mut verts);

        w.set_vertex_bytes(0, &verts[..vert_len]);
        w.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
        w.end_render_pass();
        w.present_and_commit(frame_id);

        w.len()
    };

    submit_and_wait(
        device,
        render_vq,
        irq_event,
        metal::VIRTQ_RENDER,
        render_dma.pa,
        len,
    );
}

// ── Entry point ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };

    let (device, metal_slot) = match virtio::find_device(virtio_va, virtio::DEVICE_METAL) {
        Some(d) => d,
        None => abi::thread::exit(0xA0),
    };

    if !device.negotiate() {
        abi::thread::exit(3);
    }

    let display_w = device.config_read32(0x00);
    let display_h = device.config_read32(0x04);

    // Set up two virtqueues: setup (0) and render (1).
    let setup_qsize = device
        .queue_max_size(metal::VIRTQ_SETUP)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let render_qsize = device
        .queue_max_size(metal::VIRTQ_RENDER)
        .min(virtio::DEFAULT_QUEUE_SIZE);

    let setup_vq_bytes = virtio::Virtqueue::total_bytes(setup_qsize);
    let setup_vq_alloc = setup_vq_bytes.next_multiple_of(PAGE_SIZE);
    let setup_vq_dma = match request_dma(HANDLE_INIT_EP, setup_vq_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(4),
    };

    // SAFETY: setup_vq_dma.va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(setup_vq_dma.va as *mut u8, 0, setup_vq_alloc) };

    let mut setup_vq = virtio::Virtqueue::new(setup_qsize, setup_vq_dma.va, setup_vq_dma.pa);

    device.setup_queue(
        metal::VIRTQ_SETUP,
        setup_qsize,
        setup_vq.desc_pa(),
        setup_vq.avail_pa(),
        setup_vq.used_pa(),
    );

    let render_vq_bytes = virtio::Virtqueue::total_bytes(render_qsize);
    let render_vq_alloc = render_vq_bytes.next_multiple_of(PAGE_SIZE);
    let render_vq_dma = match request_dma(HANDLE_INIT_EP, render_vq_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: render_vq_dma.va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(render_vq_dma.va as *mut u8, 0, render_vq_alloc) };

    let mut render_vq = virtio::Virtqueue::new(render_qsize, render_vq_dma.va, render_vq_dma.pa);

    device.setup_queue(
        metal::VIRTQ_RENDER,
        render_qsize,
        render_vq.desc_pa(),
        render_vq.avail_pa(),
        render_vq.used_pa(),
    );

    // DMA buffers for command data.
    let setup_buf_size = PAGE_SIZE * SETUP_BUF_PAGES;
    let setup_dma = match request_dma(HANDLE_INIT_EP, setup_buf_size) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(6),
    };

    let render_buf_size = PAGE_SIZE * RENDER_BUF_PAGES;
    let render_dma = match request_dma(HANDLE_INIT_EP, render_buf_size) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(7),
    };

    device.driver_ok();

    // Bind IRQ.
    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    let irq_num = virtio::SPI_BASE_INTID + metal_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(9);
    }

    // Look up the console for status output.
    let console_ep = match lookup_service(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(10),
    };

    console_write_u32(console_ep, b"render: display ", display_w);
    console_write_u32(console_ep, b"render: display h=", display_h);

    // Compile shaders and create the render pipeline.
    setup_pipeline(
        &device,
        &mut setup_vq,
        irq_event,
        &setup_dma,
        setup_buf_size,
    );

    console_write(console_ep, b"render: pipeline ready\n");

    // Render verification frame — solid dark background.
    render_frame(
        &device,
        &mut render_vq,
        irq_event,
        &render_dma,
        render_buf_size,
        0,
    );

    console_write(console_ep, b"render: frame 0 presented\n");

    // Register with name service.
    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(11),
    };

    register_with_name_service(HANDLE_NS_EP, b"render", own_ep);

    console_write(console_ep, b"render: ready\n");

    // Idle serve loop — Phase 3-4 will add scene graph update handling.
    ipc::server::serve(own_ep, &mut StubServer);

    abi::thread::exit(0);
}

struct StubServer;

impl ipc::server::Dispatch for StubServer {
    fn dispatch(&mut self, msg: ipc::server::Incoming<'_>) {
        let _ = msg.reply_error(protocol::STATUS_UNSUPPORTED);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
