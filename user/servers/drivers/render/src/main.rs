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
mod path;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use atlas::GlyphAtlas;
use fonts::rasterize::{RasterBuffer, RasterScratch};
use ipc::server::{Dispatch, Incoming};
use render::CommandWriter;
use scene::{Content, NULL, NodeId, SCENE_SIZE, SceneReader};

// ── Font data — well-known style IDs ──────────────────────────────

pub const STYLE_MONO: u32 = 0;
pub const STYLE_SANS: u32 = 1;
pub const STYLE_SERIF: u32 = 2;

static FONT_MONO: &[u8] = include_bytes!("../../../../../assets/jetbrains-mono.ttf");
static FONT_SANS: &[u8] = include_bytes!("../../../../../assets/inter.ttf");
static FONT_SERIF: &[u8] = include_bytes!("../../../../../assets/source-serif-4.ttf");

fn font_for_style(style_id: u32) -> &'static [u8] {
    match style_id {
        STYLE_SANS => FONT_SANS,
        STYLE_SERIF => FONT_SERIF,
        _ => FONT_MONO,
    }
}

struct FontMetricsEntry {
    ascent_fu: i16,
    upem: u16,
}

fn metrics_for_font(font_data: &[u8]) -> FontMetricsEntry {
    match fonts::metrics::font_metrics(font_data) {
        Some(m) => FontMetricsEntry {
            ascent_fu: m.ascent,
            upem: m.units_per_em,
        },
        None => FontMetricsEntry {
            ascent_fu: 800,
            upem: 1000,
        },
    }
}

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

vertex float4 vertex_stencil(VertexIn in [[stage_in]]) {
    return float4(in.position, 0.0, 1.0);
}

struct ShadowParams {
    float rect_min_x;
    float rect_min_y;
    float rect_max_x;
    float rect_max_y;
    float color_r;
    float color_g;
    float color_b;
    float color_a;
    float sigma;
    float corner_radius;
    float _pad0;
    float _pad1;
};

float erf_approx(float x) {
    float ax = abs(x);
    float t = 1.0 / (1.0 + 0.3275911 * ax);
    float poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741
                 + t * (-1.453152027 + t * 1.061405429))));
    float result = 1.0 - poly * exp(-ax * ax);
    return x >= 0.0 ? result : -result;
}

float shadow_1d(float p, float lo, float hi, float inv_s2) {
    return 0.5 * (erf_approx((hi - p) * inv_s2) - erf_approx((lo - p) * inv_s2));
}

