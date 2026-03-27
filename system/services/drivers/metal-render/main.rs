//! Metal render service — GPU driver for Metal-over-virtio.
//!
//! Reads the scene graph from shared memory and renders using Metal commands
//! sent over a custom virtio device (device ID 22). The hypervisor's
//! VirtioMetal device deserializes commands and replays them via the Metal API.
//!
//! Two virtqueues:
//!   - Queue 0 (setup): shader compilation, pipeline creation, texture creation
//!   - Queue 1 (render): per-frame command buffers
//!
//! The scene graph is the only interface — all rendering complexity is
//! internal to this driver (leaf node behind a simple boundary).

#![no_std]
#![no_main]

extern crate alloc;
extern crate drawing;
extern crate fonts;
extern crate render;
extern crate scene;

use alloc::vec::Vec;

use protocol::{
    compose::MSG_COMPOSITOR_CONFIG,
    device::MSG_DEVICE_CONFIG,
    gpu::{DisplayInfoMsg, MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY},
    metal::{self, DRAWABLE_HANDLE},
};
use render::frame_scheduler::frame_period_ns;
use scene::{Content, Node, NodeFlags, NodeId, NULL};

// ── Constants ────────────────────────────────────────────────────────────

/// Setup virtqueue index.
const VIRTQ_SETUP: u32 = 0;
/// Render virtqueue index.
const VIRTQ_RENDER: u32 = 1;

/// IPC handle for the init channel.
const INIT_HANDLE: u8 = 0;
/// IPC handle for the core→metal-render scene update channel.
const SCENE_HANDLE: u8 = 1;

/// Scene graph node index for the pointer cursor. Matches core's N_POINTER.
/// When the cursor plane is active, this node is skipped during walk_scene
/// and composited by the host's cursor plane instead.
const CURSOR_PLANE_NODE: NodeId = 8;

// ── Metal object handles (guest-assigned, must be nonzero) ──────────────

const LIB_SHADERS: u32 = 1;

const FN_VERTEX_MAIN: u32 = 10;
const FN_FRAGMENT_SOLID: u32 = 11;
const FN_FRAGMENT_GLYPH: u32 = 12;
const FN_FRAGMENT_TEXTURED: u32 = 13;
const FN_VERTEX_STENCIL: u32 = 14;
const FN_BLUR_H: u32 = 15;
const FN_BLUR_V: u32 = 16;
const FN_COPY_SRGB_TO_LINEAR: u32 = 17;
const FN_COPY_LINEAR_TO_SRGB: u32 = 18;
const FN_FRAGMENT_ROUNDED_RECT: u32 = 19;
const FN_FRAGMENT_SHADOW: u32 = 35;
const FN_FRAGMENT_DITHER: u32 = 37;

const PIPE_SOLID: u32 = 20;
const PIPE_TEXTURED: u32 = 21;
const PIPE_GLYPH: u32 = 22;
const PIPE_STENCIL_WRITE: u32 = 23;
const PIPE_SOLID_NO_MSAA: u32 = 24;
const PIPE_ROUNDED_RECT: u32 = 25;
const PIPE_SHADOW: u32 = 36;
const PIPE_DITHER: u32 = 38;
const CPIPE_BLUR_H: u32 = 26;
const CPIPE_BLUR_V: u32 = 27;
const CPIPE_SRGB_TO_LINEAR: u32 = 28;
const CPIPE_LINEAR_TO_SRGB: u32 = 29;

const DSS_NONE: u32 = 30;
const DSS_STENCIL_WRITE: u32 = 31;
const DSS_STENCIL_TEST: u32 = 32;
/// Clip test: pass where stencil != 0, KEEP stencil value (don't zero on pass).
/// Used for clip paths where multiple children need the same stencil mask.
const DSS_CLIP_TEST: u32 = 33;
/// Even-odd fill: INVERT stencil on each triangle overlap, then test for odd count.
const DSS_STENCIL_INVERT: u32 = 34;
/// Two-sided non-zero winding fill: front-face triangles INCR_WRAP, back-face DECR_WRAP.
/// The stencil accumulates signed winding number; non-zero = inside.
const DSS_STENCIL_WINDING: u32 = 35;

const SAMPLER_NEAREST: u32 = 41;
const SAMPLER_LINEAR: u32 = 42;

const TEX_MSAA: u32 = 50;
const TEX_STENCIL: u32 = 51;
const TEX_ATLAS: u32 = 52;
const TEX_BLUR_A: u32 = 53;
const TEX_BLUR_B: u32 = 54;
const TEX_IMAGE: u32 = 55;
/// Float16 resolve target — MSAA resolves here, then dither pass blits to drawable.
const TEX_RESOLVE: u32 = 56;

/// Maximum image texture dimension. All per-frame images are packed into
/// sub-rectangles of this single atlas texture via `ImageAtlas`.
const IMG_TEX_DIM: u32 = 1024;

/// Glyph atlas dimensions.
const ATLAS_WIDTH: u32 = 512;
const ATLAS_HEIGHT: u32 = 512;

/// Maximum vertex bytes per set_vertex_bytes call (Metal's 4KB limit).
const MAX_INLINE_BYTES: usize = 4096;
/// Bytes per vertex: position(f32x2) + texCoord(f32x2) + color(f32x4) = 32.
const VERTEX_BYTES: usize = 32;
/// Max quads per inline draw call: 4096 / (6 * 32) = 21.
const MAX_QUADS_PER_DRAW: usize = MAX_INLINE_BYTES / (6 * VERTEX_BYTES);

/// MSAA sample count (1 = no MSAA, 4 = 4x MSAA).
const SAMPLE_COUNT: u8 = 4;

// ── MSL shader source ───────────────────────────────────────────────────

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

// -- sRGB <-> linear conversion (IEC 61966-2-1) -------------------------
// With an sRGB render target, all fragment shader outputs must be in linear
// space. The hardware blender converts linear->sRGB on store and sRGB->linear
// on read, giving physically correct alpha compositing for free.

float3 srgb_to_linear(float3 s) {
    return select(
        pow((s + 0.055) / 1.055, float3(2.4)),
        s / 12.92,
        s <= 0.04045
    );
}

float3 linear_to_srgb(float3 l) {
    return select(
        1.055 * pow(l, float3(1.0/2.4)) - 0.055,
        12.92 * l,
        l <= 0.0031308
    );
}

fragment float4 fragment_solid(VertexOut in [[stage_in]]) {
    return float4(srgb_to_linear(in.color.rgb), in.color.a);
}

fragment float4 fragment_textured(
    VertexOut in [[stage_in]],
    texture2d<float> tex [[texture(0)]],
    sampler s [[sampler(0)]]
) {
    // Texture data is sRGB; vertex color is sRGB. Linearize both.
    float4 t = tex.sample(s, in.texCoord);
    float3 t_lin = srgb_to_linear(t.rgb);
    float3 c_lin = srgb_to_linear(in.color.rgb);
    return float4(t_lin * c_lin, t.a * in.color.a);
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

// -- SDF rounded rectangle ---------------------------------------------------
// Evaluates the signed distance from a point to a rounded rectangle.
// Negative inside, positive outside, zero on the boundary.
// p: point relative to rect center
// b: half-extents of the rect (before corner rounding)
// r: corner radius

float sd_rounded_rect(float2 p, float2 b, float r) {
    float2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

struct RoundedRectParams {
    float half_w;     // half-width in pixels
    float half_h;     // half-height in pixels
    float radius;     // corner radius in pixels
    float border_w;   // border width in pixels (0 = no border)
    float border_r;   // border color RGBA
    float border_g;
    float border_b;
    float border_a;
};

fragment float4 fragment_rounded_rect(
    VertexOut in [[stage_in]],
    constant RoundedRectParams& params [[buffer(0)]]
) {
    // texCoord carries local position relative to rect center (in pixels).
    float2 local_pos = in.texCoord;
    float2 half_ext = float2(params.half_w, params.half_h);
    float r = min(params.radius, min(params.half_w, params.half_h));

    float dist = sd_rounded_rect(local_pos, half_ext, r);

    // Anti-alias the outer edge: 1px transition zone.
    float fill_alpha = 1.0 - smoothstep(-0.5, 0.5, dist);

    // Linearize sRGB colors for correct compositing under sRGB render target.
    float3 fill_lin = srgb_to_linear(in.color.rgb);

    // Output non-premultiplied color: the hardware blender applies srcAlpha.
    float4 result;
    if (params.border_w > 0.0) {
        // Border: the border region is where dist is between -border_w and 0.
        float inner_dist = dist + params.border_w;
        float border_alpha = 1.0 - smoothstep(-0.5, 0.5, inner_dist);
        float border_mask = fill_alpha - border_alpha; // 1 in border, 0 elsewhere
        float3 border_lin = srgb_to_linear(float3(params.border_r, params.border_g,
                                                    params.border_b));
        // Composite fill and border coverage (non-overlapping regions).
        float fill_w = in.color.a * border_alpha;
        float border_w = params.border_a * border_mask;
        float total_a = fill_w + border_w;
        // Weighted average of linear colors (non-premultiplied output).
        float3 rgb = total_a > 0.001
            ? (fill_lin * fill_w + border_lin * border_w) / total_a
            : fill_lin;
        // Apply outer edge AA.
        result = float4(rgb, min(total_a, fill_alpha));
    } else {
        // No border: just fill with AA edge.
        result = float4(fill_lin, in.color.a * fill_alpha);
    }
    return result;
}

// -- Analytical Gaussian box shadow ----------------------------------------
// Evaluates the exact Gaussian shadow for a rectangle (separable erf integrals)
// or an approximate Gaussian shadow for a rounded rectangle (SDF + erfc).
// This is the closed-form solution to 'render shape, then blur with Gaussian
// kernel' -- no offscreen textures or compute passes needed.

// Abramowitz & Stegun 7.1.26 -- max |error| <= 1.5e-7.
float erf_approx(float x) {
    float ax = abs(x);
    float t = 1.0 / (1.0 + 0.3275911 * ax);
    float poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741
                 + t * (-1.453152027 + t * 1.061405429))));
    float result = 1.0 - poly * exp(-ax * ax);
    return x >= 0.0 ? result : -result;
}

// Exact 1D Gaussian integral of a segment [lo, hi] evaluated at position p.
// Returns the fraction of the Gaussian kernel that overlaps [lo, hi].
float shadow_1d(float p, float lo, float hi, float inv_s2) {
    return 0.5 * (erf_approx((hi - p) * inv_s2) - erf_approx((lo - p) * inv_s2));
}

struct ShadowParams {
    float rect_min_x;   // shadow rect min corner (pixel coords)
    float rect_min_y;
    float rect_max_x;   // shadow rect max corner (pixel coords)
    float rect_max_y;
    float color_r;
    float color_g;
    float color_b;
    float color_a;
    float sigma;         // Gaussian standard deviation (pixels)
    float corner_radius; // rounded corner radius (pixels), 0 = sharp rect
    float _pad0;
    float _pad1;
};

