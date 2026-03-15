//! Compositor — content-agnostic pixel pump.
//!
//! Reads a scene graph from shared memory (written by core) and renders
//! it to a framebuffer. Knows about geometry, color, and blending.
//! Does not know about documents, text layout, cursors, or editing.
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: core → compositor (MSG_SCENE_UPDATED signal)
//! Handle 2: compositor → GPU driver (present commands)

#![no_std]
#![no_main]

extern crate alloc;
extern crate scene;
extern crate fonts;

#[path = "scene_render.rs"]
mod scene_render;
#[path = "damage.rs"]
mod damage;
#[path = "compositing.rs"]
mod compositing;
#[path = "cursor.rs"]
mod cursor;
#[path = "svg.rs"]
mod svg;
#[path = "frame_scheduler.rs"]
mod frame_scheduler;

use alloc::{boxed::Box, vec};

use protocol::{
    compose::{
        CompositorConfig, IconConfig, ImageConfig, RtcConfig, MSG_COMPOSITOR_CONFIG,
        MSG_ICON_CONFIG, MSG_IMAGE_CONFIG, MSG_IMG_ICON_CONFIG,
    },
    present::{PresentPayload, MSG_PRESENT},
};

const FONT_SIZE: u32 = 18;
/// Display DPI. Hardcoded for QEMU's standard virtual display; configurable
/// in principle (e.g., from GPU/display driver capabilities).
const SCREEN_DPI: u16 = 96;
const CORE_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;


static mut ICON_COVERAGE: *const u8 = core::ptr::null();
static mut ICON_H: u32 = 0;
static mut ICON_W: u32 = 0;

/// Previous frame's absolute physical-pixel bounds for each node.
/// (x, y, w, h) — used to damage the OLD position when a node moves,
/// preventing "ghost" pixels at the previous location.
///
/// x/y are `i32` (not `i16`) to avoid truncation at scale > 1 where
/// physical coordinates can exceed 32767 (e.g. 1024 logical × 2 = 2048).
static mut PREV_BOUNDS: [(i32, i32, u16, u16); scene::MAX_NODES] =
    [(0, 0, 0, 0); scene::MAX_NODES];

