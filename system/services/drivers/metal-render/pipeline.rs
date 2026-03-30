//! Phase D: Metal pipeline setup (shader compilation, pipelines, textures, samplers).

use protocol::metal;

use crate::{
    dma::DmaBuf, shaders::MSL_SOURCE, virtio_helpers::send_setup, ATLAS_HEIGHT, ATLAS_WIDTH,
    CPIPE_BLUR_H, CPIPE_BLUR_V, CPIPE_LINEAR_TO_SRGB, CPIPE_SRGB_TO_LINEAR, CURSOR_TEX_SIZE,
    DSS_CLIP_TEST, DSS_NONE, DSS_STENCIL_INVERT, DSS_STENCIL_TEST, DSS_STENCIL_WINDING,
    DSS_STENCIL_WRITE, FN_BLUR_H, FN_BLUR_V, FN_COPY_LINEAR_TO_SRGB, FN_COPY_SRGB_TO_LINEAR,
    FN_FRAGMENT_DITHER, FN_FRAGMENT_GLYPH, FN_FRAGMENT_ROUNDED_RECT, FN_FRAGMENT_SHADOW,
    FN_FRAGMENT_SOLID, FN_FRAGMENT_TEXTURED, FN_VERTEX_MAIN, FN_VERTEX_STENCIL, IMG_TEX_DIM,
    LIB_SHADERS, PIPE_DITHER, PIPE_GLYPH, PIPE_ROUNDED_RECT, PIPE_SHADOW, PIPE_SOLID,
    PIPE_SOLID_NO_MSAA, PIPE_STENCIL_WRITE, PIPE_TEXTURED, SAMPLER_LINEAR, SAMPLER_NEAREST,
    SAMPLE_COUNT, TEX_ATLAS, TEX_BLUR_A, TEX_BLUR_B, TEX_CURSOR_BLUR_A, TEX_CURSOR_BLUR_B,
    TEX_CURSOR_MSAA, TEX_CURSOR_RESOLVE, TEX_CURSOR_SRGB, TEX_CURSOR_STENCIL, TEX_IMAGE, TEX_MSAA,
    TEX_RESOLVE, TEX_STENCIL,
};

pub(crate) fn setup_pipelines(
    device: &virtio::Device,
    setup_vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    setup_dma: &DmaBuf,
    width: u32,
    height: u32,
) {
    let mut cmdbuf = metal::CommandBuffer::new();

    // Compile shader library.
    cmdbuf.clear();
    cmdbuf.compile_library(LIB_SHADERS, MSL_SOURCE);
    send_setup(device, setup_vq, irq_handle, setup_dma, &cmdbuf);
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
    send_setup(device, setup_vq, irq_handle, setup_dma, &cmdbuf);
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
    send_setup(device, setup_vq, irq_handle, setup_dma, &cmdbuf);
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
    send_setup(device, setup_vq, irq_handle, setup_dma, &cmdbuf);

    // Create samplers.
    cmdbuf.clear();
    cmdbuf.create_sampler(
        SAMPLER_NEAREST,
        metal::FILTER_NEAREST,
        metal::FILTER_NEAREST,
    );
    cmdbuf.create_sampler(SAMPLER_LINEAR, metal::FILTER_LINEAR, metal::FILTER_LINEAR);
    send_setup(device, setup_vq, irq_handle, setup_dma, &cmdbuf);

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
    // Image atlas texture (BGRA8, 1024x1024). All Content::InlineImage nodes in a
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
    // precision — 8-bit would lose dark detail after sRGB->linear conversion).
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
    // Cursor textures — same pipeline as main framebuffer (4x MSAA, float16).
    cmdbuf.create_texture(
        TEX_CURSOR_MSAA,
        CURSOR_TEX_SIZE,
        CURSOR_TEX_SIZE,
        metal::PIXEL_FORMAT_RGBA16F,
        SAMPLE_COUNT,
        metal::USAGE_RENDER_TARGET,
    );
    cmdbuf.create_texture(
        TEX_CURSOR_STENCIL,
        CURSOR_TEX_SIZE,
        CURSOR_TEX_SIZE,
        metal::PIXEL_FORMAT_STENCIL8,
        SAMPLE_COUNT,
        metal::USAGE_RENDER_TARGET,
    );
    cmdbuf.create_texture(
        TEX_CURSOR_RESOLVE,
        CURSOR_TEX_SIZE,
        CURSOR_TEX_SIZE,
        metal::PIXEL_FORMAT_RGBA16F,
        1,
        metal::USAGE_SHADER_READ | metal::USAGE_RENDER_TARGET,
    );
    cmdbuf.create_texture(
        TEX_CURSOR_SRGB,
        CURSOR_TEX_SIZE,
        CURSOR_TEX_SIZE,
        metal::PIXEL_FORMAT_BGRA8_SRGB,
        1,
        metal::USAGE_SHADER_READ | metal::USAGE_RENDER_TARGET,
    );
    // Cursor shadow blur ping-pong textures (1x, float16 — same format as main blur).
    cmdbuf.create_texture(
        TEX_CURSOR_BLUR_A,
        CURSOR_TEX_SIZE,
        CURSOR_TEX_SIZE,
        metal::PIXEL_FORMAT_RGBA16F,
        1,
        metal::USAGE_SHADER_READ | metal::USAGE_SHADER_WRITE,
    );
    cmdbuf.create_texture(
        TEX_CURSOR_BLUR_B,
        CURSOR_TEX_SIZE,
        CURSOR_TEX_SIZE,
        metal::PIXEL_FORMAT_RGBA16F,
        1,
        metal::USAGE_SHADER_READ | metal::USAGE_SHADER_WRITE,
    );
    send_setup(device, setup_vq, irq_handle, setup_dma, &cmdbuf);
    sys::print(b"     textures created\n");
}
