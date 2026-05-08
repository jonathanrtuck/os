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

mod atlas;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use atlas::GlyphAtlas;
use fonts::rasterize::{RasterBuffer, RasterScratch};
use ipc::server::{Dispatch, Incoming};
use render::CommandWriter;
use scene::{Content, NULL, NodeId, SCENE_SIZE, SceneReader};

static FONT_DATA: &[u8] = include_bytes!("../../../../../assets/jetbrains-mono.ttf");

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;

// ── MSL shader source ───────────────────────────────────────────────

const MSL_SOURCE: &[u8] = b"
#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    float2 position [[attribute(0)]];
    float2 texCoord [[attribute(1)]];
    float4 color    [[attribute(2)]];
};

struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
    float4 color;
};

vertex VertexOut vertex_main(VertexIn in [[stage_in]]) {
    VertexOut out;
    out.position = float4(in.position, 0.0, 1.0);
    out.texCoord = in.texCoord;
    out.color = in.color;
    return out;
}

float3 srgb_to_linear(float3 s) {
    return select(
        pow((s + 0.055) / 1.055, float3(2.4)),
        s / 12.92,
        s <= 0.04045
    );
}

fragment float4 fragment_solid(VertexOut in [[stage_in]]) {
    return float4(srgb_to_linear(in.color.rgb), in.color.a);
}

fragment float4 fragment_glyph(
    VertexOut in [[stage_in]],
    texture2d<float> tex [[texture(0)]],
    sampler s [[sampler(0)]]
) {
    float alpha = tex.sample(s, in.texCoord).r;
    return float4(srgb_to_linear(in.color.rgb), in.color.a * alpha);
}
";

const COLOR_WRITE_ALL: u8 = 0xF;
const H_LIBRARY: u32 = 1;
const H_VERTEX_FN: u32 = 2;
const H_FRAG_SOLID: u32 = 3;
const H_FRAG_GLYPH: u32 = 4;
const PIPE_SOLID: u32 = 10;
const PIPE_GLYPH: u32 = 11;
const TEX_ATLAS: u32 = 20;
const SAMPLER_NEAREST: u32 = 30;

const FILTER_NEAREST: u8 = 0;

const ATLAS_WIDTH: u16 = 2048;
const ATLAS_HEIGHT: u16 = 2048;

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
    solid_verts: alloc::vec::Vec<u8>,
    glyph_verts: alloc::vec::Vec<u8>,
    display_w: f32,
    display_h: f32,
}

impl FrameBuilder {
    fn new(display_w: f32, display_h: f32) -> Self {
        Self {
            solid_verts: alloc::vec::Vec::with_capacity(256 * 6 * VERTEX_SIZE),
            glyph_verts: alloc::vec::Vec::with_capacity(512 * 6 * VERTEX_SIZE),
            display_w,
            display_h,
        }
    }

    fn push_solid_quad(&mut self, quad: &[Vertex; 6]) {
        // SAFETY: Vertex is repr(C) with known layout.
        let bytes =
            unsafe { core::slice::from_raw_parts(quad.as_ptr() as *const u8, 6 * VERTEX_SIZE) };

        self.solid_verts.extend_from_slice(bytes);
    }

