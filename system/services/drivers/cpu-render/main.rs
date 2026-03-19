//! CPU render service — software-rendered display pipeline.
//!
//! Combines GPU device management (virtio-gpu 2D protocol) with CPU scene
//! graph rendering (CpuBackend). Sibling to virgil-render; init selects
//! based on GPU capabilities at boot.
//!
//! Replaces the previous two-process pipeline (compositor + virtio-gpu
//! driver) with a single process that handles both CPU rendering and GPU
//! presentation. Self-allocates framebuffers via `dma_alloc`.
//!
//! # Handle layout (matches virgil-render)
//!
//! - Handle 0: init config channel
//! - Handle 1: core → cpu-render scene update channel
//! - Handle 2: IRQ handle (from interrupt_register)
//!
//! # Handshake (identical to virgil-render)
//!
//! 1. Receive MSG_DEVICE_CONFIG → init GPU device
//! 2. Query display → send MSG_DISPLAY_INFO to init
//! 3. Receive MSG_GPU_CONFIG → get fb dimensions
//! 4. Self-allocate double framebuffer, set up GPU resources
//! 5. Send MSG_GPU_READY
//! 6. Receive MSG_COMPOSITOR_CONFIG → scene_va, font data, scale
//! 7. Init CpuBackend, enter render loop

#![no_std]
#![no_main]

extern crate alloc;
extern crate render;
extern crate scene;

#[path = "gpu.rs"]
mod gpu;

#[path = "frame_scheduler.rs"]
mod frame_scheduler;

use protocol::{
    compose::{CompositorConfig, MSG_COMPOSITOR_CONFIG},
    device::{DeviceConfig, MSG_DEVICE_CONFIG},
    gpu::{DisplayInfoMsg, GpuConfig, MSG_DISPLAY_INFO, MSG_GPU_CONFIG, MSG_GPU_READY},
};
use render::{
    incremental::{all_bits_zero, IncrementalState},
    RenderBackend,
};

// ── Constants ────────────────────────────────────────────────────────────

/// Handle indices for IPC channels.
const INIT_HANDLE: u8 = 0;
const CORE_HANDLE: u8 = 1;

/// Framebuffer chunk allocation order (256 KiB per chunk = 64 pages).
const CHUNK_ORDER: u32 = 6;
const CHUNK_PAGES: usize = 1 << CHUNK_ORDER;
const CHUNK_BYTES: usize = CHUNK_PAGES * 4096;

/// Maximum chunks per buffer (covers up to 8K resolution).
const MAX_CHUNKS: usize = 512;

/// Wrapper for Sync on statics containing UnsafeCell.
struct SyncCell<T>(core::cell::UnsafeCell<T>);
// SAFETY: Single-threaded userspace process.
unsafe impl<T> Sync for SyncCell<T> {}

#[inline]
fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
    if freq == 0 {
        return 0;
    }
    (ticks / freq) * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
}