fragment float4 fragment_shadow(
    VertexOut in [[stage_in]],
    constant ShadowParams& params [[buffer(0)]]
) {
    // texCoord carries absolute pixel-space position of this fragment.
    float2 p = in.texCoord;
    float sigma = params.sigma;

    // Linearize shadow color for sRGB render target.
    float3 color_lin = srgb_to_linear(float3(params.color_r, params.color_g,
                                               params.color_b));

    // Zero sigma: hard shadow (inside = full alpha, outside = zero).
    if (sigma <= 0.0) {
        bool inside = p.x >= params.rect_min_x && p.x <= params.rect_max_x
                   && p.y >= params.rect_min_y && p.y <= params.rect_max_y;
        float a = inside ? params.color_a : 0.0;
        return float4(color_lin, a);
    }

    float inv_s2 = 1.0 / (sigma * 1.41421356); // 1 / (sigma * sqrt(2))
    float alpha;

    if (params.corner_radius <= 0.0) {
        // Exact separable Gaussian integral for axis-aligned rectangles.
        // The Gaussian-blurred shadow of a rectangle decomposes into the
        // product of two independent 1D integrals -- one per axis.
        float ix = shadow_1d(p.x, params.rect_min_x, params.rect_max_x, inv_s2);
        float iy = shadow_1d(p.y, params.rect_min_y, params.rect_max_y, inv_s2);
        alpha = ix * iy;
    } else {
        // SDF-based Gaussian falloff for rounded rectangles.
        // erfc(sdf / (sigma * sqrt(2))) / 2 gives an excellent approximation
        // for convex shapes.
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

// -- Fullscreen dither pass ------------------------------------------------
// Reads the float16 resolved framebuffer, applies 4x4 Bayer ordered dither
// in sRGB space (at the quantization boundary), and outputs to the 8-bit
// sRGB drawable. This is the architecturally correct place for dithering:
// all rendering happens in float16 with full precision, and quantization
// noise is added once, uniformly, at the final blit.

fragment float4 fragment_dither(
    VertexOut in [[stage_in]],
    texture2d<float> src [[texture(0)]],
    sampler s [[sampler(0)]]
) {
    float4 linear = src.sample(s, in.texCoord);

    // Convert to sRGB (the domain where 8-bit quantization occurs).
    float3 srgb = linear_to_srgb(linear.rgb);

    // 4x4 Bayer matrix via bit-interleave -- optimal threshold matrix
    // (minimizes max spatial frequency of quantization error).
    int x = int(in.position.x) & 3;
    int y = int(in.position.y) & 3;
    int x0 = x & 1, x1 = (x >> 1) & 1;
    int y0 = y & 1, y1 = (y >> 1) & 1;
    int b = ((x0 ^ y0) << 3) | (y0 << 2) | ((x1 ^ y1) << 1) | y1;
    float threshold = (float(b) + 0.5) / 16.0 - 0.5; // [-0.5, +0.5)

    // +/-0.5 LSB in sRGB space -- standard ordered dither amplitude.
    srgb += threshold / 255.0;

    // Convert back to linear for the sRGB render target (hardware re-encodes).
    return float4(srgb_to_linear(srgb), linear.a);
}

// srgb_to_linear / linear_to_srgb are defined above (before fragment shaders)
// so they are available to both fragment and compute shaders.

// -- Color space conversion compute kernels ------------------------------
// Used at the blur boundary: sRGB drawable <-> linear RGBA16F blur textures.

struct CopyParams {
    int src_x;
    int src_y;
    int dst_x;
    int dst_y;
    int width;
    int height;
};

kernel void copy_srgb_to_linear(
    texture2d<float, access::read> src [[texture(0)]],
    texture2d<float, access::write> dst [[texture(1)]],
    constant CopyParams& p [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (int(gid.x) >= p.width || int(gid.y) >= p.height) return;
    float4 srgb = src.read(uint2(p.src_x + int(gid.x), p.src_y + int(gid.y)));
    dst.write(float4(srgb_to_linear(srgb.rgb), srgb.a),
              uint2(p.dst_x + int(gid.x), p.dst_y + int(gid.y)));
}

kernel void copy_linear_to_srgb(
    texture2d<float, access::read> src [[texture(0)]],
    texture2d<float, access::write> dst [[texture(1)]],
    constant CopyParams& p [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (int(gid.x) >= p.width || int(gid.y) >= p.height) return;
    float4 lin = src.read(uint2(p.src_x + int(gid.x), p.src_y + int(gid.y)));
    dst.write(float4(linear_to_srgb(lin.rgb), lin.a),
              uint2(p.dst_x + int(gid.x), p.dst_y + int(gid.y)));
}

// -- Box blur compute kernels (shared memory, linear RGBA16F) -----------
// Separable H+V blur with threadgroup shared memory. Each threadgroup
// cooperatively loads a tile of pixels plus halo (blur radius on each side)
// into fast on-chip memory, then each thread accumulates from shared memory
// instead of re-fetching from the texture. This eliminates redundant reads
// where neighboring pixels share most of the same kernel window.
//
// H-blur uses threadgroup (256, 1, 1); V-blur uses (1, 256, 1).
// Non-uniform threadgroups at grid edges are handled via threads_per_threadgroup.

struct BlurParams {
    int half_width;   // box half-width for this pass (kernel = 2*half+1)
    int region_w;     // valid data width in texture
    int region_h;     // valid data height in texture
    int _pad;
};

constant int BLUR_TG = 256;
constant int BLUR_MAX_HALF = 128;
constant int BLUR_TILE = BLUR_TG + 2 * BLUR_MAX_HALF;  // 512

kernel void blur_h(
    texture2d<float, access::read> src [[texture(0)]],
    texture2d<float, access::write> dst [[texture(1)]],
    constant BlurParams& params [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]],
    uint lid [[thread_index_in_threadgroup]],
    uint2 tg_dims [[threads_per_threadgroup]]
) {
    threadgroup float4 tile[BLUR_TILE];

    int row = int(gid.y);
    if (row >= params.region_h) return;

    int r = params.half_width;
    int tg_w = int(tg_dims.x);
    int tg_start = int(gid.x) - int(lid);
    int load_start = tg_start - r;
    int load_count = min(tg_w + 2 * r, BLUR_TILE);

    for (int i = int(lid); i < load_count; i += tg_w) {
        int x = load_start + i;
        tile[i] = (x >= 0 && x < params.region_w)
            ? src.read(uint2(x, row)) : float4(0);
    }

    threadgroup_barrier(mem_flags::mem_threadgroup);

    int out_x = int(gid.x);
    if (out_x >= params.region_w) return;

    float4 sum = float4(0);
    int count = 0;
    int center = int(lid) + r;
    for (int dx = -r; dx <= r; dx++) {
        int sx = out_x + dx;
        if (sx >= 0 && sx < params.region_w) {
            sum += tile[center + dx];
            count++;
        }
    }

    dst.write(sum / float(max(count, 1)), gid);
}

kernel void blur_v(
    texture2d<float, access::read> src [[texture(0)]],
    texture2d<float, access::write> dst [[texture(1)]],
    constant BlurParams& params [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]],
    uint lid [[thread_index_in_threadgroup]],
    uint2 tg_dims [[threads_per_threadgroup]]
) {
    threadgroup float4 tile[BLUR_TILE];

    int col = int(gid.x);
    if (col >= params.region_w) return;

    int r = params.half_width;
    int tg_h = int(tg_dims.y);
    int tg_start = int(gid.y) - int(lid);
    int load_start = tg_start - r;
    int load_count = min(tg_h + 2 * r, BLUR_TILE);

    for (int i = int(lid); i < load_count; i += tg_h) {
        int y = load_start + i;
        tile[i] = (y >= 0 && y < params.region_h)
            ? src.read(uint2(col, y)) : float4(0);
    }

    threadgroup_barrier(mem_flags::mem_threadgroup);

    int out_y = int(gid.y);
    if (out_y >= params.region_h) return;

    float4 sum = float4(0);
    int count = 0;
    int center = int(lid) + r;
    for (int dy = -r; dy <= r; dy++) {
        int sy = out_y + dy;
        if (sy >= 0 && sy < params.region_h) {
            sum += tile[center + dy];
            count++;
        }
    }

    dst.write(sum / float(max(count, 1)), gid);
}
";

// ── DMA buffer helper ───────────────────────────────────────────────────

struct DmaBuf {
    va: usize,
    pa: u64,
    order: u32,
}

impl DmaBuf {
    fn alloc(order: u32) -> Self {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
            sys::print(b"metal-render: dma_alloc failed\n");
            sys::exit();
        });
        let bytes = (1usize << order) * ipc::PAGE_SIZE;
        // SAFETY: va points to freshly allocated DMA memory of `bytes` size.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, bytes) };
        Self { va, pa, order }
    }

    fn size(&self) -> usize {
        (1usize << self.order) * ipc::PAGE_SIZE
    }
}

// ── Virtio helpers ──────────────────────────────────────────────────────

