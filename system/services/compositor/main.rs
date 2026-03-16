//! Compositor — content-agnostic pixel pump.
//!
//! Reads a scene graph from shared memory (written by core) and renders
//! it to a framebuffer. Knows about geometry, color, and blending.
//! Does not know about documents, text layout, cursors, or editing.
//!
//! Rendering is delegated to the `render` library via `CpuBackend`,
//! which implements the `RenderBackend` trait. The compositor owns the
//! event loop, frame scheduling, and buffer management.
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: core → compositor (MSG_SCENE_UPDATED signal)
//! Handle 2: compositor → GPU driver (present commands)

#![no_std]
#![no_main]

extern crate alloc;
extern crate fonts;
extern crate render;
extern crate scene;

#[path = "frame_scheduler.rs"]
mod frame_scheduler;

use alloc::{boxed::Box, vec};

use protocol::{
    compose::{CompositorConfig, MSG_COMPOSITOR_CONFIG, MSG_IMAGE_CONFIG},
    present::{PresentPayload, MSG_PRESENT},
};
use render::{round_f32, FrameAction, RenderBackend};

const FONT_SIZE: u32 = 18;
/// Display DPI. Hardcoded for QEMU's standard virtual display; configurable
/// in principle (e.g., from GPU/display driver capabilities).
const SCREEN_DPI: u16 = 96;
const CORE_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;

fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}



