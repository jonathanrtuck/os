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

use protocol::metal::{self, DRAWABLE_HANDLE};
use render::frame_scheduler::frame_period_ns;
use scene::{Content, NodeFlags, NodeId, NULL};

#[path = "atlas.rs"]
mod atlas;
#[path = "device.rs"]
mod device;
#[path = "dma.rs"]
mod dma;
#[path = "path.rs"]
mod path;
#[path = "pipeline.rs"]
mod pipeline;
#[path = "scene_walk.rs"]
mod scene_walk;
#[path = "shaders.rs"]
mod shaders;
#[path = "virtio_helpers.rs"]
mod virtio_helpers;

use atlas::{GlyphAtlas, ATLAS_HEIGHT, ATLAS_WIDTH};
use dma::DmaBuf;
use path::PathPointsBuf;
use scene_walk::{
    emit_quad, flush_solid_vertices, flush_vertices_raw, pack_blur_params, pack_copy_params,
    scale_pointer_coord, BlurReq, ClipRect, ImageAtlas, RenderContext,
};
use virtio_helpers::{send_render, send_setup};

// ── Constants ────────────────────────────────────────────────────────────

/// Setup virtqueue index.
pub(crate) const VIRTQ_SETUP: u32 = 0;
/// Render virtqueue index.
pub(crate) const VIRTQ_RENDER: u32 = 1;

/// IPC handle for the init channel.
pub(crate) const INIT_HANDLE: u8 = 0;
/// IPC handle for the core->metal-render scene update channel.
pub(crate) const SCENE_HANDLE: u8 = 1;

/// Scene graph node index for the pointer cursor. Matches core's N_POINTER.
/// When the cursor plane is active, this node is skipped during walk_scene
/// and composited by the host's cursor plane instead.
pub(crate) const CURSOR_PLANE_NODE: NodeId = 8;

// ── Metal object handles (guest-assigned, must be nonzero) ──────────────

pub(crate) const LIB_SHADERS: u32 = 1;

pub(crate) const FN_VERTEX_MAIN: u32 = 10;
pub(crate) const FN_FRAGMENT_SOLID: u32 = 11;
pub(crate) const FN_FRAGMENT_GLYPH: u32 = 12;
pub(crate) const FN_FRAGMENT_TEXTURED: u32 = 13;
pub(crate) const FN_VERTEX_STENCIL: u32 = 14;
pub(crate) const FN_BLUR_H: u32 = 15;
pub(crate) const FN_BLUR_V: u32 = 16;
pub(crate) const FN_COPY_SRGB_TO_LINEAR: u32 = 17;
pub(crate) const FN_COPY_LINEAR_TO_SRGB: u32 = 18;
pub(crate) const FN_FRAGMENT_ROUNDED_RECT: u32 = 19;
pub(crate) const FN_FRAGMENT_SHADOW: u32 = 35;
pub(crate) const FN_FRAGMENT_DITHER: u32 = 37;

pub(crate) const PIPE_SOLID: u32 = 20;
pub(crate) const PIPE_TEXTURED: u32 = 21;
pub(crate) const PIPE_GLYPH: u32 = 22;
pub(crate) const PIPE_STENCIL_WRITE: u32 = 23;
pub(crate) const PIPE_SOLID_NO_MSAA: u32 = 24;
pub(crate) const PIPE_ROUNDED_RECT: u32 = 25;
pub(crate) const PIPE_SHADOW: u32 = 36;
pub(crate) const PIPE_DITHER: u32 = 38;
pub(crate) const CPIPE_BLUR_H: u32 = 26;
pub(crate) const CPIPE_BLUR_V: u32 = 27;
pub(crate) const CPIPE_SRGB_TO_LINEAR: u32 = 28;
pub(crate) const CPIPE_LINEAR_TO_SRGB: u32 = 29;