fn submit_setup(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    len: usize,
) {
    vq.push_chain(&[(dma.pa, len as u32, false)]);
    device.notify(VIRTQ_SETUP);
    let _ = sys::wait(&[irq_handle.0], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    let _ = sys::interrupt_ack(irq_handle);
}

fn submit_render(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    len: usize,
) {
    vq.push_chain(&[(dma.pa, len as u32, false)]);
    device.notify(VIRTQ_RENDER);
    let _ = sys::wait(&[irq_handle.0], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    let _ = sys::interrupt_ack(irq_handle);
}

/// Copy a CommandBuffer's bytes into DMA memory and submit.
fn send_setup(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    cmdbuf: &metal::CommandBuffer,
) {
    let data = cmdbuf.as_bytes();
    assert!(data.len() <= dma.size());
    // SAFETY: dma.va is valid DMA memory of dma.size() bytes.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dma.va as *mut u8, data.len()) };
    submit_setup(device, vq, irq_handle, dma, data.len());
}

fn send_render(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    cmdbuf: &metal::CommandBuffer,
) {
    let data = cmdbuf.as_bytes();
    assert!(data.len() <= dma.size());
    // SAFETY: dma.va is valid DMA memory of dma.size() bytes.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dma.va as *mut u8, data.len()) };
    submit_render(device, vq, irq_handle, dma, data.len());
}

// ── IPC helpers ─────────────────────────────────────────────────────────

fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

fn print_u32(n: u32) {
    sys::print_u32(n);
}

fn print_hex_u32(val: u32) {
    let mut buf = [0u8; 10];
    let prefix = b"0x";
    buf[..2].copy_from_slice(prefix);
    for i in 0..8 {
        let nibble = ((val >> (28 - i * 4)) & 0xF) as u8;
        buf[2 + i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
    }
    sys::print(&buf);
}

// ── Glyph atlas ─────────────────────────────────────────────────────────

/// Maximum glyph ID per font in the atlas lookup table.
const GLYPH_STRIDE: usize = 2048;
/// Number of font slots in the atlas (0 = mono, 1 = sans).
const MAX_FONTS: usize = 2;
/// Total atlas entry capacity (GLYPH_STRIDE * MAX_FONTS).
const MAX_GLYPH_ENTRIES: usize = GLYPH_STRIDE * MAX_FONTS;

/// Atlas entry for a single rasterized glyph.
#[derive(Clone, Copy)]
struct AtlasEntry {
    u: u16,
    v: u16,
    width: u16,
    height: u16,
    bearing_x: i16,
    bearing_y: i16,
}

/// Glyph texture atlas with row-based packing.
/// Supports multiple fonts via `font_id` offset into the entries array:
/// font 0 (mono) uses entries[0..GLYPH_STRIDE), font 1 (sans) uses
/// entries[GLYPH_STRIDE..2*GLYPH_STRIDE), etc.
struct GlyphAtlas {
    entries: [AtlasEntry; MAX_GLYPH_ENTRIES],
    pixels: [u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
    row_y: u16,
    row_x: u16,
    row_h: u16,
}

impl GlyphAtlas {
    /// Flat index for a (glyph_id, font_id) pair.
    fn effective_id(glyph_id: u16, font_id: u16) -> usize {
        font_id as usize * GLYPH_STRIDE + glyph_id as usize
    }

    fn lookup(&self, glyph_id: u16, font_id: u16) -> Option<&AtlasEntry> {
        let id = Self::effective_id(glyph_id, font_id);
        if id < MAX_GLYPH_ENTRIES && self.entries[id].width > 0 {
            Some(&self.entries[id])
        } else {
            None
        }
    }

    fn pack(
        &mut self,
        glyph_id: u16,
        font_id: u16,
        w: u16,
        h: u16,
        bearing_x: i16,
        bearing_y: i16,
        data: &[u8],
    ) -> bool {
        let id = Self::effective_id(glyph_id, font_id);
        if id >= MAX_GLYPH_ENTRIES {
            return false;
        }
        // Check if we need a new row.
        if self.row_x + w > ATLAS_WIDTH as u16 {
            self.row_y += self.row_h;
            self.row_x = 0;
            self.row_h = 0;
        }
        if self.row_y + h > ATLAS_HEIGHT as u16 {
            return false; // Atlas full.
        }

        let u = self.row_x;
        let v = self.row_y;

        // Copy glyph bitmap into atlas pixel buffer.
        for row in 0..h as usize {
            let src_start = row * w as usize;
            let dst_start = (v as usize + row) * ATLAS_WIDTH as usize + u as usize;
            let src_end = src_start + w as usize;
            let dst_end = dst_start + w as usize;
            if src_end <= data.len() && dst_end <= self.pixels.len() {
                self.pixels[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
            }
        }

        self.entries[id] = AtlasEntry {
            u,
            v,
            width: w,
            height: h,
            bearing_x,
            bearing_y,
        };

        self.row_x += w;
        if h > self.row_h {
            self.row_h = h;
        }
        true
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x94\xB1 metal-render - starting\n");

    // ── Phase A: Receive device config from init, init virtio device ─────
    // SAFETY: Channel 0 shared memory is mapped by kernel before process start.
    let ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);
    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"metal-render: no device config message\n");
        sys::exit();
    }
    let dev_config = if let Some(protocol::device::Message::DeviceConfig(c)) =
        protocol::device::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"metal-render: bad device config\n");
        sys::exit();
    };

    // Map MMIO region.
    let page_offset = dev_config.mmio_pa & (ipc::PAGE_SIZE as u64 - 1);
    let page_pa = dev_config.mmio_pa & !(ipc::PAGE_SIZE as u64 - 1);
    let page_va = sys::device_map(page_pa, ipc::PAGE_SIZE as u64).unwrap_or_else(|_| {
        sys::print(b"metal-render: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    // Feature negotiation — accept VIRTIO_F_VERSION_1 only.
    device.reset();
    device.set_status(1); // ACKNOWLEDGE
    device.set_status(1 | 2); // ACKNOWLEDGE | DRIVER
    let _dev_features = device.read_device_features();
    device.write_driver_features(1u64 << 32); // VIRTIO_F_VERSION_1
    device.set_status(1 | 2 | 8); // FEATURES_OK
    if device.read_status() & 8 == 0 {
        sys::print(b"metal-render: FEATURES_OK not set\n");
        sys::exit();
    }

    // Register IRQ.
    let irq_handle: sys::InterruptHandle =
        sys::interrupt_register(dev_config.irq).unwrap_or_else(|_| {
            sys::print(b"metal-render: interrupt_register failed\n");
            sys::exit();
        });

    // Setup two virtqueues.
    let setup_vq_size = core::cmp::min(
        device.queue_max_size(VIRTQ_SETUP),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let render_vq_size = core::cmp::min(
        device.queue_max_size(VIRTQ_RENDER),
        virtio::DEFAULT_QUEUE_SIZE,
    );

    let mut setup_vq = alloc_virtqueue(&device, VIRTQ_SETUP, setup_vq_size);
    let mut render_vq = alloc_virtqueue(&device, VIRTQ_RENDER, render_vq_size);

    device.driver_ok();
    sys::print(b"  \xF0\x9F\x94\xB1 metal-render: virtio device ready (2 queues)\n");

    // ── Phase B: Display query + init handshake ──────────────────────────
    // Read display dimensions from virtio config space.
    let disp_w = device.config_read32(0x00);
    let disp_h = device.config_read32(0x04);
    let disp_refresh = device.config_read32(0x08);
    let width = if disp_w > 0 { disp_w } else { 1024 };
    let height = if disp_h > 0 { disp_h } else { 768 };
    let refresh_rate = disp_refresh;

    sys::print(b"     display ");
    print_u32(width);
    sys::print(b"x");
    print_u32(height);
    sys::print(b"@");
    print_u32(refresh_rate);
    sys::print(b"Hz\n");

    // Send display info back to init.
    let info_msg = unsafe {
        ipc::Message::from_payload(
            MSG_DISPLAY_INFO,
            &DisplayInfoMsg {
                width,
                height,
                refresh_rate,
            },
        )
    };
    ch.send(&info_msg);
    let _ = sys::channel_signal(sys::ChannelHandle(INIT_HANDLE));

    // Wait for GPU config from init.
    sys::print(b"     waiting for gpu config\n");
    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_GPU_CONFIG {
            break;
        }
    }
    // We use display dimensions from config space; decode to consume the message type safely.
    let _ = protocol::gpu::decode(msg.msg_type, &msg.payload);

    // Signal init that we're ready.
    sys::print(b"     handshake complete, sending GPU_READY\n");
    let ready_msg = ipc::Message::new(MSG_GPU_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(sys::ChannelHandle(INIT_HANDLE));

    // ── Phase C: Receive render config ───────────────────────────────────
    sys::print(b"     waiting for render config\n");
    let mut scene_va: u64 = 0;
    let mut content_va: u64 = 0;
    let mut content_size: u32 = 0;
    let mut scale_factor: f32 = 1.0;
    let mut pointer_state_va: u64 = 0;
    let mut font_size_cfg: u16 = 18;
    let mut frame_rate_cfg: u32 = 60;

    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_COMPOSITOR_CONFIG {
            if let Some(protocol::compose::Message::CompositorConfig(config)) =
                protocol::compose::decode(msg.msg_type, &msg.payload)
            {
                scene_va = config.scene_va;
                content_va = config.content_va;
                content_size = config.content_size;
                scale_factor = config.scale_factor;
                pointer_state_va = config.pointer_state_va;
                font_size_cfg = config.font_size;
                frame_rate_cfg = if config.frame_rate > 0 {
                    config.frame_rate as u32
                } else {
                    60
                };
                break;
            }
        }
    }

    sys::print(b"     render config: scene_va=");
    print_hex_u32((scene_va >> 32) as u32);
    print_hex_u32(scene_va as u32);
    sys::print(b" content_size=");
    print_u32(content_size);
    sys::print(b"\n");

    if scene_va == 0 {
        sys::print(b"metal-render: no scene_va, idling\n");
        loop {
            let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        }
    }

    // ── Phase D: Metal pipeline setup ────────────────────────────────────
    // Allocate DMA buffers for command submission.
    // Setup buffer: 2 MiB (order 9) — enough for shader source + atlas upload + image textures.
    // Increased from 512 KiB to handle Content Region images (e.g., 800×537 BGRA = 1.6 MiB).
    let setup_dma = DmaBuf::alloc(9);
    // Render buffer: 64 KiB (order 4) — per-frame command buffer.
    let render_dma = DmaBuf::alloc(4);

    let mut cmdbuf = metal::CommandBuffer::new();

    // Compile shader library.
    cmdbuf.clear();
    cmdbuf.compile_library(LIB_SHADERS, MSL_SOURCE);
    send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);
    sys::print(b"     shaders compiled\n");

    // Get shader functions.
    cmdbuf.clear();
    cmdbuf.get_function(FN_VERTEX_MAIN, LIB_SHADERS, b"vertex_main");
    cmdbuf.get_function(FN_FRAGMENT_SOLID, LIB_SHADERS, b"fragment_solid");
    cmdbuf.get_function(FN_FRAGMENT_GLYPH, LIB_SHADERS, b"fragment_glyph");
    cmdbuf.get_function(FN_FRAGMENT_TEXTURED, LIB_SHADERS, b"fragment_textured");
    cmdbuf.get_function(FN_VERTEX_STENCIL, LIB_SHADERS, b"vertex_stencil");
    cmdbuf.get_function(FN_BLUR_H, LIB_SHADERS, b"blur_h");
    cmdbuf.get_function(FN_BLUR_V, LIB_SHADERS, b"blur_v");
    cmdbuf.get_function(FN_COPY_SRGB_TO_LINEAR, LIB_SHADERS, b"copy_srgb_to_linear");
    cmdbuf.get_function(FN_COPY_LINEAR_TO_SRGB, LIB_SHADERS, b"copy_linear_to_srgb");
    cmdbuf.get_function(
        FN_FRAGMENT_ROUNDED_RECT,
        LIB_SHADERS,
        b"fragment_rounded_rect",
    );
    cmdbuf.get_function(FN_FRAGMENT_SHADOW, LIB_SHADERS, b"fragment_shadow");
    cmdbuf.get_function(FN_FRAGMENT_DITHER, LIB_SHADERS, b"fragment_dither");
    send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);
    sys::print(b"     functions loaded\n");

    // Create render pipelines.
    cmdbuf.clear();
    // MSAA pipelines render to RGBA16F for full precision. The dither pass
    // blits from float16 to the 8-bit sRGB drawable with ordered dithering.
    let f16 = metal::PIXEL_FORMAT_RGBA16F;
    let srgb8 = metal::PIXEL_FORMAT_BGRA8_SRGB;

    // Solid fill pipeline (with blending, MSAA, stencil-compatible).
    cmdbuf.create_render_pipeline(
        PIPE_SOLID,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_SOLID,
        true,
        0x0F,
        true,
        SAMPLE_COUNT,
        f16,
    );
    // Textured pipeline (with blending, MSAA, stencil-compatible).
    cmdbuf.create_render_pipeline(
        PIPE_TEXTURED,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_TEXTURED,
        true,
        0x0F,
        true,
        SAMPLE_COUNT,
        f16,
    );
    // Glyph pipeline (with blending, MSAA, stencil-compatible).
    cmdbuf.create_render_pipeline(
        PIPE_GLYPH,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_GLYPH,
        true,
        0x0F,
        true,
        SAMPLE_COUNT,
        f16,
    );
    // Stencil write pipeline (no color output, has stencil, MSAA).
    cmdbuf.create_render_pipeline(
        PIPE_STENCIL_WRITE,
        FN_VERTEX_STENCIL,
        FN_FRAGMENT_SOLID,
        false,
        0x00,
        true,
        SAMPLE_COUNT,
        f16,
    );
    // Non-MSAA solid pipeline (for blur overlay on drawable).
    cmdbuf.create_render_pipeline(
        PIPE_SOLID_NO_MSAA,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_SOLID,
        true,
        0x0F,
        false,
        1,
        srgb8,
    );
    // Rounded rect pipeline (SDF fragment shader, with blending, MSAA, stencil).
    cmdbuf.create_render_pipeline(
        PIPE_ROUNDED_RECT,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_ROUNDED_RECT,
        true,
        0x0F,
        true,
        SAMPLE_COUNT,
        f16,
    );
    // Analytical shadow pipeline (Gaussian erf, with blending, MSAA, stencil).
    cmdbuf.create_render_pipeline(
        PIPE_SHADOW,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_SHADOW,
        true,
        0x0F,
        true,
        SAMPLE_COUNT,
        f16,
    );
    // Dither pass: reads float16 resolved texture, applies 4x4 Bayer dither
    // in sRGB space, outputs to 8-bit sRGB drawable. No blending needed.
    cmdbuf.create_render_pipeline(
        PIPE_DITHER,
        FN_VERTEX_MAIN,
        FN_FRAGMENT_DITHER,
        false,
        0x0F,
        false,
        1,
        srgb8,
    );
    // Compute pipelines for blur + color space conversion.
    cmdbuf.create_compute_pipeline(CPIPE_BLUR_H, FN_BLUR_H);
    cmdbuf.create_compute_pipeline(CPIPE_BLUR_V, FN_BLUR_V);
    cmdbuf.create_compute_pipeline(CPIPE_SRGB_TO_LINEAR, FN_COPY_SRGB_TO_LINEAR);
    cmdbuf.create_compute_pipeline(CPIPE_LINEAR_TO_SRGB, FN_COPY_LINEAR_TO_SRGB);
    send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);
    sys::print(b"     pipelines created\n");

    // Create depth/stencil states.
    cmdbuf.clear();
    cmdbuf.create_depth_stencil_state(
        DSS_NONE,
        false,
        metal::CMP_ALWAYS,
        metal::STENCIL_KEEP,
        metal::STENCIL_KEEP,
    );
    cmdbuf.create_depth_stencil_state(
        DSS_STENCIL_WRITE,
        true,
        metal::CMP_ALWAYS,
        metal::STENCIL_REPLACE,
        metal::STENCIL_KEEP,
    );
    cmdbuf.create_depth_stencil_state(
        DSS_STENCIL_TEST,
        true,
        metal::CMP_NOT_EQUAL,
        metal::STENCIL_ZERO,
        metal::STENCIL_KEEP,
    );
    // Clip test: same as stencil test but KEEPS stencil on pass.
    cmdbuf.create_depth_stencil_state(
        DSS_CLIP_TEST,
        true,
        metal::CMP_NOT_EQUAL,
        metal::STENCIL_KEEP, // keep stencil — multiple children share the mask
        metal::STENCIL_KEEP,
    );
    // Even-odd fill: INVERT stencil on every triangle, then odd stencil = inside.
    cmdbuf.create_depth_stencil_state(
        DSS_STENCIL_INVERT,
        true,
        metal::CMP_ALWAYS,
        metal::STENCIL_INVERT,
        metal::STENCIL_KEEP,
    );
    // Two-sided non-zero winding fill: front INCR_WRAP, back DECR_WRAP.
    // Correct for any polygon (convex or concave) — the standard GPU path fill algorithm.
    cmdbuf.create_depth_stencil_state_two_sided(
        DSS_STENCIL_WINDING,
        metal::CMP_ALWAYS,
        metal::STENCIL_INCR_WRAP,
        metal::STENCIL_KEEP,
        metal::STENCIL_DECR_WRAP,
        metal::STENCIL_KEEP,
    );
    send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);

    // Create samplers.
    cmdbuf.clear();
    cmdbuf.create_sampler(
        SAMPLER_NEAREST,
        metal::FILTER_NEAREST,
        metal::FILTER_NEAREST,
    );
    cmdbuf.create_sampler(SAMPLER_LINEAR, metal::FILTER_LINEAR, metal::FILTER_LINEAR);
    send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);

    // Create textures.
    cmdbuf.clear();
    // MSAA render target (RGBA16F: full precision, dither pass handles quantization).
    cmdbuf.create_texture(
        TEX_MSAA,
        width as u16,
        height as u16,
        metal::PIXEL_FORMAT_RGBA16F,
        SAMPLE_COUNT,
        metal::USAGE_RENDER_TARGET | metal::USAGE_SHADER_READ,
    );
    // Float16 resolve target — MSAA resolves here, dither pass reads it.
    cmdbuf.create_texture(
        TEX_RESOLVE,
        width as u16,
        height as u16,
        metal::PIXEL_FORMAT_RGBA16F,
        1,
        metal::USAGE_RENDER_TARGET | metal::USAGE_SHADER_READ,
    );
    // Stencil texture (for clip paths).
    cmdbuf.create_texture(
        TEX_STENCIL,
        width as u16,
        height as u16,
        metal::PIXEL_FORMAT_STENCIL8,
        SAMPLE_COUNT,
        metal::USAGE_RENDER_TARGET,
    );
    // Glyph atlas (R8, 512x512).
    cmdbuf.create_texture(
        TEX_ATLAS,
        ATLAS_WIDTH as u16,
        ATLAS_HEIGHT as u16,
        metal::PIXEL_FORMAT_R8,
        1,
        metal::USAGE_SHADER_READ,
    );
    // Image atlas texture (BGRA8, 1024×1024). All Content::InlineImage nodes in a
    // frame are packed into non-overlapping sub-rectangles via ImageAtlas.
    // Each image uploads to its own region and draws with matching UVs, so
    // no image overwrites another even though draws are deferred.
    // Non-sRGB format: the fragment_textured shader manually linearizes via
    // srgb_to_linear(). Using BGRA8_SRGB here would cause double gamma decode.
    cmdbuf.create_texture(
        TEX_IMAGE,
        IMG_TEX_DIM as u16,
        IMG_TEX_DIM as u16,
        metal::PIXEL_FORMAT_BGRA8,
        1,
        metal::USAGE_SHADER_READ,
    );
    // Blur ping-pong textures (full framebuffer size, RGBA16F for linear-light
    // precision — 8-bit would lose dark detail after sRGB→linear conversion).
    cmdbuf.create_texture(
        TEX_BLUR_A,
        width as u16,
        height as u16,
        metal::PIXEL_FORMAT_RGBA16F,
        1,
        metal::USAGE_SHADER_READ | metal::USAGE_SHADER_WRITE,
    );
    cmdbuf.create_texture(
        TEX_BLUR_B,
        width as u16,
        height as u16,
        metal::PIXEL_FORMAT_RGBA16F,
        1,
        metal::USAGE_SHADER_READ | metal::USAGE_SHADER_WRITE,
    );
    send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);
    sys::print(b"     textures created\n");

    // ── Glyph atlas initialization ──────────────────────────────────────
    // Heap-allocate atlas (~280 KiB: 2048 entries + 256 KiB pixel buffer).
    let atlas_layout = alloc::alloc::Layout::from_size_align(
        core::mem::size_of::<GlyphAtlas>(),
        core::mem::align_of::<GlyphAtlas>(),
    )
    .unwrap();
    let atlas_ptr = unsafe { alloc::alloc::alloc_zeroed(atlas_layout) as *mut GlyphAtlas };
    let glyph_atlas = unsafe { &mut *atlas_ptr };
    let mut font_ascent: u32 = 14;

    // Parse Content Region header to find font data.
    // SAFETY: content_va..+content_size is mapped read-only by init before starting us.
    let content_slice: &[u8] = if content_va != 0 && content_size > 0 {
        unsafe { core::slice::from_raw_parts(content_va as *const u8, content_size as usize) }
    } else {
        &[]
    };
    let content_header: Option<&protocol::content::ContentRegionHeader> =
        if content_slice.len() >= core::mem::size_of::<protocol::content::ContentRegionHeader>() {
            // SAFETY: content_va is page-aligned; ContentRegionHeader is repr(C).
            Some(unsafe { &*(content_va as *const protocol::content::ContentRegionHeader) })
        } else {
            None
        };
    let font_slice: &[u8] = if let Some(h) = content_header {
        if let Some(entry) =
            protocol::content::find_entry(h, protocol::content::CONTENT_ID_FONT_MONO)
        {
            let start = entry.offset as usize;
            let end = start + entry.length as usize;
            if end <= content_size as usize {
                // SAFETY: entry bounds validated within content_size; content_va is init-mapped.
                unsafe {
                    core::slice::from_raw_parts(
                        (content_va as usize + start) as *const u8,
                        entry.length as usize,
                    )
                }
            } else {
                &[]
            }
        } else {
            &[]
        }
    } else {
        &[]
    };
    let sans_font_slice: &[u8] = if let Some(h) = content_header {
        if let Some(entry) =
            protocol::content::find_entry(h, protocol::content::CONTENT_ID_FONT_SANS)
        {
            let start = entry.offset as usize;
            let end = start + entry.length as usize;
            if end <= content_size as usize {
                // SAFETY: entry bounds validated within content_size; content_va is init-mapped.
                unsafe {
                    core::slice::from_raw_parts(
                        (content_va as usize + start) as *const u8,
                        entry.length as usize,
                    )
                }
            } else {
                &[]
            }
        } else {
            &[]
        }
    } else {
        &[]
    };
    // Array of font slices indexed by font_id (0 = mono, 1 = sans).
    let font_slices: [&[u8]; MAX_FONTS] = [font_slice, sans_font_slice];

    // Rasterization scratch space — kept alive for on-demand rasterization.
    let scratch_layout_persistent = alloc::alloc::Layout::from_size_align(
        core::mem::size_of::<fonts::rasterize::RasterScratch>(),
        core::mem::align_of::<fonts::rasterize::RasterScratch>(),
    )
    .unwrap();
    let scratch_persistent_ptr = unsafe {
        alloc::alloc::alloc_zeroed(scratch_layout_persistent)
            as *mut fonts::rasterize::RasterScratch
    };
    let raster_scratch = unsafe { &mut *scratch_persistent_ptr };
    let font_size_pt: u32 = font_size_cfg as u32;
    // Rasterize glyphs at device pixel resolution (e.g. 2x for Retina)
    // for crisp rendering. Glyph metrics are stored in pixel space; the
    // renderer divides by scale_factor when positioning quads.
    let font_size_px: u32 = {
        let px = font_size_pt as f32 * scale_factor;
        if px >= 0.0 {
            (px + 0.5) as u32
        } else {
            1
        }
    };
    let scale_factor_int: u16 = (scale_factor as u16).max(1);

    if !font_slice.is_empty() {
        sys::print(b"     initializing glyph atlas\n");

        let font_data = font_slice;

        if let Some(metrics) = fonts::rasterize::font_metrics(font_data) {
            let upem = metrics.units_per_em as i32;
            let asc = metrics.ascent as i32;
            // font_ascent stays in point space (not pixels).
            let size = font_size_pt as i32;
            font_ascent = ((asc * size + upem - 1) / upem) as u32;
        }

        let ascii = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
        // Shape the full ASCII string to pre-populate the atlas. Contextual
        // features (calt) produce ligature glyph IDs for sequences like <=, =>.
        // Individual characters not covered here are handled by the LRU fallback.
        let shaped = fonts::shape(font_data, ascii, &[]);

        // 2x rasterization produces larger glyphs — increase buffer accordingly.
        let mut raster_buf = [0u8; 100 * 100];
        let mut packed = 0u32;
        let mut atlas_full_warned = false;

        for sg in &shaped {
            if glyph_atlas.lookup(sg.glyph_id, 0).is_some() {
                continue;
            }
            let mut rb = fonts::rasterize::RasterBuffer {
                data: &mut raster_buf,
                width: 100,
                height: 100,
            };
            if let Some(m) = fonts::rasterize::rasterize_with_axes(
                font_data,
                sg.glyph_id,
                font_size_px as u16,
                &mut rb,
                raster_scratch,
                &[],
                scale_factor_int,
            ) {
                if glyph_atlas.pack(
                    sg.glyph_id,
                    0,
                    m.width as u16,
                    m.height as u16,
                    m.bearing_x as i16,
                    m.bearing_y as i16,
                    &raster_buf[..m.width as usize * m.height as usize],
                ) {
                    packed += 1;
                } else if !atlas_full_warned {
                    sys::print(b"WARNING: glyph atlas full - remaining glyphs will not render\n");
                    atlas_full_warned = true;
                }
            }
        }

        // Pre-populate sans font (Inter) ASCII glyphs (font_id = 1).
        if !sans_font_slice.is_empty() {
            let sans_shaped = fonts::shape(sans_font_slice, ascii, &[]);
            for sg in &sans_shaped {
                if glyph_atlas.lookup(sg.glyph_id, 1).is_some() {
                    continue;
                }
                let mut rb = fonts::rasterize::RasterBuffer {
                    data: &mut raster_buf,
                    width: 100,
                    height: 100,
                };
                if let Some(m) = fonts::rasterize::rasterize_with_axes(
                    sans_font_slice,
                    sg.glyph_id,
                    font_size_px as u16,
                    &mut rb,
                    raster_scratch,
                    &[],
                    scale_factor_int,
                ) {
                    if glyph_atlas.pack(
                        sg.glyph_id,
                        1,
                        m.width as u16,
                        m.height as u16,
                        m.bearing_x as i16,
                        m.bearing_y as i16,
                        &raster_buf[..m.width as usize * m.height as usize],
                    ) {
                        packed += 1;
                    } else if !atlas_full_warned {
                        sys::print(
                            b"WARNING: glyph atlas full - remaining glyphs will not render\n",
                        );
                        atlas_full_warned = true;
                    }
                }
            }
            sys::print(b"     sans font pre-populated\n");
        }

        sys::print(b"     atlas: packed ");
        print_u32(packed);
        sys::print(b" glyphs\n");

        // Upload atlas to GPU texture (in row chunks to fit DMA buffer).
        let atlas_row_bytes = ATLAS_WIDTH as usize;
        let chunk_rows = setup_dma.size() / (atlas_row_bytes + 64); // Leave room for header
        let chunk_rows = core::cmp::min(chunk_rows, ATLAS_HEIGHT as usize);

        let mut y_offset: u16 = 0;
        while (y_offset as u32) < ATLAS_HEIGHT {
            let rows_left = ATLAS_HEIGHT as u16 - y_offset;
            let rows = core::cmp::min(rows_left, chunk_rows as u16);
            let src_start = y_offset as usize * ATLAS_WIDTH as usize;
            let src_end = src_start + rows as usize * ATLAS_WIDTH as usize;

            cmdbuf.clear();
            cmdbuf.upload_texture(
                TEX_ATLAS,
                0,
                y_offset,
                ATLAS_WIDTH as u16,
                rows,
                ATLAS_WIDTH,
                &glyph_atlas.pixels[src_start..src_end],
            );
            send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);
            y_offset += rows;
        }
        sys::print(b"     atlas uploaded to GPU\n");
    }

    // ── Phase E: Render loop ─────────────────────────────────────────────
    sys::print(b"     entering render loop\n");

    let scene_total_size = scene::TRIPLE_SCENE_SIZE;
    let mut last_gen: u32 = 0;
    let period_ns = frame_period_ns(frame_rate_cfg);

    // Pre-allocate vertex buffers outside the loop.
    let mut vertex_buf: Vec<u8> = Vec::with_capacity(MAX_INLINE_BYTES);
    let mut glyph_vertex_buf: Vec<u8> = Vec::with_capacity(MAX_INLINE_BYTES);

    // Heap-allocate shared path buffer to avoid 4 KiB stack arrays inside
    // recursive walk_scene (stack overflow at 16 KiB, tight at 64 KiB).
    let path_buf_layout = alloc::alloc::Layout::from_size_align(
        core::mem::size_of::<PathPointsBuf>(),
        core::mem::align_of::<PathPointsBuf>(),
    )
    .unwrap();
    let path_buf_ptr = unsafe { alloc::alloc::alloc_zeroed(path_buf_layout) as *mut PathPointsBuf };
    let path_buf = unsafe { &mut *path_buf_ptr };

    // Cursor plane state.
    let mut cursor_image_hash: u32 = 0;
    let mut cursor_visible: bool = false;
    let mut last_pointer_xy: u64 = 0;
    let mut cursor_x: f32 = 0.0;
    let mut cursor_y: f32 = 0.0;

    loop {
        // Wait for scene update signal from core, with frame-rate cadence.
        let _ = sys::wait(&[SCENE_HANDLE, INIT_HANDLE], period_ns);
        let _ = sys::interrupt_ack(sys::InterruptHandle(SCENE_HANDLE));

        // Read cursor position from the pointer state register (independent
        // of the scene graph). This lets us send cursor plane updates even
        // when the scene hasn't changed — no full render needed for mouse moves.
        let cursor_moved = if pointer_state_va != 0 {
            // SAFETY: pointer_state_va is a shared page mapped by init (read-only).
            let packed = unsafe {
                let atom = &*(pointer_state_va as *const core::sync::atomic::AtomicU64);
                atom.load(core::sync::atomic::Ordering::Acquire)
            };
            if packed != last_pointer_xy && packed != 0 {
                last_pointer_xy = packed;
                let raw_x = protocol::input::PointerState::unpack_x(packed);
                let raw_y = protocol::input::PointerState::unpack_y(packed);
                cursor_x = scale_pointer_coord(raw_x, width) as f32;
                cursor_y = scale_pointer_coord(raw_y, height) as f32;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Read scene graph.
        let reader = unsafe { scene::TripleReader::new(scene_va as *mut u8, scene_total_size) };
        let generation = reader.front_generation();
        let scene_changed = generation != last_gen;

        if !scene_changed && !cursor_moved {
            drop(reader);
            continue; // Nothing changed.
        }

        if !scene_changed && cursor_moved {
            // Cursor-only frame: send position update, no scene walk.
            // The hypervisor blits the retained frame and composites cursor.
            drop(reader);
            cmdbuf.clear();
            cmdbuf.set_cursor_position(cursor_x, cursor_y);
            cmdbuf.set_cursor_visible(cursor_visible);
            cmdbuf.present_and_commit();
            send_render(&device, &mut render_vq, irq_handle, &render_dma, &cmdbuf);
            continue;
        }

        last_gen = generation;

        let nodes = reader.front_nodes();
        let data_buf = reader.front_data_buf();
        let root = reader.front_root();

        if root == NULL || nodes.is_empty() {
            drop(reader);
            continue;
        }

        // ── Pre-scan: rasterize any missing glyphs into the atlas ─────
        // Walk all visible Glyphs nodes and check for atlas misses. If any
        // new glyphs are rasterized, upload the dirty atlas region to the GPU.
        if !font_slice.is_empty() {
            let mut raster_buf = [0u8; 100 * 100];
            let mut dirty_min_y: u16 = u16::MAX;
            let mut dirty_max_y: u16 = 0;
            // Scan all nodes for Glyphs content with missing atlas entries.
            for node in nodes.iter() {
                if !node.flags.contains(scene::NodeFlags::VISIBLE) {
                    continue;
                }
                if let scene::Content::Glyphs {
                    glyphs,
                    glyph_count,
                    axis_hash,
                    ..
                } = node.content
                {
                    // Map axis_hash to font_id (scene::FONT_MONO=0, scene::FONT_SANS=1).
                    let font_id = (axis_hash as u16).min((MAX_FONTS - 1) as u16);
                    let raster_font = font_slices[font_id as usize];
                    if raster_font.is_empty() {
                        continue;
                    }
                    let shaped = reader.front_shaped_glyphs(glyphs, glyph_count);
                    for sg in shaped {
                        if glyph_atlas.lookup(sg.glyph_id, font_id).is_some() {
                            continue;
                        }
                        let mut rb = fonts::rasterize::RasterBuffer {
                            data: &mut raster_buf,
                            width: 100,
                            height: 100,
                        };
                        if let Some(m) = fonts::rasterize::rasterize_with_axes(
                            raster_font,
                            sg.glyph_id,
                            font_size_px as u16,
                            &mut rb,
                            raster_scratch,
                            &[],
                            scale_factor_int,
                        ) {
                            let pack_y = glyph_atlas.row_y;
                            if glyph_atlas.pack(
                                sg.glyph_id,
                                font_id,
                                m.width as u16,
                                m.height as u16,
                                m.bearing_x as i16,
                                m.bearing_y as i16,
                                &raster_buf[..m.width as usize * m.height as usize],
                            ) {
                                // Track dirty region: the glyph was packed at row pack_y
                                // (or glyph_atlas.row_y if a new row was started).
                                if pack_y < dirty_min_y {
                                    dirty_min_y = pack_y;
                                }
                                let end_y = glyph_atlas.row_y + glyph_atlas.row_h;
                                if end_y > dirty_max_y {
                                    dirty_max_y = end_y;
                                }
                            }
                        }
                    }
                }
            }
            // If any new glyphs were packed, upload the dirty region to GPU.
            if dirty_min_y < dirty_max_y {
                let start_y = dirty_min_y;
                let end_y = core::cmp::min(dirty_max_y, ATLAS_HEIGHT as u16);
                let rows = end_y - start_y;
                let src_start = start_y as usize * ATLAS_WIDTH as usize;
                let src_end = src_start + rows as usize * ATLAS_WIDTH as usize;
                cmdbuf.clear();
                cmdbuf.upload_texture(
                    TEX_ATLAS,
                    0,
                    start_y,
                    ATLAS_WIDTH as u16,
                    rows,
                    ATLAS_WIDTH,
                    &glyph_atlas.pixels[src_start..src_end],
                );
                send_setup(&device, &mut setup_vq, irq_handle, &setup_dma, &cmdbuf);
            }
        }

        // Build the frame's Metal commands.
        let vw = width as f32;
        let vh = height as f32;
        let scale = scale_factor;

        cmdbuf.clear();
        vertex_buf.clear();
        glyph_vertex_buf.clear();
        let mut blurs: Vec<BlurReq> = Vec::new();

        // Begin render pass: MSAA float16 target, resolve to float16 TEX_RESOLVE.
        // Clear color in linear space; BG_BASE = sRGB(32) = linear ~0.0144.
        cmdbuf.begin_render_pass(
            TEX_MSAA,
            TEX_RESOLVE,
            TEX_STENCIL,
            metal::LOAD_CLEAR,
            metal::STORE_MSAA_RESOLVE,
            0.0144,
            0.0144,
            0.0144,
            1.0,
        );
        cmdbuf.set_render_pipeline(PIPE_SOLID);

        // Walk the scene tree and emit draw calls.
        let full_clip = ClipRect {
            x: 0.0,
            y: 0.0,
            w: vw / scale,
            h: vh / scale,
        };
        let mut image_atlas = ImageAtlas::new();
        walk_scene(
            nodes,
            data_buf,
            &reader,
            root,
            0.0,
            0.0,
            vw,
            vh,
            scale,
            &mut cmdbuf,
            &mut vertex_buf,
            &mut glyph_vertex_buf,
            glyph_atlas,
            font_ascent,
            &mut blurs,
            &full_clip,
            &device,
            &mut setup_vq,
            irq_handle,
            &setup_dma,
            path_buf,
            &mut image_atlas,
            content_slice,
        );

        // Flush remaining solid vertices.
        flush_solid_vertices(&mut cmdbuf, &mut vertex_buf);

        // Switch to glyph pipeline and flush glyph vertices.
        if !glyph_vertex_buf.is_empty() {
            cmdbuf.set_render_pipeline(PIPE_GLYPH);
            cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
            cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
            flush_vertices_raw(&mut cmdbuf, &mut glyph_vertex_buf);
        }

        cmdbuf.end_render_pass();

        // ── Dither pass: float16 → 8-bit sRGB drawable ─────────────────
        // Fullscreen quad reads TEX_RESOLVE (linear float16), applies 4x4
        // Bayer ordered dither in sRGB space, outputs to the 8-bit sRGB
        // drawable. This is the single point of 8-bit quantization.
        cmdbuf.begin_render_pass(
            DRAWABLE_HANDLE,
            0,
            0,
            metal::LOAD_DONT_CARE,
            metal::STORE_STORE,
            0.0,
            0.0,
            0.0,
            1.0,
        );
        cmdbuf.set_render_pipeline(PIPE_DITHER);
        cmdbuf.set_fragment_texture(TEX_RESOLVE, 0);
        cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
        // Fullscreen quad: x=0, y=0, w=viewport, h=viewport in points.
        emit_quad(
            &mut vertex_buf,
            0.0,
            0.0,
            vw / scale,
            vh / scale,
            vw,
            vh,
            scale,
            1.0,
            1.0,
            1.0,
            1.0,
        );
        flush_solid_vertices(&mut cmdbuf, &mut vertex_buf);
        cmdbuf.end_render_pass();

        // ── Backdrop blur processing ────────────────────────────────────
        // Pipeline per blur request:
        //   1. Compute: sRGB drawable → linear RGBA16F (with padding)
        //   2. 3× box blur H+V passes in linear space (shared memory)
        //   3. Compute: linear RGBA16F → sRGB drawable (center only)
        //   4. Render: semi-transparent bg overlay
        //
        // Uses W3C box_blur_widths for per-pass half-widths (CLT → Gaussian).
        // Padded capture eliminates edge artifacts. Linear-light blur is
        // physically correct (light intensities add linearly).
        for blur in &blurs {
            let px = (blur.x * scale) as u32;
            let py = (blur.y * scale) as u32;
            let pw = (blur.w * scale) as u32;
            let ph = (blur.h * scale) as u32;
            if pw == 0 || ph == 0 {
                continue;
            }

            // Per-pass box half-widths (W3C formula, same as cpu-render).
            let sigma = blur.radius as f32 / 2.0;
            let halves = drawing::box_blur_widths(sigma);
            let pad = halves[0] + halves[1] + halves[2];

            // Padded capture region, clamped to framebuffer bounds.
            // Saturating arithmetic prevents overflow on large blur radii.
            let cap_x = px.saturating_sub(pad);
            let cap_y = py.saturating_sub(pad);
            let cap_w = px.saturating_add(pw).saturating_add(pad).min(width) - cap_x;
            let cap_h = py.saturating_add(ph).saturating_add(pad).min(height) - cap_y;
            if cap_w == 0 || cap_h == 0 {
                continue;
            }

            // Step 1: Convert padded region from sRGB drawable → linear TEX_BLUR_A.
            let copy_in_params = pack_copy_params(
                cap_x as i32,
                cap_y as i32, // src offset (in drawable)
                0,
                0, // dst offset (in blur texture)
                cap_w as i32,
                cap_h as i32,
            );
            cmdbuf.begin_compute_pass();
            cmdbuf.set_compute_pipeline(CPIPE_SRGB_TO_LINEAR);
            cmdbuf.set_compute_texture(DRAWABLE_HANDLE, 0);
            cmdbuf.set_compute_texture(TEX_BLUR_A, 1);
            cmdbuf.set_compute_bytes(0, &copy_in_params);
            cmdbuf.dispatch_threads(cap_w as u16, cap_h as u16, 1, 16, 16, 1);
            cmdbuf.end_compute_pass();

            // Step 2: Three-pass box blur in linear RGBA16F.
            for pass in 0..3u32 {
                let half = halves[pass as usize] as i32;
                let blur_params = pack_blur_params(half, cap_w as i32, cap_h as i32);

                // Horizontal blur: TEX_BLUR_A → TEX_BLUR_B (threadgroup 256×1).
                cmdbuf.begin_compute_pass();
                cmdbuf.set_compute_pipeline(CPIPE_BLUR_H);
                cmdbuf.set_compute_texture(TEX_BLUR_A, 0);
                cmdbuf.set_compute_texture(TEX_BLUR_B, 1);
                cmdbuf.set_compute_bytes(0, &blur_params);
                cmdbuf.dispatch_threads(cap_w as u16, cap_h as u16, 1, 256, 1, 1);
                cmdbuf.end_compute_pass();

                // Vertical blur: TEX_BLUR_B → TEX_BLUR_A (threadgroup 1×256).
                cmdbuf.begin_compute_pass();
                cmdbuf.set_compute_pipeline(CPIPE_BLUR_V);
                cmdbuf.set_compute_texture(TEX_BLUR_B, 0);
                cmdbuf.set_compute_texture(TEX_BLUR_A, 1);
                cmdbuf.set_compute_bytes(0, &blur_params);
                cmdbuf.dispatch_threads(cap_w as u16, cap_h as u16, 1, 1, 256, 1);
                cmdbuf.end_compute_pass();
            }

            // Step 3: Convert center (unpadded) region from linear → sRGB drawable.
            let off_x = (px - cap_x) as i32; // offset of inner region in blur texture
            let off_y = (py - cap_y) as i32;
            let copy_out_params = pack_copy_params(
                off_x, off_y, // src offset (in blur texture)
                px as i32, py as i32, // dst offset (in drawable)
                pw as i32, ph as i32,
            );
            cmdbuf.begin_compute_pass();
            cmdbuf.set_compute_pipeline(CPIPE_LINEAR_TO_SRGB);
            cmdbuf.set_compute_texture(TEX_BLUR_A, 0);
            cmdbuf.set_compute_texture(DRAWABLE_HANDLE, 1);
            cmdbuf.set_compute_bytes(0, &copy_out_params);
            cmdbuf.dispatch_threads(pw as u16, ph as u16, 1, 16, 16, 1);
            cmdbuf.end_compute_pass();

            // Step 4: Semi-transparent background overlay on top of blur.
            if blur.bg.a > 0 {
                cmdbuf.begin_render_pass(
                    DRAWABLE_HANDLE,
                    0,
                    0,
                    metal::LOAD_LOAD,
                    metal::STORE_STORE,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                );
                cmdbuf.set_render_pipeline(PIPE_SOLID_NO_MSAA);
                let r = blur.bg.r as f32 / 255.0;
                let g = blur.bg.g as f32 / 255.0;
                let b = blur.bg.b as f32 / 255.0;
                let a = blur.bg.a as f32 / 255.0;
                emit_quad(
                    &mut vertex_buf,
                    blur.x,
                    blur.y,
                    blur.w,
                    blur.h,
                    vw,
                    vh,
                    scale,
                    r,
                    g,
                    b,
                    a,
                );
                flush_solid_vertices(&mut cmdbuf, &mut vertex_buf);
                cmdbuf.end_render_pass();
            }
        }

        // ── Cursor plane ────────────────────────────────────────────────
        // Read cursor image and visibility from the scene graph (these change
        // infrequently). Position comes from the pointer state register (read
        // at the top of the loop, independent of scene generation).
        if (CURSOR_PLANE_NODE as usize) < nodes.len() {
            let cnode = &nodes[CURSOR_PLANE_NODE as usize];
            cursor_visible = cnode.flags.contains(NodeFlags::VISIBLE) && cnode.opacity > 0;
            cmdbuf.set_cursor_visible(cursor_visible);

            // Always upload cursor image when hash changes — even if not
            // yet visible. This pre-populates the cursor texture on the first
            // frame so the render command buffer doesn't suddenly grow by ~5KB
            // when cursor becomes visible (avoids captured-frame corruption).
            if cnode.content_hash != cursor_image_hash {
                if let Content::InlineImage {
                    data,
                    src_width,
                    src_height,
                } = cnode.content
                {
                    let byte_count = src_width as usize * src_height as usize * 4;
                    let start = data.offset as usize;
                    let end = start + byte_count;
                    if data.length > 0 && end <= data_buf.len() {
                        cmdbuf.set_cursor_image(
                            src_width,
                            src_height,
                            0, // hotspot_x — baked into position by core
                            0, // hotspot_y
                            &data_buf[start..end],
                        );
                        cursor_image_hash = cnode.content_hash;
                    }
                }
            }

            if cursor_visible {
                // Position from pointer state register (already scaled).
                cmdbuf.set_cursor_position(cursor_x, cursor_y);
            }
        }

        cmdbuf.present_and_commit();

        // Submit frame.
        send_render(&device, &mut render_vq, irq_handle, &render_dma, &cmdbuf);

        drop(reader);
    }
}

/// Collected backdrop blur request.
struct BlurReq {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: u8,
    bg: scene::Color,
}

const MAX_BLURS: usize = 4;

// ── Blur parameter packing ───────────────────────────────────────────────

/// Pack BlurParams for the blur_h/blur_v compute shaders.
/// Layout matches MSL struct: { half_width: i32, region_w: i32, region_h: i32, _pad: i32 }.
fn pack_blur_params(half_width: i32, region_w: i32, region_h: i32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&half_width.to_le_bytes());
    buf[4..8].copy_from_slice(&region_w.to_le_bytes());
    buf[8..12].copy_from_slice(&region_h.to_le_bytes());
    buf
}

