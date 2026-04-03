//! MSL shader source constants.

pub(crate) const MSL_SOURCE: &[u8] = b"
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
