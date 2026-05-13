//! Metal render driver — GPU compositor for Metal-over-virtio.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!   Handle 5: font VMO (bundled font data)
//!   Handle 6: service endpoint (pre-registered by init as "render")
//!
//! Probes the virtio MMIO region for a Metal GPU device (device ID 22).
//! Sets up two virtqueues (setup + render), compiles shaders, creates
//! a render pipeline, and enters a vsync-driven render loop. The
//! presenter connects via `comp::SETUP` (passing the scene graph VMO).
//! The compositor self-drives rendering at hardware refresh rate by
//! polling generation counters on the scene graph VMO and image VMOs.

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

// ── Font data — packed style IDs ──────────────────────────────────
//
// style_id encodes font_family, weight, and flags in a single u32:
//   bits [0..2)  = font_family (0=mono, 1=sans, 2=serif)
//   bits [2..16) = weight (100-900)
//   bits [16..19) = flags (bit 0=italic)
//
// The atlas cache uses (glyph_id, font_size, style_id) as key, so
// each unique (family, weight, flags) combination gets its own entries.

pub const STYLE_MONO: u32 = 0;
pub const STYLE_SANS: u32 = 1;
pub const STYLE_SERIF: u32 = 2;

static mut FONT_VA: usize = 0;

fn font(index: usize) -> &'static [u8] {
    unsafe { init::font_data(FONT_VA, index) }
}

fn unpack_family(style_id: u32) -> u32 {
    style_id & 0x3
}

fn unpack_weight(style_id: u32) -> u16 {
    ((style_id >> 2) & 0x3FFF) as u16
}

fn unpack_flags(style_id: u32) -> u8 {
    ((style_id >> 16) & 0x7) as u8
}

