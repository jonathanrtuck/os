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
extern crate shaping;

#[path = "scene_render.rs"]
mod scene_render;

use alloc::{boxed::Box, vec};

use protocol::{
    compose::{
        CompositorConfig, IconConfig, ImageConfig, RtcConfig, MSG_COMPOSITOR_CONFIG,
        MSG_ICON_CONFIG, MSG_IMAGE_CONFIG, MSG_IMG_ICON_CONFIG,
    },
    present::{PresentPayload, MSG_PRESENT},
};

const FONT_SIZE: u32 = 18;
const CORE_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;

static mut BACK_BUF_IDX: usize = 0;
static mut ICON_COVERAGE: *const u8 = core::ptr::null();
static mut ICON_H: u32 = 0;
static mut ICON_W: u32 = 0;

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
fn rasterize_svg_icon(
    svg_data: &[u8],
    label: &[u8],
    icon_w: u32,
    icon_h: u32,
) -> Option<(*const u8, u32, u32)> {
    sys::print(label);

    let path_ptr = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::SvgPath>();

        alloc::alloc::alloc_zeroed(layout) as *mut drawing::SvgPath
    };

    if path_ptr.is_null() {
        sys::print(b"compositor: SVG path alloc failed\n");

        return None;
    }

    let scratch_ptr = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::SvgRasterScratch>();

        alloc::alloc::alloc_zeroed(layout) as *mut drawing::SvgRasterScratch
    };

    if scratch_ptr.is_null() {
        sys::print(b"compositor: SVG scratch alloc failed\n");

        unsafe {
            let layout = alloc::alloc::Layout::new::<drawing::SvgPath>();

            alloc::alloc::dealloc(path_ptr as *mut u8, layout);
        }

        return None;
    }

    let result = match drawing::svg_parse_path_into(svg_data, unsafe { &mut *path_ptr }) {
        Ok(()) => {
            let icon_size = (icon_w * icon_h) as usize;
            let mut icon_cov = vec![0u8; icon_size];

            match drawing::svg_rasterize(
                unsafe { &*path_ptr },
                unsafe { &mut *scratch_ptr },
                &mut icon_cov,
                icon_w,
                icon_h,
                drawing::SVG_FP_ONE,
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
            alloc::alloc::Layout::new::<drawing::SvgPath>(),
        );
        alloc::alloc::dealloc(
            scratch_ptr as *mut u8,
            alloc::alloc::Layout::new::<drawing::SvgRasterScratch>(),
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
    let fb_stride = config.fb_stride;
    let fb_size = fb_stride * fb_height;
    let scene_va = config.scene_va as usize;

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
    // Validate font data is parseable via shaping library.
    if shaping::rasterize::font_metrics(mono_font_data).is_none() {
        sys::print(b"compositor: font parse failed\n");
        sys::exit();
    }
    let mut mono_cache: Box<drawing::GlyphCache> = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::GlyphCache>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::GlyphCache;

        if ptr.is_null() {
            sys::print(b"compositor: glyph cache alloc failed\n");
            sys::exit();
        }

        Box::from_raw(ptr)
    };
    mono_cache.populate(mono_font_data, FONT_SIZE);
    let mono_cache_ptr = Box::into_raw(mono_cache);

    sys::print(b"     monospace font rasterized\n");

    // Load proportional font for chrome text.
    let mut prop_cache_ptr: *const drawing::GlyphCache = mono_cache_ptr;

    if config.prop_font_len > 0 {
        let prop_font_data = unsafe {
            let offset = config.mono_font_va as usize + config.mono_font_len as usize;

            core::slice::from_raw_parts(offset as *const u8, config.prop_font_len as usize)
        };

        if shaping::rasterize::font_metrics(prop_font_data).is_some() {
            let mut prop_cache: Box<drawing::GlyphCache> = unsafe {
                let layout = alloc::alloc::Layout::new::<drawing::GlyphCache>();
                let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::GlyphCache;

                if ptr.is_null() {
                    sys::print(b"compositor: prop cache alloc failed\n");
                    sys::exit();
                }

                Box::from_raw(ptr)
            };

            prop_cache.populate(prop_font_data, FONT_SIZE);

            prop_cache_ptr = Box::into_raw(prop_cache);

            sys::print(b"     proportional font rasterized\n");
        } else {
            sys::print(b"     prop font parse failed, using mono\n");
        }
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
    };
    // Channel from core (scene update notifications).
    let core_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel to GPU driver.
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };

    sys::print(b"     waiting for first scene\n");

    // Wait for the first scene graph from core.
    let _ = sys::wait(&[CORE_HANDLE], u64::MAX);

    // Drain the notification.
    while core_ch.try_recv(&mut msg) {}

    // Render first frame.
    {
        let dr = scene::DoubleReader::new(scene_buf);
        let graph = scene_render::SceneGraph {
            nodes: dr.front_nodes(),
            data: dr.front_data_buf(),
        };
        let mut fb0 = make_fb_surface(0);

        scene_render::render_scene(&mut fb0, &graph, &render_ctx);
    }

    let initial_payload = PresentPayload {
        buffer_index: 0,
        rect_count: 0,
        rects: [drawing::DirtyRect::new(0, 0, 0, 0); 6],
        _pad: [0; 4],
    };
    let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &initial_payload) };

    gpu_ch.send(&present_msg);

    let _ = sys::channel_signal(GPU_HANDLE);

    unsafe { BACK_BUF_IDX = 1 };

    sys::print(b"     entering render loop\n");

    // Render loop: wait for scene updates from core, render, present.
    loop {
        let _ = sys::wait(&[CORE_HANDLE], u64::MAX);

        // Drain all pending notifications (coalesce multiple updates).
        while core_ch.try_recv(&mut msg) {}

        let back = unsafe { BACK_BUF_IDX };

        {
            let dr = scene::DoubleReader::new(scene_buf);
            let graph = scene_render::SceneGraph {
                nodes: dr.front_nodes(),
                data: dr.front_data_buf(),
            };
            let mut fb = make_fb_surface(back);

            scene_render::render_scene(&mut fb, &graph, &render_ctx);
        }

        let payload = PresentPayload {
            buffer_index: back as u32,
            rect_count: 0,
            rects: [drawing::DirtyRect::new(0, 0, 0, 0); 6],
            _pad: [0; 4],
        };
        let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &payload) };

        gpu_ch.send(&present_msg);

        let _ = sys::channel_signal(GPU_HANDLE);

        unsafe { BACK_BUF_IDX = 1 - back };
    }
}
