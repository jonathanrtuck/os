//! Virgil3D render service — thick GPU driver.
//!
//! Initializes virtio-gpu in 3D mode (VIRTIO_GPU_F_VIRGL), creates a virgl
//! rendering context, sets up the Gallium3D pipeline (blend, DSA, rasterizer,
//! shaders, surface), and clears the screen to a solid color.
//!
//! Replaces the 2D virtio-gpu driver as a drop-in. Init spawns this for the
//! GPU device. Participates in the same IPC handshake (MSG_DEVICE_CONFIG,
//! MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY).
//!
//! The scene graph is the only interface — all rendering complexity is
//! internal to this driver (leaf node behind a simple boundary).

#![no_std]
#![no_main]

extern crate alloc;
extern crate fonts;
extern crate render;
extern crate scene;

use alloc::boxed::Box;

use protocol::{
    compose::{CompositorConfig, MSG_COMPOSITOR_CONFIG},
    device::{DeviceConfig, MSG_DEVICE_CONFIG},
    virgl::{
        self, PIPE_BUFFER, PIPE_PRIM_TRIANGLES, PIPE_SHADER_FRAGMENT, PIPE_SHADER_VERTEX,
        PIPE_TEXTURE_2D, VIRGL_FORMAT_B8G8R8A8_UNORM, VIRGL_FORMAT_R8_UNORM,
        VIRGL_FORMAT_Z32_FLOAT_S8X24_UINT, VIRGL_OBJECT_BLEND, VIRGL_OBJECT_DSA,
        VIRGL_OBJECT_VERTEX_ELEMENTS,
    },
};
use render::incremental::{all_bits_zero, IncrementalState};

#[path = "atlas.rs"]
mod atlas;
#[path = "device.rs"]
mod device;
#[path = "frame_scheduler.rs"]
mod frame_scheduler;
#[path = "pipeline.rs"]
mod pipeline;
#[path = "resources.rs"]
mod resources;
#[path = "scene_walk.rs"]
mod scene_walk;
#[path = "shaders.rs"]
mod shaders;
#[path = "wire.rs"]
mod wire;

use pipeline::submit_3d;
use resources::{print_hex_u32, print_u32};
use wire::box_zeroed;

// ── Constants ────────────────────────────────────────────────────────────

/// Control virtqueue index.
pub(crate) const VIRTQ_CONTROL: u32 = 0;

/// VIRTIO_GPU_F_VIRGL feature bit (bit 0 of device features).
pub(crate) const VIRTIO_GPU_F_VIRGL: u64 = 1 << 0;

/// Resource IDs and context IDs (arbitrary nonzero).
pub(crate) const VIRGL_CTX_ID: u32 = 1;
pub(crate) const RT_RESOURCE_ID: u32 = 1;

/// Scanout index (first/only display).
pub(crate) const SCANOUT_ID: u32 = 0;

/// Handle indices for IPC channels.
pub(crate) const INIT_HANDLE: u8 = 0;
/// Handle for the core->virgil-render scene update channel.
pub(crate) const SCENE_HANDLE: u8 = 1;

/// Virgl object handles (assigned by us, must be nonzero).
pub(crate) const HANDLE_BLEND: u32 = 1;
pub(crate) const HANDLE_DSA: u32 = 2;
pub(crate) const HANDLE_RASTERIZER: u32 = 3;
pub(crate) const HANDLE_SURFACE: u32 = 4;
pub(crate) const HANDLE_VS: u32 = 5;
pub(crate) const HANDLE_FS: u32 = 6;
pub(crate) const HANDLE_VE: u32 = 7; // vertex elements layout (color)
/// Textured pipeline handles (for glyph rendering).
pub(crate) const HANDLE_VE_TEXTURED: u32 = 8;
pub(crate) const HANDLE_VS_TEXTURED: u32 = 9;
pub(crate) const HANDLE_FS_GLYPH: u32 = 10;
pub(crate) const HANDLE_SAMPLER: u32 = 11;
pub(crate) const HANDLE_SAMPLER_VIEW: u32 = 12;
/// Image pipeline handles.
pub(crate) const HANDLE_FS_IMAGE: u32 = 13;
pub(crate) const HANDLE_SAMPLER_VIEW_IMG: u32 = 14;
/// Stencil-then-cover pipeline handles.
pub(crate) const HANDLE_DSA_STENCIL_WRITE: u32 = 15;
pub(crate) const HANDLE_DSA_STENCIL_TEST: u32 = 16;
pub(crate) const HANDLE_BLEND_NO_COLOR: u32 = 17;
pub(crate) const HANDLE_STENCIL_SURFACE: u32 = 18;

