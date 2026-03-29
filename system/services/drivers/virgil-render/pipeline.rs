//! GPU pipeline setup (Phase D) and initial clear (Phase E).
//!
//! Sets up the Gallium3D pipeline state objects (blend, DSA, rasterizer,
//! shaders, surface, vertex elements) and performs the initial screen clear.

use alloc::boxed::Box;

use protocol::metal::virgl::{
    self, PIPE_PRIM_TRIANGLES, PIPE_SHADER_FRAGMENT, PIPE_SHADER_VERTEX,
    VIRGL_FORMAT_B8G8R8A8_UNORM, VIRGL_FORMAT_Z32_FLOAT_S8X24_UINT, VIRGL_OBJECT_BLEND,
    VIRGL_OBJECT_DSA, VIRGL_OBJECT_RASTERIZER, VIRGL_OBJECT_VERTEX_ELEMENTS,
    VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_SUBMIT_3D, VIRTIO_GPU_RESP_OK_NODATA,
};

use crate::{
    resources::{gpu_cmd_ok, gpu_command, print_hex_u32, print_u32},
    wire::{
        box_zeroed, ctrl_header, ctrl_header_ctx, CtrlHeader, DmaBuf, ResourceFlush, Submit3dHeader,
    },
    ATLAS_RESOURCE_ID, BLUR_CAPTURE_RESOURCE_ID, BLUR_INTERMEDIATE_RESOURCE_ID, HANDLE_BLEND,
    HANDLE_BLEND_NO_COLOR, HANDLE_BLUR_CAPTURE_SURFACE, HANDLE_BLUR_CAPTURE_VIEW,
    HANDLE_BLUR_INTERMEDIATE_SURFACE, HANDLE_BLUR_INTERMEDIATE_VIEW, HANDLE_DSA,
    HANDLE_DSA_CLIP_TEST, HANDLE_DSA_STENCIL_TEST, HANDLE_DSA_STENCIL_WRITE, HANDLE_FS,
    HANDLE_FS_BLUR_H, HANDLE_FS_BLUR_V, HANDLE_FS_GLYPH, HANDLE_FS_IMAGE, HANDLE_RASTERIZER,
    HANDLE_SAMPLER, HANDLE_SAMPLER_VIEW, HANDLE_SAMPLER_VIEW_IMG, HANDLE_STENCIL_SURFACE,
    HANDLE_SURFACE, HANDLE_VE, HANDLE_VE_TEXTURED, HANDLE_VS, HANDLE_VS_TEXTURED, IMG_RESOURCE_ID,
    RT_RESOURCE_ID, STENCIL_RESOURCE_ID, VB_RESOURCE_ID, VIRGL_CTX_ID, VIRTQ_CONTROL,
};

// ── Phase D: GPU pipeline setup via CMD_SUBMIT_3D ────────────────────────

pub(crate) fn submit_3d(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    cmdbuf: &virgl::CommandBuffer,
) -> bool {
    let data = cmdbuf.as_dwords();
    let data_bytes = cmdbuf.size_bytes();
    let header_size = core::mem::size_of::<Submit3dHeader>();
    let total_cmd_bytes = header_size + data_bytes as usize;

    // Allocate DMA buffer large enough for header + command data + response.
    let total_with_resp = total_cmd_bytes + ipc::PAGE_SIZE; // leave room for response
    let cmd_pages = (total_with_resp + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE;
    let cmd_order = (cmd_pages.next_power_of_two().trailing_zeros()) as u32;
    let mut cmd = DmaBuf::alloc(cmd_order);

    // Write Submit3dHeader.
    // SAFETY: cmd.va points to zeroed DMA memory, writing header at start.
    unsafe {
        core::ptr::write(
            cmd.va as *mut Submit3dHeader,
            Submit3dHeader {
                hdr: ctrl_header_ctx(VIRTIO_GPU_CMD_SUBMIT_3D),
                size: data_bytes,
                _pad: 0,
            },
        );
    }

    // Copy command buffer data after the header.
    // SAFETY: data is a valid u32 slice, destination is within DMA allocation.
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr() as *const u8,
            (cmd.va + header_size) as *mut u8,
            data_bytes as usize,
        );
    }

    // Response at page-aligned offset after command data.
    let resp_offset = ((total_cmd_bytes + ipc::PAGE_SIZE - 1) / ipc::PAGE_SIZE) * ipc::PAGE_SIZE;
    let resp_pa = cmd.pa + resp_offset as u64;
    let resp_va = cmd.va + resp_offset;

    let resp_type = gpu_command(
        device,
        vq,
        irq_handle,
        cmd.pa,
        total_cmd_bytes as u32,
        resp_pa,
        resp_va,
        core::mem::size_of::<CtrlHeader>() as u32,
    );

    let ok = resp_type == VIRTIO_GPU_RESP_OK_NODATA;
    if !ok {
        sys::print(b"virgil-render: SUBMIT_3D failed (resp=");
        print_hex_u32(resp_type);
        sys::print(b")\n");
    }
    cmd.free();
    ok
}