    fn push_rect(&mut self, px: f32, py: f32, pw: f32, ph: f32, color: scene::Color) {
        let quad = self.make_quad(px, py, pw, ph, color, [0.0; 2], [0.0; 2]);

        self.push_solid_quad(&quad);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_glyph_quad(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        color: scene::Color,
        uv0: [f32; 2],
        uv1: [f32; 2],
    ) {
        let quad = self.make_quad(px, py, pw, ph, color, uv0, uv1);
        // SAFETY: Vertex is repr(C) with known layout.
        let bytes =
            unsafe { core::slice::from_raw_parts(quad.as_ptr() as *const u8, 6 * VERTEX_SIZE) };

        self.glyph_verts.extend_from_slice(bytes);
    }

    #[allow(clippy::too_many_arguments)]
    fn make_quad(
        &self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        color: scene::Color,
        uv0: [f32; 2],
        uv1: [f32; 2],
    ) -> [Vertex; 6] {
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

        [
            Vertex {
                position: [x0, y0],
                tex_coord: [uv0[0], uv0[1]],
                color: c,
            },
            Vertex {
                position: [x0, y1],
                tex_coord: [uv0[0], uv1[1]],
                color: c,
            },
            Vertex {
                position: [x1, y1],
                tex_coord: [uv1[0], uv1[1]],
                color: c,
            },
            Vertex {
                position: [x0, y0],
                tex_coord: [uv0[0], uv0[1]],
                color: c,
            },
            Vertex {
                position: [x1, y1],
                tex_coord: [uv1[0], uv1[1]],
                color: c,
            },
            Vertex {
                position: [x1, y0],
                tex_coord: [uv1[0], uv0[1]],
                color: c,
            },
        ]
    }

    fn solid_count(&self) -> usize {
        self.solid_verts.len() / VERTEX_SIZE
    }

    fn glyph_count(&self) -> usize {
        self.glyph_verts.len() / VERTEX_SIZE
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

    // Batch 1: compile shaders, get functions, create both pipelines.
    let len = {
        let mut w = CommandWriter::new(dma_buf);

        w.compile_library(H_LIBRARY, MSL_SOURCE);
        w.get_function(H_VERTEX_FN, H_LIBRARY, b"vertex_main");
        w.get_function(H_FRAG_SOLID, H_LIBRARY, b"fragment_solid");
        w.get_function(H_FRAG_GLYPH, H_LIBRARY, b"fragment_glyph");

        w.create_render_pipeline(
            PIPE_SOLID,
            H_VERTEX_FN,
            H_FRAG_SOLID,
            true,
            COLOR_WRITE_ALL,
            false,
            1,
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );
        w.create_render_pipeline(
            PIPE_GLYPH,
            H_VERTEX_FN,
            H_FRAG_GLYPH,
            true,
            COLOR_WRITE_ALL,
            false,
            1,
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );

        w.create_texture(
            TEX_ATLAS,
            ATLAS_WIDTH,
            ATLAS_HEIGHT,
            render::PIXEL_FORMAT_R8_UNORM,
            0,
            1,
            render::TEX_USAGE_SHADER_READ,
        );
        w.create_sampler(SAMPLER_NEAREST, FILTER_NEAREST, FILTER_NEAREST);

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

const RASTER_BUF_SIZE: usize = 100 * 100;
const ATLAS_W_F: f32 = atlas::ATLAS_WIDTH as f32;
const ATLAS_H_F: f32 = atlas::ATLAS_HEIGHT as f32;
const NS_PER_MS: u64 = 1_000_000;
const FRAME_INTERVAL_NS: u64 = 16_666_667;

struct WalkContext {
    atlas: alloc::boxed::Box<GlyphAtlas>,
    scratch: alloc::boxed::Box<RasterScratch>,
    raster_buf: alloc::vec::Vec<u8>,
    ascent_px: f32,
    atlas_dirty: bool,
    now_tick: u64,
    next_deadline: u64,
}

fn evaluate_animation(anim: &scene::Animation, now_ns: u64) -> (u8, u64) {
    if !anim.is_active() {
        return (255, 0);
    }

    let elapsed_ns = now_ns.saturating_sub(anim.start_tick);
    let elapsed_ms = elapsed_ns / NS_PER_MS;
    let mut total_ms = 0u64;

    for i in 0..anim.phase_count as usize {
        total_ms += anim.phases[i].duration_ms as u64;
    }

    if total_ms == 0 {
        return (255, 0);
    }

    let cycle_ms = if anim.repeat == scene::RepeatMode::Loop {
        elapsed_ms % total_ms
    } else {
        elapsed_ms.min(total_ms)
    };
    let mut acc = 0u64;
    let mut prev_value = anim.phases[anim.phase_count as usize - 1].value;

    for i in 0..anim.phase_count as usize {
        let phase = &anim.phases[i];
        let phase_end = acc + phase.duration_ms as u64;

        if cycle_ms < phase_end {
            let phase_remaining_ms = phase_end - cycle_ms;

            if phase.duration_ms == 0 || phase.easing == scene::AnimationEasing::Linear {
                return (phase.value, now_ns + phase_remaining_ms * NS_PER_MS);
            }

            let phase_elapsed = cycle_ms - acc;
            let t = phase_elapsed as f32 / phase.duration_ms as f32;
            let eased = animation::ease(animation::Easing::EaseInOut, t);
            let v = prev_value as f32 + (phase.value as f32 - prev_value as f32) * eased;

            return (v as u8, now_ns + FRAME_INTERVAL_NS);
        }

        acc = phase_end;
        prev_value = phase.value;
    }

    (prev_value, 0)
}

fn walk_node(
    reader: &SceneReader<'_>,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    frame: &mut FrameBuilder,
    ctx: &mut WalkContext,
    is_root: bool,
) {
    let node = reader.node(node_id);
    let x = parent_x + scene::mpt_to_f32(node.x);
    let y = parent_y + scene::mpt_to_f32(node.y);
    let w = scene::umpt_to_f32(node.width);
    let h = scene::umpt_to_f32(node.height);
    let effective_opacity =
        if node.animation.is_active() && node.animation.target == scene::AnimationTarget::Opacity {
            let (val, deadline) = evaluate_animation(&node.animation, ctx.now_tick);

            if deadline > 0 && (ctx.next_deadline == 0 || deadline < ctx.next_deadline) {
                ctx.next_deadline = deadline;
            }

            val
        } else {
            node.opacity
        };

    if effective_opacity == 0 {
        return;
    }

    if !is_root && node.background.a > 0 {
        let mut bg = node.background;

        bg.a = ((bg.a as u16 * effective_opacity as u16) / 255) as u8;

        frame.push_rect(x, y, w, h, bg);
    }

    if let Content::Glyphs {
        color,
        glyphs,
        glyph_count,
        font_size,
        style_id,
    } = node.content
    {
        let glyph_data = reader.shaped_glyphs(glyphs, glyph_count);
        let baseline_y = y + ctx.ascent_px;
        let mut gx = x;

        for glyph in glyph_data {
            let advance = glyph.x_advance as f32 / 65536.0;

            if glyph.glyph_id > 0 {
                let entry = lookup_or_rasterize(ctx, glyph.glyph_id, font_size, style_id);

                if let Some(e) = entry.filter(|e| e.width > 0 && e.height > 0) {
                    let px = gx + e.bearing_x as f32;
                    let py = baseline_y - e.bearing_y as f32;
                    let pw = e.width as f32;
                    let ph = e.height as f32;
                    let u0 = e.u as f32 / ATLAS_W_F;
                    let v0 = e.v as f32 / ATLAS_H_F;
                    let u1 = (e.u + e.width) as f32 / ATLAS_W_F;
                    let v1 = (e.v + e.height) as f32 / ATLAS_H_F;

                    frame.push_glyph_quad(px, py, pw, ph, color, [u0, v0], [u1, v1]);
                }
            }

            gx += advance;
        }
    }

    let mut child = node.first_child;

    while child != NULL {
        walk_node(reader, child, x, y, frame, ctx, false);

        child = reader.node(child).next_sibling;
    }
}

fn lookup_or_rasterize(
    ctx: &mut WalkContext,
    glyph_id: u16,
    font_size: u16,
    style_id: u32,
) -> Option<atlas::AtlasEntry> {
    if let Some(entry) = ctx.atlas.lookup(glyph_id, font_size, style_id) {
        return Some(*entry);
    }

    let mut buf = RasterBuffer {
        data: &mut ctx.raster_buf,
        width: 100,
        height: 100,
    };

    let metrics = fonts::rasterize::rasterize(
        FONT_DATA,
        glyph_id,
        font_size,
        &mut buf,
        &mut ctx.scratch,
        1,
    )?;

    if metrics.width == 0 || metrics.height == 0 {
        ctx.atlas.insert(
            glyph_id,
            font_size,
            style_id,
            atlas::AtlasEntry {
                u: 0,
                v: 0,
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
            },
        );

        return Some(atlas::AtlasEntry {
            u: 0,
            v: 0,
            width: 0,
            height: 0,
            bearing_x: metrics.bearing_x as i16,
            bearing_y: metrics.bearing_y as i16,
        });
    }

    let ok = ctx.atlas.pack(
        glyph_id,
        font_size,
        style_id,
        metrics.width as u16,
        metrics.height as u16,
        metrics.bearing_x as i16,
        metrics.bearing_y as i16,
        &ctx.raster_buf[..metrics.width as usize * metrics.height as usize],
    );

    if ok {
        ctx.atlas_dirty = true;

        ctx.atlas.lookup(glyph_id, font_size, style_id).copied()
    } else {
        None
    }
}

// ── Compositor ──────────────────────────────────────────────────────

struct Compositor {
    device: virtio::Device,
    setup_vq: virtio::Virtqueue,
    render_vq: virtio::Virtqueue,
    irq_event: Handle,

    setup_dma: init::DmaBuf,
    render_dma: init::DmaBuf,
    setup_buf_size: usize,
    render_buf_size: usize,

    console_ep: Handle,
    display_w: u32,
    display_h: u32,

    scene_va: usize,
    frame_count: u32,
    walk_ctx: WalkContext,
    atlas_upload_y: u16,
}

impl Compositor {
    fn upload_atlas_dirty(&mut self) {
        if !self.walk_ctx.atlas_dirty {
            return;
        }

        let atlas = &self.walk_ctx.atlas;
        let max_y = atlas.row_y + atlas.row_h;

        if max_y == 0 {
            return;
        }

        let start_y = self.atlas_upload_y;
        let height = max_y - start_y;

        if height == 0 {
            self.walk_ctx.atlas_dirty = false;

            return;
        }

        // SAFETY: setup_dma.va is a valid DMA allocation of setup_buf_size bytes.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(self.setup_dma.va as *mut u8, self.setup_buf_size)
        };

        let row_bytes = atlas::ATLAS_WIDTH as usize;
        let cmd_overhead = render::HEADER_SIZE + 16;
        let max_data_per_submit = self.setup_buf_size - cmd_overhead;
        let max_rows_per_submit = max_data_per_submit / row_bytes;

        let mut y = start_y;

        while y < max_y {
            let rows = ((max_y - y) as usize).min(max_rows_per_submit) as u16;
            let src_offset = y as usize * row_bytes;
            let pixel_count = rows as usize * row_bytes;
            let pixel_data = &atlas.pixels[src_offset..src_offset + pixel_count];

            let len = {
                let mut w = CommandWriter::new(dma_buf);

                w.upload_texture_region(
                    TEX_ATLAS,
                    0,
                    y,
                    atlas::ATLAS_WIDTH as u16,
                    rows,
                    row_bytes as u32,
                    pixel_data,
                );

                w.len()
            };

            submit_and_wait(
                &self.device,
                &mut self.setup_vq,
                self.irq_event,
                render::VIRTQ_SETUP,
                self.setup_dma.pa,
                len,
            );

            y += rows;
        }

        self.atlas_upload_y = atlas.row_y;
        self.walk_ctx.atlas_dirty = false;
    }

    fn render_frame(&mut self) -> u64 {
        if self.scene_va == 0 {
            return 0;
        }

        let now = abi::system::clock_read().unwrap_or(0);

        self.walk_ctx.now_tick = now;
        self.walk_ctx.next_deadline = 0;

        // SAFETY: scene_va is a valid RO mapping of at least SCENE_SIZE bytes.
        let scene_buf =
            unsafe { core::slice::from_raw_parts(self.scene_va as *const u8, SCENE_SIZE) };
        let reader = SceneReader::new(scene_buf);
        let root = reader.root();

        if reader.node_count() == 0 || root == NULL {
            return 0;
        }

        let root_node = reader.node(root);
        let bg = root_node.background;
        let mut frame = FrameBuilder::new(self.display_w as f32, self.display_h as f32);

        walk_node(
            &reader,
            root,
            0.0,
            0.0,
            &mut frame,
            &mut self.walk_ctx,
            true,
        );

        self.upload_atlas_dirty();

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

            let sc = frame.solid_count();

            if sc > 0 {
                w.set_render_pipeline(PIPE_SOLID);
                w.set_vertex_bytes(0, &frame.solid_verts);
                w.draw_primitives(render::PRIM_TRIANGLE, 0, sc as u32);
            }

            let gc = frame.glyph_count();

            if gc > 0 {
                w.set_render_pipeline(PIPE_GLYPH);
                w.set_fragment_texture(TEX_ATLAS, 0);
                w.set_fragment_sampler(SAMPLER_NEAREST, 0);
                w.set_vertex_bytes(0, &frame.glyph_verts);
                w.draw_primitives(render::PRIM_TRIANGLE, 0, gc as u32);
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

        self.walk_ctx.next_deadline
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

    let fm = fonts::metrics::font_metrics(FONT_DATA);
    let ascent_px = match fm {
        Some(ref m) => m.ascent as f32 * 14.0 / m.units_per_em as f32,
        None => 11.0,
    };
    let walk_ctx = WalkContext {
        atlas: GlyphAtlas::new_boxed(),
        scratch: alloc::boxed::Box::new(RasterScratch::zeroed()),
        raster_buf: alloc::vec![0u8; RASTER_BUF_SIZE],
        ascent_px,
        atlas_dirty: false,
        now_tick: 0,
        next_deadline: 0,
    };

    console::write(console_ep, b"render: atlas ready\n");

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
        walk_ctx,
        atlas_upload_y: 0,
    };
    let mut next_deadline: u64 = 0;

    loop {
        match ipc::server::serve_one_timed(own_ep, &mut compositor, next_deadline) {
            Ok(()) => {
                next_deadline = compositor.walk_ctx.next_deadline;
            }
            Err(abi::types::SyscallError::TimedOut) => {
                next_deadline = compositor.render_frame();
            }
            Err(_) => break,
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
