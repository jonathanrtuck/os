//! Metal render driver — GPU compositor for Metal-over-virtio.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!
//! Probes the virtio MMIO region for a Metal GPU device (device ID 22).
//! Sets up two virtqueues (setup + render), compiles shaders, creates
//! a render pipeline, and enters a serve loop. The presenter connects
//! via `comp::SETUP` (passing the scene graph VMO) and triggers frame
//! renders via `comp::RENDER`.

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};
use render::CommandWriter;
use scene::{Content, NULL, NodeId, SCENE_SIZE, SceneReader};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;

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

const COLOR_WRITE_ALL: u8 = 0xF;
const H_LIBRARY: u32 = 1;
const H_VERTEX_FN: u32 = 2;
const H_FRAGMENT_FN: u32 = 3;
const H_PIPELINE: u32 = 10;

const SETUP_BUF_PAGES: usize = 2;
const RENDER_BUF_PAGES: usize = 4;

// ── Vertex data ─────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    tex_coord: [f32; 2],
    color: [f32; 4],
}

const VERTEX_SIZE: usize = core::mem::size_of::<Vertex>();

struct FrameBuilder {
    verts: alloc::vec::Vec<u8>,
    display_w: f32,
    display_h: f32,
}

impl FrameBuilder {
    fn new(display_w: f32, display_h: f32) -> Self {
        Self {
            verts: alloc::vec::Vec::with_capacity(256 * 6 * VERTEX_SIZE),
            display_w,
            display_h,
        }
    }

    fn push_rect(&mut self, px: f32, py: f32, pw: f32, ph: f32, color: scene::Color) {
        let x0 = px / self.display_w * 2.0 - 1.0;
        let y0 = 1.0 - py / self.display_h * 2.0;
        let x1 = (px + pw) / self.display_w * 2.0 - 1.0;
        let y1 = 1.0 - (py + ph) / self.display_h * 2.0;
        let c = [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            color.a as f32 / 255.0,
        ];
        let zero = [0.0f32; 2];
        let quad = [
            Vertex {
                position: [x0, y0],
                tex_coord: zero,
                color: c,
            },
            Vertex {
                position: [x0, y1],
                tex_coord: zero,
                color: c,
            },
            Vertex {
                position: [x1, y1],
                tex_coord: zero,
                color: c,
            },
            Vertex {
                position: [x0, y0],
                tex_coord: zero,
                color: c,
            },
            Vertex {
                position: [x1, y1],
                tex_coord: zero,
                color: c,
            },
            Vertex {
                position: [x1, y0],
                tex_coord: zero,
                color: c,
            },
        ];
        // SAFETY: Vertex is repr(C) with known layout.
        let bytes =
            unsafe { core::slice::from_raw_parts(quad.as_ptr() as *const u8, 6 * VERTEX_SIZE) };

        self.verts.extend_from_slice(bytes);
    }

    fn vertex_count(&self) -> usize {
        self.verts.len() / VERTEX_SIZE
    }
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
    setup_dma: &init::DmaBuf,
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
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );

        w.len()
    };

    submit_and_wait(
        device,
        setup_vq,
        irq_event,
        render::VIRTQ_SETUP,
        setup_dma.pa,
        len,
    );
}

// ── Scene graph → vertex data ───────────────────────────────────────

fn walk_node(
    reader: &SceneReader<'_>,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    frame: &mut FrameBuilder,
    is_root: bool,
) {
    let node = reader.node(node_id);
    let x = parent_x + scene::mpt_to_f32(node.x);
    let y = parent_y + scene::mpt_to_f32(node.y);
    let w = scene::umpt_to_f32(node.width);
    let h = scene::umpt_to_f32(node.height);

    if !is_root && node.background.a > 0 {
        frame.push_rect(x, y, w, h, node.background);
    }

    if let Content::Glyphs {
        color,
        glyphs,
        glyph_count,
        ..
    } = node.content
    {
        let glyph_data = reader.shaped_glyphs(glyphs, glyph_count);
        let mut gx = x;

        for glyph in glyph_data {
            let advance = glyph.x_advance as f32 / 65536.0;

            if advance > 0.0 {
                frame.push_rect(gx + 1.0, y + 3.0, advance - 2.0, h - 6.0, color);
            }

            gx += advance;
        }
    }

    let mut child = node.first_child;

    while child != NULL {
        walk_node(reader, child, x, y, frame, false);

        child = reader.node(child).next_sibling;
    }
}

// ── Compositor ──────────────────────────────────────────────────────

struct Compositor {
    device: virtio::Device,
    #[allow(dead_code)]
    setup_vq: virtio::Virtqueue,
    render_vq: virtio::Virtqueue,
    irq_event: Handle,

    #[allow(dead_code)]
    setup_dma: init::DmaBuf,
    render_dma: init::DmaBuf,
    #[allow(dead_code)]
    setup_buf_size: usize,
    render_buf_size: usize,

    console_ep: Handle,
    display_w: u32,
    display_h: u32,

    scene_va: usize,
    frame_count: u32,
}