fn clamp_scale(raw: f32) -> f32 {
    if raw <= 0.0 || raw.is_nan() {
        1.0
    } else if raw > 4.0 {
        4.0
    } else {
        raw
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x96\xA5\xEF\xB8\x8F  cpu-render - starting\n");

    // ── Phase A: Receive device config, init GPU hardware ────────────
    // SAFETY: Channel 0 shared memory is mapped by kernel before process start.
    let ch = unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);
    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"cpu-render: no device config message\n");
        sys::exit();
    }
    // SAFETY: msg payload contains a valid DeviceConfig from init.
    let dev_config: DeviceConfig = unsafe { msg.payload_as() };
    let (device, mut vq, irq_handle) = gpu::init_device(dev_config.mmio_pa, dev_config.irq);
    sys::print(b"     gpu device initialized\n");

    // ── Phase B: Display query + init handshake ──────────────────────
    let (disp_w, disp_h) = gpu::get_display_info(&device, &mut vq, irq_handle);
    let width = if disp_w > 0 { disp_w } else { 1024 };
    let height = if disp_h > 0 { disp_h } else { 768 };

    {
        let mut buf = [0u8; 32];
        let prefix = b"     display ";
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        pos += sys::format_u32(width, &mut buf[pos..]);
        buf[pos] = b'x';
        pos += 1;
        pos += sys::format_u32(height, &mut buf[pos..]);
        buf[pos] = b'\n';
        pos += 1;
        sys::print(&buf[..pos]);
    }

    // Send display info back to init.
    let info_msg = unsafe {
        // SAFETY: DisplayInfoMsg is repr(C) and fits in payload.
        ipc::Message::from_payload(MSG_DISPLAY_INFO, &DisplayInfoMsg { width, height })
    };
    ch.send(&info_msg);
    let _ = sys::channel_signal(INIT_HANDLE);

    // Wait for GPU config from init.
    sys::print(b"     waiting for gpu config\n");
    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_GPU_CONFIG {
            break;
        }
    }
    // SAFETY: msg payload contains a valid GpuConfig from init.
    let gpu_config: GpuConfig = unsafe { msg.payload_as() };
    let fb_width = gpu_config.fb_width;
    let fb_height = gpu_config.fb_height;
    let stride = fb_width * gpu::FB_BPP;
    let fb_size = stride * fb_height;

    // ── Phase C: Self-allocate double framebuffer ────────────────────
    let fb_bytes = fb_size as usize;
    let chunks_per_buf = (fb_bytes + CHUNK_BYTES - 1) / CHUNK_BYTES;
    let total_entries = chunks_per_buf * 2;

    // PA table for scatter-gather attach_backing. Static to avoid blowing
    // the 16 KiB stack (1024 × 8 = 8 KiB).
    static PA_TABLE: SyncCell<[u64; MAX_CHUNKS * 2]> =
        SyncCell(core::cell::UnsafeCell::new([0u64; MAX_CHUNKS * 2]));
    // SAFETY: Single-threaded process, no concurrent access to PA_TABLE.
    let pa_table = unsafe { &mut *PA_TABLE.0.get() };

    // Front buffer (buffer 0).
    let mut fb_va0: usize = 0;
    for i in 0..chunks_per_buf {
        let mut chunk_pa: u64 = 0;
        let chunk_va = sys::dma_alloc(CHUNK_ORDER, &mut chunk_pa).unwrap_or_else(|_| {
            sys::print(b"cpu-render: dma_alloc (fb0 chunk) failed\n");
            sys::exit();
        });
        if i == 0 {
            fb_va0 = chunk_va;
        }
        pa_table[i] = chunk_pa;
        // SAFETY: chunk_va is a valid DMA allocation of CHUNK_BYTES.
        unsafe { core::ptr::write_bytes(chunk_va as *mut u8, 0, CHUNK_BYTES) };
    }

    // Back buffer (buffer 1).
    let mut fb_va1: usize = 0;
    for i in 0..chunks_per_buf {
        let mut chunk_pa: u64 = 0;
        let chunk_va = sys::dma_alloc(CHUNK_ORDER, &mut chunk_pa).unwrap_or_else(|_| {
            sys::print(b"cpu-render: dma_alloc (fb1 chunk) failed\n");
            sys::exit();
        });
        if i == 0 {
            fb_va1 = chunk_va;
        }
        pa_table[chunks_per_buf + i] = chunk_pa;
        // SAFETY: chunk_va is a valid DMA allocation of CHUNK_BYTES.
        unsafe { core::ptr::write_bytes(chunk_va as *mut u8, 0, CHUNK_BYTES) };
    }

    {
        let mut buf = [0u8; 60];
        let prefix = b"     framebuffer: ";
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        pos += sys::format_u32(fb_width, &mut buf[pos..]);
        buf[pos] = b'x';
        pos += 1;
        pos += sys::format_u32(fb_height, &mut buf[pos..]);
        let mid = b" x2 (";
        buf[pos..pos + mid.len()].copy_from_slice(mid);
        pos += mid.len();
        pos += sys::format_u32(
            (chunks_per_buf * CHUNK_BYTES * 2) as u32 / 1024,
            &mut buf[pos..],
        );
        let suffix = b" KiB)\n";
        buf[pos..pos + suffix.len()].copy_from_slice(suffix);
        pos += suffix.len();
        sys::print(&buf[..pos]);
    }

    // ── Phase D: GPU resource setup ──────────────────────────────────
    if !gpu::resource_create_2d(
        &device,
        &mut vq,
        irq_handle,
        gpu::FB_RESOURCE_ID,
        width,
        height,
    ) {
        sys::print(b"cpu-render: resource_create_2d failed\n");
        sys::exit();
    }
    if !gpu::attach_backing_sg(
        &device,
        &mut vq,
        irq_handle,
        gpu::FB_RESOURCE_ID,
        &pa_table[..total_entries],
        CHUNK_BYTES as u32,
    ) {
        sys::print(b"cpu-render: attach_backing failed\n");
        sys::exit();
    }
    if !gpu::set_scanout(
        &device,
        &mut vq,
        irq_handle,
        gpu::SCANOUT_ID,
        gpu::FB_RESOURCE_ID,
        width,
        height,
    ) {
        sys::print(b"cpu-render: set_scanout failed\n");
        sys::exit();
    }

    // Pre-allocate a DMA page for the present loop commands. Reusing one
    // page eliminates 4 syscalls (2 alloc + 2 free) per frame.
    let present_cmd = gpu::DmaBuf::alloc(0);

    sys::print(b"     device setup complete\n");

    // Signal init that device setup is complete.
    let ready_msg = ipc::Message::new(MSG_GPU_READY);
    ch.send(&ready_msg);
    let _ = sys::channel_signal(INIT_HANDLE);

    // ── Phase E: Receive render config ───────────────────────────────
    sys::print(b"     waiting for render config\n");
    loop {
        let _ = sys::wait(&[INIT_HANDLE], u64::MAX);
        if ch.try_recv(&mut msg) && msg.msg_type == MSG_COMPOSITOR_CONFIG {
            break;
        }
    }
    // SAFETY: msg payload contains a valid CompositorConfig from init.
    let config: CompositorConfig = unsafe { msg.payload_as() };
    let scene_va = config.scene_va as usize;
    let scale = clamp_scale(config.scale_factor);
    if scene_va == 0 {
        sys::print(b"cpu-render: bad render config\n");
        sys::exit();
    }

    // ── Phase F: Init render backend ─────────────────────────────────
    if config.mono_font_va == 0 || config.mono_font_len == 0 {
        sys::print(b"cpu-render: no font data\n");
        sys::exit();
    }
    // SAFETY: init mapped these pages before starting us.
    let mono = unsafe {
        core::slice::from_raw_parts(
            config.mono_font_va as *const u8,
            config.mono_font_len as usize,
        )
    };
    let prop = if config.prop_font_len > 0 {
        let off = config.mono_font_va as usize + config.mono_font_len as usize;
        Some(unsafe {
            core::slice::from_raw_parts(off as *const u8, config.prop_font_len as usize)
        })
    } else {
        None
    };
    let font_size = config.font_size as u32;
    let screen_dpi = config.screen_dpi;
    let frame_rate = if config.frame_rate > 0 {
        config.frame_rate as u32
    } else {
        60
    };
    let mut backend = match render::CpuBackend::new(
        mono,
        prop,
        font_size,
        screen_dpi,
        scale,
        fb_width as u16,
        fb_height as u16,
    ) {
        Some(b) => b,
        None => {
            sys::print(b"cpu-render: render backend init failed\n");
            sys::exit();
        }
    };

    // ── Phase G: Framebuffer surfaces + scene graph ──────────────────
    static FB_PTRS: SyncCell<[*mut u8; 2]> =
        SyncCell(core::cell::UnsafeCell::new([core::ptr::null_mut(); 2]));
    // SAFETY: Single-threaded process. FB_PTRS written once, read in make_fb.
    unsafe {
        (*FB_PTRS.0.get())[0] = fb_va0 as *mut u8;
        (*FB_PTRS.0.get())[1] = fb_va1 as *mut u8;
    }
    let make_fb = |idx: usize| -> drawing::Surface<'static> {
        // SAFETY: Single-threaded process. FB_PTRS written once above.
        let ptr = unsafe { (*FB_PTRS.0.get())[idx] };
        // SAFETY: The kernel's DMA VA bump allocator returns sequential
        // virtual addresses for consecutive dma_alloc calls. Multiple
        // CHUNK_BYTES allocations form a contiguous VA range. The slice
        // covers fb_size bytes starting at fb_va0 (or fb_va1).
        let data = unsafe { core::slice::from_raw_parts_mut(ptr, fb_size as usize) };
        drawing::Surface {
            data,
            width: fb_width,
            height: fb_height,
            stride: fb_width * 4,
            format: drawing::PixelFormat::Bgra8888,
        }
    };
    // SAFETY: scene_va is mapped into our address space by init via memory_share.
    let scene_buf =
        unsafe { core::slice::from_raw_parts(scene_va as *const u8, scene::TRIPLE_SCENE_SIZE) };

    // Byte stride of one buffer in the GPU backing memory.
    let buf_stride = (chunks_per_buf as u64) * (CHUNK_BYTES as u64);

    // ── Incremental rendering state ─────────────────────────────────
    // Heap-allocated because IncrementalState is ~12 KiB (too large for
    // the 16 KiB user stack). Persists across frames for dirty rect
    // computation and skip-frame detection.
    let mut incr_state = alloc::boxed::Box::new(IncrementalState::new());
    let fb_w16 = fb_width as u16;
    let fb_h16 = fb_height as u16;

    // ── Phase H: First frame ─────────────────────────────────────────
    sys::print(b"     waiting for first scene\n");
    let _ = sys::wait(&[CORE_HANDLE], u64::MAX);
    // SAFETY: Channel 1 shared memory was set up by init before start.
    let core_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    while core_ch.try_recv(&mut msg) {}
    {
        let tr = scene::TripleReader::new(scene_buf);
        let (gen, nodes) = (tr.front_generation(), tr.front_nodes());
        let graph = render::scene_render::SceneGraph {
            nodes,
            data: tr.front_data_buf(),
        };
        backend.render(&graph, &mut make_fb(0));
        // Update incremental state from first frame.
        incr_state.update_from_frame(nodes, nodes.len() as u16);
        tr.finish_read(gen);
    }
    // Present first frame: full-screen transfer + flush.
    gpu::transfer_to_host_reuse(
        &device,
        &mut vq,
        irq_handle,
        &present_cmd,
        gpu::FB_RESOURCE_ID,
        0,
        0,
        width,
        height,
        0, // buffer 0 at offset 0
        stride,
    );
    gpu::resource_flush_reuse(
        &device,
        &mut vq,
        irq_handle,
        &present_cmd,
        gpu::FB_RESOURCE_ID,
        0,
        0,
        width,
        height,
    );

    // ── Phase I: Render loop ─────────────────────────────────────────
    let mut sched = frame_scheduler::FrameScheduler::new(frame_rate);
    let cfreq = sys::counter_freq();
    let mut timer_h: u8 = sys::timer_create(sched.period_ns()).unwrap_or_else(|_| {
        sys::print(b"cpu-render: frame timer create failed\n");
        sys::exit();
    });
    let mut presented_buf: usize = 0;
    sys::print(b"  \xF0\x9F\x96\xA5\xEF\xB8\x8F  cpu-render: render loop starting\n");

    loop {
        let _ = sys::wait(&[CORE_HANDLE, timer_h], u64::MAX);
        let mut go = false;

        if sys::wait(&[CORE_HANDLE], 0).is_ok() {
            while core_ch.try_recv(&mut msg) {}
            go = sched.should_render_immediately(counter_to_ns(sys::counter(), cfreq));
            sched.on_scene_update();
        }
        if sys::wait(&[timer_h], 0).is_ok() {
            let _ = sys::handle_close(timer_h);
            timer_h = sys::timer_create(sched.period_ns()).unwrap_or_else(|_| {
                sys::print(b"cpu-render: timer recreate failed\n");
                sys::exit();
            });
            go |= sched.on_timer_tick_at(counter_to_ns(sys::counter(), cfreq));
        }
        if !go {
            continue;
        }

        // ── Read scene + dirty bitmap ────────────────────────────
        let render_buf = 1 - presented_buf;
        let tr = scene::TripleReader::new(scene_buf);
        let dirty_bits = *tr.dirty_bits();
        let (gen, nodes) = (tr.front_generation(), tr.front_nodes());
        let node_count = nodes.len() as u16;

        // ── Skip-frame: nothing changed ──────────────────────────
        if all_bits_zero(&dirty_bits) {
            tr.finish_read(gen);
            sched.on_render_complete_at(counter_to_ns(sys::counter(), cfreq));
            continue;
        }

        // ── Compute dirty rects for partial GPU transfer ─────────
        let damage = incr_state.compute_dirty_rects(nodes, node_count, &dirty_bits, fb_w16, fb_h16);

        // ── Render (full scene — CPU still does complete tree walk) ─
        let graph = render::scene_render::SceneGraph {
            nodes,
            data: tr.front_data_buf(),
        };
        backend.render(&graph, &mut make_fb(render_buf));

        // ── Update incremental state before releasing the reader ──
        incr_state.update_from_frame(nodes, node_count);
        tr.finish_read(gen);

        // ── Present: transfer dirty rects + flush ────────────────
        let base_offset = (render_buf as u64) * buf_stride;

        // Check for zero-count tracker (all dirty nodes were off-screen).
        // DamageTracker::dirty_rects() returns None for both "full screen"
        // and "zero rects" — distinguish here to avoid unnecessary transfer.
        let skip_transfer = damage
            .as_ref()
            .map_or(false, |d| d.count == 0 && !d.full_screen);

        if skip_transfer {
            // All dirty nodes clipped to zero area — nothing to transfer.
        } else {
            match damage.as_ref().and_then(|d| d.dirty_rects()) {
                Some(rects) => {
                    // Partial transfer: only send changed regions to the GPU.
                    for r in rects {
                        if r.w > 0 && r.h > 0 {
                            gpu::transfer_to_host_reuse(
                                &device,
                                &mut vq,
                                irq_handle,
                                &present_cmd,
                                gpu::FB_RESOURCE_ID,
                                r.x as u32,
                                r.y as u32,
                                r.w as u32,
                                r.h as u32,
                                base_offset,
                                stride,
                            );
                        }
                    }
                }
                None => {
                    // Full-screen transfer (first frame, overflow, or all-dirty).
                    gpu::transfer_to_host_reuse(
                        &device,
                        &mut vq,
                        irq_handle,
                        &present_cmd,
                        gpu::FB_RESOURCE_ID,
                        0,
                        0,
                        width,
                        height,
                        base_offset,
                        stride,
                    );
                }
            }
            gpu::resource_flush_reuse(
                &device,
                &mut vq,
                irq_handle,
                &present_cmd,
                gpu::FB_RESOURCE_ID,
                0,
                0,
                width,
                height,
            );
        }

        presented_buf = render_buf;
        sched.on_render_complete_at(counter_to_ns(sys::counter(), cfreq));
    }
}