/// Pack CopyParams for the copy_srgb_to_linear / copy_linear_to_srgb compute shaders.
/// Layout matches MSL struct: { src_x, src_y, dst_x, dst_y, width, height } (6 × i32).
fn pack_copy_params(
    src_x: i32,
    src_y: i32,
    dst_x: i32,
    dst_y: i32,
    width: i32,
    height: i32,
) -> [u8; 24] {
    let mut buf = [0u8; 24];
    buf[0..4].copy_from_slice(&src_x.to_le_bytes());
    buf[4..8].copy_from_slice(&src_y.to_le_bytes());
    buf[8..12].copy_from_slice(&dst_x.to_le_bytes());
    buf[12..16].copy_from_slice(&dst_y.to_le_bytes());
    buf[16..20].copy_from_slice(&width.to_le_bytes());
    buf[20..24].copy_from_slice(&height.to_le_bytes());
    buf
}

// ── Path flattening + fan tessellation ───────────────────────────────────

const MAX_PATH_POINTS: usize = 512;

/// Maximum number of contour boundaries tracked in a single path.
const MAX_CONTOURS: usize = 32;

/// Reusable heap buffer for path flattening. Shared across `walk_scene` and
/// `draw_path_stencil_cover` to keep the recursive `walk_scene` stack frame small
/// (~300 bytes per level instead of ~4400).
type PathPointsBuf = [(f32, f32); MAX_PATH_POINTS];