fn font_for_style(style_id: u32) -> &'static [u8] {
    let italic = unpack_flags(style_id) & 1 != 0;

    match unpack_family(style_id) {
        STYLE_SANS => {
            if italic {
                font(init::FONT_IDX_SANS_ITALIC)
            } else {
                font(init::FONT_IDX_SANS)
            }
        }
        STYLE_SERIF => {
            if italic {
                font(init::FONT_IDX_SERIF_ITALIC)
            } else {
                font(init::FONT_IDX_SERIF)
            }
        }
        _ => {
            if italic {
                font(init::FONT_IDX_MONO_ITALIC)
            } else {
                font(init::FONT_IDX_MONO)
            }
        }
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
const HANDLE_FONT_VMO: Handle = Handle(5);
const HANDLE_SVC_EP: Handle = Handle(6);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;

const IMAGE_GEN_HEADER_SIZE: usize = render::comp::IMAGE_LIVE_HEADER_SIZE;

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

struct GradientParams {
    float rect_min_x;
    float rect_min_y;
    float rect_max_x;
    float rect_max_y;
    float4 color0;
    float4 color1;
    float angle;
    float kind;
    float _pad0;
    float _pad1;
};

fragment float4 fragment_gradient(
    VertexOut in [[stage_in]],
    constant GradientParams& params [[buffer(0)]]
) {
    float2 center = 0.5 * float2(params.rect_min_x + params.rect_max_x,
                                   params.rect_min_y + params.rect_max_y);
    float2 half_ext = 0.5 * float2(params.rect_max_x - params.rect_min_x,
                                     params.rect_max_y - params.rect_min_y);
    float2 norm = (in.texCoord - center) / max(half_ext, 0.001);
    int gk = int(params.kind);
    float t;
    if (gk == 0) {
        float2 dir = float2(cos(params.angle), sin(params.angle));
        t = saturate(0.5 + 0.5 * dot(norm, dir));
    } else if (gk == 1) {
        t = saturate(length(norm));
    } else {
        float a = atan2(norm.y, norm.x) - params.angle;
        t = fract(a / 6.28318530718);
    }
    float3 c0 = srgb_to_linear(params.color0.rgb);
    float3 c1 = srgb_to_linear(params.color1.rgb);
    float a0 = params.color0.a;
    float a1 = params.color1.a;
    return float4(mix(c0, c1, t), mix(a0, a1, t));
}

fragment float4 fragment_gradient_masked(
    VertexOut in [[stage_in]],
    constant GradientParams& params [[buffer(0)]],
    texture2d<float> tex [[texture(0)]],
    sampler s [[sampler(0)]]
) {
    float coverage = tex.sample(s, in.texCoord).r;
    if (coverage <= 0.0) return float4(0.0);
    float2 p = in.position.xy;
    float2 center = 0.5 * float2(params.rect_min_x + params.rect_max_x,
                                   params.rect_min_y + params.rect_max_y);
    float2 half_ext = 0.5 * float2(params.rect_max_x - params.rect_min_x,
                                     params.rect_max_y - params.rect_min_y);
    float2 norm = (p - center) / max(half_ext, 0.001);
    int gk = int(params.kind);
    float t;
    if (gk == 0) {
        float2 dir = float2(cos(params.angle), sin(params.angle));
        t = saturate(0.5 + 0.5 * dot(norm, dir));
    } else if (gk == 1) {
        t = saturate(length(norm));
    } else {
        float a = atan2(norm.y, norm.x) - params.angle;
        t = fract(a / 6.28318530718);
    }
    float3 c0 = srgb_to_linear(params.color0.rgb);
    float3 c1 = srgb_to_linear(params.color1.rgb);
    float alpha = mix(params.color0.a, params.color1.a, t) * coverage;
    return float4(mix(c0, c1, t), alpha);
}

struct BlurParams {
    float2 step;
    float2 rect_center;
    float2 rect_half_ext;
    float corner_radius;
    float _pad;
    float4 inv_xform;
};

fragment float4 fragment_blur(
    VertexOut in [[stage_in]],
    texture2d<float> tex [[texture(0)]],
    sampler s [[sampler(0)]],
    constant BlurParams& params [[buffer(0)]]
) {
    float2 uv = in.texCoord;
    float2 step = params.step;
    const float sigma = 2.0;
    const float inv_2s2 = 1.0 / (2.0 * sigma * sigma);
    float4 sum = tex.sample(s, uv);
    float wsum = 1.0;
    for (int i = 1; i <= 6; i++) {
        float w = exp(-float(i*i) * inv_2s2);
        float2 off = step * float(i);
        sum += (tex.sample(s, uv + off) + tex.sample(s, uv - off)) * w;
        wsum += 2.0 * w;
    }
    float4 blurred = sum / wsum;
    if (params.corner_radius > 0.0) {
        float2 d = in.position.xy - params.rect_center;
        float2 local_d = float2(
            d.x * params.inv_xform.x + d.y * params.inv_xform.z,
            d.x * params.inv_xform.y + d.y * params.inv_xform.w
        );
        float dist = sd_rounded_rect(local_d, params.rect_half_ext, params.corner_radius);
        if (dist > 0.5) discard_fragment();
    }
    return blurred;
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
const H_FRAG_GRADIENT: u32 = 8;
const H_FRAG_GRADIENT_MASKED: u32 = 9;
const PIPE_SOLID: u32 = 10;
const PIPE_GLYPH: u32 = 11;
const PIPE_SHADOW: u32 = 12;
#[allow(dead_code)]
const PIPE_STENCIL_WRITE: u32 = 13;
#[allow(dead_code)]
const PIPE_STENCIL_COVER: u32 = 14;
const PIPE_TEXTURED: u32 = 15;
const PIPE_GRADIENT: u32 = 16;
const PIPE_GRADIENT_MASKED: u32 = 17;
#[allow(dead_code)]
const DSS_STENCIL_WRITE: u32 = 40;
#[allow(dead_code)]
const DSS_STENCIL_TEST: u32 = 41;
const TEX_ATLAS: u32 = 20;
#[allow(dead_code)]
const TEX_STENCIL: u32 = 21;
const TEX_IMAGE: u32 = 22; // + slot index; occupies 22..22+MAX_IMAGES-1
const H_FRAG_BLUR: u32 = 18;
const PIPE_BLUR: u32 = 19;
const TEX_BLUR: u32 = TEX_IMAGE + MAX_IMAGES as u32;
const TEX_PATH_COVERAGE: u32 = TEX_BLUR + 1;
const H_COMPUTE_LIB: u32 = 50;
const H_PATH_COVERAGE_FN: u32 = 51;
const PIPE_PATH_COVERAGE: u32 = 52;
const SAMPLER_NEAREST: u32 = 30;
const SAMPLER_LINEAR: u32 = 31;

const FILTER_NEAREST: u8 = 0;
const FILTER_LINEAR: u8 = 1;

const ATLAS_WIDTH: u16 = 2048;
const ATLAS_HEIGHT: u16 = 2048;

const SETUP_BUF_PAGES: usize = 8;
const RENDER_BUF_PAGES: usize = 8;

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
    Textured(u32),
    Gradient,
    GradientMasked,
    BackdropBlur,
}

#[allow(clippy::too_many_arguments)]
fn pack_gradient_params(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    c0: scene::Color,
    c1: scene::Color,
    alpha_mul: f32,
    angle: f32,
    kind: u8,
    scale: f32,
) -> [u8; 64] {
    let mut p = [0u8; 64];

    p[0..4].copy_from_slice(&(x0 * scale).to_le_bytes());
    p[4..8].copy_from_slice(&(y0 * scale).to_le_bytes());
    p[8..12].copy_from_slice(&(x1 * scale).to_le_bytes());
    p[12..16].copy_from_slice(&(y1 * scale).to_le_bytes());
    p[16..20].copy_from_slice(&(c0.r as f32 / 255.0).to_le_bytes());
    p[20..24].copy_from_slice(&(c0.g as f32 / 255.0).to_le_bytes());
    p[24..28].copy_from_slice(&(c0.b as f32 / 255.0).to_le_bytes());
    p[28..32].copy_from_slice(&(c0.a as f32 / 255.0 * alpha_mul).to_le_bytes());
    p[32..36].copy_from_slice(&(c1.r as f32 / 255.0).to_le_bytes());
    p[36..40].copy_from_slice(&(c1.g as f32 / 255.0).to_le_bytes());
    p[40..44].copy_from_slice(&(c1.b as f32 / 255.0).to_le_bytes());
    p[44..48].copy_from_slice(&(c1.a as f32 / 255.0 * alpha_mul).to_le_bytes());
    p[48..52].copy_from_slice(&angle.to_le_bytes());
    p[52..56].copy_from_slice(&(kind as f32).to_le_bytes());

    p
}

struct DrawOp {
    pipe: Pipe,
    vert_offset: usize,
    vert_bytes: usize,
    params: [u8; 64],
}

struct DrawList {
    verts: alloc::vec::Vec<u8>,
    ops: alloc::vec::Vec<DrawOp>,
    display_w: f32,
    display_h: f32,
    current_pipe: Pipe,
    current_start: usize,
}

impl Default for DrawList {
    fn default() -> Self {
        Self {
            verts: alloc::vec::Vec::new(),
            ops: alloc::vec::Vec::new(),
            display_w: 0.0,
            display_h: 0.0,
            current_pipe: Pipe::Solid,
            current_start: 0,
        }
    }
}

impl DrawList {
    fn reset(&mut self, display_w: f32, display_h: f32) {
        self.verts.clear();
        self.ops.clear();
        self.display_w = display_w;
        self.display_h = display_h;
        self.current_pipe = Pipe::Solid;
        self.current_start = 0;
    }

    fn flush_current(&mut self) {
        let end = self.verts.len();

        if end > self.current_start {
            self.ops.push(DrawOp {
                pipe: self.current_pipe,
                vert_offset: self.current_start,
                vert_bytes: end - self.current_start,
                params: [0; 64],
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

    fn transform_corners(
        tf: &scene::AffineTransform,
        cx: f32,
        cy: f32,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
    ) -> [(f32, f32); 4] {
        let tl = tf.transform_point(px - cx, py - cy);
        let tr = tf.transform_point(px + pw - cx, py - cy);
        let br = tf.transform_point(px + pw - cx, py + ph - cy);
        let bl = tf.transform_point(px - cx, py + ph - cy);

        [
            (tl.0 + cx, tl.1 + cy),
            (tr.0 + cx, tr.1 + cy),
            (br.0 + cx, br.1 + cy),
            (bl.0 + cx, bl.1 + cy),
        ]
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
    fn push_quad_maybe_transformed(
        &mut self,
        pipe: Pipe,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        color: scene::Color,
        uv0: [f32; 2],
        uv1: [f32; 2],
        tf_args: Option<(&scene::AffineTransform, f32, f32)>,
    ) {
        self.ensure_pipe(pipe);

        if let Some((tf, cx, cy)) = tf_args {
            let corners = Self::transform_corners(tf, cx, cy, px, py, pw, ph);

            push_quad_corners(
                &mut self.verts,
                self.display_w,
                self.display_h,
                corners,
                color,
                uv0,
                uv1,
            );
        } else {
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
    }

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

        let mut op_params = [0u8; 64];

        op_params[..48].copy_from_slice(&params);

        self.ops.push(DrawOp {
            pipe: Pipe::Shadow,
            vert_offset: start,
            vert_bytes: self.verts.len() - start,
            params: op_params,
        });

        self.current_start = self.verts.len();
    }

    #[allow(clippy::too_many_arguments)]
    fn push_gradient(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        color_start: scene::Color,
        color_end: scene::Color,
        kind: u8,
        angle: f32,
        scale: f32,
        tf_args: Option<(&scene::AffineTransform, f32, f32)>,
    ) {
        self.flush_current();

        let start = self.verts.len();
        let uv0 = [px * scale, py * scale];
        let uv1 = [(px + pw) * scale, (py + ph) * scale];

        if let Some((tf, cx, cy)) = tf_args {
            let corners = Self::transform_corners(tf, cx, cy, px, py, pw, ph);

            push_quad_corners(
                &mut self.verts,
                self.display_w,
                self.display_h,
                corners,
                scene::Color::TRANSPARENT,
                uv0,
                uv1,
            );
        } else {
            push_quad(
                &mut self.verts,
                self.display_w,
                self.display_h,
                px,
                py,
                pw,
                ph,
                scene::Color::TRANSPARENT,
                uv0,
                uv1,
            );
        }

        let params = pack_gradient_params(
            px,
            py,
            px + pw,
            py + ph,
            color_start,
            color_end,
            1.0,
            angle,
            kind,
            scale,
        );

        self.ops.push(DrawOp {
            pipe: Pipe::Gradient,
            vert_offset: start,
            vert_bytes: self.verts.len() - start,
            params,
        });

        self.current_start = self.verts.len();
    }

    #[allow(clippy::too_many_arguments)]
    fn push_gradient_masked(
        &mut self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        grad_w: f32,
        grad_h: f32,
        color_start: scene::Color,
        color_end: scene::Color,
        kind: u8,
        angle: f32,
        opacity: u8,
        scale: f32,
        atlas_uv0: [f32; 2],
        atlas_uv1: [f32; 2],
        tf_args: Option<(&scene::AffineTransform, f32, f32)>,
    ) {
        self.flush_current();

        let start = self.verts.len();

        if let Some((tf, cx, cy)) = tf_args {
            let corners = Self::transform_corners(tf, cx, cy, px, py, pw, ph);

            push_quad_corners(
                &mut self.verts,
                self.display_w,
                self.display_h,
                corners,
                scene::Color::TRANSPARENT,
                atlas_uv0,
                atlas_uv1,
            );
        } else {
            push_quad(
                &mut self.verts,
                self.display_w,
                self.display_h,
                px,
                py,
                pw,
                ph,
                scene::Color::TRANSPARENT,
                atlas_uv0,
                atlas_uv1,
            );
        }

        let opa = opacity as f32 / 255.0;
        let params = pack_gradient_params(
            px,
            py,
            px + grad_w,
            py + grad_h,
            color_start,
            color_end,
            opa,
            angle,
            kind,
            scale,
        );

        self.ops.push(DrawOp {
            pipe: Pipe::GradientMasked,
            vert_offset: start,
            vert_bytes: self.verts.len() - start,
            params,
        });

        self.current_start = self.verts.len();
    }

    fn finalize(&mut self) {
        self.flush_current();
    }
}

// ── Pipeline metrics ───────────────────────────────────────────────

fn copy_into_buf(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());

    dst[..n].copy_from_slice(&src[..n]);

    n
}

struct PipelineMetrics {
    render_count: u32,
    render_total_ns: u64,
    render_max_ns: u64,
    walk_total_ns: u64,
    walk_max_ns: u64,
    atlas_upload_total_ns: u64,
    atlas_upload_count: u32,
    gpu_path_total_ns: u64,
    gpu_path_count: u32,
    emit_total_ns: u64,
    live_upload_total_ns: u64,
    live_upload_count: u32,
    live_upload_max_ns: u64,
    scene_dirty_count: u32,
    images_dirty_count: u32,
    timer_due_count: u32,
    idle_count: u32,
    ipc_count: u32,
    atlas_reset_count: u32,
    live_check_count: u32,
    live_last_gen: u32,
    loop_max_ns: u64,
    period_start_ns: u64,
}

impl PipelineMetrics {
    const fn new() -> Self {
        Self {
            render_count: 0,
            render_total_ns: 0,
            render_max_ns: 0,
            walk_total_ns: 0,
            walk_max_ns: 0,
            atlas_upload_total_ns: 0,
            atlas_upload_count: 0,
            gpu_path_total_ns: 0,
            gpu_path_count: 0,
            emit_total_ns: 0,
            live_upload_total_ns: 0,
            live_upload_count: 0,
            live_upload_max_ns: 0,
            scene_dirty_count: 0,
            images_dirty_count: 0,
            timer_due_count: 0,
            idle_count: 0,
            ipc_count: 0,
            atlas_reset_count: 0,
            live_check_count: 0,
            live_last_gen: 0,
            loop_max_ns: 0,
            period_start_ns: 0,
        }
    }

    fn report_and_reset(&mut self, ep: Handle) {
        if self.render_count == 0 && self.idle_count == 0 {
            return;
        }

        let now = abi::system::clock_read().unwrap_or(0);
        let wall_ms = now.saturating_sub(self.period_start_ns) / 1_000_000;
        let r_avg = if self.render_count > 0 {
            (self.render_total_ns / self.render_count as u64) / 1000
        } else {
            0
        };
        let w_avg = if self.render_count > 0 {
            (self.walk_total_ns / self.render_count as u64) / 1000
        } else {
            0
        };
        let lu_avg = if self.live_upload_count > 0 {
            (self.live_upload_total_ns / self.live_upload_count as u64) / 1000
        } else {
            0
        };
        let e_avg = if self.render_count > 0 {
            (self.emit_total_ns / self.render_count as u64) / 1000
        } else {
            0
        };
        let a_avg = if self.atlas_upload_count > 0 {
            (self.atlas_upload_total_ns / self.atlas_upload_count as u64) / 1000
        } else {
            0
        };
        let gp_avg = if self.gpu_path_count > 0 {
            (self.gpu_path_total_ns / self.gpu_path_count as u64) / 1000
        } else {
            0
        };
        let mut buf = [0u8; 300];
        let mut p = 0;

        // Line 1: frame totals
        p += copy_into_buf(&mut buf[p..], b"comp: ");
        p += console::format_u32(wall_ms as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"ms r=");
        p += console::format_u32(self.render_count, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b" frame=");
        p += console::format_u32(r_avg as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"/");
        p += console::format_u32((self.render_max_ns / 1000) as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"us walk=");
        p += console::format_u32(w_avg as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"/");
        p += console::format_u32((self.walk_max_ns / 1000) as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"us atlas=");
        p += console::format_u32(a_avg as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"us*");
        p += console::format_u32(self.atlas_upload_count, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b" gpu=");
        p += console::format_u32(gp_avg as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"us*");
        p += console::format_u32(self.gpu_path_count, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b" emit=");
        p += console::format_u32(e_avg as u32, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b"us");

        if self.live_upload_count > 0 {
            p += copy_into_buf(&mut buf[p..], b" live=");
            p += console::format_u32(lu_avg as u32, &mut buf[p..]);
            p += copy_into_buf(&mut buf[p..], b"us*");
            p += console::format_u32(self.live_upload_count, &mut buf[p..]);
        }

        // Event counts
        p += copy_into_buf(&mut buf[p..], b" s=");
        p += console::format_u32(self.scene_dirty_count, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b" i=");
        p += console::format_u32(self.images_dirty_count, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b" t=");
        p += console::format_u32(self.timer_due_count, &mut buf[p..]);
        p += copy_into_buf(&mut buf[p..], b" idle=");
        p += console::format_u32(self.idle_count, &mut buf[p..]);

        if self.atlas_reset_count > 0 {
            p += copy_into_buf(&mut buf[p..], b" atlas_rst=");
            p += console::format_u32(self.atlas_reset_count, &mut buf[p..]);
        }
        if self.loop_max_ns > 0 {
            p += copy_into_buf(&mut buf[p..], b" loop_max=");
            p += console::format_u32((self.loop_max_ns / 1000) as u32, &mut buf[p..]);
            p += copy_into_buf(&mut buf[p..], b"us");
        }

        buf[p] = b'\n';
        p += 1;

        console::write(ep, &buf[..p]);

        *self = Self::new();
        self.period_start_ns = now;
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

fn submit_async(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    queue_index: u32,
    dma_pa: u64,
    cmd_len: usize,
) {
    vq.push(dma_pa, cmd_len as u32, false);
    device.notify(queue_index);
}

fn ensure_completed(vq: &mut virtio::Virtqueue, irq_event: Handle, device: &virtio::Device) {
    if vq.pop_used().is_none() {
        let _ = abi::event::wait(&[(irq_event, 0x1)]);

        device.ack_interrupt();

        let _ = abi::event::clear(irq_event, 0x1);

        vq.pop_used();
    }
}

// ── GPU pipeline setup ──────────────────────────────────────────────

fn setup_pipeline(
    device: &virtio::Device,
    setup_vq: &mut virtio::Virtqueue,
    irq_event: Handle,
    setup_dma: &init::DmaBuf,
    buf_size: usize,
    phys_w: u16,
    phys_h: u16,
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
        w.get_function(H_FRAG_GRADIENT, H_LIBRARY, b"fragment_gradient");
        w.get_function(
            H_FRAG_GRADIENT_MASKED,
            H_LIBRARY,
            b"fragment_gradient_masked",
        );
        w.get_function(H_FRAG_BLUR, H_LIBRARY, b"fragment_blur");

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
        w.create_render_pipeline(
            PIPE_GRADIENT,
            H_VERTEX_FN,
            H_FRAG_GRADIENT,
            true,
            COLOR_WRITE_ALL,
            false,
            1,
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );
        w.create_render_pipeline(
            PIPE_GRADIENT_MASKED,
            H_VERTEX_FN,
            H_FRAG_GRADIENT_MASKED,
            true,
            COLOR_WRITE_ALL,
            false,
            1,
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );
        w.create_sampler(SAMPLER_NEAREST, FILTER_NEAREST, FILTER_NEAREST);
        w.create_sampler(SAMPLER_LINEAR, FILTER_LINEAR, FILTER_LINEAR);
        w.create_render_pipeline(
            PIPE_BLUR,
            H_VERTEX_FN,
            H_FRAG_BLUR,
            false,
            COLOR_WRITE_ALL,
            false,
            1,
            render::PIXEL_FORMAT_BGRA8_SRGB,
        );
        w.create_texture(
            TEX_BLUR,
            phys_w,
            phys_h,
            render::PIXEL_FORMAT_BGRA8_SRGB,
            0,
            1,
            render::TEX_USAGE_SHADER_READ | render::TEX_USAGE_RENDER_TARGET,
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

    // Batch 3: compile compute library + create compute pipeline for GPU path coverage.
    let compute_msl: &[u8] = b"
#include <metal_stdlib>
using namespace metal;

kernel void path_coverage(
    device const float4* segments [[buffer(0)]],
    device const uint& segment_count [[buffer(1)]],
    device const float4& bounds [[buffer(2)]],
    texture2d<float, access::write> output [[texture(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    float out_w = bounds.z;
    float out_h = bounds.w;
    if (float(gid.x) >= out_w || float(gid.y) >= out_h) return;

    float px = float(gid.x) + 0.5;
    float py = float(gid.y) + 0.5;
    float coverage = 0.0;
    uint n = segment_count;
    const int OVERSAMPLE = 4;

    for (int sub = 0; sub < OVERSAMPLE; sub++) {
        float spy = py + (float(sub) - 1.5) / float(OVERSAMPLE);
        float winding = 0.0;
        for (uint i = 0; i < n; i++) {
            float4 seg = segments[i];
            float y0 = seg.y, y1 = seg.w;
            float x0 = seg.x, x1 = seg.z;
            if ((y0 <= spy && y1 > spy) || (y1 <= spy && y0 > spy)) {
                float t = (spy - y0) / (y1 - y0);
                float ix = x0 + t * (x1 - x0);
                if (ix <= px) {
                    winding += (y1 > y0) ? 1.0 : -1.0;
                }
            }
        }
        coverage += clamp(abs(winding), 0.0, 1.0);
    }
    coverage /= float(OVERSAMPLE);
    output.write(float4(coverage, 0.0, 0.0, 0.0), gid);
}
";
    let len = {
        let mut w = CommandWriter::new(dma_buf);

        w.compile_library(H_COMPUTE_LIB, compute_msl);
        w.get_function(H_PATH_COVERAGE_FN, H_COMPUTE_LIB, b"path_coverage");
        w.create_compute_pipeline(PIPE_PATH_COVERAGE, H_PATH_COVERAGE_FN);
        // Create temporary R8 coverage texture for compute output (max icon size).
        w.create_texture(
            TEX_PATH_COVERAGE,
            256,
            256,
            render::PIXEL_FORMAT_R8_UNORM,
            0,
            1,
            render::TEX_USAGE_SHADER_READ | render::TEX_USAGE_SHADER_WRITE,
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

const RASTER_BUF_SIZE: usize = 100 * 100;
const ATLAS_W_F: f32 = atlas::ATLAS_WIDTH as f32;
const ATLAS_H_F: f32 = atlas::ATLAS_HEIGHT as f32;
const NS_PER_MS: u64 = 1_000_000;
const MAX_GPU_SEGMENTS: usize = 256;

struct PendingGpuPath {
    seg_count: u16,
    atlas_u: u16,
    atlas_v: u16,
    width: u16,
    height: u16,
    seg_buf: alloc::vec::Vec<u8>,
}

struct WalkContext {
    atlas: alloc::boxed::Box<GlyphAtlas>,
    scratch: alloc::boxed::Box<RasterScratch>,
    raster_buf: alloc::vec::Vec<u8>,
    font_metrics: [FontMetricsEntry; 3],
    scale: u32,
    frame_interval_ns: u64,
    atlas_dirty: bool,
    atlas_full: bool,
    now_tick: u64,
    next_deadline: u64,
    images: [ImageSlot; MAX_IMAGES],
    pending_gpu_paths: alloc::vec::Vec<PendingGpuPath>,
}

fn evaluate_animation(anim: &scene::Animation, now_ns: u64, frame_interval_ns: u64) -> (u8, u64) {
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

            return (v as u8, now_ns + frame_interval_ns);
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
            let (val, deadline) =
                evaluate_animation(&node.animation, ctx.now_tick, ctx.frame_interval_ns);

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

    if node.backdrop_blur_radius > 0 && w > 0.0 && h > 0.0 {
        draws.flush_current();

        let corners = if has_transform {
            DrawList::transform_corners(&node.transform, x + w / 2.0, y + h / 2.0, x, y, w, h)
        } else {
            [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
        };
        let mut params = [0u8; 64];

        // Pack 4 corners (8 floats = 32 bytes)
        for (i, &(cx, cy)) in corners.iter().enumerate() {
            params[i * 8..i * 8 + 4].copy_from_slice(&cx.to_le_bytes());
            params[i * 8 + 4..i * 8 + 8].copy_from_slice(&cy.to_le_bytes());
        }

        params[32..36].copy_from_slice(&(node.backdrop_blur_radius as f32).to_le_bytes());
        params[36..40].copy_from_slice(&(ctx.scale as f32).to_le_bytes());
        params[40..44].copy_from_slice(&(node.corner_radius as f32).to_le_bytes());

        draws.ops.push(DrawOp {
            pipe: Pipe::BackdropBlur,
            vert_offset: 0,
            vert_bytes: 0,
            params,
        });
    }

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
            let fm = &ctx.font_metrics[unpack_family(style_id) as usize];
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

                        draws.push_quad_maybe_transformed(
                            Pipe::Glyph,
                            px,
                            py,
                            pw,
                            ph,
                            color,
                            [u0, v0],
                            [u1, v1],
                            tf_args,
                        );
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
                    tf_args,
                );
            }
        }
        Content::Image {
            content_id,
            src_width,
            src_height,
        } if content_id != 0 && src_width > 0 && src_height > 0 => {
            let slot = ctx
                .images
                .iter()
                .find(|s| s.content_id == content_id && s.tex_created);

            if let Some(img) = slot {
                let tex_id = img.tex_id;
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

                draws.push_quad_maybe_transformed(
                    Pipe::Textured(tex_id),
                    draw_x,
                    draw_y,
                    draw_w,
                    draw_h,
                    scene::Color::rgba(255, 255, 255, 255),
                    [0.0, 0.0],
                    [1.0, 1.0],
                    tf_args,
                );
            }
        }
        Content::Gradient {
            color_start,
            color_end,
            kind,
            angle_fp,
            ..
        } => {
            let angle = angle_fp as f32 / 65536.0 * core::f32::consts::TAU;

            draws.push_gradient(
                x,
                y,
                w,
                h,
                color_start,
                color_end,
                kind as u8,
                angle,
                ctx.scale as f32,
                tf_args,
            );
        }
        Content::GradientPath {
            color_start,
            color_end,
            kind,
            angle_fp,
            contours,
            ..
        } => {
            let path_data = reader.data(contours);

            if !path_data.is_empty() {
                let scale = ctx.scale as f32;
                let inv_scale = 1.0 / scale;
                let pw = (w * scale) as u32;
                let ph = (h * scale) as u32;

                if pw > 0 && ph > 0 && pw <= 512 && ph <= 512 {
                    let cache_key = node.content_hash | PATH_STYLE_SENTINEL;
                    let entry = lookup_or_rasterize_path(
                        ctx,
                        path_data,
                        pw,
                        ph,
                        scale,
                        scene::FillRule::Winding,
                        None,
                        pw as u16,
                        cache_key,
                    );

                    if let Some(e) = entry.filter(|e| e.width > 0 && e.height > 0) {
                        let angle = angle_fp as f32 / 65536.0 * core::f32::consts::TAU;
                        let qw = e.width as f32 * inv_scale;
                        let qh = e.height as f32 * inv_scale;
                        let u0 = e.u as f32 / ATLAS_W_F;
                        let v0 = e.v as f32 / ATLAS_H_F;
                        let u1 = (e.u + e.width) as f32 / ATLAS_W_F;
                        let v1 = (e.v + e.height) as f32 / ATLAS_H_F;

                        draws.push_gradient_masked(
                            x,
                            y,
                            qw,
                            qh,
                            w,
                            h,
                            color_start,
                            color_end,
                            kind as u8,
                            angle,
                            effective_opacity,
                            scale,
                            [u0, v0],
                            [u1, v1],
                            tf_args,
                        );
                    }
                }
            }
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
    let weight = unpack_weight(style_id);
    let mut axes_buf = [fonts::metrics::AxisValue {
        tag: [0; 4],
        value: 0.0,
    }; 1];
    let mut axis_count = 0;

    if weight != 0 && weight != 400 {
        axes_buf[axis_count] = fonts::metrics::AxisValue {
            tag: *b"wght",
            value: weight as f32,
        };
        axis_count += 1;
    }

    let metrics = fonts::rasterize::rasterize_with_axes(
        font_data,
        glyph_id,
        font_size,
        &mut buf,
        &mut ctx.scratch,
        &axes_buf[..axis_count],
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
        ctx.atlas_full = true;

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
    tf_args: Option<(&scene::AffineTransform, f32, f32)>,
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

            draws.push_quad_maybe_transformed(
                Pipe::Glyph,
                x,
                y,
                e.width as f32 * inv_scale,
                e.height as f32 * inv_scale,
                c,
                [u0, v0],
                [u1, v1],
                tf_args,
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

                draws.push_quad_maybe_transformed(
                    Pipe::Glyph,
                    x,
                    y,
                    e.width as f32 * inv_scale,
                    e.height as f32 * inv_scale,
                    c,
                    [u0, v0],
                    [u1, v1],
                    tf_args,
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

    // GPU compute path: flatten to segments, reserve space in the GPU
    // region of the atlas (no CPU pixels written), queue for dispatch.
    if pw > 0 && ph > 0 && pw <= 256 && ph <= 256 {
        let mut seg_buf = alloc::vec![0u8; MAX_GPU_SEGMENTS * 16];
        let seg_count = path::flatten_to_buffer(path_data, scale, stroke_data, &mut seg_buf);

        if seg_count > 0
            && seg_count <= MAX_GPU_SEGMENTS
            && ctx
                .atlas
                .pack_gpu(1, cache_size, cache_hash, pw as u16, ph as u16)
            && let Some(e) = ctx.atlas.lookup(1, cache_size, cache_hash)
        {
            seg_buf.truncate(seg_count * 16);

            ctx.pending_gpu_paths.push(PendingGpuPath {
                seg_count: seg_count as u16,
                atlas_u: e.u,
                atlas_v: e.v,
                width: pw as u16,
                height: ph as u16,
                seg_buf,
            });

            return Some(*e);
        }
    }

    // CPU fallback: rasterize on CPU, pack into the CPU region.
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
        ctx.atlas_full = true;

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
    let has_closed = icon.paths.iter().any(|p| p.is_closed());
    let has_open = icon.paths.iter().any(|p| !p.is_closed());
    let (body, outline) = if !has_closed {
        // All open (I-beam): stroke as black body, wider stroke as white outline.
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
    } else if !has_open {
        // All closed (arrow): fill as black body, stroke as white outline.
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
    } else {
        // Mixed (arrow + badge): fill only closed shapes, stroke open
        // shapes separately, union. Each group gets its own outline width.
        let mut closed_cmds = alloc::vec::Vec::new();
        let mut open_cmds = alloc::vec::Vec::new();

        for path in icon.paths {
            if path.is_closed() {
                closed_cmds.extend_from_slice(path.commands);
            } else {
                open_cmds.extend_from_slice(path.commands);
            }
        }

        let closed_data = offset_path(&closed_cmds, margin_vb, margin_vb);
        let open_data = offset_path(&open_cmds, margin_vb, margin_vb);
        let fill = path::rasterize_path(
            &closed_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            None,
        );
        let closed_outline_exp = scene::stroke::expand_stroke(&closed_data, stroke_w);
        let closed_outline = path::rasterize_path(
            &closed_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            Some(&closed_outline_exp),
        );
        let open_body_exp = scene::stroke::expand_stroke(&open_data, stroke_w);
        let open_body = path::rasterize_path(
            &open_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            Some(&open_body_exp),
        );
        let open_outline_exp = scene::stroke::expand_stroke(&open_data, stroke_w + 2.0);
        let open_outline = path::rasterize_path(
            &open_data,
            tex_sz,
            tex_sz,
            raster_scale,
            scene::FillRule::Winding,
            Some(&open_outline_exp),
        );
        let n = (tex_sz * tex_sz) as usize;
        let mut body = fill;
        let mut outline = closed_outline;

        for i in 0..n {
            body[i] = body[i].max(open_body[i]);
            outline[i] = outline[i].max(open_outline[i]);
        }

        (body, outline)
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
    render_dma: [init::DmaBuf; 2],
    render_dma_idx: usize,
    in_flight: bool,
    setup_buf_size: usize,
    render_buf_size: usize,

    console_ep: Handle,
    logical_w: u32,
    logical_h: u32,
    scale: u32,
    refresh_hz: u32,

    swap_va: usize,
    scene_vas: [usize; 2],
    frame_count: u32,
    last_scene_gen: u32,
    walk_ctx: WalkContext,
    atlas_upload_y: u16,

    cursor_uploaded: bool,
    cursor_visible: bool,
    cursor_shape: u8,

    images: [ImageSlot; MAX_IMAGES],

    pending_cursor: Option<(f32, f32)>,
    draw_list: DrawList,
    metrics: PipelineMetrics,
}

const MAX_IMAGES: usize = 4;

#[derive(Clone, Copy)]
struct ImageSlot {
    content_id: u32,
    tex_id: u32,
    va: usize,
    w: u16,
    h: u16,
    pixel_size: u32,
    pixel_offset: usize,
    last_gen: u64,
    tex_created: bool,
    is_live: bool,
    host_bound: bool,
    host_handle: u32,
}

impl ImageSlot {
    const EMPTY: Self = Self {
        content_id: 0,
        tex_id: 0,
        va: 0,
        w: 0,
        h: 0,
        pixel_size: 0,
        pixel_offset: 0,
        last_gen: 0,
        tex_created: false,
        is_live: false,
        host_bound: false,
        host_handle: 0,
    };
}

impl Compositor {
    fn ensure_render_idle(&mut self) {
        if self.in_flight {
            ensure_completed(&mut self.render_vq, self.irq_event, &self.device);

            self.in_flight = false;
        }
    }

    fn flush_pending_cursor(&mut self) {
        let (x, y) = match self.pending_cursor.take() {
            Some(pos) => pos,
            None => return,
        };

        self.ensure_render_idle();

        // SAFETY: render_dma.va is a valid DMA allocation.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(
                self.render_dma[self.render_dma_idx].va as *mut u8,
                self.render_buf_size,
            )
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
            self.render_dma[self.render_dma_idx].pa,
            len,
        );
    }

    fn dispatch_gpu_paths(&mut self) {
        if self.walk_ctx.pending_gpu_paths.is_empty() {
            return;
        }

        for pending in core::mem::take(&mut self.walk_ctx.pending_gpu_paths) {
            if pending.seg_count == 0 {
                continue;
            }

            self.ensure_render_idle();

            // SAFETY: render_dma.va is a valid DMA allocation.
            let dma_buf = unsafe {
                core::slice::from_raw_parts_mut(
                    self.render_dma[self.render_dma_idx].va as *mut u8,
                    self.render_buf_size,
                )
            };
            let seg_bytes = &pending.seg_buf[..pending.seg_count as usize * 16];
            let bounds = [0.0f32, 0.0f32, pending.width as f32, pending.height as f32];
            // SAFETY: [f32; 4] and [u8; 16] have the same size and alignment requirements are met.
            let bounds_bytes: [u8; 16] = unsafe { core::mem::transmute(bounds) };
            let len = {
                let mut w = CommandWriter::new(dma_buf);

                w.begin_compute_pass();
                w.set_compute_pipeline(PIPE_PATH_COVERAGE);
                w.set_compute_texture(TEX_PATH_COVERAGE, 0);
                w.set_compute_bytes(0, seg_bytes);
                w.set_compute_bytes(1, &(pending.seg_count as u32).to_le_bytes());
                w.set_compute_bytes(2, &bounds_bytes);
                w.dispatch_threads(pending.width, pending.height, 1, 16, 16, 1);
                w.end_compute_pass();
                w.begin_blit_pass();
                w.copy_texture_region(
                    TEX_PATH_COVERAGE,
                    TEX_ATLAS,
                    0,
                    0,
                    pending.width,
                    pending.height,
                    pending.atlas_u,
                    pending.atlas_v,
                );
                w.end_blit_pass();

                w.len()
            };

            submit_and_wait(
                &self.device,
                &mut self.render_vq,
                self.irq_event,
                render::VIRTQ_RENDER,
                self.render_dma[self.render_dma_idx].pa,
                len,
            );
        }
    }

    fn upload_atlas_dirty(&mut self) {
        if !self.walk_ctx.atlas_dirty {
            return;
        }

        let max_y = self.walk_ctx.atlas.row_y + self.walk_ctx.atlas.row_h;

        if max_y == 0 {
            return;
        }

        let start_y = self.atlas_upload_y;

        if max_y <= start_y {
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
            let pixel_data = &self.walk_ctx.atlas.pixels[src_offset..src_offset + pixel_count];
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

        self.atlas_upload_y = self.walk_ctx.atlas.row_y;
        self.walk_ctx.atlas_dirty = false;
    }

    fn cursor_icon_name(&self) -> &'static str {
        match self.cursor_shape {
            scene::CURSOR_TEXT => "cursor-text",
            scene::CURSOR_PRESSABLE => "pointer-plus",
            scene::CURSOR_DISABLED => "pointer-x",
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

        self.ensure_render_idle();

        // SAFETY: render_dma.va is a valid DMA allocation.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(
                self.render_dma[self.render_dma_idx].va as *mut u8,
                self.render_buf_size,
            )
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
            self.render_dma[self.render_dma_idx].pa,
            len,
        );

        self.cursor_uploaded = true;
        self.cursor_visible = true;
    }

    fn update_cursor_position(&mut self, x: f32, y: f32) {
        self.upload_cursor();

        self.pending_cursor = Some((x, y));
    }

    fn upload_image_slot(&mut self, slot: usize) {
        let img = &self.images[slot];

        if img.va == 0 || img.w == 0 || img.h == 0 {
            return;
        }

        let tex_id = img.tex_id;
        let img_w = img.w;
        let img_h = img.h;
        let pixel_size = img.pixel_size;
        let img_va = img.va;
        // SAFETY: setup_dma.va is a valid DMA allocation of setup_buf_size bytes.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(self.setup_dma.va as *mut u8, self.setup_buf_size)
        };

        if !self.images[slot].tex_created {
            let len = {
                let mut w = CommandWriter::new(dma_buf);

                w.create_texture(
                    tex_id,
                    img_w,
                    img_h,
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

            self.images[slot].tex_created = true;
        }

        let pixel_offset = self.images[slot].pixel_offset;
        let row_bytes = img_w as usize * 4;
        let cmd_overhead = render::HEADER_SIZE + 16;
        let max_data_per_submit = self.setup_buf_size - cmd_overhead;
        let max_rows_per_submit = (max_data_per_submit / row_bytes).max(1);
        let total_rows = img_h as usize;
        // SAFETY: img_va + pixel_offset is the start of pixel data.
        let pixels = unsafe {
            core::slice::from_raw_parts((img_va + pixel_offset) as *const u8, pixel_size as usize)
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
                    tex_id,
                    0,
                    y as u16,
                    img_w,
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

    fn check_scene_dirty(&mut self) -> bool {
        if self.swap_va == 0 {
            return false;
        }

        let swap_gen = scene::SceneSwapHeader::read_generation(self.swap_va);

        if swap_gen != self.last_scene_gen {
            self.last_scene_gen = swap_gen;

            true
        } else {
            false
        }
    }

    fn refresh_live_images(&mut self) -> bool {
        let mut changed = false;

        for i in 0..MAX_IMAGES {
            let img = &self.images[i];

            if !img.is_live || !img.tex_created {
                continue;
            }

            if img.host_bound {
                // Zero-copy path: the host already updated the IOSurface.
                // Check gen counter in the signal VMO to know when to re-render.
                if img.va == 0 {
                    continue;
                }

                let img_va = img.va;
                let img_tex_id = img.tex_id;
                let img_host_handle = img.host_handle;
                let img_last_gen = img.last_gen;
                // SAFETY: va is a valid RO mapping. Gen counter at offset 0.
                let current_gen = unsafe {
                    let ptr = img_va as *const core::sync::atomic::AtomicU64;

                    (*ptr).load(core::sync::atomic::Ordering::Acquire)
                };

                self.metrics.live_check_count += 1;

                if current_gen != img_last_gen {
                    let t0 = abi::system::clock_read().unwrap_or(0);

                    self.images[i].last_gen = current_gen;

                    // Re-bind in case the host rotated the IOSurface.
                    // SAFETY: setup_dma.va is a valid DMA allocation.
                    let dma_buf = unsafe {
                        core::slice::from_raw_parts_mut(
                            self.setup_dma.va as *mut u8,
                            self.setup_buf_size,
                        )
                    };
                    let len = {
                        let mut w = CommandWriter::new(dma_buf);

                        w.bind_host_texture(img_tex_id, img_host_handle);

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

                    let dt = abi::system::clock_read().unwrap_or(0).saturating_sub(t0);

                    self.metrics.live_upload_count += 1;
                    self.metrics.live_upload_total_ns += dt;

                    if dt > self.metrics.live_upload_max_ns {
                        self.metrics.live_upload_max_ns = dt;
                    }

                    changed = true;
                }

                continue;
            }

            if img.va == 0 {
                continue;
            }

            // SAFETY: va is a valid RO mapping. The generation counter is an
            // aligned u64 at offset 0. Acquire ordering ensures subsequent
            // pixel reads see data written before the gen store.
            let current_gen = unsafe {
                let ptr = img.va as *const core::sync::atomic::AtomicU64;

                (*ptr).load(core::sync::atomic::Ordering::Acquire)
            };

            self.metrics.live_check_count += 1;
            self.metrics.live_last_gen = current_gen as u32;

            if current_gen != img.last_gen {
                let t0 = abi::system::clock_read().unwrap_or(0);

                self.images[i].last_gen = current_gen;

                self.upload_image_slot(i);

                let dt = abi::system::clock_read().unwrap_or(0).saturating_sub(t0);

                self.metrics.live_upload_count += 1;
                self.metrics.live_upload_total_ns += dt;

                if dt > self.metrics.live_upload_max_ns {
                    self.metrics.live_upload_max_ns = dt;
                }

                changed = true;
            }
        }

        changed
    }

    fn find_or_alloc_image_slot(&self, content_id: u32) -> Option<usize> {
        (0..MAX_IMAGES)
            .find(|&i| self.images[i].content_id == content_id)
            .or_else(|| (0..MAX_IMAGES).find(|&i| self.images[i].content_id == 0))
    }

    fn render_frame(&mut self) -> u64 {
        let frame_t0 = abi::system::clock_read().unwrap_or(0);

        if self.swap_va == 0 {
            return 0;
        }

        if self.in_flight {
            self.ensure_render_idle();

            self.render_dma_idx = 1 - self.render_dma_idx;
        }

        if self.walk_ctx.atlas_full {
            self.metrics.atlas_reset_count += 1;
            self.walk_ctx.atlas.reset();
            self.walk_ctx.atlas_full = false;
            self.atlas_upload_y = 0;
        }

        let now = frame_t0;

        self.walk_ctx.now_tick = now;
        self.walk_ctx.next_deadline = 0;
        self.walk_ctx.images = self.images;

        let active = scene::SceneSwapHeader::read_active_index(self.swap_va);
        let scene_va = self.scene_vas[active & 1];

        if scene_va == 0 {
            return 0;
        }

        // SAFETY: scene_va is a valid RO mapping of at least SCENE_SIZE bytes.
        // The buffer is complete and immutable — the presenter only writes
        // to the back buffer and swaps atomically.
        let scene_buf = unsafe { core::slice::from_raw_parts(scene_va as *const u8, SCENE_SIZE) };
        let reader = SceneReader::new(scene_buf);
        let root = reader.root();

        if reader.node_count() == 0 || root == NULL {
            return 0;
        }

        let root_node = reader.node(root);
        let bg = root_node.background;
        let mut draws = core::mem::take(&mut self.draw_list);

        draws.reset(self.logical_w as f32, self.logical_h as f32);

        let walk_t0 = abi::system::clock_read().unwrap_or(0);

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

        let walk_dt = abi::system::clock_read()
            .unwrap_or(0)
            .saturating_sub(walk_t0);

        self.metrics.walk_total_ns += walk_dt;

        if walk_dt > self.metrics.walk_max_ns {
            self.metrics.walk_max_ns = walk_dt;
        }

        draws.finalize();

        let atlas_t0 = abi::system::clock_read().unwrap_or(0);

        self.upload_atlas_dirty();

        let atlas_dt = abi::system::clock_read()
            .unwrap_or(0)
            .saturating_sub(atlas_t0);

        self.metrics.atlas_upload_total_ns += atlas_dt;

        if atlas_dt > 0 {
            self.metrics.atlas_upload_count += 1;
        }

        let gpu_t0 = abi::system::clock_read().unwrap_or(0);
        let gpu_path_n = self.walk_ctx.pending_gpu_paths.len() as u32;

        self.dispatch_gpu_paths();

        let gpu_dt = abi::system::clock_read()
            .unwrap_or(0)
            .saturating_sub(gpu_t0);

        self.metrics.gpu_path_total_ns += gpu_dt;
        self.metrics.gpu_path_count += gpu_path_n;
        self.flush_pending_cursor();

        let emit_t0 = abi::system::clock_read().unwrap_or(0);
        let clear_r = srgb_to_linear(bg.r as f32 / 255.0);
        let clear_g = srgb_to_linear(bg.g as f32 / 255.0);
        let clear_b = srgb_to_linear(bg.b as f32 / 255.0);
        // SAFETY: render_dma.va is a valid DMA allocation of render_buf_size bytes.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(
                self.render_dma[self.render_dma_idx].va as *mut u8,
                self.render_buf_size,
            )
        };
        let len = {
            let mut w = CommandWriter::new(dma_buf);
            let mut in_render_pass = false;
            let mut active_pipe: Option<Pipe> = None;
            let mut op_idx = 0;

            while op_idx < draws.ops.len() {
                if matches!(draws.ops[op_idx].pipe, Pipe::BackdropBlur) {
                    let op = &draws.ops[op_idx];

                    if in_render_pass {
                        w.end_render_pass();
                        in_render_pass = false;
                    }

                    let mut corners = [(0.0f32, 0.0f32); 4];

                    for (i, corner) in corners.iter_mut().enumerate() {
                        corner.0 =
                            f32::from_le_bytes(op.params[i * 8..i * 8 + 4].try_into().unwrap());
                        corner.1 =
                            f32::from_le_bytes(op.params[i * 8 + 4..i * 8 + 8].try_into().unwrap());
                    }

                    let radius_pt = f32::from_le_bytes(op.params[32..36].try_into().unwrap());
                    let scale = f32::from_le_bytes(op.params[36..40].try_into().unwrap());
                    let dw = self.logical_w as f32;
                    let dh = self.logical_h as f32;
                    let phys_w = dw * scale;
                    let phys_h = dh * scale;
                    let sigma_px = radius_pt * scale / 2.0;
                    let step_px = if sigma_px > 0.0 { sigma_px / 6.0 } else { 1.0 };
                    let mut min_x = corners[0].0;
                    let mut min_y = corners[0].1;
                    let mut max_x = corners[0].0;
                    let mut max_y = corners[0].1;

                    for &(cx, cy) in &corners[1..] {
                        if cx < min_x {
                            min_x = cx;
                        }
                        if cy < min_y {
                            min_y = cy;
                        }
                        if cx > max_x {
                            max_x = cx;
                        }
                        if cy > max_y {
                            max_y = cy;
                        }
                    }

                    let pad = (6.0 * step_px) as u16 + 1;
                    let sx = ((min_x * scale) as u16).saturating_sub(pad);
                    let sy = ((min_y * scale) as u16).saturating_sub(pad);
                    let sx1 = (max_x * scale) as u16 + pad;
                    let sy1 = (max_y * scale) as u16 + pad;
                    let sw = (sx1.min(phys_w as u16)).saturating_sub(sx);
                    let sh = (sy1.min(phys_h as u16)).saturating_sub(sy);
                    let to_ndc = |px: f32, py: f32| (px / dw * 2.0 - 1.0, 1.0 - py / dh * 2.0);
                    let to_uv = |px: f32, py: f32| (px / dw, py / dh);
                    let tl = corners[0];
                    let tr = corners[1];
                    let br = corners[2];
                    let bl = corners[3];
                    let tri_verts: [((f32, f32), (f32, f32)); 6] = [
                        (to_ndc(tl.0, tl.1), to_uv(tl.0, tl.1)),
                        (to_ndc(bl.0, bl.1), to_uv(bl.0, bl.1)),
                        (to_ndc(br.0, br.1), to_uv(br.0, br.1)),
                        (to_ndc(tl.0, tl.1), to_uv(tl.0, tl.1)),
                        (to_ndc(br.0, br.1), to_uv(br.0, br.1)),
                        (to_ndc(tr.0, tr.1), to_uv(tr.0, tr.1)),
                    ];
                    let mut quad = [0u8; QUAD_BYTES];
                    let c = [1.0f32, 1.0, 1.0, 1.0];
                    let mut off = 0;

                    for &(pos, tc) in &tri_verts {
                        quad[off..off + 4].copy_from_slice(&pos.0.to_le_bytes());
                        quad[off + 4..off + 8].copy_from_slice(&pos.1.to_le_bytes());
                        quad[off + 8..off + 12].copy_from_slice(&tc.0.to_le_bytes());
                        quad[off + 12..off + 16].copy_from_slice(&tc.1.to_le_bytes());
                        quad[off + 16..off + 20].copy_from_slice(&c[0].to_le_bytes());
                        quad[off + 20..off + 24].copy_from_slice(&c[1].to_le_bytes());
                        quad[off + 24..off + 28].copy_from_slice(&c[2].to_le_bytes());
                        quad[off + 28..off + 32].copy_from_slice(&c[3].to_le_bytes());
                        off += VERTEX_SIZE;
                    }

                    let corner_radius_pt =
                        f32::from_le_bytes(op.params[40..44].try_into().unwrap());
                    let phys_cr = corner_radius_pt * scale;
                    let dx_tr = tr.0 - tl.0;
                    let dy_tr = tr.1 - tl.1;
                    let dx_bl = bl.0 - tl.0;
                    let dy_bl = bl.1 - tl.1;
                    let node_w = libm::sqrtf(dx_tr * dx_tr + dy_tr * dy_tr);
                    let node_h = libm::sqrtf(dx_bl * dx_bl + dy_bl * dy_bl);
                    let center_x = (tl.0 + br.0) / 2.0 * scale;
                    let center_y = (tl.1 + br.1) / 2.0 * scale;
                    let half_w = node_w / 2.0 * scale;
                    let half_h = node_h / 2.0 * scale;
                    let (inv_a, inv_b, inv_c, inv_d) = if node_w > 1e-6 && node_h > 1e-6 {
                        let fwd_a = dx_tr / node_w;
                        let fwd_b = dy_tr / node_w;
                        let fwd_c = dx_bl / node_h;
                        let fwd_d = dy_bl / node_h;
                        let det = fwd_a * fwd_d - fwd_c * fwd_b;

                        if det.abs() > 1e-6 {
                            let inv_det = 1.0 / det;

                            (
                                fwd_d * inv_det,
                                -fwd_b * inv_det,
                                -fwd_c * inv_det,
                                fwd_a * inv_det,
                            )
                        } else {
                            (1.0f32, 0.0f32, 0.0f32, 1.0f32)
                        }
                    } else {
                        (1.0f32, 0.0f32, 0.0f32, 1.0f32)
                    };
                    let blur_params = |dx: f32, dy: f32| -> [u8; 48] {
                        let vals: [f32; 12] = [
                            dx, dy, center_x, center_y, half_w, half_h, phys_cr, 0.0, inv_a, inv_b,
                            inv_c, inv_d,
                        ];
                        let mut out = [0u8; 48];

                        for i in 0..12 {
                            out[i * 4..(i + 1) * 4].copy_from_slice(&vals[i].to_le_bytes());
                        }

                        out
                    };
                    let h_params = blur_params(step_px / phys_w, 0.0);
                    let v_params = blur_params(0.0, step_px / phys_h);

                    w.begin_blit_pass();
                    w.copy_texture_region(
                        render::DRAWABLE_HANDLE,
                        TEX_BLUR,
                        sx,
                        sy,
                        sw,
                        sh,
                        sx,
                        sy,
                    );
                    w.end_blit_pass();
                    w.begin_render_pass(
                        render::DRAWABLE_HANDLE,
                        0,
                        0,
                        render::LOAD_LOAD,
                        render::STORE_STORE,
                        0,
                        0,
                        0.0,
                        0.0,
                        0.0,
                        1.0,
                    );
                    w.set_render_pipeline(PIPE_BLUR);
                    w.set_fragment_texture(TEX_BLUR, 0);
                    w.set_fragment_sampler(SAMPLER_LINEAR, 0);
                    w.set_fragment_bytes(0, &h_params);

                    render::batch::emit_draws(&mut w, &quad);

                    w.end_render_pass();
                    w.begin_blit_pass();
                    w.copy_texture_region(
                        render::DRAWABLE_HANDLE,
                        TEX_BLUR,
                        sx,
                        sy,
                        sw,
                        sh,
                        sx,
                        sy,
                    );
                    w.end_blit_pass();
                    w.begin_render_pass(
                        render::DRAWABLE_HANDLE,
                        0,
                        0,
                        render::LOAD_LOAD,
                        render::STORE_STORE,
                        0,
                        0,
                        0.0,
                        0.0,
                        0.0,
                        1.0,
                    );
                    w.set_render_pipeline(PIPE_BLUR);
                    w.set_fragment_texture(TEX_BLUR, 0);
                    w.set_fragment_sampler(SAMPLER_LINEAR, 0);
                    w.set_fragment_bytes(0, &v_params);

                    render::batch::emit_draws(&mut w, &quad);

                    w.end_render_pass();

                    active_pipe = None;
                    op_idx += 1;

                    continue;
                }

                if !in_render_pass {
                    let load = if op_idx == 0 {
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

                    in_render_pass = true;
                }

                let op = &draws.ops[op_idx];
                let verts = &draws.verts[op.vert_offset..op.vert_offset + op.vert_bytes];
                let needs_rebind = active_pipe != Some(op.pipe)
                    || op.pipe == Pipe::Shadow
                    || op.pipe == Pipe::Gradient
                    || op.pipe == Pipe::GradientMasked;

                if needs_rebind {
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
                            w.set_fragment_bytes(0, &op.params[..48]);
                        }
                        Pipe::Gradient => {
                            w.set_render_pipeline(PIPE_GRADIENT);
                            w.set_fragment_bytes(0, &op.params);
                        }
                        Pipe::GradientMasked => {
                            w.set_render_pipeline(PIPE_GRADIENT_MASKED);
                            w.set_fragment_bytes(0, &op.params);
                            w.set_fragment_texture(TEX_ATLAS, 0);
                            w.set_fragment_sampler(SAMPLER_NEAREST, 0);
                        }
                        Pipe::Textured(tex_id) => {
                            w.set_render_pipeline(PIPE_TEXTURED);
                            w.set_fragment_texture(tex_id, 0);
                            w.set_fragment_sampler(SAMPLER_LINEAR, 0);
                        }
                        Pipe::BackdropBlur => unreachable!(),
                    }

                    active_pipe = Some(op.pipe);
                }

                render::batch::emit_draws(&mut w, verts);

                op_idx += 1;
            }

            if !in_render_pass {
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
            }

            w.end_render_pass();
            w.present_and_commit(self.frame_count);

            w.len()
        };

        submit_async(
            &self.device,
            &mut self.render_vq,
            render::VIRTQ_RENDER,
            self.render_dma[self.render_dma_idx].pa,
            len,
        );

        self.in_flight = true;
        self.frame_count += 1;

        let now = abi::system::clock_read().unwrap_or(0);
        let frame_dt = now.saturating_sub(frame_t0);
        let emit_dt = now.saturating_sub(emit_t0);

        self.metrics.render_count += 1;
        self.metrics.render_total_ns += frame_dt;
        self.metrics.emit_total_ns += emit_dt;

        if frame_dt > self.metrics.render_max_ns {
            self.metrics.render_max_ns = frame_dt;
        }

        self.draw_list = draws;

        self.walk_ctx.next_deadline
    }
}

impl Dispatch for Compositor {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            render::comp::SETUP => {
                if msg.handles.len() < 3 {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let swap_vmo = Handle(msg.handles[0]);
                let scene_vmo0 = Handle(msg.handles[1]);
                let scene_vmo1 = Handle(msg.handles[2]);
                let swap_va = match abi::vmo::map(swap_vmo, 0, Rights::READ_MAP) {
                    Ok(va) => va,
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);

                        return;
                    }
                };
                let scene_va0 = match abi::vmo::map(scene_vmo0, 0, Rights::READ_MAP) {
                    Ok(va) => va,
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);

                        return;
                    }
                };
                let scene_va1 = match abi::vmo::map(scene_vmo1, 0, Rights::READ_MAP) {
                    Ok(va) => va,
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);

                        return;
                    }
                };

                self.swap_va = swap_va;
                self.scene_vas = [scene_va0, scene_va1];

                console::write(self.console_ep, b"render: scene connected\n");

                let reply = render::comp::SetupReply {
                    display_width: self.logical_w,
                    display_height: self.logical_h,
                    refresh_hz: self.refresh_hz,
                };
                let mut data = [0u8; render::comp::SetupReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
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
                    let live = req.flags & render::comp::IMAGE_FLAG_LIVE != 0;
                    let pixel_offset = if live { IMAGE_GEN_HEADER_SIZE } else { 0 };

                    if let Ok(va) = abi::vmo::map(vmo, 0, Rights::READ_MAP) {
                        let slot = self.find_or_alloc_image_slot(req.content_id);

                        if let Some(idx) = slot {
                            self.images[idx] = ImageSlot {
                                content_id: req.content_id,
                                tex_id: TEX_IMAGE + idx as u32,
                                va,
                                w: req.width,
                                h: req.height,
                                pixel_size: req.pixel_size,
                                pixel_offset,
                                last_gen: 0,
                                tex_created: false,
                                is_live: live,
                                host_bound: false,
                                host_handle: 0,
                            };

                            self.upload_image_slot(idx);

                            console::write(self.console_ep, b"render: image uploaded\n");
                        }
                    }
                }

                let _ = msg.reply_empty();
            }
            render::comp::BIND_HOST_TEXTURE => {
                if msg.payload.len() >= render::comp::BindHostTextureRequest::SIZE {
                    let req = render::comp::BindHostTextureRequest::read_from(msg.payload);

                    if let Some(idx) = self.find_or_alloc_image_slot(req.content_id) {
                        // Send CMD_BIND_HOST_TEXTURE to the host GPU.
                        // SAFETY: setup_dma.va is a valid DMA allocation.
                        let dma_buf = unsafe {
                            core::slice::from_raw_parts_mut(
                                self.setup_dma.va as *mut u8,
                                self.setup_buf_size,
                            )
                        };
                        let len = {
                            let mut w = CommandWriter::new(dma_buf);

                            w.bind_host_texture(TEX_IMAGE + idx as u32, req.host_handle);

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

                        // Map the signal VMO for gen counter reads.
                        let va = if !msg.handles.is_empty() {
                            let vmo = Handle(msg.handles[0]);

                            abi::vmo::map(vmo, 0, Rights::READ_MAP).unwrap_or(0)
                        } else {
                            0
                        };

                        self.images[idx] = ImageSlot {
                            content_id: req.content_id,
                            tex_id: TEX_IMAGE + idx as u32,
                            va,
                            w: req.width,
                            h: req.height,
                            pixel_size: 0,
                            pixel_offset: 0,
                            last_gen: 0,
                            tex_created: true,
                            is_live: true,
                            host_bound: true,
                            host_handle: req.host_handle,
                        };

                        console::write(self.console_ep, b"render: host texture bound\n");
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
    let _ = abi::thread::set_priority(Handle::SELF, abi::types::Priority::High);

    unsafe {
        FONT_VA = abi::vmo::map(HANDLE_FONT_VMO, 0, Rights::READ_MAP).unwrap_or(0);
    }

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
    let refresh_hz = {
        let raw = device.config_read32(0x08);

        if raw == 0 { 60 } else { raw }
    };
    let scale = {
        let raw = device.config_read32(0x0C);

        if raw == 0 { 1 } else { raw }
    };
    let logical_w = display_w / scale;
    let logical_h = display_h / scale;
    let frame_interval_ns = 1_000_000_000u64 / refresh_hz as u64;
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
    let render_dma_0 = match init::request_dma(HANDLE_INIT_EP, render_buf_size) {
        Ok(d) => d,
        Err(_) => abi::thread::exit(7),
    };
    let render_dma_1 = match init::request_dma(HANDLE_INIT_EP, render_buf_size) {
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
    console::write_u32(console_ep, b"render: refresh=", refresh_hz);
    console::write_u32(console_ep, b"render: scale=", scale);
    console::write_u32(console_ep, b"render: logical w=", logical_w);
    console::write_u32(console_ep, b"render: logical h=", logical_h);

    setup_pipeline(
        &device,
        &mut setup_vq,
        irq_event,
        &setup_dma,
        setup_buf_size,
        display_w as u16,
        display_h as u16,
    );

    console::write(console_ep, b"render: pipeline ready\n");

    let walk_ctx = WalkContext {
        atlas: GlyphAtlas::new_boxed(),
        scratch: alloc::boxed::Box::new(RasterScratch::zeroed()),
        raster_buf: alloc::vec![0u8; RASTER_BUF_SIZE],
        font_metrics: [
            metrics_for_font(font(init::FONT_IDX_MONO)),
            metrics_for_font(font(init::FONT_IDX_SANS)),
            metrics_for_font(font(init::FONT_IDX_SERIF)),
        ],
        scale,
        frame_interval_ns,
        atlas_dirty: false,
        atlas_full: false,
        now_tick: 0,
        next_deadline: 0,
        images: [ImageSlot::EMPTY; MAX_IMAGES],
        pending_gpu_paths: alloc::vec::Vec::new(),
    };

    console::write(console_ep, b"render: atlas ready\n");

    console::write(console_ep, b"render: ready\n");

    let mut compositor = Compositor {
        device,
        setup_vq,
        render_vq,
        irq_event,
        setup_dma,
        render_dma: [render_dma_0, render_dma_1],
        render_dma_idx: 0,
        in_flight: false,
        setup_buf_size,
        render_buf_size,
        console_ep,
        logical_w,
        logical_h,
        scale,
        refresh_hz,
        swap_va: 0,
        scene_vas: [0, 0],
        frame_count: 0,
        last_scene_gen: 0,
        walk_ctx,
        atlas_upload_y: 0,
        cursor_uploaded: false,
        cursor_visible: false,
        cursor_shape: scene::CURSOR_DEFAULT,
        images: [ImageSlot::EMPTY; MAX_IMAGES],
        pending_cursor: None,
        draw_list: DrawList::default(),
        metrics: PipelineMetrics::new(),
    };

    {
        // SAFETY: render_dma.va is a valid DMA allocation.
        let dma_buf = unsafe {
            core::slice::from_raw_parts_mut(compositor.render_dma[0].va as *mut u8, render_buf_size)
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
                srgb_to_linear(32.0 / 255.0),
                srgb_to_linear(32.0 / 255.0),
                srgb_to_linear(32.0 / 255.0),
                1.0,
            );
            w.end_render_pass();
            w.present_and_commit(0);

            w.len()
        };

        submit_and_wait(
            &compositor.device,
            &mut compositor.render_vq,
            compositor.irq_event,
            render::VIRTQ_RENDER,
            compositor.render_dma[0].pa,
            len,
        );
    }

    let frame_interval = compositor.walk_ctx.frame_interval_ns;
    let mut next_vsync = abi::system::clock_read().unwrap_or(0) + frame_interval;
    let mut render_deadline: u64 = 0;
    let report_interval_ns: u64 = 1_000_000_000;
    let mut next_report = abi::system::clock_read().unwrap_or(0) + report_interval_ns;

    compositor.metrics.period_start_ns = abi::system::clock_read().unwrap_or(0);

    loop {
        let deadline = if render_deadline > 0 && render_deadline < next_vsync {
            render_deadline
        } else {
            next_vsync
        };

        let loop_t0 = abi::system::clock_read().unwrap_or(0);
        let got_ipc = match ipc::server::serve_one_timed(HANDLE_SVC_EP, &mut compositor, deadline) {
            Ok(()) => true,
            Err(abi::types::SyscallError::TimedOut) => false,
            Err(_) => break,
        };
        let now = abi::system::clock_read().unwrap_or(0);

        if got_ipc {
            compositor.metrics.ipc_count += 1;
        }

        if now < next_vsync && (render_deadline == 0 || now < render_deadline) {
            continue;
        }

        let images_changed = compositor.refresh_live_images();
        let scene_changed = compositor.check_scene_dirty();
        let timer_due = render_deadline > 0 && now >= render_deadline;

        if scene_changed {
            compositor.metrics.scene_dirty_count += 1;
        }
        if images_changed {
            compositor.metrics.images_dirty_count += 1;
        }
        if timer_due {
            compositor.metrics.timer_due_count += 1;
        }

        if scene_changed || images_changed || timer_due {
            let next = compositor.render_frame();

            render_deadline = if next > now { next } else { 0 };
        } else {
            compositor.metrics.idle_count += 1;
        }

        let loop_dt = abi::system::clock_read()
            .unwrap_or(0)
            .saturating_sub(loop_t0);

        if loop_dt > compositor.metrics.loop_max_ns {
            compositor.metrics.loop_max_ns = loop_dt;
        }

        if now >= next_vsync {
            next_vsync = now + frame_interval;
        }

        if now >= next_report {
            compositor.metrics.report_and_reset(compositor.console_ep);

            next_report = now + report_interval_ns;
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