/// Set up the GPU pipeline. Returns true if stencil-then-cover is available.
pub(crate) fn setup_pipeline(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    width: u32,
    height: u32,
) -> bool {
    // Heap-allocate the CommandBuffer (16 KiB — same size as user stack).
    let mut cmdbuf: Box<virgl::CommandBuffer> = box_zeroed();

    // Create pipeline state objects.
    cmdbuf.cmd_create_blend(HANDLE_BLEND);
    cmdbuf.cmd_create_dsa(HANDLE_DSA);
    cmdbuf.cmd_create_rasterizer(HANDLE_RASTERIZER, true);

    // Create surface wrapping our render target resource.
    cmdbuf.cmd_create_surface(HANDLE_SURFACE, RT_RESOURCE_ID, VIRGL_FORMAT_B8G8R8A8_UNORM);

    // Create vertex elements layout (position float2 + color float4).
    cmdbuf.cmd_create_vertex_elements_color(HANDLE_VE);

    // Create shaders from TGSI text.
    cmdbuf.cmd_create_shader_text(HANDLE_VS, PIPE_SHADER_VERTEX, crate::shaders::COLOR_VS);
    cmdbuf.cmd_create_shader_text(HANDLE_FS, PIPE_SHADER_FRAGMENT, crate::shaders::COLOR_FS);

    // Textured pipeline objects (for glyph rendering).
    cmdbuf.cmd_create_vertex_elements_textured(HANDLE_VE_TEXTURED);
    cmdbuf.cmd_create_shader_text(
        HANDLE_VS_TEXTURED,
        PIPE_SHADER_VERTEX,
        crate::shaders::TEXTURED_VS,
    );
    cmdbuf.cmd_create_shader_text(
        HANDLE_FS_GLYPH,
        PIPE_SHADER_FRAGMENT,
        crate::shaders::GLYPH_FS,
    );
    cmdbuf.cmd_create_sampler_state(HANDLE_SAMPLER);
    cmdbuf.cmd_create_sampler_view(
        HANDLE_SAMPLER_VIEW,
        ATLAS_RESOURCE_ID,
        protocol::metal::virgl::VIRGL_FORMAT_R8_UNORM,
    );

    // Image pipeline: full-color fragment shader (TEXTURED_FS) + sampler view.
    cmdbuf.cmd_create_shader_text(
        HANDLE_FS_IMAGE,
        PIPE_SHADER_FRAGMENT,
        crate::shaders::TEXTURED_FS,
    );
    cmdbuf.cmd_create_sampler_view(
        HANDLE_SAMPLER_VIEW_IMG,
        IMG_RESOURCE_ID,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
    );

    // Bind color pipeline state (initial state for background rendering).
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_RASTERIZER, HANDLE_RASTERIZER);
    cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE);
    cmdbuf.cmd_bind_shader(HANDLE_VS, PIPE_SHADER_VERTEX);
    cmdbuf.cmd_bind_shader(HANDLE_FS, PIPE_SHADER_FRAGMENT);

    // Set framebuffer (color only — stencil attached separately if available).
    cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, 0);
    cmdbuf.cmd_set_viewport(width as f32, height as f32);
    cmdbuf.cmd_set_vertex_buffers(crate::scene_walk::VERTEX_STRIDE, 0, VB_RESOURCE_ID);

    if cmdbuf.overflowed() {
        sys::print(b"virgil-render: pipeline command buffer overflowed!\n");
        sys::exit();
    }

    sys::print(b"     submitting pipeline setup (");
    print_u32(cmdbuf.size_bytes());
    sys::print(b" bytes)\n");

    if !submit_3d(device, vq, irq_handle, &cmdbuf) {
        sys::print(b"virgil-render: pipeline setup SUBMIT_3D failed\n");
        sys::exit();
    }
    sys::print(b"     pipeline setup complete\n");

    // ── Stencil-then-cover setup (each object in its own submission) ──
    cmdbuf.clear();
    cmdbuf.cmd_create_dsa_stencil_write(HANDLE_DSA_STENCIL_WRITE);
    cmdbuf.cmd_create_dsa_stencil_test(HANDLE_DSA_STENCIL_TEST);
    cmdbuf.cmd_create_dsa_clip_test(HANDLE_DSA_CLIP_TEST);
    cmdbuf.cmd_create_blend_no_color(HANDLE_BLEND_NO_COLOR);
    cmdbuf.cmd_create_surface(
        HANDLE_STENCIL_SURFACE,
        STENCIL_RESOURCE_ID,
        VIRGL_FORMAT_Z32_FLOAT_S8X24_UINT, // depth32f + stencil8 (Apple Silicon compatible)
    );
    cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, HANDLE_STENCIL_SURFACE);
    let stencil_ok = submit_3d(device, vq, irq_handle, &cmdbuf);

    if stencil_ok {
        sys::print(b"     stencil pipeline ready\n");
    } else {
        sys::print(b"     stencil pipeline FAILED - recovering\n");
        cmdbuf.clear();
        cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, 0);
        cmdbuf.cmd_set_viewport(width as f32, height as f32);
        let _ = submit_3d(device, vq, irq_handle, &cmdbuf);
    }

    // ── Blur pipeline setup (separate submission — shaders are large) ────
    //
    // Two 9-tap Gaussian blur fragment shaders (~400 DWORDs each) and four
    // pipeline objects (two surfaces + two sampler views). Placed in their
    // own SUBMIT_3D to avoid overflowing the main pipeline command buffer.
    cmdbuf.clear();
    cmdbuf.cmd_create_shader_text(
        HANDLE_FS_BLUR_H,
        PIPE_SHADER_FRAGMENT,
        crate::shaders::BLUR_H_FS,
    );
    cmdbuf.cmd_create_shader_text(
        HANDLE_FS_BLUR_V,
        PIPE_SHADER_FRAGMENT,
        crate::shaders::BLUR_V_FS,
    );
    cmdbuf.cmd_create_surface(
        HANDLE_BLUR_CAPTURE_SURFACE,
        BLUR_CAPTURE_RESOURCE_ID,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
    );
    cmdbuf.cmd_create_sampler_view(
        HANDLE_BLUR_CAPTURE_VIEW,
        BLUR_CAPTURE_RESOURCE_ID,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
    );
    cmdbuf.cmd_create_surface(
        HANDLE_BLUR_INTERMEDIATE_SURFACE,
        BLUR_INTERMEDIATE_RESOURCE_ID,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
    );
    cmdbuf.cmd_create_sampler_view(
        HANDLE_BLUR_INTERMEDIATE_VIEW,
        BLUR_INTERMEDIATE_RESOURCE_ID,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
    );
    if cmdbuf.overflowed() {
        sys::print(b"virgil-render: blur pipeline command buffer overflowed!\n");
        // Blur is non-fatal — continue without it.
    } else if submit_3d(device, vq, irq_handle, &cmdbuf) {
        sys::print(b"     blur pipeline ready\n");
    } else {
        sys::print(b"     blur pipeline FAILED\n");
    }

    stencil_ok
}