float sd_rounded_rect(float2 p, float2 b, float r) {
    float2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

fragment float4 fragment_shadow(
    VertexOut in [[stage_in]],
    constant ShadowParams& params [[buffer(0)]]
) {
    float2 p = in.texCoord;
    float sigma = params.sigma;
    float3 color_lin = srgb_to_linear(float3(params.color_r, params.color_g,
                                               params.color_b));

    if (sigma <= 0.0) {
        bool inside = p.x >= params.rect_min_x && p.x <= params.rect_max_x
                   && p.y >= params.rect_min_y && p.y <= params.rect_max_y;
        float a = inside ? params.color_a : 0.0;
        return float4(color_lin, a);
    }

    float inv_s2 = 1.0 / (sigma * 1.41421356);
    float alpha;

    if (params.corner_radius <= 0.0) {
        float ix = shadow_1d(p.x, params.rect_min_x, params.rect_max_x, inv_s2);
        float iy = shadow_1d(p.y, params.rect_min_y, params.rect_max_y, inv_s2);
        alpha = ix * iy;
    } else {
        float2 center = 0.5 * float2(params.rect_min_x + params.rect_max_x,
                                       params.rect_min_y + params.rect_max_y);
        float2 half_ext = 0.5 * float2(params.rect_max_x - params.rect_min_x,
                                         params.rect_max_y - params.rect_min_y);
        float r = min(params.corner_radius, min(half_ext.x, half_ext.y));
        float dist = sd_rounded_rect(p - center, half_ext, r);
        alpha = 0.5 * (1.0 - erf_approx(dist * inv_s2));
    }

    return float4(color_lin, params.color_a * alpha);
}

fragment float4 fragment_textured(
    VertexOut in [[stage_in]],
    texture2d<float> tex [[texture(0)]],
    sampler s [[sampler(0)]]
) {
    return tex.sample(s, in.texCoord);
}
";

const COLOR_WRITE_ALL: u8 = 0xF;
const H_LIBRARY: u32 = 1;
const H_VERTEX_FN: u32 = 2;
const H_FRAG_SOLID: u32 = 3;
const H_FRAG_GLYPH: u32 = 4;
const H_FRAG_SHADOW: u32 = 5;
const H_VERTEX_STENCIL: u32 = 6;
const H_FRAG_TEXTURED: u32 = 7;
const PIPE_SOLID: u32 = 10;
const PIPE_GLYPH: u32 = 11;
const PIPE_SHADOW: u32 = 12;
#[allow(dead_code)]
const PIPE_STENCIL_WRITE: u32 = 13;
#[allow(dead_code)]
const PIPE_STENCIL_COVER: u32 = 14;
const PIPE_TEXTURED: u32 = 15;
#[allow(dead_code)]
const DSS_STENCIL_WRITE: u32 = 40;
#[allow(dead_code)]
const DSS_STENCIL_TEST: u32 = 41;
const TEX_ATLAS: u32 = 20;
#[allow(dead_code)]
const TEX_STENCIL: u32 = 21;
const TEX_IMAGE: u32 = 22;
const SAMPLER_NEAREST: u32 = 30;
const SAMPLER_LINEAR: u32 = 31;

const FILTER_NEAREST: u8 = 0;
const FILTER_LINEAR: u8 = 1;

const ATLAS_WIDTH: u16 = 2048;
const ATLAS_HEIGHT: u16 = 2048;

const SETUP_BUF_PAGES: usize = 8;
const RENDER_BUF_PAGES: usize = 4;

// ── Vertex data ─────────────────────────────────────────────────────

const VERTEX_SIZE: usize = render::batch::VERTEX_SIZE;
const QUAD_BYTES: usize = VERTEX_SIZE * 6;

#[allow(clippy::too_many_arguments)]
fn push_quad(
    buf: &mut alloc::vec::Vec<u8>,
    display_w: f32,
    display_h: f32,
    px: f32,
    py: f32,
    pw: f32,
    ph: f32,
    color: scene::Color,
    uv0: [f32; 2],
    uv1: [f32; 2],
) {
    let x0 = px / display_w * 2.0 - 1.0;
    let y0 = 1.0 - py / display_h * 2.0;
    let x1 = (px + pw) / display_w * 2.0 - 1.0;
    let y1 = 1.0 - (py + ph) / display_h * 2.0;
    let c = [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        color.a as f32 / 255.0,
    ];

    for &(pos, tc) in &[
        ([x0, y0], [uv0[0], uv0[1]]),
        ([x0, y1], [uv0[0], uv1[1]]),
        ([x1, y1], [uv1[0], uv1[1]]),
        ([x0, y0], [uv0[0], uv0[1]]),
        ([x1, y1], [uv1[0], uv1[1]]),
        ([x1, y0], [uv1[0], uv0[1]]),
    ] {
        buf.extend_from_slice(&pos[0].to_le_bytes());
        buf.extend_from_slice(&pos[1].to_le_bytes());
        buf.extend_from_slice(&tc[0].to_le_bytes());
        buf.extend_from_slice(&tc[1].to_le_bytes());
        buf.extend_from_slice(&c[0].to_le_bytes());
        buf.extend_from_slice(&c[1].to_le_bytes());
        buf.extend_from_slice(&c[2].to_le_bytes());
        buf.extend_from_slice(&c[3].to_le_bytes());
    }
}

#[allow(clippy::too_many_arguments)]
fn push_quad_corners(
    buf: &mut alloc::vec::Vec<u8>,
    display_w: f32,
    display_h: f32,
    corners: [(f32, f32); 4],
    color: scene::Color,
    uv0: [f32; 2],
    uv1: [f32; 2],
) {
    let to_ndc = |px: f32, py: f32| -> (f32, f32) {
        (px / display_w * 2.0 - 1.0, 1.0 - py / display_h * 2.0)
    };
    let (x0, y0) = to_ndc(corners[0].0, corners[0].1);
    let (x1, y1) = to_ndc(corners[1].0, corners[1].1);
    let (x2, y2) = to_ndc(corners[2].0, corners[2].1);
    let (x3, y3) = to_ndc(corners[3].0, corners[3].1);
    let c = [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        color.a as f32 / 255.0,
    ];
    let tcs = [
        [uv0[0], uv0[1]],
        [uv0[0], uv1[1]],
        [uv1[0], uv1[1]],
        [uv1[0], uv0[1]],
    ];
    let positions = [(x0, y0), (x3, y3), (x2, y2), (x0, y0), (x2, y2), (x1, y1)];
    let tex_idx = [0, 3, 2, 0, 2, 1];

    for i in 0..6 {
        let (px, py) = positions[i];
        let tc = tcs[tex_idx[i]];

        buf.extend_from_slice(&px.to_le_bytes());
        buf.extend_from_slice(&py.to_le_bytes());
        buf.extend_from_slice(&tc[0].to_le_bytes());
        buf.extend_from_slice(&tc[1].to_le_bytes());
        buf.extend_from_slice(&c[0].to_le_bytes());
        buf.extend_from_slice(&c[1].to_le_bytes());
        buf.extend_from_slice(&c[2].to_le_bytes());
        buf.extend_from_slice(&c[3].to_le_bytes());
    }
}

#[allow(clippy::too_many_arguments)]
fn pack_shadow_params(
    rect_min_x: f32,
    rect_min_y: f32,
    rect_max_x: f32,
    rect_max_y: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
    sigma: f32,
    corner_radius: f32,
) -> [u8; 48] {
    let mut buf = [0u8; 48];

    buf[0..4].copy_from_slice(&rect_min_x.to_le_bytes());
    buf[4..8].copy_from_slice(&rect_min_y.to_le_bytes());
    buf[8..12].copy_from_slice(&rect_max_x.to_le_bytes());
    buf[12..16].copy_from_slice(&rect_max_y.to_le_bytes());
    buf[16..20].copy_from_slice(&color_r.to_le_bytes());
    buf[20..24].copy_from_slice(&color_g.to_le_bytes());
    buf[24..28].copy_from_slice(&color_b.to_le_bytes());
    buf[28..32].copy_from_slice(&color_a.to_le_bytes());
    buf[32..36].copy_from_slice(&sigma.to_le_bytes());
    buf[36..40].copy_from_slice(&corner_radius.to_le_bytes());

    buf
}

fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        libm::powf((s + 0.055) / 1.055, 2.4)
    }
}

// ── Draw list ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pipe {
    Solid,
    Glyph,
    Shadow,
    Textured,
}

struct DrawOp {
    pipe: Pipe,
    vert_offset: usize,
    vert_bytes: usize,
    shadow_params: [u8; 48],
}

struct DrawList {
    verts: alloc::vec::Vec<u8>,
    ops: alloc::vec::Vec<DrawOp>,
    display_w: f32,
    display_h: f32,
    current_pipe: Pipe,
    current_start: usize,
}

impl DrawList {
    fn new(display_w: f32, display_h: f32) -> Self {
        Self {
            verts: alloc::vec::Vec::with_capacity(1024 * QUAD_BYTES),
            ops: alloc::vec::Vec::with_capacity(64),
            display_w,
            display_h,
            current_pipe: Pipe::Solid,
            current_start: 0,
        }
    }

    fn flush_current(&mut self) {
        let end = self.verts.len();

        if end > self.current_start {
            self.ops.push(DrawOp {
                pipe: self.current_pipe,
                vert_offset: self.current_start,
                vert_bytes: end - self.current_start,
                shadow_params: [0; 48],
            });
            self.current_start = end;
        }
    }