/// Resource ID for the vertex buffer (PIPE_BUFFER).
pub(crate) const VB_RESOURCE_ID: u32 = 2;
/// Resource ID for the glyph atlas texture (R8_UNORM).
pub(crate) const ATLAS_RESOURCE_ID: u32 = 3;
/// Resource ID for the textured vertex buffer (PIPE_BUFFER).
pub(crate) const TEXT_VB_RESOURCE_ID: u32 = 4;
/// Resource ID for the image texture (B8G8R8A8_UNORM).
pub(crate) const IMG_RESOURCE_ID: u32 = 5;
/// Resource ID for the depth/stencil surface (Z32_FLOAT_S8X24_UINT).
pub(crate) const STENCIL_RESOURCE_ID: u32 = 6;

// ── Entry point ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render - starting\n");

    // ── Phase A: Receive device config from init, init virtio device ─────
    // SAFETY: Channel 0 shared memory is mapped by kernel before process start.
    let ch = unsafe { ipc::Channel::from_base(resources::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);
    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virgil-render: no device config message\n");
        sys::exit();
    }
    // SAFETY: msg payload contains a valid DeviceConfig from init.
    let dev_config: DeviceConfig = unsafe { msg.payload_as() };
    let (device, mut vq, irq_handle) = device::init_device(dev_config.mmio_pa, dev_config.irq);

    // ── Phase B: Display query + init handshake ──────────────────────────
    let (width, height) = device::init_handshake(&device, &mut vq, irq_handle, &ch);

    sys::print(b"     render target: ");
    print_u32(width);
    sys::print(b"x");
    print_u32(height);
    sys::print(b"\n");

    // ── Phase C: Virgl 3D initialization ─────────────────────────────────
    resources::ctx_create(&device, &mut vq, irq_handle);
    resources::resource_create_3d(&device, &mut vq, irq_handle, width, height);
    resources::attach_backing(&device, &mut vq, irq_handle, width, height);
    resources::ctx_attach_resource(&device, &mut vq, irq_handle);
    resources::set_scanout(&device, &mut vq, irq_handle, width, height);

    // Create color vertex buffer resource. Sized for backgrounds + path fan + path cover.
    let vbo_size = scene_walk::TOTAL_COLOR_VBO_BYTES as u32;
    resources::resource_create_vbo(&device, &mut vq, irq_handle, vbo_size);
    let (vbo_va, _vbo_pa, _vbo_order) =
        resources::attach_backing_vbo(&device, &mut vq, irq_handle, vbo_size);
    resources::ctx_attach_vbo(&device, &mut vq, irq_handle);

    // Create glyph atlas texture resource (R8_UNORM, 512x512).
    resources::resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        ATLAS_RESOURCE_ID,
        PIPE_TEXTURE_2D,
        VIRGL_FORMAT_R8_UNORM,
        virgl::PIPE_BIND_SAMPLER_VIEW,
        atlas::ATLAS_WIDTH,
        atlas::ATLAS_HEIGHT,
    );
    let (atlas_va, _atlas_pa, _atlas_order) = resources::attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        ATLAS_RESOURCE_ID,
        atlas::ATLAS_BYTES as u32,
    );
    sys::print(b"     glyph atlas texture created\n");

    // Create textured vertex buffer resource. Sized for image quads + glyphs.
    let text_vbo_size = scene_walk::TOTAL_TEXTURED_VBO_BYTES as u32;
    resources::resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        TEXT_VB_RESOURCE_ID,
        PIPE_BUFFER,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
        virgl::PIPE_BIND_VERTEX_BUFFER,
        text_vbo_size,
        1,
    );
    let (text_vbo_va, _text_vbo_pa, _text_vbo_order) = resources::attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        TEXT_VB_RESOURCE_ID,
        text_vbo_size,
    );
    sys::print(b"     textured VBO created\n");

    // Create depth/stencil surface resource (Z32_FLOAT, same size as render target).
    resources::resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        STENCIL_RESOURCE_ID,
        PIPE_TEXTURE_2D,
        VIRGL_FORMAT_Z32_FLOAT_S8X24_UINT, // depth32f + stencil8 (Apple Silicon; D24_S8 is Intel-only)
        virgl::VIRGL_BIND_DEPTH_STENCIL,
        width,
        height,
    );
    let (_stencil_va, _stencil_pa, _stencil_order) = resources::attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        STENCIL_RESOURCE_ID,
        width * height * 8, // Z32F_S8X24 = 8 bytes/pixel
    );
    sys::print(b"     stencil surface created\n");

    // Image texture will be created lazily on first image frame.
    // Pre-allocate a DMA buffer for the max image size we support (64x64 BGRA).
    let max_img_bytes: u32 = 64 * 64 * 4; // 16 KiB for a 64x64 BGRA image.
    resources::resource_create_3d_generic(
        &device,
        &mut vq,
        irq_handle,
        IMG_RESOURCE_ID,
        PIPE_TEXTURE_2D,
        VIRGL_FORMAT_B8G8R8A8_UNORM,
        virgl::PIPE_BIND_SAMPLER_VIEW,
        64, // Max supported width — matches DMA backing size.
        64, // Max supported height.
    );
    let (img_dma_va, _img_dma_pa, _img_dma_order) = resources::attach_and_ctx_resource(
        &device,
        &mut vq,
        irq_handle,
        IMG_RESOURCE_ID,
        max_img_bytes,
    );
    sys::print(b"     image texture created (64x64)\n");

    // ── Phase D: GPU pipeline setup ──────────────────────────────────────
    let stencil_available = pipeline::setup_pipeline(&device, &mut vq, irq_handle, width, height);

    // ── Phase E: Clear screen + flush ────────────────────────────────────
    pipeline::clear_screen(&device, &mut vq, irq_handle, width, height);

    // ── Phase F: Receive render config, init glyph atlas, render loop ────
    sys::print(b"     waiting for render config\n");

    let mut scene_va: u64 = 0;
    let mut font_va: u64 = 0;
    let mut font_len: u32 = 0;
    let mut scale_factor: f32 = 1.0;
    let mut font_size_cfg: u16 = 18;
    let mut frame_rate_cfg: u32 = 60;

    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_COMPOSITOR_CONFIG {
            // SAFETY: msg payload is a valid CompositorConfig from init.
            let config: CompositorConfig = unsafe { msg.payload_as() };
            scene_va = config.scene_va;
            font_va = config.mono_font_va;
            font_len = config.mono_font_len;
            scale_factor = config.scale_factor;
            font_size_cfg = config.font_size;
            frame_rate_cfg = if config.frame_rate > 0 {
                config.frame_rate as u32
            } else {
                60
            };

            sys::print(b"     render config: scene_va=");
            print_hex_u32((scene_va >> 32) as u32);
            print_hex_u32(scene_va as u32);
            sys::print(b" font_len=");
            print_u32(font_len);
            sys::print(b" scale=");
            print_u32((scale_factor * 100.0) as u32);
            sys::print(b"%\n");
            break;
        }
    }

    if scene_va == 0 {
        sys::print(b"virgil-render: no scene_va in config, idling\n");
        loop {
            let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        }
    }

    // ── Glyph atlas initialization ───────────────────────────────────────
    //
    // Rasterize ASCII glyphs on the CPU via GlyphCache, pack into atlas
    // DMA backing memory, then transfer to GPU texture.
    // Heap-allocate atlas (~24 KiB) directly — cannot use Box::new() because
    // the struct exceeds the 16 KiB stack. alloc_zeroed produces valid initial
    // state (all entries empty, cursors at 0), then we set dma_va.
    let mut glyph_atlas: Box<atlas::GlyphAtlas> = box_zeroed();
    glyph_atlas.set_dma_va(atlas_va);
    let mut font_ascent: u32 = 14;

    if font_va != 0 && font_len > 0 {
        sys::print(b"     initializing glyph atlas via HarfBuzz shaping\n");

        // SAFETY: font_va is mapped read-only into our address space by init.
        let font_data =
            unsafe { core::slice::from_raw_parts(font_va as *const u8, font_len as usize) };

        // Font size from config (logical pixels). The scene graph x_advance/x_offset
        // are in logical pixels at this size. Rasterize at the LOGICAL size —
        // the scene_walk applies * scale for NDC.
        let font_size_px: u32 = font_size_cfg as u32;

        // Axes must match core's shaping axes (MONO=1.0).
        let mono_axes = [fonts::rasterize::AxisValue {
            tag: *b"MONO",
            value: 1.0,
        }];

        // Get font metrics for ascent.
        // scale_fu_ceil(val, size, upem) = (val * size + upem - 1) / upem
        if let Some(metrics) = fonts::rasterize::font_metrics(font_data) {
            let upem = metrics.units_per_em as i32;
            let asc = metrics.ascent as i32;
            let size = font_size_px as i32;
            font_ascent = ((asc * size + upem - 1) / upem) as u32;

            sys::print(b"     font ascent=");
            print_u32(font_ascent);
            sys::print(b" size=");
            print_u32(font_size_px);
            sys::print(b"\n");
        }

        // Shape all printable ASCII through HarfBuzz to get real glyph IDs
        // (including GSUB substitutions like Recursive's MONO alternates).
        // Then rasterize each unique glyph ID directly into the atlas.
        let ascii: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
        let shaped = fonts::shape_with_variations(font_data, ascii, &[], &mono_axes);

        // Heap-allocate rasterization scratch space (~39 KiB).
        let mut scratch: Box<fonts::rasterize::RasterScratch> = box_zeroed();

        // Raster buffer for individual glyph rasterization (50x50 max).
        let mut raster_buf = [0u8; 50 * 50];

        let mut packed = 0u32;
        for sg in &shaped {
            if glyph_atlas.lookup(sg.glyph_id).is_some() {
                continue; // Already packed.
            }
            let mut rb = fonts::rasterize::RasterBuffer {
                data: &mut raster_buf,
                width: 50,
                height: 50,
            };
            if let Some(m) = fonts::rasterize::rasterize_with_axes(
                font_data,
                sg.glyph_id,
                font_size_px as u16,
                &mut rb,
                &mut scratch,
                &mono_axes,
            ) {
                if m.width > 0 && m.height > 0 {
                    let coverage = &raster_buf[..(m.width * m.height) as usize];
                    glyph_atlas.pack_glyph(
                        sg.glyph_id,
                        m.width,
                        m.height,
                        m.bearing_x,
                        m.bearing_y,
                        coverage,
                    );
                    packed += 1;
                }
            }
        }

        sys::print(b"     atlas packed ");
        print_u32(packed);
        sys::print(b" glyphs (");
        print_u32(shaped.len() as u32);
        sys::print(b" shaped)\n");

        // Transfer atlas texture to GPU.
        resources::transfer_texture_to_host(
            &device,
            &mut vq,
            irq_handle,
            ATLAS_RESOURCE_ID,
            atlas::ATLAS_WIDTH,
            atlas::ATLAS_HEIGHT,
            atlas::ATLAS_WIDTH, // stride = width for R8
        );
        sys::print(b"     glyph atlas uploaded to GPU\n");
    } else {
        sys::print(b"     no font data, text rendering disabled\n");
    }

    // Scene graph shared memory.
    let scene_buf = unsafe {
        // SAFETY: scene_va is mapped into our address space by init via
        // memory_share before process start. Size is TRIPLE_SCENE_SIZE.
        core::slice::from_raw_parts(scene_va as *const u8, scene::TRIPLE_SCENE_SIZE)
    };

    // Heap-allocate batches and command buffer directly (all are zero-valid).
    // Cannot use Box::new() — TexturedBatch (~96 KiB) exceeds 16 KiB stack.
    let mut batch: Box<scene_walk::QuadBatch> = box_zeroed();
    let mut text_batch: Box<scene_walk::TexturedBatch> = box_zeroed();
    let mut image_batch: Box<scene_walk::ImageBatch> = box_zeroed();
    let mut path_batch: Box<scene_walk::PathBatch> = box_zeroed();
    let mut cmdbuf: Box<virgl::CommandBuffer> = box_zeroed();

    // Heap-allocated because IncrementalState is ~12 KiB (too large for
    // the 16 KiB user stack). Persists across frames for dirty bitmap
    // tracking and skip-frame detection.
    let mut incr_state = Box::new(IncrementalState::new());

    let mut frame_count: u32 = 0;
    let mut sched = frame_scheduler::FrameScheduler::new(frame_rate_cfg);
    let cfreq = sys::counter_freq();

    let counter_to_ns = |ticks: u64, freq: u64| -> u64 {
        if freq == 0 {
            return 0;
        }
        (ticks / freq) * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
    };

    let mut timer_h: u8 = sys::timer_create(sched.period_ns()).unwrap_or_else(|_| {
        sys::print(b"virgil-render: frame timer create failed\n");
        sys::exit();
    });

    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render: render loop starting\n");

    // SAFETY: Channel 1 shared memory was set up by init before start.
    let scene_ch =
        unsafe { ipc::Channel::from_base(resources::channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    loop {
        let _ = sys::wait(&[SCENE_HANDLE, timer_h], u64::MAX);
        let mut go = false;

        // Check for scene updates.
        if sys::wait(&[SCENE_HANDLE], 0).is_ok() {
            let mut drain_msg = ipc::Message::new(0);
            while scene_ch.try_recv(&mut drain_msg) {}
            go = sched.should_render_immediately(counter_to_ns(sys::counter(), cfreq));
            sched.on_scene_update();
        }
        // Check for timer tick.
        if sys::wait(&[timer_h], 0).is_ok() {
            let _ = sys::handle_close(timer_h);
            timer_h = sys::timer_create(sched.period_ns()).unwrap_or_else(|_| {
                sys::print(b"virgil-render: timer recreate failed\n");
                sys::exit();
            });
            go |= sched.on_timer_tick_at(counter_to_ns(sys::counter(), cfreq));
        }
        if !go {
            continue;
        }

        // Read the latest scene graph and dirty bitmap.
        let reader = scene::TripleReader::new(scene_buf);
        let nodes = reader.front_nodes();
        let node_count = nodes.len() as u16;
        let gen = reader.front_generation();
        let dirty_bits = *reader.dirty_bits();

        // Skip-frame: nothing changed since last render.
        if all_bits_zero(&dirty_bits) {
            reader.finish_read(gen);
            sched.on_render_complete_at(counter_to_ns(sys::counter(), cfreq));
            continue;
        }

        // Walk scene tree: accumulate colored quads (backgrounds) and
        // textured quads (glyphs) in a single pass.
        let root = reader.front_root();
        let data_buf = reader.front_data_buf();
        scene_walk::walk_scene(
            nodes,
            root,
            scale_factor,
            width,
            height,
            &mut batch,
            &mut text_batch,
            &mut image_batch,
            &mut path_batch,
            data_buf,
            &glyph_atlas,
            font_ascent,
        );

        if frame_count < 3 {
            sys::print(b"     frame ");
            print_u32(frame_count);
            sys::print(b": bg=");
            print_u32(batch.vertex_count);
            sys::print(b" img=");
            print_u32(image_batch.count as u32);
            sys::print(b" path=");
            print_u32(path_batch.fan_vertex_count);
            sys::print(b" text=");
            print_u32(text_batch.vertex_count);
            sys::print(b"\n");
        }

        if batch.dropped_count() > 0
            || text_batch.dropped_count() > 0
            || path_batch.dropped_count() > 0
        {
            sys::print(b"WARN: vertices dropped\n");
        }

        // ── Color VBO: pack backgrounds + path fan + path cover at offsets ─
        let color_data = batch.as_vertex_data();
        let color_dwords = color_data.len();
        let color_bytes = color_dwords * 4;

        let has_paths = path_batch.fan_vertex_count > 0 && stencil_available;
        let fan_data = path_batch.as_fan_data();
        let fan_dwords = fan_data.len();
        let fan_bytes = fan_dwords * 4;
        let cover_data = path_batch.as_cover_data();
        let cover_dwords = cover_data.len();
        let cover_bytes = cover_dwords * 4;

        let fan_vbo_offset = color_bytes;
        let cover_vbo_offset = fan_vbo_offset + fan_bytes;
        let total_color_bytes = cover_vbo_offset + cover_bytes;

        // Upload all color vertex data in one transfer.
        if total_color_bytes > 0 {
            if color_bytes > 0 {
                // SAFETY: vbo_va is valid DMA of TOTAL_COLOR_VBO_BYTES size.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        color_data.as_ptr(),
                        vbo_va as *mut u32,
                        color_dwords,
                    );
                }
            }
            if has_paths && fan_bytes > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        fan_data.as_ptr(),
                        (vbo_va + fan_vbo_offset) as *mut u32,
                        fan_dwords,
                    );
                }
            }
            if has_paths && cover_bytes > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        cover_data.as_ptr(),
                        (vbo_va + cover_vbo_offset) as *mut u32,
                        cover_dwords,
                    );
                }
            }
            resources::transfer_vbo_to_host(&device, &mut vq, irq_handle, total_color_bytes as u32);
        }

        // Build GPU commands (single cmdbuf for entire frame).
        // Re-set framebuffer state each frame so the render loop is
        // self-contained — doesn't depend on GPU state from prior submits
        // (e.g. the image loop's mid-frame submit/clear cycle).
        cmdbuf.clear();
        let zsurf = if stencil_available {
            HANDLE_STENCIL_SURFACE
        } else {
            0
        };
        cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, zsurf);
        cmdbuf.cmd_set_viewport(width as f32, height as f32);
        cmdbuf.cmd_clear(0.13, 0.13, 0.16, 1.0);
        if has_paths {
            cmdbuf.cmd_clear_stencil();
        }

        // Draw backgrounds (color pipeline, VBO offset 0).
        if batch.vertex_count > 0 {
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE);
            cmdbuf.cmd_bind_shader(HANDLE_VS, PIPE_SHADER_VERTEX);
            cmdbuf.cmd_bind_shader(HANDLE_FS, PIPE_SHADER_FRAGMENT);
            cmdbuf.cmd_set_vertex_buffers(scene_walk::VERTEX_STRIDE, 0, VB_RESOURCE_ID);
            cmdbuf.cmd_draw_vbo(0, batch.vertex_count, PIPE_PRIM_TRIANGLES, false);
        }

        // Stencil-then-cover paths (VBO offsets for fan + cover).
        if has_paths {
            // Pass A: stencil write (fan triangles, no color).
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND_NO_COLOR);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA_STENCIL_WRITE);
            cmdbuf.cmd_set_vertex_buffers(
                scene_walk::VERTEX_STRIDE,
                fan_vbo_offset as u32,
                VB_RESOURCE_ID,
            );
            cmdbuf.cmd_set_stencil_ref(0, 0);
            cmdbuf.cmd_draw_vbo(0, path_batch.fan_vertex_count, PIPE_PRIM_TRIANGLES, false);

            // Pass B: stencil test + cover (colored quads where stencil != 0).
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_BLEND, HANDLE_BLEND);
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA_STENCIL_TEST);
            cmdbuf.cmd_set_vertex_buffers(
                scene_walk::VERTEX_STRIDE,
                cover_vbo_offset as u32,
                VB_RESOURCE_ID,
            );
            cmdbuf.cmd_draw_vbo(0, path_batch.cover_vertex_count, PIPE_PRIM_TRIANGLES, false);

            // Restore normal DSA.
            cmdbuf.cmd_bind_object(VIRGL_OBJECT_DSA, HANDLE_DSA);
        }

        // ── Pass 3: Upload + draw images (TEXTURED_FS) ──────────────────
        // Each image shares a single GPU texture resource, so we must
        // upload, transfer, and draw each image individually.  Vertices
        // are written sequentially into the text VBO (image 0 at offset
        // 0, image 1 at offset 192, etc.) and each image is drawn
        // immediately after its texture transfer.
        let mut images_drawn: usize = 0;
        {
            let vw = width as f32;
            let vh = height as f32;
            let white = 1.0f32.to_bits();

            for idx in 0..image_batch.count {
                let img = match image_batch.get(idx) {
                    Some(i) => i,
                    None => break,
                };
                let img_pixels = img.src_width as u32 * img.src_height as u32 * 4;
                let src_offset = img.data_offset as usize;
                let src_end = src_offset + img_pixels as usize;

                if src_end > data_buf.len() || img_pixels > max_img_bytes {
                    continue;
                }

                // Copy image pixel data to DMA backing.
                // SAFETY: img_dma_va is valid DMA of max_img_bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        data_buf[src_offset..src_end].as_ptr(),
                        img_dma_va as *mut u8,
                        img_pixels as usize,
                    );
                }
                // Transfer image texture to GPU.
                resources::transfer_texture_to_host(
                    &device,
                    &mut vq,
                    irq_handle,
                    IMG_RESOURCE_ID,
                    img.src_width as u32,
                    img.src_height as u32,
                    img.src_width as u32 * 4, // BGRA stride
                );

                // Build textured quad vertices for the image.
                let x0 = img.x / vw * 2.0 - 1.0;
                let y0 = 1.0 - img.y / vh * 2.0;
                let x1 = (img.x + img.w) / vw * 2.0 - 1.0;
                let y1 = 1.0 - (img.y + img.h) / vh * 2.0;

                // 6 vertices x 8 floats = 48 dwords.
                let dwords = scene_walk::DWORDS_PER_IMAGE_QUAD;
                let mut img_verts = [0u32; 48];
                // pos(x,y) + texcoord(u,v) + color(r,g,b,a)
                let verts: [(f32, f32, f32, f32); 6] = [
                    (x0, y0, 0.0, 0.0), // top-left
                    (x0, y1, 0.0, 1.0), // bottom-left
                    (x1, y0, 1.0, 0.0), // top-right
                    (x1, y0, 1.0, 0.0), // top-right
                    (x0, y1, 0.0, 1.0), // bottom-left
                    (x1, y1, 1.0, 1.0), // bottom-right
                ];
                for (i, &(px, py, u, v)) in verts.iter().enumerate() {
                    let base = i * 8;
                    img_verts[base] = px.to_bits();
                    img_verts[base + 1] = py.to_bits();
                    img_verts[base + 2] = u.to_bits();
                    img_verts[base + 3] = v.to_bits();
                    img_verts[base + 4] = white; // r
                    img_verts[base + 5] = white; // g
                    img_verts[base + 6] = white; // b
                    img_verts[base + 7] = white; // a
                }

                // Write this image's vertices at its slot in the text VBO.
                let vbo_dword_offset = images_drawn * dwords;
                // SAFETY: text_vbo_va is valid DMA of TOTAL_TEXTURED_VBO_BYTES;
                // vbo_dword_offset is bounded by MAX_IMAGES * dwords.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        img_verts.as_ptr(),
                        (text_vbo_va as *mut u32).add(vbo_dword_offset),
                        dwords,
                    );
                }

                // Upload this image's vertices to the GPU immediately.
                let vbo_byte_offset = (vbo_dword_offset * 4) as u32;
                let vbo_byte_len = (dwords * 4) as u32;
                resources::transfer_buffer_to_host(
                    &device,
                    &mut vq,
                    irq_handle,
                    TEXT_VB_RESOURCE_ID,
                    vbo_byte_offset + vbo_byte_len,
                );

                // Draw this image's quad immediately (texture will be
                // overwritten by the next image's upload).
                cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE_TEXTURED);
                cmdbuf.cmd_bind_shader(HANDLE_VS_TEXTURED, PIPE_SHADER_VERTEX);
                cmdbuf.cmd_bind_shader(HANDLE_FS_IMAGE, PIPE_SHADER_FRAGMENT);
                cmdbuf.cmd_set_vertex_buffers(
                    scene_walk::TEXTURED_VERTEX_STRIDE,
                    vbo_byte_offset,
                    TEXT_VB_RESOURCE_ID,
                );
                cmdbuf.cmd_bind_sampler_states(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER);
                cmdbuf.cmd_set_sampler_views(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER_VIEW_IMG);
                cmdbuf.cmd_draw_vbo(0, 6, PIPE_PRIM_TRIANGLES, false);

                // Submit + flush between images so the GPU consumes the
                // texture before we overwrite it with the next image.
                if !cmdbuf.overflowed() {
                    submit_3d(&device, &mut vq, irq_handle, &cmdbuf);
                }
                cmdbuf.clear();
                let zsurf = if stencil_available {
                    HANDLE_STENCIL_SURFACE
                } else {
                    0
                };
                cmdbuf.cmd_set_framebuffer_state(HANDLE_SURFACE, zsurf);
                cmdbuf.cmd_set_viewport(width as f32, height as f32);

                images_drawn += 1;
            }
        }

        // ── Pass 4: Upload glyph vertices to text VBO and draw.
        //
        // Layout: [image vertices (MAX_IMAGES * 192 bytes)] [glyph vertices]
        // Glyph draw uses VBO offset after all image data.
        let text_data = text_batch.as_vertex_data();
        let text_dwords = text_data.len();
        let text_bytes = text_dwords * 4;

        // Reserve space for MAX_IMAGES image quads so glyph offset is stable.
        let img_vbo_bytes: usize = scene_walk::MAX_IMAGES * scene_walk::DWORDS_PER_IMAGE_QUAD * 4;
        let glyph_vbo_offset = img_vbo_bytes; // glyphs start after all image slots

        if text_bytes > 0 {
            // Copy glyph data after image region in DMA buffer.
            // SAFETY: text_vbo_va is valid DMA of TOTAL_TEXTURED_VBO_BYTES.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    text_data.as_ptr(),
                    (text_vbo_va + img_vbo_bytes) as *mut u32,
                    text_dwords,
                );
            }

            let total_upload = img_vbo_bytes + text_bytes;
            resources::transfer_buffer_to_host(
                &device,
                &mut vq,
                irq_handle,
                TEXT_VB_RESOURCE_ID,
                total_upload as u32,
            );

            if text_batch.vertex_count > 0 {
                cmdbuf.cmd_bind_object(VIRGL_OBJECT_VERTEX_ELEMENTS, HANDLE_VE_TEXTURED);
                cmdbuf.cmd_bind_shader(HANDLE_VS_TEXTURED, PIPE_SHADER_VERTEX);
                cmdbuf.cmd_bind_shader(HANDLE_FS_GLYPH, PIPE_SHADER_FRAGMENT);
                cmdbuf.cmd_set_vertex_buffers(
                    scene_walk::TEXTURED_VERTEX_STRIDE,
                    glyph_vbo_offset as u32, // glyphs start after image data
                    TEXT_VB_RESOURCE_ID,
                );
                cmdbuf.cmd_bind_sampler_states(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER);
                cmdbuf.cmd_set_sampler_views(PIPE_SHADER_FRAGMENT, HANDLE_SAMPLER_VIEW);
                cmdbuf.cmd_draw_vbo(0, text_batch.vertex_count, PIPE_PRIM_TRIANGLES, false);
            }
        }

        if cmdbuf.overflowed() {
            sys::print(b"virgil-render: command buffer overflow!\n");
        } else {
            submit_3d(&device, &mut vq, irq_handle, &cmdbuf);
            resources::flush_resource(&device, &mut vq, irq_handle, width, height);
        }

        // Update incremental state before releasing the reader.
        incr_state.update_from_frame(nodes, node_count);
        reader.finish_read(gen);
        sched.on_render_complete_at(counter_to_ns(sys::counter(), cfreq));
        frame_count = frame_count.wrapping_add(1);
    }
}