#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xA8 compositor - starting\n");

    // Read compositor config from init.
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_COMPOSITOR_CONFIG {
        sys::print(b"compositor: no config message\n");
        sys::exit();
    }

    let config: CompositorConfig = unsafe { msg.payload_as() };
    let fb_va = config.fb_va as usize;
    let fb_va2 = config.fb_va2 as usize;
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;
    // fb_stride is always fb_width * 4 (BGRA8888) — derived, not in config.
    let fb_stride = fb_width * 4;
    let fb_size = fb_stride * fb_height;
    let scene_va = config.scene_va as usize;
    // Validate and clamp fractional scale factor.
    // - Negative or zero → default to 1.0
    // - > 4.0 → clamp to 4.0 (extreme scales would overflow u16 coordinates)
    let scale_factor: f32 = {
        let raw = config.scale_factor;
        if raw <= 0.0 || raw.is_nan() {
            1.0
        } else if raw > 4.0 {
            4.0
        } else {
            raw
        }
    };

    if fb_va == 0 || fb_va2 == 0 || fb_width == 0 || fb_height == 0 || scene_va == 0 {
        sys::print(b"compositor: bad config\n");
        sys::exit();
    }

    // Load monospace font and build glyph cache.
    if config.mono_font_va == 0 || config.mono_font_len == 0 {
        sys::print(b"compositor: no font data\n");
        sys::exit();
    }

    let mono_font_data = unsafe {
        core::slice::from_raw_parts(
            config.mono_font_va as *const u8,
            config.mono_font_len as usize,
        )
    };
    // Validate font data is parseable via fonts library.
    if fonts::rasterize::font_metrics(mono_font_data).is_none() {
        sys::print(b"compositor: font parse failed\n");
        sys::exit();
    }

    let mut mono_cache: Box<fonts::cache::GlyphCache> = unsafe {
        let layout = alloc::alloc::Layout::new::<fonts::cache::GlyphCache>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;

        if ptr.is_null() {
            sys::print(b"compositor: glyph cache alloc failed\n");
            sys::exit();
        }

        Box::from_raw(ptr)
    };

    // Rasterize at physical pixel size: logical FONT_SIZE × scale_factor.
    let physical_font_size = round_f32(FONT_SIZE as f32 * scale_factor).max(1) as u32;

    // Recursive Variable: MONO=1 for monospace (code content).
    let mono_axes = [fonts::rasterize::AxisValue {
        tag: *b"MONO",
        value: 1.0,
    }];
    mono_cache.populate_with_axes(mono_font_data, physical_font_size, SCREEN_DPI, &mono_axes);

    sys::print(b"     monospace font rasterized (MONO=1)\n");

    // Proportional cache: separate allocation.
    // When no separate prop font, use the same font data with MONO=0.
    let prop_font_data = if config.prop_font_len > 0 {
        unsafe {
            let offset = config.mono_font_va as usize + config.mono_font_len as usize;
            core::slice::from_raw_parts(offset as *const u8, config.prop_font_len as usize)
        }
    } else {
        mono_font_data
    };

    let mut prop_cache: Box<fonts::cache::GlyphCache> = unsafe {
        let layout = alloc::alloc::Layout::new::<fonts::cache::GlyphCache>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;

        if ptr.is_null() {
            sys::print(b"compositor: prop cache alloc failed\n");
            sys::exit();
        }

        Box::from_raw(ptr)
    };

    if fonts::rasterize::font_metrics(prop_font_data).is_some() {
        // Recursive Variable: MONO=0 for proportional (prose/UI content).
        let prop_axes = [fonts::rasterize::AxisValue {
            tag: *b"MONO",
            value: 0.0,
        }];
        prop_cache.populate_with_axes(prop_font_data, physical_font_size, SCREEN_DPI, &prop_axes);
        sys::print(b"     proportional font rasterized (MONO=0)\n");
    } else {
        // Fallback: populate with mono font data + MONO=1 axes.
        prop_cache.populate_with_axes(mono_font_data, physical_font_size, SCREEN_DPI, &mono_axes);
        sys::print(b"     prop font parse failed, using mono\n");
    }

    // Check for image config (we don't decode it — core handles mode toggle,
    // but we may need the decoded pixels for image viewer rendering).
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMAGE_CONFIG {
        // Consumed but not used by compositor.
    }



    // ── Construct CpuBackend ────────────────────────────────────────
    let mut backend = render::CpuBackend {
        mono_cache,
        prop_cache,
        scale: scale_factor,
        pool: render::surface_pool::SurfacePool::new(render::surface_pool::DEFAULT_BUDGET),
        damage: render::damage::DamageTracker::new(fb_width as u16, fb_height as u16),
        prev_bounds: [(0, 0, 0, 0); scene::MAX_NODES],
        prev_node_count: 0,
    };

    // Framebuffer setup.
    static mut FB_PTRS: [*mut u8; 2] = [core::ptr::null_mut(); 2];

    unsafe {
        FB_PTRS[0] = fb_va as *mut u8;
        FB_PTRS[1] = fb_va2 as *mut u8;
    }

    let make_fb_surface = |idx: usize| -> drawing::Surface<'static> {
        let ptr = unsafe { FB_PTRS[idx] };
        let data = unsafe { core::slice::from_raw_parts_mut(ptr, fb_size as usize) };

        drawing::Surface {
            data,
            width: fb_width,
            height: fb_height,
            stride: fb_stride,
            format: drawing::PixelFormat::Bgra8888,
        }
    };

    // Scene graph shared memory (read-only from compositor's perspective,
    // but mapped read-write because the kernel doesn't have read-only sharing yet).
    let scene_buf =
        unsafe { core::slice::from_raw_parts(scene_va as *const u8, scene::DOUBLE_SCENE_SIZE) };

    // Channel from core (scene update notifications).
    let core_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel to GPU driver.
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };

    sys::print(b"     waiting for first scene\n");

    // Wait for the first scene graph from core.
    let _ = sys::wait(&[CORE_HANDLE], u64::MAX);

    // Drain the notification.
    while core_ch.try_recv(&mut msg) {}

    // Render first frame (always full repaint via backend).
    {
        let dr = scene::DoubleReader::new(scene_buf);
        let read_gen = dr.front_generation();
        let nodes = dr.front_nodes();
        let graph = render::scene_render::SceneGraph {
            nodes,
            data: dr.front_data_buf(),
        };
        let mut fb0 = make_fb_surface(0);

        // Mark as full repaint for the first frame.
        backend.damage.mark_full_screen();
        backend.render(&graph, &mut fb0);
        backend.finish_frame(nodes, nodes.len() as u16, None);

        // Signal that we're done reading the initial frame.
        dr.finish_read(read_gen);
    }

    let initial_payload = PresentPayload {
        buffer_index: 0,
        rect_count: 0,
        rects: [protocol::DirtyRect::new(0, 0, 0, 0); 6],
        _pad: [0; 4],
    };
    let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &initial_payload) };

    gpu_ch.send(&present_msg);
    let _ = sys::channel_signal(GPU_HANDLE);

    // Track which buffer was last presented. Partial updates render
    // directly into it (no copy, no swap). Full repaints use double
    // buffering to avoid tearing during the full render pass.
    let mut presented_buf: usize = 0;

    // ── Frame scheduler ─────────────────────────────────────────────
    //
    // Instead of rendering immediately on every scene update, the
    // compositor renders at a fixed cadence (default 60fps). Between
    // ticks, scene updates set a dirty flag. On each tick, the
    // compositor checks the flag: if dirty, render + present; if
    // clean, skip (idle optimization). This provides:
    //
    // - Event coalescing: multiple scene updates per frame → one render
    // - Idle optimization: no renders when nothing changed
    // - Configurable cadence: timer period controls frame rate
    // - Frame budgeting: skip overdue frames after render overrun
    // - Idle-to-active wakeup: render immediately after idle period
    let frame_rate: u32 = if config.frame_rate > 0 {
        config.frame_rate as u32
    } else {
        60
    };
    let mut scheduler = frame_scheduler::FrameScheduler::new(frame_rate);

    // Counter frequency for converting ticks to nanoseconds.
    let counter_freq = sys::counter_freq();

    // Create the first frame timer. One-shot timers that we recreate
    // on each tick (same pattern as core's clock timer).
    let mut frame_timer_handle: u8 = match sys::timer_create(scheduler.period_ns()) {
        Ok(h) => h,
        Err(_) => {
            sys::print(b"compositor: frame timer create failed\n");
            sys::exit();
        }
    };

    sys::print(b"     entering render loop (frame-scheduled)\n");

    /// Convert a hardware counter value to nanoseconds.
    #[inline]
    fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
        if freq == 0 {
            return 0;
        }
        // Compute (ticks * 1_000_000_000) / freq avoiding overflow by
        // splitting into seconds and remainder.
        let secs = ticks / freq;
        let rem = ticks % freq;
        secs * 1_000_000_000 + rem * 1_000_000_000 / freq
    }

    // Render loop: wait for scene updates OR frame timer tick.
    //
    // Two-handle wait: CORE_HANDLE (scene updates) and frame timer.
    // - Core signal: drain messages, mark dirty. If idle-to-active
    //   wakeup triggers, render immediately.
    // - Timer tick: if dirty → render + present; if clean → skip.
    //   Frame budgeting: skip overdue ticks after render overrun.
    //
    // This replaces the old pattern of rendering on every scene update.
    loop {
        // Block until either core signals a scene update or the frame
        // timer fires. The compositor never busy-spins — it's always
        // blocked in sys::wait when there's nothing to do.
        let _ = sys::wait(&[CORE_HANDLE, frame_timer_handle], u64::MAX);

        let mut should_render = false;

        // Check if core signaled (scene update available).
        // Poll (timeout=0) to avoid blocking — we just want to know
        // if the core channel has a pending signal.
        if sys::wait(&[CORE_HANDLE], 0).is_ok() {
            // Drain all pending notifications (coalesce multiple updates).
            while core_ch.try_recv(&mut msg) {}

            // Idle-to-active wakeup: if the timer hasn't fired recently
            // (more than half a period ago), render immediately rather
            // than making the user wait for the next tick.
            let now_ns = counter_to_ns(sys::counter(), counter_freq);
            if scheduler.should_render_immediately(now_ns) {
                scheduler.on_scene_update();
                should_render = true;
            } else {
                scheduler.on_scene_update();
            }
        }

        // Check if frame timer fired.
        if sys::wait(&[frame_timer_handle], 0).is_ok() {
            // Timer is one-shot and level-triggered — close and recreate.
            let _ = sys::handle_close(frame_timer_handle);
            frame_timer_handle = match sys::timer_create(scheduler.period_ns()) {
                Ok(h) => h,
                Err(_) => {
                    // Timer creation failed — fall back to rendering on
                    // every scene update to avoid a frozen display.
                    sys::print(b"compositor: frame timer recreate failed\n");
                    sys::exit();
                }
            };

            // Ask the scheduler if we should render this tick.
            // Timestamp-aware: passes current time for frame budgeting.
            let now_ns = counter_to_ns(sys::counter(), counter_freq);
            if scheduler.on_timer_tick_at(now_ns) {
                should_render = true;
            }
        }

        if !should_render {
            continue;
        }

        // ── Render phase (only reached when timer fired AND scene is dirty) ──

        let dr = scene::DoubleReader::new(scene_buf);
        let read_gen = dr.front_generation();
        let curr_nodes = dr.front_nodes();
        let curr_count = curr_nodes.len() as u16;

        // Delegate damage computation to the backend.
        let action =
            backend.prepare_frame(curr_nodes, curr_count, dr.change_list(), dr.is_full_repaint());

        if action == FrameAction::Skip {
            // No changes — skip rendering.
            dr.finish_read(read_gen);
            scheduler.on_render_complete();
            continue;
        }

        // Choose rendering strategy based on damage extent.
        //
        // Partial update: render directly into the last-presented buffer.
        //   No buffer copy, no swap. The GPU transfer sends only dirty rects
        //   from this buffer. Safe because virtio-gpu transfers are explicit
        //   (no hardware scanout tearing).
        //
        // Full repaint: render into the other buffer, then swap. This avoids
        //   visible tearing during the full render pass (the old buffer stays
        //   on screen until the new one is ready).
        let render_buf;
        let graph = render::scene_render::SceneGraph {
            nodes: curr_nodes,
            data: dr.front_data_buf(),
        };

        if backend.is_full_repaint() {
            render_buf = 1 - presented_buf;
            let mut fb = make_fb_surface(render_buf);
            backend.render(&graph, &mut fb);
            presented_buf = render_buf;
            backend.finish_frame(curr_nodes, curr_count, None);
        } else if backend.damage.count > 0 {
            render_buf = presented_buf;
            let mut fb = make_fb_surface(render_buf);
            backend.render(&graph, &mut fb);
            backend.finish_frame(curr_nodes, curr_count, dr.change_list());
        } else {
            // No damage rects — nothing to render. Acknowledge the read.
            dr.finish_read(read_gen);
            scheduler.on_render_complete();
            continue;
        }

        // Done reading scene data — signal the writer that this buffer
        // is safe to reuse. This must happen after all reads from
        // curr_nodes and dr.front_data_buf() are complete.
        dr.finish_read(read_gen);

        // Build present payload with dirty rects from the backend.
        let backend_rects = backend.dirty_rects();
        let payload = if !backend.is_full_repaint() && !backend_rects.is_empty() {
            let mut dirty = [protocol::DirtyRect::new(0, 0, 0, 0); 6];
            let n = backend_rects.len().min(6);
            dirty[..n].copy_from_slice(&backend_rects[..n]);
            PresentPayload {
                buffer_index: render_buf as u32,
                rect_count: n as u32,
                rects: dirty,
                _pad: [0; 4],
            }
        } else {
            PresentPayload {
                buffer_index: render_buf as u32,
                rect_count: 0,
                rects: [protocol::DirtyRect::new(0, 0, 0, 0); 6],
                _pad: [0; 4],
            }
        };
        let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &payload) };

        gpu_ch.send(&present_msg);
        let _ = sys::channel_signal(GPU_HANDLE);

        // Frame complete — clear dirty flag and update counters.
        // Timestamp-aware: records render end time for frame budgeting.
        let render_end_ns = counter_to_ns(sys::counter(), counter_freq);
        scheduler.on_render_complete_at(render_end_ns);
    }
}