fn append_u32(buf: &mut [u8], start: usize, val: u32) -> usize {
    let mut ci = start;

    if val == 0 {
        if ci < buf.len() {
            buf[ci] = b'0';
            ci += 1;
        }

        return ci;
    }

    let mut digits = [0u8; 10];
    let mut di = 10;
    let mut n = val;

    while n > 0 {
        di -= 1;
        digits[di] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    while di < 10 && ci < buf.len() {
        buf[ci] = digits[di];
        ci += 1;
        di += 1;
    }

    ci
}
fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Manual implementation for `no_std`.
#[inline]
fn round_f32(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// Scale a logical coordinate to physical pixels (rounding).
#[inline]
fn scale_coord(logical: i32, scale: f32) -> i32 {
    round_f32(logical as f32 * scale)
}

/// Compute gap-free physical size from logical position and size.
#[inline]
fn scale_size_u16(logical_pos: i32, logical_size: u32, scale: f32) -> u16 {
    let phys_start = round_f32(logical_pos as f32 * scale);
    let phys_end = round_f32((logical_pos as f32 + logical_size as f32) * scale);
    (phys_end - phys_start).max(0) as u16
}

/// Populate PREV_BOUNDS for all live nodes from the current scene graph.
/// Computes each node's absolute position in physical pixel coords and
/// stores it. Called after first frame and after every full repaint.
///
/// # Safety
///
/// Writes to the `PREV_BOUNDS` static mut. Must be called from the
/// single-threaded render loop only.
unsafe fn populate_prev_bounds(nodes: &[scene::Node], count: usize, scale: f32) {
    let n = count.min(nodes.len()).min(scene::MAX_NODES);
    let parent_map = scene::build_parent_map(nodes, n);

    for i in 0..n {
        let (ax, ay, aw, ah) = scene::abs_bounds(nodes, &parent_map, i);
        // Scale logical bounds to physical pixel coords and clamp to non-negative.
        // x/y stored as i32 to avoid truncation at high scale factors where
        // physical coordinates can exceed i16::MAX (32767).
        let px = scale_coord(ax, scale).max(0);
        let py = scale_coord(ay, scale).max(0);
        let pw = scale_size_u16(ax, aw, scale);
        let ph = scale_size_u16(ay, ah, scale);

        PREV_BOUNDS[i] = (px, py, pw, ph);
    }

    // Zero out entries beyond live node count.
    for i in n..scene::MAX_NODES {
        PREV_BOUNDS[i] = (0, 0, 0, 0);
    }
}
fn rasterize_svg_icon(
    svg_data: &[u8],
    label: &[u8],
    icon_w: u32,
    icon_h: u32,
) -> Option<(*const u8, u32, u32)> {
    sys::print(label);

    let path_ptr = unsafe {
        let layout = alloc::alloc::Layout::new::<svg::SvgPath>();

        alloc::alloc::alloc_zeroed(layout) as *mut svg::SvgPath
    };

    if path_ptr.is_null() {
        sys::print(b"compositor: SVG path alloc failed\n");

        return None;
    }

    let scratch_ptr = unsafe {
        let layout = alloc::alloc::Layout::new::<svg::SvgRasterScratch>();

        alloc::alloc::alloc_zeroed(layout) as *mut svg::SvgRasterScratch
    };

    if scratch_ptr.is_null() {
        sys::print(b"compositor: SVG scratch alloc failed\n");

        unsafe {
            let layout = alloc::alloc::Layout::new::<svg::SvgPath>();

            alloc::alloc::dealloc(path_ptr as *mut u8, layout);
        }

        return None;
    }

    let result = match svg::svg_parse_path_into(svg_data, unsafe { &mut *path_ptr }) {
        Ok(()) => {
            let icon_size = (icon_w * icon_h) as usize;
            let mut icon_cov = vec![0u8; icon_size];

            match svg::svg_rasterize(
                unsafe { &*path_ptr },
                unsafe { &mut *scratch_ptr },
                &mut icon_cov,
                icon_w,
                icon_h,
                svg::SVG_FP_ONE,
                0,
                0,
            ) {
                Ok(()) => {
                    let mut rgb_cov = vec![0u8; icon_size * 3];

                    for i in 0..icon_size {
                        let c = icon_cov[i];

                        rgb_cov[i * 3] = c;
                        rgb_cov[i * 3 + 1] = c;
                        rgb_cov[i * 3 + 2] = c;
                    }

                    let leaked = rgb_cov.leak();

                    Some((leaked.as_ptr(), icon_w, icon_h))
                }
                Err(_) => {
                    sys::print(b"     SVG rasterize failed\n");
                    None
                }
            }
        }
        Err(_) => {
            sys::print(b"     SVG parse failed\n");
            None
        }
    };
    unsafe {
        alloc::alloc::dealloc(
            path_ptr as *mut u8,
            alloc::alloc::Layout::new::<svg::SvgPath>(),
        );
        alloc::alloc::dealloc(
            scratch_ptr as *mut u8,
            alloc::alloc::Layout::new::<svg::SvgRasterScratch>(),
        );
    }
    result
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
    let mono_cache_ptr = Box::into_raw(mono_cache);

    sys::print(b"     monospace font rasterized (MONO=1)\n");

    // Proportional cache: same font data, MONO=0 for sans-serif (prose/UI).
    let mut prop_cache_ptr: *const fonts::cache::GlyphCache = mono_cache_ptr;

    // When no separate prop font, use the same font data with MONO=0.
    let prop_font_data = if config.prop_font_len > 0 {
        unsafe {
            let offset = config.mono_font_va as usize + config.mono_font_len as usize;
            core::slice::from_raw_parts(offset as *const u8, config.prop_font_len as usize)
        }
    } else {
        mono_font_data
    };

    if fonts::rasterize::font_metrics(prop_font_data).is_some() {
        let mut prop_cache: Box<fonts::cache::GlyphCache> = unsafe {
            let layout = alloc::alloc::Layout::new::<fonts::cache::GlyphCache>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;

            if ptr.is_null() {
                sys::print(b"compositor: prop cache alloc failed\n");
                sys::exit();
            }

            Box::from_raw(ptr)
        };

        // Recursive Variable: MONO=0 for proportional (prose/UI content).
        let prop_axes = [fonts::rasterize::AxisValue {
            tag: *b"MONO",
            value: 0.0,
        }];
        prop_cache.populate_with_axes(prop_font_data, physical_font_size, SCREEN_DPI, &prop_axes);

        prop_cache_ptr = Box::into_raw(prop_cache);

        sys::print(b"     proportional font rasterized (MONO=0)\n");
    } else {
        sys::print(b"     prop font parse failed, using mono\n");
    }
    // Check for image config (we don't decode it — core handles mode toggle,
    // but we may need the decoded pixels for image viewer rendering).
    // For now, skip — image viewer support can be added later via a scene
    // graph Image content node.
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMAGE_CONFIG {
        // Consumed but not used by compositor.
    }

    // Load SVG icons.
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_ICON_CONFIG {
        let icn: IconConfig = unsafe { msg.payload_as() };

        if icn.icon_va != 0 && icn.icon_len > 0 {
            let svg_data = unsafe {
                core::slice::from_raw_parts(icn.icon_va as *const u8, icn.icon_len as usize)
            };

            if let Some((ptr, w, h)) =
                rasterize_svg_icon(svg_data, b"     parsing SVG doc icon\n", 20, 24)
            {
                sys::print(b"     SVG icon rasterized\n");
                unsafe {
                    ICON_COVERAGE = ptr;
                    ICON_W = w;
                    ICON_H = h;
                }
            }
        }
    }
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMG_ICON_CONFIG {
        // Image icon — consumed. Could store for future use.
    }

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
    let icon_cov: &[u8] = unsafe {
        if !ICON_COVERAGE.is_null() && ICON_W > 0 && ICON_H > 0 {
            core::slice::from_raw_parts(ICON_COVERAGE, (ICON_W * ICON_H * 3) as usize)
        } else {
            &[]
        }
    };
    let render_ctx = scene_render::RenderCtx {
        mono_cache: unsafe { &*mono_cache_ptr },
        prop_cache: unsafe { &*prop_cache_ptr },
        icon_coverage: icon_cov,
        icon_w: unsafe { ICON_W },
        icon_h: unsafe { ICON_H },
        icon_color: drawing::CHROME_ICON,
        icon_node: 2, // N_TITLE_TEXT — well-known index
        scale: scale_factor, // f32 fractional scale
    };
    // Channel from core (scene update notifications).
    let core_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel to GPU driver.
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };

    // Previous frame's node count — used to detect structural changes
    // (selection rects added/removed). Starts at 0 so first frame is
    // always a full repaint.
    let mut prev_node_count: u16 = 0;

    sys::print(b"     waiting for first scene\n");

    // Wait for the first scene graph from core.
    let _ = sys::wait(&[CORE_HANDLE], u64::MAX);

    // Drain the notification.
    while core_ch.try_recv(&mut msg) {}

    // Render first frame (always full repaint).
    {
        let dr = scene::DoubleReader::new(scene_buf);
        let read_gen = dr.front_generation();
        let nodes = dr.front_nodes();
        let graph = scene_render::SceneGraph {
            nodes,
            data: dr.front_data_buf(),
        };
        let mut fb0 = make_fb_surface(0);

        scene_render::render_scene(&mut fb0, &graph, &render_ctx);

        prev_node_count = nodes.len() as u16;

        // SAFETY: Single-threaded render loop; no concurrent access to
        // PREV_BOUNDS. Populates initial positions so the next frame's
        // change-list damage can reference old bounds.
        unsafe {
            populate_prev_bounds(nodes, nodes.len(), scale_factor);
        }

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

        // Change-list-driven damage tracking: read which nodes changed
        // from the scene header instead of byte-comparing all nodes.
        let mut damage = damage::DamageTracker::new(fb_width as u16, fb_height as u16);

        if curr_count != prev_node_count {
            // Node count changed (selection rects added/removed) — full repaint.
            damage.mark_full_screen();
        } else if dr.is_full_repaint() {
            // Full rebuild or change list overflow — full repaint.
            damage.mark_full_screen();
        } else {
            match dr.change_list() {
                Some(changed) if changed.is_empty() => {
                    // No nodes changed — skip rendering entirely.
                    // Signal that we're done reading so the writer can
                    // safely reuse this buffer.
                    dr.finish_read(read_gen);
                    prev_node_count = curr_count;
                    // Scene was marked dirty but change list is empty
                    // (possible if core swapped without real changes).
                    // Still count as render complete to clear dirty flag.
                    scheduler.on_render_complete();
                    continue;
                }
                Some(changed) => {
                    // Compute dirty rects from changed node absolute positions.
                    // For each changed node, damage BOTH the old position (from
                    // prev_bounds) AND the new position (from current scene graph).
                    // This prevents "ghost" pixels when a node moves — e.g., the
                    // cursor leaving stale pixels at its previous location.
                    let parent_map = scene::build_parent_map(curr_nodes, curr_count as usize);
                    let sf = scale_factor;
                    let fbw = fb_width as u16;
                    let fbh = fb_height as u16;

                    for &node_id in changed {
                        if (node_id as usize) >= curr_nodes.len() {
                            continue;
                        }

                        // Damage the OLD position (previous frame's bounds).
                        // SAFETY: Single-threaded render loop; no concurrent
                        // access to PREV_BOUNDS. node_id < MAX_NODES by the
                        // guard above (curr_nodes.len() <= MAX_NODES).
                        let (ox, oy, ow, oh) = unsafe { PREV_BOUNDS[node_id as usize] };

                        if ow > 0 && oh > 0 && ox >= 0 && oy >= 0 {
                            let old_x = (ox as u32).min(fbw as u32) as u16;
                            let old_y = (oy as u32).min(fbh as u32) as u16;
                            let old_w = ow.min(fbw - old_x);
                            let old_h = oh.min(fbh - old_y);

                            damage.add(old_x, old_y, old_w, old_h);
                        }

                        // Damage the NEW position (current frame's bounds).
                        let (ax, ay, aw, ah) =
                            scene::abs_bounds(curr_nodes, &parent_map, node_id as usize);

                        let px = scale_coord(ax, sf).max(0) as u16;
                        let py = scale_coord(ay, sf).max(0) as u16;
                        let x = px.min(fbw);
                        let y = py.min(fbh);
                        let w = scale_size_u16(ax, aw, sf).min(fbw - x);
                        let h = scale_size_u16(ay, ah, sf).min(fbh - y);

                        damage.add(x, y, w, h);
                    }
                }
                None => {
                    // Defensive fallback: is_full_repaint() is checked above,
                    // so this arm should only fire if the sentinel is set but
                    // the earlier guard missed it (e.g., a race or logic gap).
                    // Treat as full repaint for safety.
                    damage.mark_full_screen();
                }
            }
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
        let graph = scene_render::SceneGraph {
            nodes: curr_nodes,
            data: dr.front_data_buf(),
        };

        if damage.full_screen {
            render_buf = 1 - presented_buf;
            let mut fb = make_fb_surface(render_buf);
            scene_render::render_scene(&mut fb, &graph, &render_ctx);
            presented_buf = render_buf;

            // Full repaint: refresh all prev_bounds from current positions.
            // SAFETY: Single-threaded render loop; no concurrent access to
            // PREV_BOUNDS.
            unsafe {
                populate_prev_bounds(curr_nodes, curr_count as usize, scale_factor);
            }
        } else if let Some(rects) = damage.dirty_rects() {
            render_buf = presented_buf;
            let mut fb = make_fb_surface(render_buf);
            let bbox = protocol::DirtyRect::union_all(rects);
            scene_render::render_scene_clipped(&mut fb, &graph, &render_ctx, &bbox);

            // Partial update: refresh prev_bounds for changed nodes only.
            // SAFETY: Single-threaded render loop; no concurrent access to
            // PREV_BOUNDS. node_id < MAX_NODES (bounded by curr_nodes.len()).
            if let Some(changed) = dr.change_list() {
                let parent_map = scene::build_parent_map(curr_nodes, curr_count as usize);
                let sf = scale_factor;

                for &node_id in changed {
                    if (node_id as usize) >= curr_nodes.len() {
                        continue;
                    }

                    let (ax, ay, aw, ah) =
                        scene::abs_bounds(curr_nodes, &parent_map, node_id as usize);
                    let px = scale_coord(ax, sf).max(0);
                    let py = scale_coord(ay, sf).max(0);
                    let pw = scale_size_u16(ax, aw, sf);
                    let ph = scale_size_u16(ay, ah, sf);

                    unsafe {
                        PREV_BOUNDS[node_id as usize] = (px, py, pw, ph);
                    }
                }
            }
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

        // Track node count for next frame's structural change detection.
        prev_node_count = curr_count;

        // Build present payload with dirty rects.
        let payload = match damage.dirty_rects() {
            Some(rects) if !damage.full_screen => {
                let mut dirty = [protocol::DirtyRect::new(0, 0, 0, 0); 6];
                let n = rects.len().min(6);
                dirty[..n].copy_from_slice(&rects[..n]);
                PresentPayload {
                    buffer_index: render_buf as u32,
                    rect_count: n as u32,
                    rects: dirty,
                    _pad: [0; 4],
                }
            }
            _ => PresentPayload {
                buffer_index: render_buf as u32,
                rect_count: 0,
                rects: [protocol::DirtyRect::new(0, 0, 0, 0); 6],
                _pad: [0; 4],
            },
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