/// One-time warning for path truncation.
static mut PATH_TRUNCATION_WARNED: bool = false;

fn read_f32_le(data: &[u8], offset: usize) -> f32 {
    if offset + 4 > data.len() {
        return 0.0;
    }
    f32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() {
        return u32::MAX;
    }
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn flatten_cubic(
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    points: &mut [(f32, f32)],
    count: &mut usize,
    depth: u32,
) {
    if *count >= points.len() || depth >= 10 {
        if *count < points.len() {
            points[*count] = (x3, y3);
            *count += 1;
        }
        return;
    }
    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x0) * dy - (c1y - y0) * dx).abs();
    let d2 = ((c2x - x0) * dy - (c2y - y0) * dx).abs();
    let max_d = if d1 > d2 { d1 } else { d2 };
    let chord_sq = dx * dx + dy * dy;
    if max_d * max_d <= 0.25 * chord_sq || chord_sq < 0.001 {
        points[*count] = (x3, y3);
        *count += 1;
        return;
    }
    let m01x = (x0 + c1x) * 0.5;
    let m01y = (y0 + c1y) * 0.5;
    let m12x = (c1x + c2x) * 0.5;
    let m12y = (c1y + c2y) * 0.5;
    let m23x = (c2x + x3) * 0.5;
    let m23y = (c2y + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;
    flatten_cubic(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        points,
        count,
        depth + 1,
    );
    flatten_cubic(
        mx,
        my,
        m123x,
        m123y,
        m23x,
        m23y,
        x3,
        y3,
        points,
        count,
        depth + 1,
    );
}

/// Parsed path result: flat point array plus contour boundary indices.
struct ParsedPath {
    /// Total number of points.
    n: usize,
    /// Start index of each contour in the points array.
    /// `contour_starts[0..num_contours]` are valid.
    contour_starts: [usize; MAX_CONTOURS],
    /// Number of contours.
    num_contours: usize,
}

fn parse_path_to_points(data: &[u8], out: &mut [(f32, f32); MAX_PATH_POINTS]) -> ParsedPath {
    let mut n: usize = 0;
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    let mut sx: f32 = 0.0;
    let mut sy: f32 = 0.0;
    let mut pos: usize = 0;
    let mut contour_starts = [0usize; MAX_CONTOURS];
    let mut num_contours: usize = 0;
    while pos + 4 <= data.len() {
        let tag = read_u32_le(data, pos);
        match tag {
            scene::PATH_MOVE_TO => {
                if pos + 12 > data.len() {
                    break;
                }
                // Record the start of a new contour.
                if num_contours < MAX_CONTOURS {
                    contour_starts[num_contours] = n;
                    num_contours += 1;
                }
                cx = read_f32_le(data, pos + 4);
                cy = read_f32_le(data, pos + 8);
                sx = cx;
                sy = cy;
                if n < MAX_PATH_POINTS {
                    out[n] = (cx, cy);
                    n += 1;
                }
                pos += 12;
            }
            scene::PATH_LINE_TO => {
                if pos + 12 > data.len() {
                    break;
                }
                cx = read_f32_le(data, pos + 4);
                cy = read_f32_le(data, pos + 8);
                if n < MAX_PATH_POINTS {
                    out[n] = (cx, cy);
                    n += 1;
                }
                pos += 12;
            }
            scene::PATH_CUBIC_TO => {
                if pos + 28 > data.len() {
                    break;
                }
                let c1x = read_f32_le(data, pos + 4);
                let c1y = read_f32_le(data, pos + 8);
                let c2x = read_f32_le(data, pos + 12);
                let c2y = read_f32_le(data, pos + 16);
                let x3 = read_f32_le(data, pos + 20);
                let y3 = read_f32_le(data, pos + 24);
                flatten_cubic(cx, cy, c1x, c1y, c2x, c2y, x3, y3, out, &mut n, 0);
                cx = x3;
                cy = y3;
                pos += 28;
            }
            scene::PATH_CLOSE => {
                if n < MAX_PATH_POINTS && (cx != sx || cy != sy) {
                    out[n] = (sx, sy);
                    n += 1;
                }
                cx = sx;
                cy = sy;
                pos += 4;
            }
            _ => break,
        }
    }
    if n >= MAX_PATH_POINTS {
        // SAFETY: single-threaded driver, no data race possible.
        unsafe {
            if !PATH_TRUNCATION_WARNED {
                PATH_TRUNCATION_WARNED = true;
                sys::print(b"WARNING: path flattening hit MAX_PATH_POINTS (");
                print_u32(MAX_PATH_POINTS as u32);
                sys::print(b") - complex paths may lose detail\n");
            }
        }
    }
    // If no MoveTo was encountered, treat the whole thing as one contour.
    if num_contours == 0 && n > 0 {
        contour_starts[0] = 0;
        num_contours = 1;
    }
    ParsedPath {
        n,
        contour_starts,
        num_contours,
    }
}