impl Compositor {
    fn render_frame(&mut self) {
        if self.scene_va == 0 {
            return;
        }

        // SAFETY: scene_va is a valid RO mapping of at least SCENE_SIZE bytes.
        let scene_buf =
            unsafe { core::slice::from_raw_parts(self.scene_va as *const u8, SCENE_SIZE) };
        let reader = SceneReader::new(scene_buf);
        let root = reader.root();

        if reader.node_count() == 0 || root == NULL {
            return;
        }

        let root_node = reader.node(root);
        let bg = root_node.background;
        let mut frame = FrameBuilder::new(self.display_w as f32, self.display_h as f32);

        walk_node(&reader, root, 0.0, 0.0, &mut frame, true);

        // SAFETY: render_dma.va is a valid DMA allocation of render_buf_size bytes.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(self.render_dma.va as *mut u8, self.render_buf_size)
        };
        let len = {
            let mut w = CommandWriter::new(dma_buf);

            w.begin_render_pass(
                render::DRAWABLE_HANDLE,
                0,
                0,
                render::LOAD_CLEAR,
                render::STORE_STORE,
                0,
                0,
                bg.r as f32 / 255.0,
                bg.g as f32 / 255.0,
                bg.b as f32 / 255.0,
                1.0,
            );
            w.set_render_pipeline(H_PIPELINE);

            let vc = frame.vertex_count();

            if vc > 0 {
                w.set_vertex_bytes(0, &frame.verts);
                w.draw_primitives(render::PRIM_TRIANGLE, 0, vc as u32);
            }

            w.end_render_pass();
            w.present_and_commit(self.frame_count);

            w.len()
        };

        submit_and_wait(
            &self.device,
            &mut self.render_vq,
            self.irq_event,
            render::VIRTQ_RENDER,
            self.render_dma.pa,
            len,
        );

        self.frame_count += 1;
    }
}

impl Dispatch for Compositor {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            render::comp::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let vmo = Handle(msg.handles[0]);

                match abi::vmo::map(vmo, 0, Rights::READ_MAP) {
                    Ok(va) => {
                        self.scene_va = va;

                        console::write(self.console_ep, b"render: scene connected\n");

                        let reply = render::comp::SetupReply {
                            display_width: self.display_w,
                            display_height: self.display_h,
                        };
                        let mut data = [0u8; render::comp::SetupReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }
            render::comp::RENDER => {
                self.render_frame();

                let _ = msg.reply_empty();
            }
            render::comp::GET_INFO => {
                let reply = render::comp::InfoReply {
                    display_width: self.display_w,
                    display_height: self.display_h,
                    frame_count: self.frame_count,
                };
                let mut data = [0u8; render::comp::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
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
    let setup_qsize = device
        .queue_max_size(render::VIRTQ_SETUP)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let render_qsize = device
        .queue_max_size(render::VIRTQ_RENDER)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let setup_vq_bytes = virtio::Virtqueue::total_bytes(setup_qsize);
    let setup_vq_alloc = setup_vq_bytes.next_multiple_of(PAGE_SIZE);
    let setup_vq_dma = match init::request_dma(HANDLE_INIT_EP, setup_vq_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(4),
    };

    // SAFETY: setup_vq_dma.va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(setup_vq_dma.va as *mut u8, 0, setup_vq_alloc) };

    let mut setup_vq = virtio::Virtqueue::new(setup_qsize, setup_vq_dma.va, setup_vq_dma.pa);

    device.setup_queue(
        render::VIRTQ_SETUP,
        setup_qsize,
        setup_vq.desc_pa(),
        setup_vq.avail_pa(),
        setup_vq.used_pa(),
    );

    let render_vq_bytes = virtio::Virtqueue::total_bytes(render_qsize);
    let render_vq_alloc = render_vq_bytes.next_multiple_of(PAGE_SIZE);
    let render_vq_dma = match init::request_dma(HANDLE_INIT_EP, render_vq_alloc) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(5),
    };

    // SAFETY: render_vq_dma.va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(render_vq_dma.va as *mut u8, 0, render_vq_alloc) };

    let render_vq = virtio::Virtqueue::new(render_qsize, render_vq_dma.va, render_vq_dma.pa);

    device.setup_queue(
        render::VIRTQ_RENDER,
        render_qsize,
        render_vq.desc_pa(),
        render_vq.avail_pa(),
        render_vq.used_pa(),
    );

    let setup_buf_size = PAGE_SIZE * SETUP_BUF_PAGES;
    let setup_dma = match init::request_dma(HANDLE_INIT_EP, setup_buf_size) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(6),
    };
    let render_buf_size = PAGE_SIZE * RENDER_BUF_PAGES;
    let render_dma = match init::request_dma(HANDLE_INIT_EP, render_buf_size) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(7),
    };

    device.driver_ok();

    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };
    let irq_num = virtio::SPI_BASE_INTID + metal_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(9);
    }

    let console_ep = match name::lookup(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(10),
    };

    console::write_u32(console_ep, b"render: display w=", display_w);
    console::write_u32(console_ep, b"render: display h=", display_h);

    setup_pipeline(
        &device,
        &mut setup_vq,
        irq_event,
        &setup_dma,
        setup_buf_size,
    );

    console::write(console_ep, b"render: pipeline ready\n");

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(11),
    };

    name::register(HANDLE_NS_EP, b"render", own_ep);
    console::write(console_ep, b"render: ready\n");

    let mut compositor = Compositor {
        device,
        setup_vq,
        render_vq,
        irq_event,
        setup_dma,
        render_dma,
        setup_buf_size,
        render_buf_size,
        console_ep,
        display_w,
        display_h,
        scene_va: 0,
        frame_count: 0,
    };

    ipc::server::serve(own_ep, &mut compositor);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
