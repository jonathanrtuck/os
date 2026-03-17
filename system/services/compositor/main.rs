//! Compositor — content-agnostic pixel pump. Reads a scene graph from shared
//! memory and renders it to a framebuffer via `CpuBackend`.

#![no_std]
#![no_main]

extern crate alloc;
extern crate render;
extern crate scene;

#[path = "frame_scheduler.rs"]
mod frame_scheduler;

use protocol::{
    compose::{CompositorConfig, MSG_COMPOSITOR_CONFIG, MSG_IMAGE_CONFIG},
    present::{PresentPayload, MSG_PRESENT, MSG_PRESENT_DONE},
};
use render::{FrameAction, RenderBackend};

const FONT_SIZE: u32 = 18;
const SCREEN_DPI: u16 = 96;
const CORE_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;

fn channel_shm_va(idx: usize) -> usize { protocol::channel_shm_va(idx) }

#[inline]
fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
    if freq == 0 { return 0; }
    (ticks / freq) * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
}

fn clamp_scale(raw: f32) -> f32 {
    if raw <= 0.0 || raw.is_nan() { 1.0 } else if raw > 4.0 { 4.0 } else { raw }
}

fn present(gpu_ch: &ipc::Channel, buf_idx: usize, rects: &[protocol::DirtyRect]) {
    let mut dirty = [protocol::DirtyRect::new(0, 0, 0, 0); 6];
    let n = rects.len().min(6);
    dirty[..n].copy_from_slice(&rects[..n]);
    let payload = PresentPayload {
        buffer_index: buf_idx as u32, rect_count: n as u32, rects: dirty, _pad: [0; 4],
    };
    gpu_ch.send(&unsafe { ipc::Message::from_payload(MSG_PRESENT, &payload) });
    let _ = sys::channel_signal(GPU_HANDLE);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xA8 compositor - starting\n");

    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);
    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_COMPOSITOR_CONFIG {
        sys::print(b"compositor: no config message\n"); sys::exit();
    }
    let config: CompositorConfig = unsafe { msg.payload_as() };
    let (fb_va, fb_va2) = (config.fb_va as usize, config.fb_va2 as usize);
    let (fb_width, fb_height) = (config.fb_width, config.fb_height);
    let fb_size = fb_width * 4 * fb_height;
    let scene_va = config.scene_va as usize;
    let scale = clamp_scale(config.scale_factor);
    if fb_va == 0 || fb_va2 == 0 || fb_width == 0 || fb_height == 0 || scene_va == 0 {
        sys::print(b"compositor: bad config\n"); sys::exit();
    }

    // Render backend — font rasterization handled internally by CpuBackend.
    if config.mono_font_va == 0 || config.mono_font_len == 0 {
        sys::print(b"compositor: no font data\n"); sys::exit();
    }
    // SAFETY: init mapped these pages before starting the compositor.
    let mono = unsafe {
        core::slice::from_raw_parts(config.mono_font_va as *const u8, config.mono_font_len as usize)
    };
    let prop = if config.prop_font_len > 0 {
        let off = config.mono_font_va as usize + config.mono_font_len as usize;
        Some(unsafe { core::slice::from_raw_parts(off as *const u8, config.prop_font_len as usize) })
    } else { None };
    let mut backend = match render::CpuBackend::new(
        mono, prop, FONT_SIZE, SCREEN_DPI, scale, fb_width as u16, fb_height as u16,
    ) {
        Some(b) => b,
        None => { sys::print(b"compositor: render backend init failed\n"); sys::exit(); }
    };
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMAGE_CONFIG {} // consume optional

    // Framebuffer + scene graph + IPC.
    static mut FB_PTRS: [*mut u8; 2] = [core::ptr::null_mut(); 2];
    unsafe { FB_PTRS[0] = fb_va as *mut u8; FB_PTRS[1] = fb_va2 as *mut u8; }
    let make_fb = |idx: usize| -> drawing::Surface<'static> {
        let ptr = unsafe { FB_PTRS[idx] };
        let data = unsafe { core::slice::from_raw_parts_mut(ptr, fb_size as usize) };
        drawing::Surface { data, width: fb_width, height: fb_height,
            stride: fb_width * 4, format: drawing::PixelFormat::Bgra8888 }
    };
    let scene_buf = unsafe { core::slice::from_raw_parts(scene_va as *const u8, scene::TRIPLE_SCENE_SIZE) };
    let core_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };

    // First frame — wait for core, full repaint.
    sys::print(b"     waiting for first scene\n");
    let _ = sys::wait(&[CORE_HANDLE], u64::MAX);
    while core_ch.try_recv(&mut msg) {}
    {
        let tr = scene::TripleReader::new(scene_buf);
        let (gen, nodes) = (tr.front_generation(), tr.front_nodes());
        let graph = render::scene_render::SceneGraph { nodes, data: tr.front_data_buf() };
        backend.damage.mark_full_screen();
        backend.render(&graph, &mut make_fb(0));
        backend.finish_frame(nodes, nodes.len() as u16, None);
        tr.finish_read(gen);
    }
    present(&gpu_ch, 0, &[]);

    // Frame scheduler + render loop state.
    let fps = if config.frame_rate > 0 { config.frame_rate as u32 } else { 60 };
    let mut sched = frame_scheduler::FrameScheduler::new(fps);
    let cfreq = sys::counter_freq();
    let mut timer_h: u8 = sys::timer_create(sched.period_ns()).unwrap_or_else(|_| {
        sys::print(b"compositor: frame timer create failed\n"); sys::exit();
    });
    let mut presented_buf: usize = 0;
    // GPU flow control: track whether a framebuffer is in-flight to the GPU.
    // After sending MSG_PRESENT, the presented buffer is in-flight until we
    // receive MSG_PRESENT_DONE. We must not render into the in-flight buffer.
    let mut gpu_in_flight = true; // first frame is in-flight after present above
    sys::print(b"     entering render loop\n");

    loop {
        let _ = sys::wait(&[CORE_HANDLE, timer_h, GPU_HANDLE], u64::MAX);
        let mut go = false;
        // Drain GPU completion signals.
        if sys::wait(&[GPU_HANDLE], 0).is_ok() {
            while gpu_ch.try_recv(&mut msg) {
                if msg.msg_type == MSG_PRESENT_DONE {
                    gpu_in_flight = false;
                }
            }
        }
        if sys::wait(&[CORE_HANDLE], 0).is_ok() {
            while core_ch.try_recv(&mut msg) {}
            go = sched.should_render_immediately(counter_to_ns(sys::counter(), cfreq));
            sched.on_scene_update();
        }
        if sys::wait(&[timer_h], 0).is_ok() {
            let _ = sys::handle_close(timer_h);
            timer_h = sys::timer_create(sched.period_ns()).unwrap_or_else(|_| {
                sys::print(b"compositor: timer recreate failed\n"); sys::exit();
            });
            go |= sched.on_timer_tick_at(counter_to_ns(sys::counter(), cfreq));
        }
        if !go { continue; }

        // Wait for GPU completion if the target buffer is still in-flight.
        // The render target is always 1 - presented_buf (the non-displayed buffer).
        if gpu_in_flight {
            // Block until the GPU signals completion.
            loop {
                let _ = sys::wait(&[GPU_HANDLE], u64::MAX);
                while gpu_ch.try_recv(&mut msg) {
                    if msg.msg_type == MSG_PRESENT_DONE {
                        gpu_in_flight = false;
                    }
                }
                if !gpu_in_flight { break; }
            }
        }

        let tr = scene::TripleReader::new(scene_buf);
        let (gen, nodes) = (tr.front_generation(), tr.front_nodes());
        let count = nodes.len() as u16;
        let action = backend.prepare_frame(nodes, count, tr.change_list(), tr.is_full_repaint());
        if action == FrameAction::Skip {
            // Update prev_bounds bookkeeping even on skip so the next
            // rendered frame's damage calculation uses accurate bounds.
            backend.update_bounds_for_skip(nodes, count);
            tr.finish_read(gen);
            sched.on_render_complete();
            continue;
        }
        let graph = render::scene_render::SceneGraph { nodes, data: tr.front_data_buf() };
        // Always render to the non-displayed buffer (1 - presented_buf).
        // This prevents tearing on both full and partial updates: we never
        // write into the buffer the GPU is currently displaying.
        let render_buf = 1 - presented_buf;
        if backend.is_full_repaint() {
            backend.render(&graph, &mut make_fb(render_buf));
            backend.finish_frame(nodes, count, None);
        } else if backend.damage.count > 0 {
            backend.render(&graph, &mut make_fb(render_buf));
            backend.finish_frame(nodes, count, tr.change_list());
        } else {
            tr.finish_read(gen); sched.on_render_complete(); continue;
        };
        tr.finish_read(gen);
        let rects = backend.dirty_rects();
        if !backend.is_full_repaint() && !rects.is_empty() {
            present(&gpu_ch, render_buf, rects);
        } else {
            present(&gpu_ch, render_buf, &[]);
        }
        presented_buf = render_buf;
        gpu_in_flight = true;
        sched.on_render_complete_at(counter_to_ns(sys::counter(), cfreq));
    }
}