/// Draw a Content::Path using stencil-then-cover within the current render pass.
fn draw_path_stencil_cover(
    cmdbuf: &mut metal::CommandBuffer,
    solid_verts: &mut Vec<u8>,
    data_buf: &[u8],
    contours: scene::DataRef,
    color: scene::Color,
    fill_rule: scene::FillRule,
    node_x: f32,
    node_y: f32,
    node_w: f32,
    node_h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    opacity: f32,
    path_buf: &mut PathPointsBuf,
) {
    let offset = contours.offset as usize;
    let end = offset + contours.length as usize;
    if end > data_buf.len() {
        return;
    }

    let parsed = parse_path_to_points(&data_buf[offset..end], path_buf);
    if parsed.n < 3 {
        return;
    }

    // Flush any pending solid geometry before changing pipeline.
    flush_solid_vertices(cmdbuf, solid_verts);

    // Build fan triangle vertices from a single arbitrary point (origin = 0,0).
    // With two-sided stencil (front INCR_WRAP, back DECR_WRAP), any fan origin
    // produces correct stencil winding for any polygon — convex, concave, or
    // multi-contour. This is the standard GPU path fill algorithm.
    let n = parsed.n;
    let mut fan_verts: Vec<u8> = Vec::with_capacity(n * 3 * VERTEX_BYTES);
    let to_ndc_x = |px: f32| -> f32 { ((node_x + px) * scale / vw) * 2.0 - 1.0 };
    let to_ndc_y = |py: f32| -> f32 { 1.0 - ((node_y + py) * scale / vh) * 2.0 };

    // Use (0, 0) as the fan origin — outside the path, which is fine.
    // Fan each contour separately to avoid spurious triangles spanning
    // contour boundaries (MoveTo discontinuities).
    let fan_ox = 0.0f32;
    let fan_oy = 0.0f32;

    for ci in 0..parsed.num_contours {
        let start = parsed.contour_starts[ci];
        let end_idx = if ci + 1 < parsed.num_contours {
            parsed.contour_starts[ci + 1]
        } else {
            parsed.n
        };
        if end_idx - start < 2 {
            continue;
        }
        for i in start..end_idx - 1 {
            let (ax, ay) = path_buf[i];
            let (bx, by) = path_buf[i + 1];
            for &(px, py) in &[(fan_ox, fan_oy), (ax, ay), (bx, by)] {
                let ndc_x = to_ndc_x(px);
                let ndc_y = to_ndc_y(py);
                fan_verts.extend_from_slice(&ndc_x.to_le_bytes());
                fan_verts.extend_from_slice(&ndc_y.to_le_bytes());
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // u
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // v
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // r
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // g
                fan_verts.extend_from_slice(&0.0f32.to_le_bytes()); // b
                fan_verts.extend_from_slice(&1.0f32.to_le_bytes()); // a=1
            }
        }
    }

    // Pass 1: Stencil write (fan triangles, no color).
    // Winding rule: two-sided INCR_WRAP/DECR_WRAP — correct for any polygon.
    //   Front-facing triangles increment, back-facing decrement.
    //   Stencil != 0 means inside (non-zero winding number).
    // Even-odd rule: INVERT flips stencil bit on each triangle overlap,
    //   so odd overlap count = 1 (inside), even = 0 (outside/hole).
    cmdbuf.set_render_pipeline(PIPE_STENCIL_WRITE);
    match fill_rule {
        scene::FillRule::Winding => {
            cmdbuf.set_depth_stencil_state(DSS_STENCIL_WINDING);
            cmdbuf.set_stencil_ref(0);
        }
        scene::FillRule::EvenOdd => {
            cmdbuf.set_depth_stencil_state(DSS_STENCIL_INVERT);
            cmdbuf.set_stencil_ref(0); // ref unused for INVERT, but set for clarity
        }
    }

    // Flush fan in 4KB chunks.
    let mut sent = 0;
    while sent < fan_verts.len() {
        let chunk_end = core::cmp::min(sent + MAX_INLINE_BYTES, fan_verts.len());
        let chunk = &fan_verts[sent..chunk_end];
        let vc = chunk.len() / VERTEX_BYTES;
        cmdbuf.set_vertex_bytes(0, chunk);
        cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vc as u32);
        sent = chunk_end;
    }

    // Pass 2: Stencil test + cover (colored quad where stencil != 0).
    // ref=0: NOT_EQUAL passes where stencil != 0 (i.e., inside the path).
    cmdbuf.set_render_pipeline(PIPE_SOLID);
    cmdbuf.set_depth_stencil_state(DSS_STENCIL_TEST);
    cmdbuf.set_stencil_ref(0);

    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = (color.a as f32 / 255.0) * opacity;
    emit_quad(
        solid_verts,
        node_x,
        node_y,
        node_w,
        node_h,
        vw,
        vh,
        scale,
        r,
        g,
        b,
        a,
    );
    flush_solid_vertices(cmdbuf, solid_verts);

    // Restore normal state.
    cmdbuf.set_depth_stencil_state(DSS_NONE);
}

// ── Pointer coordinate scaling ──────────────────────────────────────────

/// Scale a raw pointer coordinate [0, 32767] to framebuffer pixels.
/// Same function as core's scale_pointer_coord — must produce identical results.
fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;
    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}

// ── Scene graph tree walk ───────────────────────────────────────────────

/// Clip rectangle in points (pre-scale).
#[derive(Clone, Copy)]
struct ClipRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl ClipRect {
    fn intersect(&self, other: &ClipRect) -> ClipRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = (self.x + self.w).min(other.x + other.w);
        let b = (self.y + self.h).min(other.y + other.h);
        ClipRect {
            x,
            y,
            w: (r - x).max(0.0),
            h: (b - y).max(0.0),
        }
    }

    fn to_pixel_scissor(&self, scale: f32) -> (u16, u16, u16, u16) {
        let w_px = self.w * scale;
        let h_px = self.h * scale;
        (
            (self.x * scale) as u16,
            (self.y * scale) as u16,
            // Manual ceil: if fractional part > 0, round up.
            (w_px as u16) + if w_px > w_px as u16 as f32 { 1 } else { 0 },
            (h_px as u16) + if h_px > h_px as u16 as f32 { 1 } else { 0 },
        )
    }
}

/// Per-frame image atlas packer. Each Content::InlineImage uploads to the next
/// available sub-rectangle within the shared 1024×1024 TEX_IMAGE texture.
/// Draws use matching UV coordinates, so no image overwrites another even
/// though uploads are synchronous and draws are deferred.
///
/// Simple row-based packing: images fill left-to-right in the current row.
/// When an image doesn't fit horizontally, advance to a new row (height =
/// tallest image in the previous row). Reset at frame start.
struct ImageAtlas {
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
}

impl ImageAtlas {
    fn new() -> Self {
        Self {
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
        }
    }

    /// Reserve a sub-rectangle for an image. Returns (x, y) atlas offset
    /// in pixels, or None if the image doesn't fit.
    fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > IMG_TEX_DIM || h > IMG_TEX_DIM {
            return None;
        }
        // Doesn't fit in current row — start a new one.
        if self.cursor_x + w > IMG_TEX_DIM {
            self.cursor_y += self.row_height;
            self.cursor_x = 0;
            self.row_height = 0;
        }
        // Doesn't fit vertically — atlas is full.
        if self.cursor_y + h > IMG_TEX_DIM {
            return None;
        }
        let x = self.cursor_x;
        let y = self.cursor_y;
        self.cursor_x += w;
        if h > self.row_height {
            self.row_height = h;
        }
        Some((x, y))
    }
}