    fn ensure_pipe(&mut self, pipe: Pipe) {
        if self.current_pipe != pipe {
            self.flush_current();
            self.current_pipe = pipe;
        }
    }

    fn push_transformed_rect(&mut self, corners: [(f32, f32); 4], color: scene::Color) {
        self.ensure_pipe(Pipe::Solid);

        push_quad_corners(
            &mut self.verts,
            self.display_w,
            self.display_h,
            corners,
            color,
            [0.0; 2],
            [0.0; 2],
        );
    }

    fn push_rect(&mut self, px: f32, py: f32, pw: f32, ph: f32, color: scene::Color) {
        self.ensure_pipe(Pipe::Solid);

        push_quad(
            &mut self.verts,
            self.display_w,
            self.display_h,
            px,
            py,
            pw,
            ph,
            color,
            [0.0; 2],
            [0.0; 2],
        );
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
        self.ensure_pipe(Pipe::Glyph);

        push_quad(
            &mut self.verts,
            self.display_w,
            self.display_h,
            px,
            py,
            pw,
            ph,
            color,
            uv0,
            uv1,
        );
    }

    fn push_textured_quad(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        uv0: [f32; 2],
        uv1: [f32; 2],
    ) {
        self.ensure_pipe(Pipe::Textured);

        push_quad(
            &mut self.verts,
            self.display_w,
            self.display_h,
            px,
            py,
            pw,
            ph,
            scene::Color::rgba(255, 255, 255, 255),
            uv0,
            uv1,
        );
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn push_shadow_inner(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        blur_radius: f32,
        spread: f32,
        offset_x: f32,
        offset_y: f32,
        shadow_color: scene::Color,
        corner_radius: f32,
        scale: f32,
        transform: Option<(&scene::AffineTransform, f32, f32)>,
    ) {
        self.flush_current();

        let sx = px + offset_x - spread;
        let sy = py + offset_y - spread;
        let sw = pw + 2.0 * spread;
        let sh = ph + 2.0 * spread;
        let sigma = blur_radius / 2.0;
        let pad = 3.0 * sigma;
        let qx = sx - pad;
        let qy = sy - pad;
        let qw = sw + 2.0 * pad;
        let qh = sh + 2.0 * pad;
        let px_sx = sx * scale;
        let px_sy = sy * scale;
        let px_sw = sw * scale;
        let px_sh = sh * scale;
        let px_sigma = sigma * scale;
        let px_cr = corner_radius * scale;
        let params = pack_shadow_params(
            px_sx,
            px_sy,
            px_sx + px_sw,
            px_sy + px_sh,
            shadow_color.r as f32 / 255.0,
            shadow_color.g as f32 / 255.0,
            shadow_color.b as f32 / 255.0,
            shadow_color.a as f32 / 255.0,
            px_sigma,
            px_cr,
        );
        let start = self.verts.len();
        let uv0 = [qx * scale, qy * scale];
        let uv1 = [(qx + qw) * scale, (qy + qh) * scale];

        if let Some((tf, cx, cy)) = transform {
            let corners_local = [
                (qx - cx, qy - cy),
                (qx + qw - cx, qy - cy),
                (qx + qw - cx, qy + qh - cy),
                (qx - cx, qy + qh - cy),
            ];
            let corners = [
                tf.transform_point(corners_local[0].0, corners_local[0].1),
                tf.transform_point(corners_local[1].0, corners_local[1].1),
                tf.transform_point(corners_local[2].0, corners_local[2].1),
                tf.transform_point(corners_local[3].0, corners_local[3].1),
            ];

            push_quad_corners(
                &mut self.verts,
                self.display_w,
                self.display_h,
                [
                    (corners[0].0 + cx, corners[0].1 + cy),
                    (corners[1].0 + cx, corners[1].1 + cy),
                    (corners[2].0 + cx, corners[2].1 + cy),
                    (corners[3].0 + cx, corners[3].1 + cy),
                ],
                scene::Color::TRANSPARENT,
                uv0,
                uv1,
            );
        } else {
            push_quad(
                &mut self.verts,
                self.display_w,
                self.display_h,
                qx,
                qy,
                qw,
                qh,
                scene::Color::TRANSPARENT,
                uv0,
                uv1,
            );
        }

        self.ops.push(DrawOp {
            pipe: Pipe::Shadow,
            vert_offset: start,
            vert_bytes: self.verts.len() - start,
            shadow_params: params,
        });

        self.current_start = self.verts.len();
    }

    fn finalize(&mut self) {
        self.flush_current();
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

    device.ack_interrupt();

    let _ = abi::event::clear(irq_event, 0x1);

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
    // Batch 1: compile shaders, get functions.
    let len = {
        let mut w = CommandWriter::new(dma_buf);

        w.compile_library(H_LIBRARY, MSL_SOURCE);
        w.get_function(H_VERTEX_FN, H_LIBRARY, b"vertex_main");
        w.get_function(H_FRAG_SOLID, H_LIBRARY, b"fragment_solid");
        w.get_function(H_FRAG_GLYPH, H_LIBRARY, b"fragment_glyph");
        w.get_function(H_FRAG_SHADOW, H_LIBRARY, b"fragment_shadow");
        w.get_function(H_VERTEX_STENCIL, H_LIBRARY, b"vertex_stencil");
        w.get_function(H_FRAG_TEXTURED, H_LIBRARY, b"fragment_textured");

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

    // Batch 2: create pipelines, textures, samplers, depth-stencil states.
    let len = {
        let mut w = CommandWriter::new(dma_buf);

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
        w.create_render_pipeline(
            PIPE_SHADOW,
            H_VERTEX_FN,
            H_FRAG_SHADOW,
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
        w.create_render_pipeline(
            PIPE_TEXTURED,
            H_VERTEX_FN,
            H_FRAG_TEXTURED,
            true,
            COLOR_WRITE_ALL,
            false,
            1,
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );
        w.create_sampler(SAMPLER_NEAREST, FILTER_NEAREST, FILTER_NEAREST);
        w.create_sampler(SAMPLER_LINEAR, FILTER_LINEAR, FILTER_LINEAR);

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
    font_metrics: [FontMetricsEntry; 3],
    scale: u32,
    atlas_dirty: bool,
    now_tick: u64,
    next_deadline: u64,
    image_content_id: u32,
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

type ClipRect = Option<[f32; 4]>;

fn rects_overlap(ax: f32, ay: f32, aw: f32, ah: f32, clip: &[f32; 4]) -> bool {
    ax + aw > clip[0] && ax < clip[0] + clip[2] && ay + ah > clip[1] && ay < clip[1] + clip[3]
}

fn intersect_clip(a: &[f32; 4], b: &[f32; 4]) -> [f32; 4] {
    let x0 = if a[0] > b[0] { a[0] } else { b[0] };
    let y0 = if a[1] > b[1] { a[1] } else { b[1] };
    let x1_a = a[0] + a[2];
    let x1_b = b[0] + b[2];
    let y1_a = a[1] + a[3];
    let y1_b = b[1] + b[3];
    let x1 = if x1_a < x1_b { x1_a } else { x1_b };
    let y1 = if y1_a < y1_b { y1_a } else { y1_b };
    let w = if x1 > x0 { x1 - x0 } else { 0.0 };
    let h = if y1 > y0 { y1 - y0 } else { 0.0 };

    [x0, y0, w, h]
}

#[allow(clippy::too_many_arguments)]
fn walk_node(
    reader: &SceneReader<'_>,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    clip: ClipRect,
    draws: &mut DrawList,
    ctx: &mut WalkContext,
    is_root: bool,
) {
    let node = reader.node(node_id);
    let x = parent_x + scene::mpt_to_f32(node.x);
    let y = parent_y + scene::mpt_to_f32(node.y);
    let w = scene::umpt_to_f32(node.width);
    let h = scene::umpt_to_f32(node.height);

    if let Some(ref cr) = clip
        && !rects_overlap(x, y, w, h, cr)
    {
        return;
    }

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

    let has_transform = !node.transform.is_identity();
    let tf_args = if has_transform {
        Some((&node.transform, x + w / 2.0, y + h / 2.0))
    } else {
        None
    };

    if node.has_shadow() {
        let mut sc = node.shadow_color;

        sc.a = ((sc.a as u16 * effective_opacity as u16) / 255) as u8;

        draws.push_shadow_inner(
            x,
            y,
            w,
            h,
            node.shadow_blur_radius as f32,
            node.shadow_spread as f32,
            node.shadow_offset_x as f32,
            node.shadow_offset_y as f32,
            sc,
            node.corner_radius as f32,
            ctx.scale as f32,
            tf_args,
        );
    }

    if !is_root && node.background.a > 0 {
        let mut bg = node.background;

        bg.a = ((bg.a as u16 * effective_opacity as u16) / 255) as u8;

        if node.corner_radius > 0 {
            draws.push_shadow_inner(
                x,
                y,
                w,
                h,
                0.5,
                0.0,
                0.0,
                0.0,
                bg,
                node.corner_radius as f32,
                ctx.scale as f32,
                tf_args,
            );
        } else if has_transform {
            let cx = x + w / 2.0;
            let cy = y + h / 2.0;
            let tf = &node.transform;
            let corners = [
                tf.transform_point(x - cx, y - cy),
                tf.transform_point(x + w - cx, y - cy),
                tf.transform_point(x + w - cx, y + h - cy),
                tf.transform_point(x - cx, y + h - cy),
            ];

            draws.push_transformed_rect(
                [
                    (corners[0].0 + cx, corners[0].1 + cy),
                    (corners[1].0 + cx, corners[1].1 + cy),
                    (corners[2].0 + cx, corners[2].1 + cy),
                    (corners[3].0 + cx, corners[3].1 + cy),
                ],
                bg,
            );
        } else {
            draws.push_rect(x, y, w, h, bg);
        }
    }

    match node.content {
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            font_size,
            style_id,
        } => {
            let glyph_data = reader.shaped_glyphs(glyphs, glyph_count);
            let fm = &ctx.font_metrics[(style_id as usize).min(2)];
            let ascent_pt = fm.ascent_fu as f32 * font_size as f32 / fm.upem as f32;
            let baseline_y = y + ascent_pt;
            let inv_scale = 1.0 / ctx.scale as f32;
            let raster_size = font_size.saturating_mul(ctx.scale as u16);
            let mut gx = x;

            for glyph in glyph_data {
                let advance = glyph.x_advance as f32 / 65536.0;

                if glyph.glyph_id > 0 {
                    let entry = lookup_or_rasterize(ctx, glyph.glyph_id, raster_size, style_id);

                    if let Some(e) = entry.filter(|e| e.width > 0 && e.height > 0) {
                        let px = gx + e.bearing_x as f32 * inv_scale;
                        let py = baseline_y - e.bearing_y as f32 * inv_scale;
                        let pw = e.width as f32 * inv_scale;
                        let ph = e.height as f32 * inv_scale;
                        let u0 = e.u as f32 / ATLAS_W_F;
                        let v0 = e.v as f32 / ATLAS_H_F;
                        let u1 = (e.u + e.width) as f32 / ATLAS_W_F;
                        let v1 = (e.v + e.height) as f32 / ATLAS_H_F;

                        draws.push_glyph_quad(px, py, pw, ph, color, [u0, v0], [u1, v1]);
                    }
                }

                gx += advance;
            }
        }
        Content::Path {
            color,
            stroke_color,
            fill_rule,
            stroke_width,
            contours,
        } => {
            let path_data = reader.data(contours);

            if !path_data.is_empty() {
                render_path_node(
                    path_data,
                    x,
                    y,
                    w,
                    h,
                    color,
                    stroke_color,
                    fill_rule,
                    stroke_width,
                    node.content_hash,
                    effective_opacity,
                    draws,
                    ctx,
                );
            }
        }
        Content::Image {
            content_id,
            src_width,
            src_height,
        } if ctx.image_content_id == content_id
            && content_id != 0
            && src_width > 0
            && src_height > 0 =>
        {
            let fit_scale_w = w / src_width as f32;
            let fit_scale_h = h / src_height as f32;
            let fit_scale = if fit_scale_w < fit_scale_h {
                fit_scale_w
            } else {
                fit_scale_h
            };
            let draw_w = src_width as f32 * fit_scale;
            let draw_h = src_height as f32 * fit_scale;
            let draw_x = x + (w - draw_w) / 2.0;
            let draw_y = y + (h - draw_h) / 2.0;

            draws.push_textured_quad(draw_x, draw_y, draw_w, draw_h, [0.0, 0.0], [1.0, 1.0]);
        }
        _ => {}
    }

    let child_x = x + node.child_offset_x;
    let child_y = y + node.child_offset_y;
    let child_clip = if node.clips_children() {
        let node_clip = [x, y, w, h];

        Some(match clip {
            Some(ref cr) => intersect_clip(&node_clip, cr),
            None => node_clip,
        })
    } else {
        clip
    };
    let mut child = node.first_child;

    while child != NULL {
        walk_node(
            reader, child, child_x, child_y, child_clip, draws, ctx, false,
        );

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

    let font_data = font_for_style(style_id);
    let mut buf = RasterBuffer {
        data: &mut ctx.raster_buf,
        width: 100,
        height: 100,
    };
    let metrics = fonts::rasterize::rasterize(
        font_data,
        glyph_id,
        font_size,
        &mut buf,
        &mut ctx.scratch,
        ctx.scale as u16,
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

const PATH_STYLE_SENTINEL: u32 = 0x8000_0000;

#[allow(clippy::too_many_arguments)]
fn render_path_node(
    path_data: &[u8],
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill_color: scene::Color,
    stroke_color: scene::Color,
    fill_rule: scene::FillRule,
    stroke_width: u16,
    content_hash: u32,
    opacity: u8,
    draws: &mut DrawList,
    ctx: &mut WalkContext,
) {
    let scale = ctx.scale as f32;
    let inv_scale = 1.0 / scale;
    let pw = (w * scale) as u32;
    let ph = (h * scale) as u32;

    if pw == 0 || ph == 0 || pw > 512 || ph > 512 {
        return;
    }

    let cache_key_size = pw as u16;
    let cache_key_hash = content_hash | PATH_STYLE_SENTINEL;

    if fill_color.a > 0 {
        let entry = lookup_or_rasterize_path(
            ctx,
            path_data,
            pw,
            ph,
            scale,
            fill_rule,
            None,
            cache_key_size,
            cache_key_hash,
        );

        if let Some(e) = entry.filter(|e| e.width > 0 && e.height > 0) {
            let mut c = fill_color;

            c.a = ((c.a as u16 * opacity as u16) / 255) as u8;

            let u0 = e.u as f32 / ATLAS_W_F;
            let v0 = e.v as f32 / ATLAS_H_F;
            let u1 = (e.u + e.width) as f32 / ATLAS_W_F;
            let v1 = (e.v + e.height) as f32 / ATLAS_H_F;

            draws.push_glyph_quad(
                x,
                y,
                e.width as f32 * inv_scale,
                e.height as f32 * inv_scale,
                c,
                [u0, v0],
                [u1, v1],
            );
        }
    }

    if stroke_width > 0 && stroke_color.a > 0 {
        let sw_pt = stroke_width as f32 / 256.0;
        let stroke_expanded = scene::stroke::expand_stroke(path_data, sw_pt);

        if !stroke_expanded.is_empty() {
            let stroke_hash = content_hash.wrapping_add(0x1234) | PATH_STYLE_SENTINEL;
            let entry = lookup_or_rasterize_path(
                ctx,
                path_data,
                pw,
                ph,
                scale,
                scene::FillRule::Winding,
                Some(&stroke_expanded),
                cache_key_size,
                stroke_hash,
            );

            if let Some(e) = entry.filter(|e| e.width > 0 && e.height > 0) {
                let mut c = stroke_color;

                c.a = ((c.a as u16 * opacity as u16) / 255) as u8;

                let u0 = e.u as f32 / ATLAS_W_F;
                let v0 = e.v as f32 / ATLAS_H_F;
                let u1 = (e.u + e.width) as f32 / ATLAS_W_F;
                let v1 = (e.v + e.height) as f32 / ATLAS_H_F;

                draws.push_glyph_quad(
                    x,
                    y,
                    e.width as f32 * inv_scale,
                    e.height as f32 * inv_scale,
                    c,
                    [u0, v0],
                    [u1, v1],
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lookup_or_rasterize_path(
    ctx: &mut WalkContext,
    path_data: &[u8],
    pw: u32,
    ph: u32,
    scale: f32,
    fill_rule: scene::FillRule,
    stroke_data: Option<&[u8]>,
    cache_size: u16,
    cache_hash: u32,
) -> Option<atlas::AtlasEntry> {
    if let Some(entry) = ctx.atlas.lookup(1, cache_size, cache_hash) {
        return Some(*entry);
    }

    let coverage = path::rasterize_path(path_data, pw, ph, scale, fill_rule, stroke_data);

    if coverage.is_empty() {
        ctx.atlas.insert(
            1,
            cache_size,
            cache_hash,
            atlas::AtlasEntry {
                u: 0,
                v: 0,
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
            },
        );

        return None;
    }

    let ok = ctx.atlas.pack(
        1, cache_size, cache_hash, pw as u16, ph as u16, 0, 0, &coverage,
    );

    if ok {
        ctx.atlas_dirty = true;

        ctx.atlas.lookup(1, cache_size, cache_hash).copied()
    } else {
        None
    }
}

// ── Compositor ──────────────────────────────────────────────────────

// ── Cursor ─────────────────────────────────────────────────────────

const CURSOR_DISPLAY_PT: f32 = 24.0;
const CURSOR_SHADOW_DX: f32 = 1.5;
const CURSOR_SHADOW_DY: f32 = 1.5;
const CURSOR_SHADOW_SIGMA: f32 = 4.0;
const CURSOR_SHADOW_ALPHA: u8 = 64;

fn offset_path(src: &[u8], dx: f32, dy: f32) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::with_capacity(src.len());
    let mut off = 0;

    while off < src.len() {
        let tag = u32::from_le_bytes([src[off], src[off + 1], src[off + 2], src[off + 3]]);

        match tag {
            scene::PATH_MOVE_TO | scene::PATH_LINE_TO => {
                out.extend_from_slice(&src[off..off + 4]);

                let x = f32::from_le_bytes(src[off + 4..off + 8].try_into().unwrap()) + dx;
                let y = f32::from_le_bytes(src[off + 8..off + 12].try_into().unwrap()) + dy;

                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                off += 12;
            }
            scene::PATH_CUBIC_TO => {
                out.extend_from_slice(&src[off..off + 4]);

                for i in 0..3 {
                    let base = off + 4 + i * 8;
                    let x = f32::from_le_bytes(src[base..base + 4].try_into().unwrap()) + dx;
                    let y = f32::from_le_bytes(src[base + 4..base + 8].try_into().unwrap()) + dy;

                    out.extend_from_slice(&x.to_le_bytes());
                    out.extend_from_slice(&y.to_le_bytes());
                }

                off += 28;
            }
            scene::PATH_CLOSE => {
                out.extend_from_slice(&src[off..off + 4]);
                off += 4;
            }
            _ => break,
        }
    }

    out
}

fn rasterize_cursor_icon(icon_name: &str, scale: u32) -> (alloc::vec::Vec<u8>, u16, i16, i16) {
    let icon = icons::get(icon_name, None);
    let viewbox = icon.viewbox;
    let stroke_w = icon.stroke_width;
    let px_scale = CURSOR_DISPLAY_PT * scale as f32 / viewbox;

    let stroke_margin = stroke_w / 2.0 + 1.0;
    let shadow_pad_vb =
        (CURSOR_SHADOW_DX.max(CURSOR_SHADOW_DY) + CURSOR_SHADOW_SIGMA * 3.0) / px_scale;
    let margin_vb = stroke_margin + shadow_pad_vb;
    let total_vb = viewbox + 2.0 * margin_vb;
    let tex_sz = (total_vb * px_scale) as u32;

    if tex_sz == 0 || tex_sz > 256 {
        return (alloc::vec::Vec::new(), 0, 0, 0);
    }

    let mut combined_path = alloc::vec::Vec::new();

    for path in icon.paths {
        combined_path.extend_from_slice(path.commands);
    }

    let path_data = offset_path(&combined_path, margin_vb, margin_vb);
    let raster_scale = px_scale;
    let stroke_only = !icon.all_paths_closed();

    // For closed paths (arrow): fill = black body, stroke = white outline.
    // For open paths (I-beam): fill is garbage (implicit closure artifacts),
    // so use stroke as black body with a wider outline stroke for white border.
    let (body, outline) = if stroke_only {
        let body_expanded = scene::stroke::expand_stroke(&path_data, stroke_w);
        let body = path::rasterize_path(
            &path_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            Some(&body_expanded),
        );
        let outline_expanded = scene::stroke::expand_stroke(&path_data, stroke_w + 2.0);
        let outline = path::rasterize_path(
            &path_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            Some(&outline_expanded),
        );

        (body, outline)
    } else {
        let fill = path::rasterize_path(
            &path_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            None,
        );
        let stroke_expanded = scene::stroke::expand_stroke(&path_data, stroke_w);
        let stroke = path::rasterize_path(
            &path_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            Some(&stroke_expanded),
        );

        (fill, stroke)
    };

    // Rasterize shadow: same shape at offset, then blur.
    let shadow_ox = (CURSOR_SHADOW_DX * px_scale) as i32;
    let shadow_oy = (CURSOR_SHADOW_DY * px_scale) as i32;
    let mut shadow = alloc::vec![0u8; (tex_sz * tex_sz) as usize];

    for y in 0..tex_sz as i32 {
        for x in 0..tex_sz as i32 {
            let sx = (x - shadow_ox) as usize;
            let sy = (y - shadow_oy) as usize;

            if sx < tex_sz as usize && sy < tex_sz as usize {
                let idx = sy * tex_sz as usize + sx;
                let ba = body.get(idx).copied().unwrap_or(0) as u16;
                let oa = outline.get(idx).copied().unwrap_or(0) as u16;
                let a = ba.max(oa).min(255) as u8;

                shadow[(y as usize) * tex_sz as usize + (x as usize)] = a;
            }
        }
    }

    // Box blur (3-pass approximation of Gaussian).
    let sigma = CURSOR_SHADOW_SIGMA * scale as f32;
    let radius = (sigma * 1.5) as usize;

    for _ in 0..3 {
        box_blur_1d(&mut shadow, tex_sz as usize, tex_sz as usize, radius, true);
        box_blur_1d(&mut shadow, tex_sz as usize, tex_sz as usize, radius, false);
    }

    // Composite: shadow (bottom) + outline (white, middle) + body (black, top) → BGRA.
    let mut bgra = alloc::vec![0u8; (tex_sz * tex_sz * 4) as usize];

    for i in 0..(tex_sz * tex_sz) as usize {
        let body_a = body.get(i).copied().unwrap_or(0) as u16;
        let outline_a = outline.get(i).copied().unwrap_or(0) as u16;
        let shadow_a = shadow[i] as u16;
        let border_only = outline_a.saturating_sub(body_a);

        // Shadow layer: black at reduced alpha.
        let sha = (shadow_a * CURSOR_SHADOW_ALPHA as u16 / 255).min(255);
        // Cursor: black body + white outline border, composited over shadow.
        let cursor_a = body_a.max(border_only).min(255);
        let cursor_lum = if cursor_a > 0 {
            (255 * border_only / cursor_a.max(1)) as u8
        } else {
            0
        };

        // Alpha composite cursor over shadow.
        let out_a = cursor_a + (sha * (255 - cursor_a) / 255);
        let out_a = out_a.min(255);

        if out_a == 0 {
            continue;
        }

        // Shadow is black (lum=0), so only the cursor body contributes luminance.
        let out_lum = (cursor_lum as u16 * cursor_a / out_a) as u8;

        bgra[i * 4] = out_lum;
        bgra[i * 4 + 1] = out_lum;
        bgra[i * 4 + 2] = out_lum;
        bgra[i * 4 + 3] = out_a as u8;
    }

    let (hx_vb, hy_vb) = match icon_name {
        "cursor-text" => (12.0_f32, 12.0_f32),
        _ => (4.0_f32, 4.0_f32),
    };
    let hotspot_x = ((hx_vb + margin_vb) * px_scale) as i16;
    let hotspot_y = ((hy_vb + margin_vb) * px_scale) as i16;

    (bgra, tex_sz as u16, hotspot_x, hotspot_y)
}

fn box_blur_1d(buf: &mut [u8], w: usize, h: usize, radius: usize, horizontal: bool) {
    if radius == 0 {
        return;
    }

    let mut tmp = alloc::vec![0u8; buf.len()];

    if horizontal {
        for y in 0..h {
            let mut sum: u32 = 0;

            for x in 0..=radius.min(w - 1) {
                sum += buf[y * w + x] as u32;
            }

            for x in 0..w {
                let right = (x + radius).min(w - 1);
                let left = (x as isize - radius as isize - 1).max(0) as usize;
                let count = right - left;

                if x + radius < w {
                    sum += buf[y * w + x + radius] as u32;
                }

                tmp[y * w + x] = (sum / count as u32).min(255) as u8;

                if x >= radius {
                    sum -= buf[y * w + x - radius] as u32;
                }
            }
        }
    } else {
        for x in 0..w {
            let mut sum: u32 = 0;

            for y in 0..=radius.min(h - 1) {
                sum += buf[y * w + x] as u32;
            }

            for y in 0..h {
                let bottom = (y + radius).min(h - 1);
                let top = (y as isize - radius as isize - 1).max(0) as usize;
                let count = bottom - top;

                if y + radius < h {
                    sum += buf[(y + radius) * w + x] as u32;
                }

                tmp[y * w + x] = (sum / count as u32).min(255) as u8;

                if y >= radius {
                    sum -= buf[(y - radius) * w + x] as u32;
                }
            }
        }
    }

    buf.copy_from_slice(&tmp);
}

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
    logical_w: u32,
    logical_h: u32,
    scale: u32,

    scene_va: usize,
    frame_count: u32,
    walk_ctx: WalkContext,
    atlas_upload_y: u16,

    cursor_uploaded: bool,
    cursor_visible: bool,
    cursor_shape: u8,

    image_va: usize,
    image_w: u16,
    image_h: u16,
    image_pixel_size: u32,
    image_tex_created: bool,
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

    fn cursor_icon_name(&self) -> &'static str {
        match self.cursor_shape {
            scene::CURSOR_TEXT => "cursor-text",
            _ => "pointer",
        }
    }

    fn upload_cursor(&mut self) {
        if self.cursor_uploaded {
            return;
        }

        let (bgra, sz, hotspot_x, hotspot_y) =
            rasterize_cursor_icon(self.cursor_icon_name(), self.scale);

        if bgra.is_empty() {
            return;
        }
        // SAFETY: render_dma.va is a valid DMA allocation.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(self.render_dma.va as *mut u8, self.render_buf_size)
        };
        let len = {
            let mut w = CommandWriter::new(dma_buf);

            w.set_cursor_image(sz, sz, hotspot_x, hotspot_y, &bgra);
            w.set_cursor_visible(true);

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

        self.cursor_uploaded = true;
        self.cursor_visible = true;
    }

    fn update_cursor_position(&mut self, x: f32, y: f32) {
        self.upload_cursor();

        // SAFETY: render_dma.va is a valid DMA allocation.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(self.render_dma.va as *mut u8, self.render_buf_size)
        };
        let len = {
            let mut w = CommandWriter::new(dma_buf);

            w.set_cursor_position(x, y);

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
    }

    fn upload_image(&mut self) {
        if self.image_va == 0 || self.image_w == 0 || self.image_h == 0 {
            return;
        }

        // SAFETY: setup_dma.va is a valid DMA allocation of setup_buf_size bytes.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(self.setup_dma.va as *mut u8, self.setup_buf_size)
        };

        if !self.image_tex_created {
            let len = {
                let mut w = CommandWriter::new(dma_buf);

                w.create_texture(
                    TEX_IMAGE,
                    self.image_w,
                    self.image_h,
                    render::PIXEL_FORMAT_BGRA8_SRGB,
                    0,
                    1,
                    render::TEX_USAGE_SHADER_READ,
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

            self.image_tex_created = true;
        }

        let row_bytes = self.image_w as usize * 4;
        let cmd_overhead = render::HEADER_SIZE + 16;
        let max_data_per_submit = self.setup_buf_size - cmd_overhead;
        let max_rows_per_submit = (max_data_per_submit / row_bytes).max(1);
        let total_rows = self.image_h as usize;
        // SAFETY: image_va is a valid RO mapping of the pixel VMO.
        let pixels = unsafe {
            core::slice::from_raw_parts(self.image_va as *const u8, self.image_pixel_size as usize)
        };
        let mut y: usize = 0;

        while y < total_rows {
            let rows = (total_rows - y).min(max_rows_per_submit);
            let src_offset = y * row_bytes;
            let pixel_count = rows * row_bytes;
            let pixel_data = &pixels[src_offset..src_offset + pixel_count];
            let len = {
                let mut w = CommandWriter::new(dma_buf);

                w.upload_texture_region(
                    TEX_IMAGE,
                    0,
                    y as u16,
                    self.image_w,
                    rows as u16,
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
        let mut draws = DrawList::new(self.logical_w as f32, self.logical_h as f32);

        walk_node(
            &reader,
            root,
            0.0,
            0.0,
            None,
            &mut draws,
            &mut self.walk_ctx,
            true,
        );

        draws.finalize();

        self.upload_atlas_dirty();

        let clear_r = srgb_to_linear(bg.r as f32 / 255.0);
        let clear_g = srgb_to_linear(bg.g as f32 / 255.0);
        let clear_b = srgb_to_linear(bg.b as f32 / 255.0);

        if draws.ops.is_empty() {
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
                    clear_r,
                    clear_g,
                    clear_b,
                    1.0,
                );
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
        } else {
            let mut first = true;
            let mut active_pipe: Option<Pipe> = None;

            for op_idx in 0..draws.ops.len() {
                let op = &draws.ops[op_idx];
                let is_last = op_idx == draws.ops.len() - 1;
                let verts = &draws.verts[op.vert_offset..op.vert_offset + op.vert_bytes];
                // SAFETY: render_dma.va is a valid DMA allocation of render_buf_size bytes.
                let dma_buf = unsafe {
                    core::slice::from_raw_parts_mut(
                        self.render_dma.va as *mut u8,
                        self.render_buf_size,
                    )
                };
                let len = {
                    let mut w = CommandWriter::new(dma_buf);
                    let load = if first {
                        render::LOAD_CLEAR
                    } else {
                        render::LOAD_LOAD
                    };

                    w.begin_render_pass(
                        render::DRAWABLE_HANDLE,
                        0,
                        0,
                        load,
                        render::STORE_STORE,
                        0,
                        0,
                        clear_r,
                        clear_g,
                        clear_b,
                        1.0,
                    );

                    if active_pipe != Some(op.pipe) || op.pipe == Pipe::Shadow {
                        match op.pipe {
                            Pipe::Solid => {
                                w.set_render_pipeline(PIPE_SOLID);
                            }
                            Pipe::Glyph => {
                                w.set_render_pipeline(PIPE_GLYPH);
                                w.set_fragment_texture(TEX_ATLAS, 0);
                                w.set_fragment_sampler(SAMPLER_NEAREST, 0);
                            }
                            Pipe::Shadow => {
                                w.set_render_pipeline(PIPE_SHADOW);
                                w.set_fragment_bytes(0, &op.shadow_params);
                            }
                            Pipe::Textured => {
                                w.set_render_pipeline(PIPE_TEXTURED);
                                w.set_fragment_texture(TEX_IMAGE, 0);
                                w.set_fragment_sampler(SAMPLER_LINEAR, 0);
                            }
                        }
                        active_pipe = Some(op.pipe);
                    }

                    render::batch::emit_draws(&mut w, verts);

                    w.end_render_pass();

                    if is_last {
                        w.present_and_commit(self.frame_count);
                    }

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
                first = false;
            }
        }

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
                            display_width: self.logical_w,
                            display_height: self.logical_h,
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
                    display_width: self.logical_w,
                    display_height: self.logical_h,
                    frame_count: self.frame_count,
                };
                let mut data = [0u8; render::comp::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            render::comp::POINTER => {
                if msg.payload.len() >= 8 {
                    let x = f32::from_le_bytes(msg.payload[0..4].try_into().unwrap());
                    let y = f32::from_le_bytes(msg.payload[4..8].try_into().unwrap());

                    self.update_cursor_position(x, y);
                }

                let _ = msg.reply_empty();
            }
            render::comp::SET_CURSOR_SHAPE => {
                if !msg.payload.is_empty() {
                    let shape = msg.payload[0];

                    if shape != self.cursor_shape {
                        self.cursor_shape = shape;
                        self.cursor_uploaded = false;
                        self.upload_cursor();
                    }
                }

                let _ = msg.reply_empty();
            }
            render::comp::UPLOAD_IMAGE => {
                if msg.payload.len() >= render::comp::UploadImageRequest::SIZE
                    && !msg.handles.is_empty()
                {
                    let req = render::comp::UploadImageRequest::read_from(msg.payload);
                    let vmo = Handle(msg.handles[0]);

                    if let Ok(va) = abi::vmo::map(vmo, 0, Rights::READ_MAP) {
                        self.image_va = va;
                        self.image_w = req.width;
                        self.image_h = req.height;
                        self.image_pixel_size = req.pixel_size;
                        self.image_tex_created = false;
                        self.walk_ctx.image_content_id = req.content_id;
                        self.upload_image();

                        console::write(self.console_ep, b"render: image uploaded\n");
                    }
                }

                let _ = msg.reply_empty();
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
    let scale = {
        let raw = device.config_read32(0x0C);

        if raw == 0 { 1 } else { raw }
    };
    let logical_w = display_w / scale;
    let logical_h = display_h / scale;
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
    console::write_u32(console_ep, b"render: scale=", scale);
    console::write_u32(console_ep, b"render: logical w=", logical_w);
    console::write_u32(console_ep, b"render: logical h=", logical_h);

    setup_pipeline(
        &device,
        &mut setup_vq,
        irq_event,
        &setup_dma,
        setup_buf_size,
    );

    console::write(console_ep, b"render: pipeline ready\n");

    let walk_ctx = WalkContext {
        atlas: GlyphAtlas::new_boxed(),
        scratch: alloc::boxed::Box::new(RasterScratch::zeroed()),
        raster_buf: alloc::vec![0u8; RASTER_BUF_SIZE],
        font_metrics: [
            metrics_for_font(FONT_MONO),
            metrics_for_font(FONT_SANS),
            metrics_for_font(FONT_SERIF),
        ],
        scale,
        atlas_dirty: false,
        now_tick: 0,
        next_deadline: 0,
        image_content_id: 0,
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
        logical_w,
        logical_h,
        scale,
        scene_va: 0,
        frame_count: 0,
        walk_ctx,
        atlas_upload_y: 0,
        cursor_uploaded: false,
        cursor_visible: false,
        cursor_shape: scene::CURSOR_POINTER,
        image_va: 0,
        image_w: 0,
        image_h: 0,
        image_pixel_size: 0,
        image_tex_created: false,
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