pub(crate) const DSS_NONE: u32 = 30;
pub(crate) const DSS_STENCIL_WRITE: u32 = 31;
pub(crate) const DSS_STENCIL_TEST: u32 = 32;
/// Clip test: pass where stencil != 0, KEEP stencil value (don't zero on pass).
/// Used for clip paths where multiple children need the same stencil mask.
pub(crate) const DSS_CLIP_TEST: u32 = 33;
/// Even-odd fill: INVERT stencil on each triangle overlap, then test for odd count.
pub(crate) const DSS_STENCIL_INVERT: u32 = 34;
/// Two-sided non-zero winding fill: front-face triangles INCR_WRAP, back-face DECR_WRAP.
/// The stencil accumulates signed winding number; non-zero = inside.
pub(crate) const DSS_STENCIL_WINDING: u32 = 35;

pub(crate) const SAMPLER_NEAREST: u32 = 41;
pub(crate) const SAMPLER_LINEAR: u32 = 42;

pub(crate) const TEX_MSAA: u32 = 50;
pub(crate) const TEX_STENCIL: u32 = 51;
pub(crate) const TEX_ATLAS: u32 = 52;
pub(crate) const TEX_BLUR_A: u32 = 53;
pub(crate) const TEX_BLUR_B: u32 = 54;
pub(crate) const TEX_IMAGE: u32 = 55;
/// Float16 resolve target — MSAA resolves here, then dither pass blits to drawable.
pub(crate) const TEX_RESOLVE: u32 = 56;

/// Maximum image texture dimension. All per-frame images are packed into
/// sub-rectangles of this single atlas texture via `ImageAtlas`.
pub(crate) const IMG_TEX_DIM: u32 = 1024;

// Glyph atlas dimensions imported from atlas module (ATLAS_WIDTH, ATLAS_HEIGHT).

/// Maximum vertex bytes per set_vertex_bytes call (Metal's 4KB limit).
pub(crate) const MAX_INLINE_BYTES: usize = 4096;
/// Bytes per vertex: position(f32x2) + texCoord(f32x2) + color(f32x4) = 32.
pub(crate) const VERTEX_BYTES: usize = 32;
/// Max quads per inline draw call: 4096 / (6 * 32) = 21.
pub(crate) const MAX_QUADS_PER_DRAW: usize = MAX_INLINE_BYTES / (6 * VERTEX_BYTES);

/// MSAA sample count (1 = no MSAA, 4 = 4x MSAA).
pub(crate) const SAMPLE_COUNT: u8 = 4;

// ── IPC helpers ─────────────────────────────────────────────────────────

fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

pub(crate) fn print_u32(n: u32) {
    sys::print_u32(n);
}

pub(crate) fn print_hex_u32(val: u32) {
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

/// Round a font size in points to device pixels, matching the rasterization
/// key used by the atlas hash table.
#[inline]
pub(crate) fn round_font_size(font_size_pt: u16, scale_factor: f32) -> u16 {
    let px = font_size_pt as f32 * scale_factor;
    if px >= 0.0 {
        (px + 0.5) as u16
    } else {
        1
    }
}

/// Font data entry for content_id-based lookup.
struct FontDataEntry<'a> {
    content_id: u32,
    data: &'a [u8],
}