fn walk_scene(
    nodes: &[Node],
    data_buf: &[u8],
    reader: &scene::TripleReader,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    cmdbuf: &mut metal::CommandBuffer,
    solid_verts: &mut Vec<u8>,
    glyph_verts: &mut Vec<u8>,
    atlas: &GlyphAtlas,
    font_ascent: u32,
    blurs: &mut Vec<BlurReq>,
    clip: &ClipRect,
    // Setup queue for inline image upload.
    device: &virtio::Device,
    setup_vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    setup_dma: &DmaBuf,
    // Shared heap buffer for path flattening (avoids 4 KiB stack per recursion).
    path_buf: &mut PathPointsBuf,
    // Per-frame image atlas — packs Content::InlineImage nodes into non-overlapping
    // sub-rectangles of TEX_IMAGE so uploads don't overwrite each other.
    image_atlas: &mut ImageAtlas,
    // Content Region shared memory for resolving Content::Image content_ids.
    content_region: &[u8],
) {
    if node_id == NULL || node_id as usize >= nodes.len() {
        return;
    }

    // Skip cursor node — composited by the host's cursor plane.
    if node_id == CURSOR_PLANE_NODE {
        return;
    }

    let node = &nodes[node_id as usize];

    if !node.flags.contains(NodeFlags::VISIBLE) {
        return;
    }

    let opacity = node.opacity as f32 / 255.0;
    if opacity <= 0.0 {
        return;
    }

    // Compute absolute position in points.
    // node.transform applies around the node's own origin (node.x, node.y in parent space).
    // For identity/pure-translation transforms, this collapses to a simple offset.
    let t = &node.transform;
    let node_origin_x = parent_x + scene::mpt_to_f32(node.x);
    let node_origin_y = parent_y + scene::mpt_to_f32(node.y);
    let w = scene::umpt_to_f32(node.width);
    let h = scene::umpt_to_f32(node.height);
    // abs_x/abs_y: for identity transforms, same as before. For non-trivial transforms,
    // we use the AABB of the transformed rect for clip/scissor purposes, but vertex
    // positions are computed per-corner through the transform.
    let (abs_x, abs_y) = if t.is_identity() {
        (node_origin_x, node_origin_y)
    } else if t.is_pure_translation() {
        (node_origin_x + t.tx, node_origin_y + t.ty)
    } else {
        // For rotation/scale/skew, abs_x/abs_y is the AABB top-left for clip purposes.
        let (bx, by, _, _) = t.transform_aabb(0.0, 0.0, w, h);
        (node_origin_x + bx, node_origin_y + by)
    };
    let has_nontrivial_transform = !t.is_identity() && !t.is_pure_translation();

    // Collect backdrop blur request (processed after initial render pass).
    let is_blur_node = node.backdrop_blur_radius > 0;
    let has_clip_path = node.clip_path.length > 0;
    if is_blur_node && blurs.len() < MAX_BLURS {
        blurs.push(BlurReq {
            x: abs_x,
            y: abs_y,
            w,
            h,
            radius: node.backdrop_blur_radius,
            bg: node.background,
        });
    }

    // Flush any pending glyph vertices before drawing this node's solid content.
    // This ensures correct depth ordering: previous node's text is behind this
    // node's background.
    if !glyph_verts.is_empty() {
        flush_solid_vertices(cmdbuf, solid_verts);
        cmdbuf.set_render_pipeline(PIPE_GLYPH);
        cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
        cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
        flush_vertices_raw(cmdbuf, glyph_verts);
        cmdbuf.set_render_pipeline(PIPE_SOLID);
    }

    // Draw shadow if present (behind everything else).
    // Uses an analytical Gaussian fragment shader: the exact Gaussian integral
    // for rectangles (separable erf), SDF+erfc approximation for rounded rects.
    let sc = node.shadow_color;
    if sc.a > 0 {
        let sx = abs_x + node.shadow_offset_x as f32 - node.shadow_spread as f32;
        let sy = abs_y + node.shadow_offset_y as f32 - node.shadow_spread as f32;
        let sw = w + node.shadow_spread as f32 * 2.0;
        let sh = h + node.shadow_spread as f32 * 2.0;

        // Gaussian sigma: blur_radius / 2 (W3C convention), in pixel space.
        let sigma_pt = node.shadow_blur_radius as f32 / 2.0;
        let sigma_px = sigma_pt * scale;

        if sigma_px > 0.0 {
            // Blurred shadow: switch to shadow pipeline, draw extended quad.
            flush_solid_vertices(cmdbuf, solid_verts);

            // Shadow rect in pixel coordinates for the fragment shader.
            let params = pack_shadow_params(
                sx * scale,
                sy * scale,
                (sx + sw) * scale,
                (sy + sh) * scale,
                sc.r as f32 / 255.0,
                sc.g as f32 / 255.0,
                sc.b as f32 / 255.0,
                (sc.a as f32 / 255.0) * opacity,
                sigma_px,
                node.corner_radius as f32 * scale,
            );

            // Pad the quad by 3σ to capture 99.7% of the Gaussian energy.
            let pad_pt = sigma_pt * 3.0;

            cmdbuf.set_render_pipeline(PIPE_SHADOW);
            cmdbuf.set_fragment_bytes(0, &params);
            emit_shadow_quad(solid_verts, sx, sy, sw, sh, pad_pt, vw, vh, scale);
            flush_solid_vertices(cmdbuf, solid_verts);
            cmdbuf.set_render_pipeline(PIPE_SOLID);
        } else {
            // Zero blur radius: hard shadow (flat solid quad).
            let sr = sc.r as f32 / 255.0;
            let sg = sc.g as f32 / 255.0;
            let sb = sc.b as f32 / 255.0;
            let sa = (sc.a as f32 / 255.0) * opacity;
            emit_quad(solid_verts, sx, sy, sw, sh, vw, vh, scale, sr, sg, sb, sa);
            if solid_verts.len() + 6 * VERTEX_BYTES > MAX_INLINE_BYTES {
                flush_solid_vertices(cmdbuf, solid_verts);
            }
        }
    }

    // Draw background if not transparent. Skip for blur nodes and clip_path
    // nodes — clip_path backgrounds are drawn after the stencil is set up.
    let bg = node.background;
    let corner_r = node.corner_radius;
    let has_border = node.border.width > 0 && node.border.color.a > 0;
    if bg.a > 0 && !is_blur_node && !has_clip_path {
        let r = bg.r as f32 / 255.0;
        let g = bg.g as f32 / 255.0;
        let b = bg.b as f32 / 255.0;
        let a = (bg.a as f32 / 255.0) * opacity;

        if has_nontrivial_transform && (corner_r > 0 || has_border) {
            // Transformed rounded rect: SDF evaluation in local space.
            // Vertex NDC positions are transformed; texCoords stay in local
            // pixel space. GPU interpolation is linear, so each fragment gets
            // the correct local-space coordinate for SDF evaluation.
            flush_solid_vertices(cmdbuf, solid_verts);
            let half_w_px = w * scale * 0.5;
            let half_h_px = h * scale * 0.5;
            let radius_px = corner_r as f32 * scale;
            let (bw_px, br, bg_b, bb, ba) = if has_border {
                let bc = node.border.color;
                (
                    node.border.width as f32 * scale,
                    bc.r as f32 / 255.0,
                    bc.g as f32 / 255.0,
                    bc.b as f32 / 255.0,
                    (bc.a as f32 / 255.0) * opacity,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let params =
                pack_rounded_rect_params(half_w_px, half_h_px, radius_px, bw_px, br, bg_b, bb, ba);
            cmdbuf.set_render_pipeline(PIPE_ROUNDED_RECT);
            cmdbuf.set_fragment_bytes(0, &params);
            let mut rrect_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
            emit_transformed_rounded_rect_quad(
                &mut rrect_verts,
                node_origin_x,
                node_origin_y,
                w,
                h,
                t,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            cmdbuf.set_vertex_bytes(0, &rrect_verts);
            cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
            cmdbuf.set_render_pipeline(PIPE_SOLID);
        } else if has_nontrivial_transform {
            // Transformed solid quad (no corner rounding, no border).
            flush_solid_vertices(cmdbuf, solid_verts);
            let mut xf_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
            emit_transformed_quad(
                &mut xf_verts,
                node_origin_x,
                node_origin_y,
                w,
                h,
                t,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            cmdbuf.set_vertex_bytes(0, &xf_verts);
            cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
        } else if corner_r > 0 || has_border {
            // SDF rounded rect: flush pending solid verts, switch pipeline,
            // set uniform params, draw, then switch back.
            flush_solid_vertices(cmdbuf, solid_verts);
            let half_w_px = w * scale * 0.5;
            let half_h_px = h * scale * 0.5;
            let radius_px = corner_r as f32 * scale;
            let (bw_px, br, bg_b, bb, ba) = if has_border {
                let bc = node.border.color;
                (
                    node.border.width as f32 * scale,
                    bc.r as f32 / 255.0,
                    bc.g as f32 / 255.0,
                    bc.b as f32 / 255.0,
                    (bc.a as f32 / 255.0) * opacity,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let params =
                pack_rounded_rect_params(half_w_px, half_h_px, radius_px, bw_px, br, bg_b, bb, ba);
            cmdbuf.set_render_pipeline(PIPE_ROUNDED_RECT);
            cmdbuf.set_fragment_bytes(0, &params);
            let mut rrect_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
            emit_rounded_rect_quad(
                &mut rrect_verts,
                abs_x,
                abs_y,
                w,
                h,
                vw,
                vh,
                scale,
                r,
                g,
                b,
                a,
            );
            cmdbuf.set_vertex_bytes(0, &rrect_verts);
            cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
            cmdbuf.set_render_pipeline(PIPE_SOLID);
        } else {
            emit_quad(solid_verts, abs_x, abs_y, w, h, vw, vh, scale, r, g, b, a);
            // Flush if we're close to the 4KB limit.
            if solid_verts.len() + 6 * VERTEX_BYTES > MAX_INLINE_BYTES {
                flush_solid_vertices(cmdbuf, solid_verts);
            }
        }
    } else if bg.a == 0 && has_border && !is_blur_node && !has_clip_path {
        // Border-only node (no fill): draw with transparent fill.
        flush_solid_vertices(cmdbuf, solid_verts);
        let half_w_px = w * scale * 0.5;
        let half_h_px = h * scale * 0.5;
        let radius_px = corner_r as f32 * scale;
        let bc = node.border.color;
        let params = pack_rounded_rect_params(
            half_w_px,
            half_h_px,
            radius_px,
            node.border.width as f32 * scale,
            bc.r as f32 / 255.0,
            bc.g as f32 / 255.0,
            bc.b as f32 / 255.0,
            (bc.a as f32 / 255.0) * opacity,
        );
        cmdbuf.set_render_pipeline(PIPE_ROUNDED_RECT);
        cmdbuf.set_fragment_bytes(0, &params);
        let mut rrect_verts: Vec<u8> = Vec::with_capacity(6 * VERTEX_BYTES);
        if has_nontrivial_transform {
            emit_transformed_rounded_rect_quad(
                &mut rrect_verts,
                node_origin_x,
                node_origin_y,
                w,
                h,
                t,
                vw,
                vh,
                scale,
                0.0,
                0.0,
                0.0,
                0.0,
            );
        } else {
            emit_rounded_rect_quad(
                &mut rrect_verts,
                abs_x,
                abs_y,
                w,
                h,
                vw,
                vh,
                scale,
                0.0,
                0.0,
                0.0,
                0.0,
            );
        }
        cmdbuf.set_vertex_bytes(0, &rrect_verts);
        cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, 6);
        cmdbuf.set_render_pipeline(PIPE_SOLID);
    }

    // Draw content.
    match node.content {
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            axis_hash,
            ..
        } => {
            let shaped = reader.front_shaped_glyphs(glyphs, glyph_count);
            let r = color.r as f32 / 255.0;
            let g = color.g as f32 / 255.0;
            let b = color.b as f32 / 255.0;
            let a = (color.a as f32 / 255.0) * opacity;

            let atlas_w = ATLAS_WIDTH as f32;
            let atlas_h = ATLAS_HEIGHT as f32;

            // Walk glyphs with a pen cursor that accumulates x_advance.
            // x_advance and x_offset are in scaled points (NOT 26.6 fixed-point).
            let mut pen_x = abs_x;
            let baseline_y = abs_y + font_ascent as f32;

            // Glyph atlas contains device-pixel-resolution bitmaps.
            // Divide bearing/width/height by scale to position in point space.
            let glyph_scale = scale;

            // 16.16 fixed-point to f32 conversion factor.
            let fp16 = 65536.0f32;

            // Map axis_hash to font_id for atlas lookup (scene::FONT_MONO=0, scene::FONT_SANS=1).
            let font_id = (axis_hash as u16).min((MAX_FONTS - 1) as u16);

            for sg in shaped {
                if let Some(entry) = atlas.lookup(sg.glyph_id, font_id) {
                    let gx =
                        pen_x + entry.bearing_x as f32 / glyph_scale + sg.x_offset as f32 / fp16;
                    let gy = baseline_y - entry.bearing_y as f32 / glyph_scale
                        + sg.y_offset as f32 / fp16;
                    let gw = entry.width as f32 / glyph_scale;
                    let gh = entry.height as f32 / glyph_scale;

                    // UV coordinates in atlas.
                    let u0 = entry.u as f32 / atlas_w;
                    let v0 = entry.v as f32 / atlas_h;
                    let u1 = (entry.u + entry.width) as f32 / atlas_w;
                    let v1 = (entry.v + entry.height) as f32 / atlas_h;

                    emit_textured_quad(
                        glyph_verts,
                        gx,
                        gy,
                        gw,
                        gh,
                        vw,
                        vh,
                        scale,
                        u0,
                        v0,
                        u1,
                        v1,
                        r,
                        g,
                        b,
                        a,
                    );

                    if glyph_verts.len() + 6 * VERTEX_BYTES > MAX_INLINE_BYTES {
                        flush_solid_vertices(cmdbuf, solid_verts);
                        cmdbuf.set_render_pipeline(PIPE_GLYPH);
                        cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
                        cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
                        flush_vertices_raw(cmdbuf, glyph_verts);
                        cmdbuf.set_render_pipeline(PIPE_SOLID);
                    }
                }
                pen_x += sg.x_advance as f32 / fp16;
            }
        }
        Content::Path {
            color,
            fill_rule,
            stroke_width,
            contours,
        } => {
            if contours.length > 0 {
                if stroke_width > 0 {
                    // Expand stroked path to filled geometry before rendering.
                    let offset = contours.offset as usize;
                    let end = offset + contours.length as usize;
                    if end <= data_buf.len() {
                        let src = &data_buf[offset..end];
                        let sw_pt = stroke_width as f32 / 256.0;
                        let expanded = scene::stroke::expand_stroke(src, sw_pt);
                        if !expanded.is_empty() {
                            let exp_ref = scene::DataRef {
                                offset: 0,
                                length: expanded.len() as u32,
                            };
                            draw_path_stencil_cover(
                                cmdbuf,
                                solid_verts,
                                &expanded,
                                exp_ref,
                                color,
                                scene::FillRule::Winding,
                                abs_x,
                                abs_y,
                                w,
                                h,
                                vw,
                                vh,
                                scale,
                                opacity,
                                path_buf,
                            );
                        }
                    }
                } else {
                    draw_path_stencil_cover(
                        cmdbuf,
                        solid_verts,
                        data_buf,
                        contours,
                        color,
                        fill_rule,
                        abs_x,
                        abs_y,
                        w,
                        h,
                        vw,
                        vh,
                        scale,
                        opacity,
                        path_buf,
                    );
                }
            }
        }
        Content::InlineImage {
            data,
            src_width,
            src_height,
        } => {
            let pixel_bytes = src_width as u32 * src_height as u32 * 4;
            let src_start = data.offset as usize;
            let src_end = src_start + pixel_bytes as usize;
            if data.length > 0 && src_width > 0 && src_height > 0 && src_end <= data_buf.len() {
                // Pack this image into the per-frame atlas. Each image
                // gets a unique sub-rectangle so deferred draw commands
                // sample the correct pixels from the shared TEX_IMAGE.
                if let Some((atlas_x, atlas_y)) =
                    image_atlas.allocate(src_width as u32, src_height as u32)
                {
                    flush_solid_vertices(cmdbuf, solid_verts);

                    // Upload to the image's sub-rectangle in the atlas.
                    let mut setup_cmdbuf = metal::CommandBuffer::new();
                    setup_cmdbuf.upload_texture(
                        TEX_IMAGE,
                        atlas_x as u16,
                        atlas_y as u16,
                        src_width,
                        src_height,
                        src_width as u32 * 4,
                        &data_buf[src_start..src_end],
                    );
                    send_setup(device, setup_vq, irq_handle, setup_dma, &setup_cmdbuf);

                    // UV coordinates into this image's atlas sub-rectangle.
                    let u0 = atlas_x as f32 / IMG_TEX_DIM as f32;
                    let v0 = atlas_y as f32 / IMG_TEX_DIM as f32;
                    let u1 = (atlas_x + src_width as u32) as f32 / IMG_TEX_DIM as f32;
                    let v1 = (atlas_y + src_height as u32) as f32 / IMG_TEX_DIM as f32;
                    cmdbuf.set_render_pipeline(PIPE_TEXTURED);
                    cmdbuf.set_fragment_texture(TEX_IMAGE, 0);
                    cmdbuf.set_fragment_sampler(SAMPLER_LINEAR, 0);
                    emit_textured_quad(
                        solid_verts,
                        abs_x,
                        abs_y,
                        w,
                        h,
                        vw,
                        vh,
                        scale,
                        u0,
                        v0,
                        u1,
                        v1,
                        1.0,
                        1.0,
                        1.0,
                        1.0,
                    );
                    flush_solid_vertices(cmdbuf, solid_verts);
                    cmdbuf.set_render_pipeline(PIPE_SOLID);
                }
            }
        }
        Content::Image {
            content_id,
            src_width,
            src_height,
        } => {
            // Resolve content_id from the Content Region registry.
            if !content_region.is_empty()
                && content_region.len()
                    >= core::mem::size_of::<protocol::content::ContentRegionHeader>()
            {
                // SAFETY: content_region is page-aligned shared memory; header is repr(C).
                let header = unsafe {
                    &*(content_region.as_ptr() as *const protocol::content::ContentRegionHeader)
                };
                if let Some(entry) = protocol::content::find_entry(header, content_id) {
                    let start = entry.offset as usize;
                    let end = start + entry.length as usize;
                    if end <= content_region.len() && src_width > 0 && src_height > 0 {
                        let pixel_data = &content_region[start..end];
                        if let Some((atlas_x, atlas_y)) =
                            image_atlas.allocate(src_width as u32, src_height as u32)
                        {
                            flush_solid_vertices(cmdbuf, solid_verts);

                            let mut setup_cmdbuf = metal::CommandBuffer::new();
                            setup_cmdbuf.upload_texture(
                                TEX_IMAGE,
                                atlas_x as u16,
                                atlas_y as u16,
                                src_width,
                                src_height,
                                src_width as u32 * 4,
                                pixel_data,
                            );
                            send_setup(device, setup_vq, irq_handle, setup_dma, &setup_cmdbuf);

                            let u0 = atlas_x as f32 / IMG_TEX_DIM as f32;
                            let v0 = atlas_y as f32 / IMG_TEX_DIM as f32;
                            let u1 = (atlas_x + src_width as u32) as f32 / IMG_TEX_DIM as f32;
                            let v1 = (atlas_y + src_height as u32) as f32 / IMG_TEX_DIM as f32;
                            cmdbuf.set_render_pipeline(PIPE_TEXTURED);
                            cmdbuf.set_fragment_texture(TEX_IMAGE, 0);
                            cmdbuf.set_fragment_sampler(SAMPLER_LINEAR, 0);
                            emit_textured_quad(
                                solid_verts,
                                abs_x,
                                abs_y,
                                w,
                                h,
                                vw,
                                vh,
                                scale,
                                u0,
                                v0,
                                u1,
                                v1,
                                1.0,
                                1.0,
                                1.0,
                                1.0,
                            );
                            flush_solid_vertices(cmdbuf, solid_verts);
                            cmdbuf.set_render_pipeline(PIPE_SOLID);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // Walk children with content transform applied.
    let tx = node.content_transform.tx;
    let ty = node.content_transform.ty;
    let child_base_x = abs_x + tx;
    let child_base_y = abs_y + ty;

    // If this node clips children, set up clipping.
    let child_clip = if node.flags.contains(NodeFlags::CLIPS_CHILDREN) {
        let node_rect = ClipRect {
            x: abs_x,
            y: abs_y,
            w,
            h,
        };
        let clipped = clip.intersect(&node_rect);

        // Flush pending vertices before changing clip state.
        flush_solid_vertices(cmdbuf, solid_verts);
        if !glyph_verts.is_empty() {
            cmdbuf.set_render_pipeline(PIPE_GLYPH);
            cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
            cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
            flush_vertices_raw(cmdbuf, glyph_verts);
            cmdbuf.set_render_pipeline(PIPE_SOLID);
        }

        if has_clip_path {
            // Stencil-based clip: rasterize clip path to stencil buffer,
            // then all children draw with stencil test (pass where != 0).
            let cp = node.clip_path;
            let cp_off = cp.offset as usize;
            let cp_end = cp_off + cp.length as usize;

            if cp_end <= data_buf.len() {
                let cp_parsed = parse_path_to_points(&data_buf[cp_off..cp_end], path_buf);
                let n_pts = cp_parsed.n;

                if n_pts >= 3 {
                    // Build fan triangles for the clip path.
                    let mut fan_verts: Vec<u8> = Vec::with_capacity(n_pts * 3 * VERTEX_BYTES);
                    let mut cx_sum: f32 = 0.0;
                    let mut cy_sum: f32 = 0.0;
                    for i in 0..n_pts {
                        cx_sum += path_buf[i].0;
                        cy_sum += path_buf[i].1;
                    }
                    let centroid_x = cx_sum / n_pts as f32;
                    let centroid_y = cy_sum / n_pts as f32;

                    for i in 0..n_pts - 1 {
                        let (ax, ay) = path_buf[i];
                        let (bx, by) = path_buf[i + 1];
                        for &(px, py) in &[(centroid_x, centroid_y), (bx, by), (ax, ay)] {
                            let ndc_x = ((abs_x + px) * scale / vw) * 2.0 - 1.0;
                            let ndc_y = 1.0 - ((abs_y + py) * scale / vh) * 2.0;
                            fan_verts.extend_from_slice(&ndc_x.to_le_bytes());
                            fan_verts.extend_from_slice(&ndc_y.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&0.0f32.to_le_bytes());
                            fan_verts.extend_from_slice(&1.0f32.to_le_bytes()); // a=1 (non-zero)
                        }
                    }

                    // Write clip path to stencil.
                    cmdbuf.set_render_pipeline(PIPE_STENCIL_WRITE);
                    cmdbuf.set_depth_stencil_state(DSS_STENCIL_WRITE);
                    cmdbuf.set_stencil_ref(1);
                    let mut sent = 0;
                    while sent < fan_verts.len() {
                        let chunk_end = core::cmp::min(sent + MAX_INLINE_BYTES, fan_verts.len());
                        let chunk = &fan_verts[sent..chunk_end];
                        let vc = chunk.len() / VERTEX_BYTES;
                        cmdbuf.set_vertex_bytes(0, chunk);
                        cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vc as u32);
                        sent = chunk_end;
                    }

                    // Use stencil test for clipped children.
                    cmdbuf.set_render_pipeline(PIPE_SOLID);
                    cmdbuf.set_depth_stencil_state(DSS_CLIP_TEST);
                    cmdbuf.set_stencil_ref(0);

                    // Draw the clip node's own background inside the stencil.
                    if bg.a > 0 {
                        let r = bg.r as f32 / 255.0;
                        let g = bg.g as f32 / 255.0;
                        let b = bg.b as f32 / 255.0;
                        let a = (bg.a as f32 / 255.0) * opacity;
                        emit_quad(solid_verts, abs_x, abs_y, w, h, vw, vh, scale, r, g, b, a);
                        flush_solid_vertices(cmdbuf, solid_verts);
                    }
                }
            }
        } else {
            // Rectangular scissor clip.
            let (sx, sy, sw, sh) = clipped.to_pixel_scissor(scale);
            cmdbuf.set_scissor(sx, sy, sw, sh);
        }
        clipped
    } else {
        *clip
    };

    let mut child = node.first_child;
    while child != NULL {
        walk_scene(
            nodes,
            data_buf,
            reader,
            child,
            child_base_x,
            child_base_y,
            vw,
            vh,
            scale,
            cmdbuf,
            solid_verts,
            glyph_verts,
            atlas,
            font_ascent,
            blurs,
            &child_clip,
            device,
            setup_vq,
            irq_handle,
            setup_dma,
            path_buf,
            image_atlas,
            content_region,
        );
        if child as usize >= nodes.len() {
            break;
        }
        child = nodes[child as usize].next_sibling;
    }

    // Restore clip state after children.
    if node.flags.contains(NodeFlags::CLIPS_CHILDREN) {
        flush_solid_vertices(cmdbuf, solid_verts);
        if !glyph_verts.is_empty() {
            cmdbuf.set_render_pipeline(PIPE_GLYPH);
            cmdbuf.set_fragment_texture(TEX_ATLAS, 0);
            cmdbuf.set_fragment_sampler(SAMPLER_NEAREST, 0);
            flush_vertices_raw(cmdbuf, glyph_verts);
            cmdbuf.set_render_pipeline(PIPE_SOLID);
        }

        if has_clip_path {
            // Clear stencil, restore normal DSA.
            cmdbuf.set_depth_stencil_state(DSS_NONE);
        } else {
            // Restore parent scissor.
            let (sx, sy, sw, sh) = clip.to_pixel_scissor(scale);
            cmdbuf.set_scissor(sx, sy, sw, sh);
        }
    }
}

// ── Vertex emission helpers ─────────────────────────────────────────────

/// Push a solid-color quad (6 vertices) into the vertex buffer.
fn emit_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    // Convert points to pixels, then to NDC.
    let l = (x * scale / vw) * 2.0 - 1.0;
    let r_ndc = ((x + w) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (y * scale / vh) * 2.0;
    let b_ndc = 1.0 - ((y + h) * scale / vh) * 2.0;

    // 6 vertices: two triangles.
    let verts: [[f32; 8]; 6] = [
        [l, t, 0.0, 0.0, r, g, b, a],
        [r_ndc, t, 1.0, 0.0, r, g, b, a],
        [l, b_ndc, 0.0, 1.0, r, g, b, a],
        [r_ndc, t, 1.0, 0.0, r, g, b, a],
        [r_ndc, b_ndc, 1.0, 1.0, r, g, b, a],
        [l, b_ndc, 0.0, 1.0, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a textured quad (6 vertices) with custom UV coordinates.
fn emit_textured_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = (x * scale / vw) * 2.0 - 1.0;
    let r_ndc = ((x + w) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (y * scale / vh) * 2.0;
    let b_ndc = 1.0 - ((y + h) * scale / vh) * 2.0;

    let verts: [[f32; 8]; 6] = [
        [l, t, u0, v0, r, g, b, a],
        [r_ndc, t, u1, v0, r, g, b, a],
        [l, b_ndc, u0, v1, r, g, b, a],
        [r_ndc, t, u1, v0, r, g, b, a],
        [r_ndc, b_ndc, u1, v1, r, g, b, a],
        [l, b_ndc, u0, v1, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a solid-color quad with an affine transform applied to each vertex.
/// The transform maps local coordinates (0,0)→(w,h) to parent space at (ox, oy).
fn emit_transformed_quad(
    buf: &mut Vec<u8>,
    ox: f32,
    oy: f32,
    w: f32,
    h: f32,
    t: &scene::AffineTransform,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    // Transform 4 corners of the local rect through the affine, then offset to parent space.
    let corners = [(0.0f32, 0.0f32), (w, 0.0), (0.0, h), (w, h)];
    let mut tc = [(0.0f32, 0.0f32); 4];
    for (i, &(lx, ly)) in corners.iter().enumerate() {
        let (tx, ty) = t.transform_point(lx, ly);
        tc[i] = (ox + tx, oy + ty);
    }
    // Convert to NDC.
    let to_ndc = |px: f32, py: f32| -> (f32, f32) {
        ((px * scale / vw) * 2.0 - 1.0, 1.0 - (py * scale / vh) * 2.0)
    };
    let (x0, y0) = to_ndc(tc[0].0, tc[0].1);
    let (x1, y1) = to_ndc(tc[1].0, tc[1].1);
    let (x2, y2) = to_ndc(tc[2].0, tc[2].1);
    let (x3, y3) = to_ndc(tc[3].0, tc[3].1);

    // Two triangles: (0,1,2) and (1,3,2).
    let verts: [[f32; 8]; 6] = [
        [x0, y0, 0.0, 0.0, r, g, b, a],
        [x1, y1, 1.0, 0.0, r, g, b, a],
        [x2, y2, 0.0, 1.0, r, g, b, a],
        [x1, y1, 1.0, 0.0, r, g, b, a],
        [x3, y3, 1.0, 1.0, r, g, b, a],
        [x2, y2, 0.0, 1.0, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a rounded-rect quad. texCoord carries local pixel coordinates
/// relative to the rect center (the SDF evaluates per-pixel in local space).
fn emit_rounded_rect_quad(
    buf: &mut Vec<u8>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let l = (x * scale / vw) * 2.0 - 1.0;
    let r_ndc = ((x + w) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (y * scale / vh) * 2.0;
    let b_ndc = 1.0 - ((y + h) * scale / vh) * 2.0;

    // Local pixel coords: center of rect = (0,0), corners at (±half_w_px, ±half_h_px).
    let half_w_px = w * scale * 0.5;
    let half_h_px = h * scale * 0.5;

    let verts: [[f32; 8]; 6] = [
        [l, t, -half_w_px, -half_h_px, r, g, b, a],
        [r_ndc, t, half_w_px, -half_h_px, r, g, b, a],
        [l, b_ndc, -half_w_px, half_h_px, r, g, b, a],
        [r_ndc, t, half_w_px, -half_h_px, r, g, b, a],
        [r_ndc, b_ndc, half_w_px, half_h_px, r, g, b, a],
        [l, b_ndc, -half_w_px, half_h_px, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Push a rounded-rect quad with an affine transform applied to vertex positions.
///
/// Like `emit_transformed_quad`, the 4 corners are transformed through the
/// affine to produce NDC positions. But unlike a solid quad, texCoords carry
/// LOCAL pixel coordinates (center-relative), identical to `emit_rounded_rect_quad`.
///
/// Because the affine is linear and GPU barycentric interpolation is linear,
/// each fragment receives the correct local-space coordinate. The SDF shader
/// evaluates in that local space — no shader changes needed.
fn emit_transformed_rounded_rect_quad(
    buf: &mut Vec<u8>,
    ox: f32,
    oy: f32,
    w: f32,
    h: f32,
    t: &scene::AffineTransform,
    vw: f32,
    vh: f32,
    scale: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    // Transform 4 corners of the local rect, offset to parent space.
    let corners = [
        (0.0f32, 0.0f32), // top-left
        (w, 0.0),         // top-right
        (0.0, h),         // bottom-left
        (w, h),           // bottom-right
    ];
    let mut ndc = [(0.0f32, 0.0f32); 4];
    for (i, &(lx, ly)) in corners.iter().enumerate() {
        let (tx, ty) = t.transform_point(lx, ly);
        let px = ox + tx;
        let py = oy + ty;
        ndc[i] = ((px * scale / vw) * 2.0 - 1.0, 1.0 - (py * scale / vh) * 2.0);
    }

    // Local pixel coords: center of rect = (0,0), same as axis-aligned version.
    let half_w_px = w * scale * 0.5;
    let half_h_px = h * scale * 0.5;

    // Two triangles: (0,1,2) and (1,3,2).
    // NDC positions are transformed; texCoords are in local (pre-transform) space.
    let verts: [[f32; 8]; 6] = [
        [ndc[0].0, ndc[0].1, -half_w_px, -half_h_px, r, g, b, a],
        [ndc[1].0, ndc[1].1, half_w_px, -half_h_px, r, g, b, a],
        [ndc[2].0, ndc[2].1, -half_w_px, half_h_px, r, g, b, a],
        [ndc[1].0, ndc[1].1, half_w_px, -half_h_px, r, g, b, a],
        [ndc[3].0, ndc[3].1, half_w_px, half_h_px, r, g, b, a],
        [ndc[2].0, ndc[2].1, -half_w_px, half_h_px, r, g, b, a],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Pack RoundedRectParams for the fragment_rounded_rect shader.
/// Layout: { half_w, half_h, radius, border_w, border_r, border_g, border_b, border_a } (8 × f32 = 32 bytes).
fn pack_rounded_rect_params(
    half_w: f32,
    half_h: f32,
    radius: f32,
    border_w: f32,
    border_r: f32,
    border_g: f32,
    border_b: f32,
    border_a: f32,
) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..4].copy_from_slice(&half_w.to_le_bytes());
    buf[4..8].copy_from_slice(&half_h.to_le_bytes());
    buf[8..12].copy_from_slice(&radius.to_le_bytes());
    buf[12..16].copy_from_slice(&border_w.to_le_bytes());
    buf[16..20].copy_from_slice(&border_r.to_le_bytes());
    buf[20..24].copy_from_slice(&border_g.to_le_bytes());
    buf[24..28].copy_from_slice(&border_b.to_le_bytes());
    buf[28..32].copy_from_slice(&border_a.to_le_bytes());
    buf
}

/// Pack ShadowParams for the fragment_shadow shader.
/// Layout: { rect_min_x, rect_min_y, rect_max_x, rect_max_y,
///           color_r, color_g, color_b, color_a,
///           sigma, corner_radius, _pad0, _pad1 } (12 × f32 = 48 bytes).
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

/// Emit a shadow quad (6 vertices) covering the shadow rect plus blur padding.
/// texCoord carries absolute pixel-space coordinates for the fragment shader.
fn emit_shadow_quad(
    buf: &mut Vec<u8>,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    pad: f32,
    vw: f32,
    vh: f32,
    scale: f32,
) {
    // Quad extends beyond shadow rect by pad on all sides.
    let qx = sx - pad;
    let qy = sy - pad;
    let qw = sw + 2.0 * pad;
    let qh = sh + 2.0 * pad;

    // NDC for rasterization.
    let l = (qx * scale / vw) * 2.0 - 1.0;
    let r = ((qx + qw) * scale / vw) * 2.0 - 1.0;
    let t = 1.0 - (qy * scale / vh) * 2.0;
    let b = 1.0 - ((qy + qh) * scale / vh) * 2.0;

    // Pixel-space coordinates for the fragment shader's Gaussian evaluation.
    let px_l = qx * scale;
    let px_r = (qx + qw) * scale;
    let px_t = qy * scale;
    let px_b = (qy + qh) * scale;

    // Color fields unused by fragment_shadow (reads from uniform buffer).
    let verts: [[f32; 8]; 6] = [
        [l, t, px_l, px_t, 0.0, 0.0, 0.0, 0.0],
        [r, t, px_r, px_t, 0.0, 0.0, 0.0, 0.0],
        [l, b, px_l, px_b, 0.0, 0.0, 0.0, 0.0],
        [r, t, px_r, px_t, 0.0, 0.0, 0.0, 0.0],
        [r, b, px_r, px_b, 0.0, 0.0, 0.0, 0.0],
        [l, b, px_l, px_b, 0.0, 0.0, 0.0, 0.0],
    ];
    for v in &verts {
        for f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// Flush accumulated solid-color vertices: set_vertex_bytes + draw.
fn flush_solid_vertices(cmdbuf: &mut metal::CommandBuffer, buf: &mut Vec<u8>) {
    if buf.is_empty() {
        return;
    }
    let vertex_count = buf.len() / VERTEX_BYTES;
    cmdbuf.set_vertex_bytes(0, buf.as_slice());
    cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vertex_count as u32);
    buf.clear();
}

/// Flush a raw vertex buffer: set_vertex_bytes + draw (any pipeline).
fn flush_vertices_raw(cmdbuf: &mut metal::CommandBuffer, buf: &mut Vec<u8>) {
    if buf.is_empty() {
        return;
    }
    let vertex_count = buf.len() / VERTEX_BYTES;
    cmdbuf.set_vertex_bytes(0, buf.as_slice());
    cmdbuf.draw_primitives(metal::PRIM_TRIANGLE, 0, vertex_count as u32);
    buf.clear();
}

// ── Virtqueue allocation helper ─────────────────────────────────────────

fn alloc_virtqueue(device: &virtio::Device, index: u32, size: u32) -> virtio::Virtqueue {
    let order = virtio::Virtqueue::allocation_order(size);
    let mut pa: u64 = 0;
    let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
        sys::print(b"metal-render: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let bytes = (1usize << order) * ipc::PAGE_SIZE;
    // SAFETY: va is freshly allocated DMA memory of `bytes` size.
    unsafe { core::ptr::write_bytes(va as *mut u8, 0, bytes) };
    let vq = virtio::Virtqueue::new(size, va, pa);
    device.setup_queue(index, size, vq.desc_pa(), vq.avail_pa(), vq.used_pa());
    vq
}