// ── Phase E: Clear screen + flush ────────────────────────────────────────

pub(crate) fn clear_screen(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    width: u32,
    height: u32,
) {
    // Clear to OS theme dark background.
    let mut cmdbuf: Box<virgl::CommandBuffer> = box_zeroed();
    cmdbuf.cmd_clear(0.13, 0.13, 0.16, 1.0);

    if !submit_3d(device, vq, irq_handle, &cmdbuf) {
        sys::print(b"virgil-render: clear SUBMIT_3D failed\n");
        sys::exit();
    }
    sys::print(b"     clear submitted\n");

    // Flush the render target to display.
    let mut cmd = DmaBuf::alloc(0);
    // SAFETY: cmd.va points to zeroed DMA page.
    unsafe {
        core::ptr::write(
            cmd.va as *mut ResourceFlush,
            ResourceFlush {
                hdr: ctrl_header(VIRTIO_GPU_CMD_RESOURCE_FLUSH),
                rect_x: 0,
                rect_y: 0,
                rect_width: width,
                rect_height: height,
                resource_id: RT_RESOURCE_ID,
                _padding: 0,
            },
        );
    }
    if !gpu_cmd_ok(
        device,
        vq,
        irq_handle,
        &cmd,
        core::mem::size_of::<ResourceFlush>() as u32,
    ) {
        sys::print(b"virgil-render: RESOURCE_FLUSH failed\n");
        sys::exit();
    }
    cmd.free();
    sys::print(b"     flush complete \xe2\x80\x94 pixels on screen\n");
}