/// Find font data by content_id in the font data map.
fn find_font_data<'a>(content_id: u32, entries: &'a [FontDataEntry<'a>]) -> &'a [u8] {
    for e in entries {
        if e.content_id == content_id {
            return e.data;
        }
    }
    &[]
}

// ── Entry point ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x94\xB1 metal-render - starting\n");

    // ── Phase A: Receive device config from init, init virtio device ─────
    // SAFETY: Channel 0 shared memory is mapped by kernel before process start.
    let ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let (device, irq_handle) = device::phase_a(&ch);

    // Setup two virtqueues.
    let (mut setup_vq, mut render_vq) = device::setup_virtqueues(&device);

    // ── Phase B: Display query + init handshake ──────────────────────────
    let disp = device::phase_b(&device, &ch);
    let width = disp.width;
    let height = disp.height;

    // ── Phase C: Receive render config ───────────────────────────────────
    let rcfg = device::phase_c(&ch);
    let scene_va = rcfg.scene_va;
    let content_va = rcfg.content_va;
    let content_size = rcfg.content_size;
    let scale_factor = rcfg.scale_factor;
    let pointer_state_va = rcfg.pointer_state_va;
    let font_size_cfg = rcfg.font_size_cfg;
    let frame_rate_cfg = rcfg.frame_rate_cfg;

    // ── Phase D: Metal pipeline setup ────────────────────────────────────
    // Allocate DMA buffers for command submission.
    // Setup buffer: 2 MiB (order 9) — enough for shader source + atlas upload + image textures.
    // Increased from 512 KiB to handle Content Region images (e.g., 800x537 BGRA = 1.6 MiB).
    let setup_dma = DmaBuf::alloc(9);
    // Render buffer: 64 KiB (order 4) — per-frame command buffer.
    let render_dma = DmaBuf::alloc(4);

    pipeline::setup_pipelines(
        &device,
        &mut setup_vq,
        irq_handle,
        &setup_dma,
        width,
        height,
    );

    // ── Glyph atlas initialization ──────────────────────────────────────
    // Heap-allocate atlas (~280 KiB: 2048 entries + 256 KiB pixel buffer).
    let atlas_layout = alloc::alloc::Layout::from_size_align(
        core::mem::size_of::<GlyphAtlas>(),
        core::mem::align_of::<GlyphAtlas>(),
    )
    .unwrap();
    let atlas_ptr = unsafe { alloc::alloc::alloc_zeroed(atlas_layout) as *mut GlyphAtlas };
    // SAFETY: atlas_ptr is valid, properly aligned, and zeroed. We must call
    // reset() because alloc_zeroed produces key=0 in every slot, but the
    // hash-map atlas uses u64::MAX as the empty sentinel.
    let glyph_atlas = unsafe { &mut *atlas_ptr };
    glyph_atlas.reset();

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
    let serif_font_slice: &[u8] = if let Some(h) = content_header {
        if let Some(entry) =
            protocol::content::find_entry(h, protocol::content::CONTENT_ID_FONT_SERIF)
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
    // Load italic font slices from Content Region.
    let load_font = |cid: u32| -> &[u8] {
        if let Some(h) = content_header {
            if let Some(entry) = protocol::content::find_entry(h, cid) {
                let start = entry.offset as usize;
                let end = start + entry.length as usize;
                if end <= content_size as usize {
                    // SAFETY: entry bounds validated; content_va is init-mapped.
                    return unsafe {
                        core::slice::from_raw_parts(
                            (content_va as usize + start) as *const u8,
                            entry.length as usize,
                        )
                    };
                }
            }
        }
        &[]
    };
    let mono_italic_slice = load_font(protocol::content::CONTENT_ID_FONT_MONO_ITALIC);
    let sans_italic_slice = load_font(protocol::content::CONTENT_ID_FONT_SANS_ITALIC);
    let serif_italic_slice = load_font(protocol::content::CONTENT_ID_FONT_SERIF_ITALIC);

    // Font data map for content_id-based lookup (used by pre-scan and style registry).
    let font_data_map: [FontDataEntry; 6] = [
        FontDataEntry {
            content_id: protocol::content::CONTENT_ID_FONT_MONO,
            data: font_slice,
        },
        FontDataEntry {
            content_id: protocol::content::CONTENT_ID_FONT_SANS,
            data: sans_font_slice,
        },
        FontDataEntry {
            content_id: protocol::content::CONTENT_ID_FONT_SERIF,
            data: serif_font_slice,
        },
        FontDataEntry {
            content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
            data: mono_italic_slice,
        },
        FontDataEntry {
            content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
            data: sans_italic_slice,
        },
        FontDataEntry {
            content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
            data: serif_italic_slice,
        },
    ];

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

    // Heap-allocate raster buffer (256x256 = 64 KiB) — too large for
    // the 64 KiB userspace stack. Kept alive for on-demand rasterization
    // in the per-frame pre-scan loop.
    let raster_buf_layout = alloc::alloc::Layout::from_size_align(256 * 256, 16).unwrap();
    let raster_buf_ptr = unsafe { alloc::alloc::alloc_zeroed(raster_buf_layout) };
    let raster_buf_slice: &mut [u8] =
        unsafe { core::slice::from_raw_parts_mut(raster_buf_ptr, 256 * 256) };

    if !font_slice.is_empty() {
        sys::print(b"     initializing glyph atlas\n");

        let ascii = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

        let mut packed = 0u32;
        let mut atlas_full_warned = false;

        // Pre-populate mono font (JetBrains Mono) ASCII glyphs (style_id = 0).
        {
            let font_data = font_slice;
            let shaped = fonts::shape(font_data, ascii, &[]);
            for sg in &shaped {
                if glyph_atlas
                    .lookup(sg.glyph_id, font_size_px as u16, 0)
                    .is_some()
                {
                    continue;
                }
                let mut rb = fonts::rasterize::RasterBuffer {
                    data: raster_buf_slice,
                    width: 256,
                    height: 256,
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
                        font_size_px as u16,
                        0,
                        m.width as u16,
                        m.height as u16,
                        m.bearing_x as i16,
                        m.bearing_y as i16,
                        &raster_buf_slice[..m.width as usize * m.height as usize],
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
        }

        // Pre-populate sans font (Inter) ASCII glyphs (style_id = 1).
        if !sans_font_slice.is_empty() {
            let sans_shaped = fonts::shape(sans_font_slice, ascii, &[]);
            for sg in &sans_shaped {
                if glyph_atlas
                    .lookup(sg.glyph_id, font_size_px as u16, 1)
                    .is_some()
                {
                    continue;
                }
                let mut rb = fonts::rasterize::RasterBuffer {
                    data: raster_buf_slice,
                    width: 256,
                    height: 256,
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
                        font_size_px as u16,
                        1,
                        m.width as u16,
                        m.height as u16,
                        m.bearing_x as i16,
                        m.bearing_y as i16,
                        &raster_buf_slice[..m.width as usize * m.height as usize],
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
        let mut cmdbuf = metal::CommandBuffer::new();
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

    let mut cmdbuf = metal::CommandBuffer::new();

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
        // Parse style registry from the scene data buffer (written by core).
        let style_registry = protocol::content::read_style_registry(data_buf).unwrap_or(&[]);

        if !font_slice.is_empty() {
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
                    font_size,
                    style_id,
                    ..
                } = node.content
                {
                    // Per-node font size in device pixels.
                    let node_font_size_px = round_font_size(font_size, scale_factor);

                    // Look up style in registry to find font data and axes.
                    let style_entry = style_registry.iter().find(|e| e.style_id == style_id);

                    // Get font data by content_id from style entry.
                    let raster_font = if let Some(entry) = style_entry {
                        find_font_data(entry.content_id, &font_data_map)
                    } else {
                        // Fallback: style_id 0 = mono, 1 = sans, else sans.
                        if style_id == 0 {
                            font_slice
                        } else {
                            sans_font_slice
                        }
                    };
                    if raster_font.is_empty() {
                        continue;
                    }

                    // Build axes from registry entry.
                    let mut axes_buf = [fonts::rasterize::AxisValue {
                        tag: [0; 4],
                        value: 0.0,
                    }; 8];
                    let axis_count =
                        style_entry.map_or(0, |e| (e.axis_count as usize).min(axes_buf.len()));
                    if let Some(entry) = style_entry {
                        let mut i = 0;
                        while i < axis_count {
                            axes_buf[i] = fonts::rasterize::AxisValue {
                                tag: entry.axes[i].tag,
                                value: entry.axes[i].value,
                            };
                            i += 1;
                        }
                    }
                    let axes = &axes_buf[..axis_count];

                    let shaped = reader.front_shaped_glyphs(glyphs, glyph_count);
                    for sg in shaped {
                        if glyph_atlas
                            .lookup(sg.glyph_id, node_font_size_px, style_id)
                            .is_some()
                        {
                            continue;
                        }
                        let mut rb = fonts::rasterize::RasterBuffer {
                            data: raster_buf_slice,
                            width: 256,
                            height: 256,
                        };
                        if let Some(m) = fonts::rasterize::rasterize_with_axes(
                            raster_font,
                            sg.glyph_id,
                            node_font_size_px,
                            &mut rb,
                            raster_scratch,
                            axes,
                            scale_factor_int,
                        ) {
                            let pack_y = glyph_atlas.row_y;
                            if glyph_atlas.pack(
                                sg.glyph_id,
                                node_font_size_px,
                                style_id,
                                m.width as u16,
                                m.height as u16,
                                m.bearing_x as i16,
                                m.bearing_y as i16,
                                &raster_buf_slice[..m.width as usize * m.height as usize],
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
        {
            let mut render_ctx = RenderContext {
                cmdbuf: &mut cmdbuf,
                solid_verts: &mut vertex_buf,
                glyph_verts: &mut glyph_vertex_buf,
                atlas: glyph_atlas,
                style_registry,
                scale_factor,
                blurs: &mut blurs,
                device: &device,
                setup_vq: &mut setup_vq,
                irq_handle,
                setup_dma: &setup_dma,
                path_buf,
                image_atlas: &mut image_atlas,
                content_region: content_slice,
                vw,
                vh,
                scale,
            };
            scene_walk::walk_scene(
                nodes,
                data_buf,
                &reader,
                root,
                0.0,
                0.0,
                &full_clip,
                &mut render_ctx,
            );
        }

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

        // ── Dither pass: float16 -> 8-bit sRGB drawable ─────────────────
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
        //   1. Compute: sRGB drawable -> linear RGBA16F (with padding)
        //   2. 3x box blur H+V passes in linear space (shared memory)
        //   3. Compute: linear RGBA16F -> sRGB drawable (center only)
        //   4. Render: semi-transparent bg overlay
        //
        // Uses W3C box_blur_widths for per-pass half-widths (CLT -> Gaussian).
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

            // Step 1: Convert padded region from sRGB drawable -> linear TEX_BLUR_A.
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

                // Horizontal blur: TEX_BLUR_A -> TEX_BLUR_B (threadgroup 256x1).
                cmdbuf.begin_compute_pass();
                cmdbuf.set_compute_pipeline(CPIPE_BLUR_H);
                cmdbuf.set_compute_texture(TEX_BLUR_A, 0);
                cmdbuf.set_compute_texture(TEX_BLUR_B, 1);
                cmdbuf.set_compute_bytes(0, &blur_params);
                cmdbuf.dispatch_threads(cap_w as u16, cap_h as u16, 1, 256, 1, 1);
                cmdbuf.end_compute_pass();

                // Vertical blur: TEX_BLUR_B -> TEX_BLUR_A (threadgroup 1x256).
                cmdbuf.begin_compute_pass();
                cmdbuf.set_compute_pipeline(CPIPE_BLUR_V);
                cmdbuf.set_compute_texture(TEX_BLUR_B, 0);
                cmdbuf.set_compute_texture(TEX_BLUR_A, 1);
                cmdbuf.set_compute_bytes(0, &blur_params);
                cmdbuf.dispatch_threads(cap_w as u16, cap_h as u16, 1, 1, 256, 1);
                cmdbuf.end_compute_pass();
            }

            // Step 3: Convert center (unpadded) region from linear -> sRGB drawable.
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
