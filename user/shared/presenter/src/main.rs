//! Presenter — compiles document state + layout into a scene graph.
//!
//! The OS Service from architecture.md. Reads the document buffer (RO),
//! writes viewport state for the layout service, reads layout results,
//! and builds a scene graph tree (root → viewport → line glyphs + cursor).
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;
extern crate piecetable;

mod build;
mod handlers;
mod input;
mod pointer;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};
use scene::{SCENE_SIZE, SceneWriter, ShapedGlyph};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_RTC_VMO: Handle = Handle(3);
const HANDLE_FONT_VMO: Handle = Handle(4);
const HANDLE_SVC_EP: Handle = Handle(5);

const PAGE_SIZE: usize = 16384;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE201;
const EXIT_DOC_NOT_FOUND: u32 = 0xE202;
const EXIT_DOC_SETUP: u32 = 0xE203;
const EXIT_LAYOUT_NOT_FOUND: u32 = 0xE205;
const EXIT_VIEWPORT_CREATE: u32 = 0xE206;
const EXIT_VIEWPORT_MAP: u32 = 0xE207;
const EXIT_LAYOUT_SETUP: u32 = 0xE208;
const EXIT_SCENE_CREATE: u32 = 0xE20A;
const EXIT_SCENE_MAP: u32 = 0xE20B;
const EXIT_RENDER_NOT_FOUND: u32 = 0xE20D;
const EXIT_RENDER_SETUP: u32 = 0xE20E;
const EXIT_EDITOR_NOT_FOUND: u32 = 0xE20F;

pub(crate) fn read_rtc_seconds(rtc_va: usize) -> u64 {
    if rtc_va == 0 {
        return 0;
    }

    // SAFETY: rtc_va is a valid device VMO mapping of the PL031 RTC.
    // Register 0 (RTCDR) contains the current time as a Unix epoch.
    let val = unsafe { core::ptr::read_volatile(rtc_va as *const u32) };

    val as u64
}

// ── Font data — packed style IDs ──────────────────────────────────
//
// style_id encodes font_family, weight, and flags in a single u32
// so the compositor can rasterize with the correct variable font axes.
//   bits [0..2)  = font_family (0=mono, 1=sans, 2=serif)
//   bits [2..16) = weight (100-900)
//   bits [16..19) = flags (bit 0=italic)

pub(crate) const STYLE_MONO: u32 = 0;
pub(crate) const STYLE_SANS: u32 = 1;
#[allow(dead_code)]
pub(crate) const STYLE_SERIF: u32 = 2;

pub(crate) fn pack_style_id(family: u32, weight: u16, flags: u8) -> u32 {
    (family & 0x3) | ((weight as u32) << 2) | ((flags as u32 & 0x7) << 16)
}

static mut FONT_VA: usize = 0;

pub(crate) fn font(index: usize) -> &'static [u8] {
    // SAFETY: FONT_VA is set once in _start before any use, and the
    // mapping persists for the process lifetime. Single-threaded.
    unsafe { init::font_data(FONT_VA, index) }
}

fn build_cmap_table(font_data: &[u8]) -> [u16; 128] {
    let mut table = [0u16; 128];

    for ch in 0u8..128 {
        table[ch as usize] = fonts::metrics::glyph_id_for_char(font_data, ch as char).unwrap_or(0);
    }

    table
}

fn compute_char_advance_mpt(font_data: &[u8]) -> scene::Mpt {
    let gid = fonts::metrics::glyph_id_for_char(font_data, 'M').unwrap_or(0);

    if let (Some((advance_fu, _)), Some(fm)) = (
        fonts::metrics::glyph_h_metrics(font_data, gid),
        fonts::metrics::font_metrics(font_data),
    ) {
        return (advance_fu as i64 * presenter_service::FONT_SIZE as i64 * scene::MPT_PER_PT as i64
            / fm.units_per_em as i64) as scene::Mpt;
    }

    // 10.8pt fallback ≈ 11059 mpt (10 * 1024 + 819)
    11059
}

pub(crate) fn shape_text(
    font_data: &[u8],
    text: &str,
    font_size: u16,
    features: &[fonts::Feature],
    out: &mut [ShapedGlyph],
) -> (usize, scene::Mpt) {
    let upem = fonts::metrics::font_metrics(font_data)
        .map(|m| m.units_per_em)
        .unwrap_or(1000) as i64;
    let shaped = fonts::shape(font_data, text, features);
    let count = shaped.len().min(out.len());
    let fs = font_size as i64;
    let mut total_advance_fp: i64 = 0;

    for (i, sg) in shaped.iter().take(count).enumerate() {
        let adv = (sg.x_advance as i64 * fs * 65536 / upem) as i32;

        out[i] = ShapedGlyph {
            glyph_id: sg.glyph_id,
            _pad: 0,
            x_advance: adv,
            x_offset: (sg.x_offset as i64 * fs * 65536 / upem) as i32,
            y_offset: (sg.y_offset as i64 * fs * 65536 / upem) as i32,
        };
        total_advance_fp += adv as i64;
    }

    (count, (total_advance_fp / 64) as scene::Mpt)
}

pub(crate) fn copy_into(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());

    dst[..len].copy_from_slice(&src[..len]);

    len
}

// ── Layout results parsing (from seqlock-read buffer) ────────────

pub(crate) fn parse_layout_header(buf: &[u8]) -> layout_service::LayoutHeader {
    layout_service::LayoutHeader::read_from(buf)
}

pub(crate) struct LineInfoMpt {
    pub byte_offset: u32,
    pub byte_length: u32,
    pub x_mpt: scene::Mpt,
    pub y: i32,
    pub width_mpt: scene::Mpt,
}

pub(crate) fn parse_line_at(buf: &[u8], index: usize) -> LineInfoMpt {
    let offset = layout_service::LayoutHeader::SIZE + index * layout_service::LineInfo::SIZE;
    let b = &buf[offset..];

    LineInfoMpt {
        byte_offset: u32::from_le_bytes(b[0..4].try_into().unwrap()),
        byte_length: u32::from_le_bytes(b[4..8].try_into().unwrap()),
        x_mpt: f32_bytes_to_mpt(b[8..12].try_into().unwrap()),
        y: i32::from_le_bytes(b[12..16].try_into().unwrap()),
        width_mpt: f32_bytes_to_mpt(b[16..20].try_into().unwrap()),
    }
}

fn f32_bytes_to_mpt(bytes: [u8; 4]) -> scene::Mpt {
    let bits = u32::from_le_bytes(bytes);

    if bits & 0x7FFF_FFFF == 0 {
        return 0;
    }

    let sign: i32 = if bits >> 31 != 0 { -1 } else { 1 };
    let exp = ((bits >> 23) & 0xFF) as i32;

    if exp == 0 || exp == 255 {
        return 0;
    }

    let frac = (bits & 0x7F_FFFF) as i64 | 0x80_0000;
    let shifted = frac * scene::MPT_PER_PT as i64;
    let shift = exp - 150;
    let result = if shift >= 0 {
        (shifted << shift.min(40)) as i32
    } else {
        (shifted >> (-shift).min(40)) as i32
    };

    sign * result
}

pub(crate) fn parse_visible_run_at(buf: &[u8], index: usize) -> layout_service::VisibleRun {
    let offset = layout_service::VISIBLE_RUNS_OFFSET + index * layout_service::VisibleRun::SIZE;

    layout_service::VisibleRun::read_from(&buf[offset..])
}

pub(crate) fn font_data_for_style(family: u8, flags: u8) -> &'static [u8] {
    let italic = flags & piecetable::FLAG_ITALIC != 0;

    match family {
        piecetable::FONT_MONO => {
            if italic {
                font(init::FONT_IDX_MONO_ITALIC)
            } else {
                font(init::FONT_IDX_MONO)
            }
        }
        piecetable::FONT_SERIF => {
            if italic {
                font(init::FONT_IDX_SERIF_ITALIC)
            } else {
                font(init::FONT_IDX_SERIF)
            }
        }
        _ => {
            if italic {
                font(init::FONT_IDX_SANS_ITALIC)
            } else {
                font(init::FONT_IDX_SANS)
            }
        }
    }
}

pub(crate) fn style_id_for_family(family: u8) -> u32 {
    match family {
        piecetable::FONT_MONO => STYLE_MONO,
        piecetable::FONT_SERIF => STYLE_SERIF,
        _ => STYLE_SANS,
    }
}

pub(crate) fn pack_run_style_id(family: u8, weight: u16, flags: u8) -> u32 {
    pack_style_id(style_id_for_family(family), weight, flags)
}

// Space enum eliminated — child viewers owned by WorkspaceViewer.

// ── Presenter server ──────────────────────────────────────────────

pub(crate) struct Presenter {
    pub(crate) doc_va: usize,
    pub(crate) doc_ep: Handle,
    pub(crate) layout_ep: Handle,

    pub(crate) results_reader: ipc::register::Reader,
    pub(crate) results_buf: [u8; layout_service::RESULTS_VALUE_SIZE],

    pub(crate) scene_bufs: [&'static mut [u8]; 2],
    pub(crate) scene_vmos: [Handle; 2],
    pub(crate) swap_va: usize,
    pub(crate) swap_gen: u32,

    pub(crate) viewport_va: usize,

    pub(crate) display_width: u32,
    pub(crate) display_height: u32,

    pub(crate) workspace: handlers::WorkspaceViewer,

    pub(crate) last_line_count: u32,
    pub(crate) last_cursor_line: u32,
    pub(crate) last_cursor_col: u32,
    pub(crate) last_content_len: u32,
    pub(crate) last_clock_secs: u64,

    pub(crate) sticky_col: Option<u32>,

    pub(crate) render_ep: Handle,
    pub(crate) editor_ep: Handle,

    pub(crate) rtc_va: usize,

    pub(crate) pointer_x: i32,
    pub(crate) pointer_y: i32,
    pub(crate) cursor_shape_name: u8,

    pub(crate) last_click_ms: u64,
    pub(crate) last_click_x: u32,
    pub(crate) last_click_y: u32,
    pub(crate) click_count: u8,
    pub(crate) dragging: bool,
    pub(crate) drag_origin_start: usize,
    pub(crate) drag_origin_end: usize,

    pub(crate) frame_stats: FrameStats,

    pub(crate) audio_ep: Handle,
    pub(crate) audio_vmo: Handle,
    pub(crate) audio_data_len: u32,

    pub(crate) console_ep: Handle,
    pub(crate) layout_dirty: bool,
}

impl Presenter {
    #[allow(dead_code)]
    pub(crate) fn active_space(&self) -> usize {
        self.workspace.active
    }

    pub(crate) fn text_viewer(&self) -> Option<&handlers::TextViewer> {
        for child in &self.workspace.children {
            if let handlers::ViewerKind::Text(v) = &child.viewer {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn text_viewer_mut(&mut self) -> Option<&mut handlers::TextViewer> {
        for child in &mut self.workspace.children {
            if let handlers::ViewerKind::Text(v) = &mut child.viewer {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn active_is_text(&self) -> bool {
        self.workspace
            .children
            .get(self.workspace.active)
            .is_some_and(|c| matches!(c.viewer, handlers::ViewerKind::Text(_)))
    }

    pub(crate) fn active_is_video(&self) -> bool {
        self.workspace
            .children
            .get(self.workspace.active)
            .is_some_and(|c| matches!(c.viewer, handlers::ViewerKind::Video(_)))
    }

    pub(crate) fn scroll_y(&self) -> i32 {
        self.text_viewer().map_or(0, |v| v.scroll_y)
    }

    pub(crate) fn set_scroll_y(&mut self, y: i32) {
        if let Some(v) = self.text_viewer_mut() {
            v.scroll_y = y;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn blink_start(&self) -> u64 {
        self.text_viewer().map_or(0, |v| v.blink_start)
    }

    pub(crate) fn set_blink_start(&mut self, t: u64) {
        if let Some(v) = self.text_viewer_mut() {
            v.blink_start = t;
        }
    }

    pub(crate) fn char_width_mpt(&self) -> scene::Mpt {
        self.text_viewer().map_or(11059, |v| v.char_width_mpt)
    }
}

pub(crate) struct FrameStats {
    pub(crate) frame_count: u32,
    pub(crate) total_ns: u64,
    pub(crate) min_ns: u64,
    pub(crate) max_ns: u64,
}

impl FrameStats {
    const fn new() -> Self {
        Self {
            frame_count: 0,
            total_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
        }
    }

    fn record(&mut self, ns: u64) {
        self.frame_count += 1;
        self.total_ns += ns;

        if ns < self.min_ns {
            self.min_ns = ns;
        }
        if ns > self.max_ns {
            self.max_ns = ns;
        }
    }

    fn report(&self, console_ep: Handle) {
        if self.frame_count == 0 {
            return;
        }

        let avg_us = (self.total_ns / self.frame_count as u64) / 1000;
        let min_us = self.min_ns / 1000;
        let max_us = self.max_ns / 1000;
        let fps = 1_000_000u64.checked_div(avg_us).unwrap_or(0);
        let mut buf = [0u8; 80];
        let mut pos = 0;

        pos += copy_into(&mut buf[pos..], b"frame: ");
        pos += console::format_u32(self.frame_count, &mut buf[pos..]);
        pos += copy_into(&mut buf[pos..], b"f avg=");
        pos += console::format_u32(avg_us as u32, &mut buf[pos..]);
        pos += copy_into(&mut buf[pos..], b"us min=");
        pos += console::format_u32(min_us as u32, &mut buf[pos..]);
        pos += copy_into(&mut buf[pos..], b"us max=");
        pos += console::format_u32(max_us as u32, &mut buf[pos..]);
        pos += copy_into(&mut buf[pos..], b"us ~");
        pos += console::format_u32(fps as u32, &mut buf[pos..]);
        pos += copy_into(&mut buf[pos..], b"fps\n");

        console::write(console_ep, &buf[..pos]);
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Presenter {
    fn read_active_index(&self) -> usize {
        scene::SceneSwapHeader::read_active_index(self.swap_va)
    }

    fn swap_scene(&mut self) {
        let active = self.read_active_index();
        let back = 1 - active;

        self.swap_gen = self.swap_gen.wrapping_add(1);

        scene::SceneSwapHeader::swap(self.swap_va, self.swap_gen, back as u32);
    }

    fn text_area_dims(&self) -> (u32, u32) {
        let content_h = self
            .display_height
            .saturating_sub(presenter_service::TITLE_BAR_H);
        let page_h = content_h.saturating_sub(2 * presenter_service::PAGE_MARGIN_V);
        let page_w = ((page_h as u64 * 210 / 297) as u32).min(
            self.display_width
                .saturating_sub(2 * presenter_service::PAGE_MARGIN_V),
        );
        let pad = presenter_service::PAGE_PADDING;

        (
            page_w.saturating_sub(2 * pad),
            page_h.saturating_sub(2 * pad),
        )
    }

    fn viewport_height(&self) -> i32 {
        self.text_area_dims().1 as i32
    }

    fn ensure_cursor_visible(&mut self, cursor_line: u32) {
        let line_h = presenter_service::LINE_HEIGHT as i32;
        let cursor_top = cursor_line as i32 * line_h;
        let vp_h = self.viewport_height();
        let sy = self.scroll_y();

        if cursor_top < sy {
            self.set_scroll_y(cursor_top);
        } else if cursor_top + line_h > sy + vp_h {
            self.set_scroll_y(cursor_top + line_h - vp_h);
        }
    }

    fn ensure_cursor_visible_at(&mut self, cursor_y: i32, cursor_h: i32) {
        let vp_h = self.viewport_height();
        let sy = self.scroll_y();

        if cursor_y < sy {
            self.set_scroll_y(cursor_y);
        } else if cursor_y + cursor_h > sy + vp_h {
            self.set_scroll_y(cursor_y + cursor_h - vp_h);
        }
    }

    fn clamp_scroll(&mut self) {
        let header = parse_layout_header(&self.results_buf);
        let total_h = header.total_height;
        let vp_h = self.viewport_height();
        let max_scroll = if total_h > vp_h { total_h - vp_h } else { 0 };
        let mut sy = self.scroll_y();

        if sy < 0 {
            sy = 0;
        }

        if sy > max_scroll {
            sy = max_scroll;
        }

        self.set_scroll_y(sy);
    }

    fn write_viewport(&self) {
        let (tw, th) = self.text_area_dims();
        let state = layout_service::ViewportState {
            scroll_y: self.scroll_y(),
            viewport_width: tw,
            viewport_height: th,
            char_width_fp: (self.char_width_mpt() as u32) * 64,
            line_height: presenter_service::LINE_HEIGHT,
        };
        let mut buf = [0u8; layout_service::ViewportState::SIZE];

        state.write_to(&mut buf);

        let mut writer = unsafe {
            ipc::register::Writer::new(
                self.viewport_va as *mut u8,
                layout_service::ViewportState::SIZE,
            )
        };

        writer.write(&buf);
    }
}

// ── Dispatch ──────────────────────────────────────────────────────

impl Dispatch for Presenter {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            presenter_service::SETUP => {
                let active = self.read_active_index();

                match abi::handle::dup(self.scene_vmos[active], Rights::READ_MAP) {
                    Ok(dup) => {
                        let reply = presenter_service::SetupReply {
                            display_width: self.display_width,
                            display_height: self.display_height,
                        };
                        let mut data = [0u8; presenter_service::SetupReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[dup.0]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }
            presenter_service::BUILD => {
                self.layout_dirty = true;
                self.build_scene();

                let reply = self.make_info_reply();
                let mut data = [0u8; presenter_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            presenter_service::GET_INFO => {
                let reply = self.make_info_reply();
                let mut data = [0u8; presenter_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            presenter_service::KEY_EVENT => {
                if msg.payload.len() >= text_editor::KeyDispatch::SIZE {
                    let dispatch = text_editor::KeyDispatch::read_from(msg.payload);

                    self.handle_key_event(dispatch);
                }

                let _ = msg.reply_empty();
            }
            presenter_service::SCROLL_EVENT => {
                if msg.payload.len() >= presenter_service::ScrollEvent::SIZE {
                    let event = presenter_service::ScrollEvent::read_from(msg.payload);

                    self.set_scroll_y(self.scroll_y() + event.delta_y);
                    self.clamp_scroll();
                    self.write_viewport();
                    self.layout_dirty = true;
                    self.build_scene();
                }

                let _ = msg.reply_empty();
            }
            presenter_service::POINTER_EVENT => {
                if msg.payload.len() >= presenter_service::PointerEvent::SIZE {
                    let event = presenter_service::PointerEvent::read_from(msg.payload);
                    let px = (event.abs_x as u64 * self.display_width as u64 / 32768) as i32;
                    let py = (event.abs_y as u64 * self.display_height as u64 / 32768) as i32;

                    self.pointer_x = px;
                    self.pointer_y = py;

                    let mut payload = [0u8; 8];

                    payload[0..4].copy_from_slice(&px.to_le_bytes());
                    payload[4..8].copy_from_slice(&py.to_le_bytes());

                    let _ =
                        ipc::client::call_simple(self.render_ep, render::comp::POINTER, &payload);

                    self.update_cursor_shape();
                    self.handle_pointer_drag(event.abs_x, event.abs_y);
                }

                let _ = msg.reply_empty();
            }
            presenter_service::POINTER_BUTTON => {
                if msg.payload.len() >= presenter_service::PointerButton::SIZE {
                    let btn = presenter_service::PointerButton::read_from(msg.payload);

                    self.handle_pointer_button(btn);
                }

                let _ = msg.reply_empty();
            }
            presenter_service::VIDEO_PLAYBACK_ENDED => {
                if let Some(child) = self.workspace.children.get_mut(self.workspace.active)
                    && let handlers::ViewerKind::Video(vid) = &mut child.viewer
                {
                    vid.playing = false;
                }

                self.build_scene();

                let _ = msg.reply_empty();
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

// ── Store helper ─────────────────────────────────────────────────

fn store_load_type(media_type: &[u8]) -> Option<(Handle, usize)> {
    let store_ep = name::lookup(HANDLE_NS_EP, b"store").ok()?;
    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let mut req_buf = [0u8; ipc::message::MAX_PAYLOAD];
    let qt_req = store_service::QueryTypeRequest {
        media_type_len: media_type.len() as u16,
    };

    qt_req.write_to(&mut req_buf);
    req_buf[store_service::QueryTypeRequest::SIZE..][..media_type.len()]
        .copy_from_slice(media_type);

    let payload_len = store_service::QueryTypeRequest::SIZE + media_type.len();
    let mut handles = [0u32; 4];
    let reply = ipc::client::call(
        store_ep,
        store_service::LOAD_TYPE,
        &req_buf[..payload_len],
        &[],
        &mut handles,
        &mut call_buf,
    )
    .ok()?;

    if reply.is_error()
        || reply.payload.len() < store_service::QueryTypeReply::SIZE
        || handles[0] == 0
    {
        return None;
    }

    let qt = store_service::QueryTypeReply::read_from(reply.payload);

    Some((Handle(handles[0]), qt.size as usize))
}

// ── Image loading from store + decode + upload ──────────────────

static NEXT_CONTENT_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);

fn alloc_content_id() -> u32 {
    NEXT_CONTENT_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

fn load_image_child(render_ep: Handle, console_ep: Handle) -> Option<handlers::ChildViewer> {
    let (file_vmo, bytes_read) = store_load_type(b"image/jpeg")?;
    let decoder_ep = match name::lookup(HANDLE_NS_EP, b"jpeg-decoder") {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::handle::close(file_vmo);

            return None;
        }
    };
    let decode_req = jpeg_decoder::DecodeRequest {
        file_size: bytes_read as u32,
    };
    let mut decode_buf = [0u8; jpeg_decoder::DecodeRequest::SIZE];

    decode_req.write_to(&mut decode_buf);

    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let mut decode_handles = [0u32; 4];
    let decode_reply = match ipc::client::call(
        decoder_ep,
        jpeg_decoder::DECODE,
        &decode_buf,
        &[file_vmo.0],
        &mut decode_handles,
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return None,
    };

    if decode_reply.is_error()
        || decode_reply.payload.len() < jpeg_decoder::DecodeReply::SIZE
        || decode_reply.handle_count == 0
    {
        return None;
    }

    let dr = jpeg_decoder::DecodeReply::read_from(decode_reply.payload);
    let pixel_vmo = Handle(decode_handles[0]);
    let content_id = alloc_content_id();
    let width = dr.width as u16;
    let height = dr.height as u16;
    let pixel_size = dr.pixel_size;
    let pixel_dup = match abi::handle::dup(pixel_vmo, Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => return None,
    };

    upload_image_to_compositor(
        render_ep, pixel_dup, content_id, width, height, pixel_size, 0,
    );

    console::write(console_ep, b"presenter: image loaded from store\n");

    Some(handlers::ChildViewer {
        viewer: handlers::ViewerKind::Image(handlers::ImageViewer::new(content_id, width, height)),
        mimetype: b"image/jpeg",
    })
}

fn upload_image_to_compositor(
    render_ep: Handle,
    pixel_vmo: Handle,
    content_id: u32,
    width: u16,
    height: u16,
    pixel_size: u32,
    flags: u32,
) {
    let upload_req = render::comp::UploadImageRequest {
        content_id,
        width,
        height,
        pixel_size,
        flags,
    };
    let mut upload_buf = [0u8; render::comp::UploadImageRequest::SIZE];

    upload_req.write_to(&mut upload_buf);

    let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
    let mut upload_handles = [0u32; 4];
    let _ = ipc::client::call(
        render_ep,
        render::comp::UPLOAD_IMAGE,
        &upload_buf,
        &[pixel_vmo.0],
        &mut upload_handles,
        &mut reply_buf,
    );
}

// ── Video loading from document store ───────────────────────────

fn load_video_child(render_ep: Handle, console_ep: Handle) -> Option<handlers::ChildViewer> {
    try_open_video(render_ep, console_ep, b"video/mp4")
        .or_else(|| try_open_video(render_ep, console_ep, b"video/avi"))
}

fn try_open_video(
    render_ep: Handle,
    console_ep: Handle,
    media_type: &'static [u8],
) -> Option<handlers::ChildViewer> {
    let (file_vmo, bytes_read) = store_load_type(media_type)?;
    let decoder_ep = match name::lookup(HANDLE_NS_EP, b"video-decoder") {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::handle::close(file_vmo);

            return None;
        }
    };
    let open_req = video_decoder::OpenRequest {
        file_size: bytes_read as u32,
    };
    let mut open_buf = [0u8; video_decoder::OpenRequest::SIZE];

    open_req.write_to(&mut open_buf);

    let notify_ep = match abi::handle::dup(HANDLE_SVC_EP, Rights(Rights::READ.0 | Rights::WRITE.0))
    {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::handle::close(file_vmo);
            let _ = abi::handle::close(decoder_ep);

            return None;
        }
    };

    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let mut open_handles = [0u32; 4];
    let open_reply = match ipc::client::call(
        decoder_ep,
        video_decoder::OPEN,
        &open_buf,
        &[file_vmo.0, notify_ep.0],
        &mut open_handles,
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return None,
    };

    if open_reply.is_error()
        || open_reply.payload.len() < video_decoder::OpenReply::SIZE
        || open_handles[0] == 0
    {
        return None;
    }

    let or = video_decoder::OpenReply::read_from(open_reply.payload);
    let frame_vmo = Handle(open_handles[0]);

    if or.total_frames == 0 {
        let _ = abi::handle::close(frame_vmo);

        return None;
    }

    let content_id = alloc_content_id();
    let width = or.width as u16;
    let height = or.height as u16;
    let host_texture_handle = or.host_texture_handle;
    let frame_va = match abi::vmo::map(frame_vmo, 0, Rights(Rights::READ.0 | Rights::MAP.0)) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::handle::close(decoder_ep);
            let _ = abi::handle::close(frame_vmo);

            return None;
        }
    };

    if host_texture_handle != 0 {
        // Zero-copy: bind the host's IOSurface-backed texture directly.
        // The frame VMO is kept as a 16-byte signal buffer (gen counter + flags).
        let frame_dup = match abi::handle::dup(frame_vmo, Rights(Rights::READ.0 | Rights::MAP.0)) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::handle::close(decoder_ep);
                let _ = abi::vmo::unmap(frame_va);
                let _ = abi::handle::close(frame_vmo);

                return None;
            }
        };
        let bind_req = render::comp::BindHostTextureRequest {
            content_id,
            host_handle: host_texture_handle,
            width,
            height,
        };
        let mut bind_buf = [0u8; render::comp::BindHostTextureRequest::SIZE];

        bind_req.write_to(&mut bind_buf);

        let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
        let mut bind_handles = [0u32; 4];
        let _ = ipc::client::call(
            render_ep,
            render::comp::BIND_HOST_TEXTURE,
            &bind_buf,
            &[frame_dup.0],
            &mut bind_handles,
            &mut reply_buf,
        );
    } else {
        // Fallback: pixel upload path for codecs without host textures.
        let frame_dup = match abi::handle::dup(frame_vmo, Rights(Rights::READ.0 | Rights::MAP.0)) {
            Ok(h) => h,
            Err(_) => {
                let _ = abi::handle::close(decoder_ep);
                let _ = abi::vmo::unmap(frame_va);
                let _ = abi::handle::close(frame_vmo);

                return None;
            }
        };
        let pixel_size = width as u32 * height as u32 * 4;

        upload_image_to_compositor(
            render_ep,
            frame_dup,
            content_id,
            width,
            height,
            pixel_size,
            render::comp::IMAGE_FLAG_LIVE,
        );
    }

    console::write(console_ep, b"presenter: video loaded from store\n");

    let mut vid = handlers::VideoViewer::new(content_id, width, height);

    vid.decoder_ep = decoder_ep;
    vid.frame_vmo = frame_vmo;
    vid.frame_va = frame_va;
    vid.total_frames = or.total_frames;
    vid.playing = false;

    Some(handlers::ChildViewer {
        viewer: handlers::ViewerKind::Video(vid),
        mimetype: media_type,
    })
}

// ── Audio clip loading from store ─────────────────────────────────

fn load_audio_clip(server: &mut Presenter) {
    let (data_vmo, bytes_read) = match store_load_type(b"audio/wav") {
        Some(r) => r,
        None => return,
    };
    let audio_ep = match name::lookup(HANDLE_NS_EP, b"audio") {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::handle::close(data_vmo);

            return;
        }
    };

    server.audio_ep = audio_ep;
    server.audio_vmo = data_vmo;
    server.audio_data_len = bytes_read as u32;

    console::write(server.console_ep, b"presenter: audio clip loaded\n");
}

// ── Audio playback ────────────────────────────────────────

impl Presenter {
    fn play_audio_clip(&self) {
        if self.audio_ep.0 == 0 || self.audio_vmo.0 == 0 {
            return;
        }

        let ro = Rights(Rights::READ.0 | Rights::MAP.0);
        let vmo_dup = match abi::handle::dup(self.audio_vmo, ro) {
            Ok(h) => h,
            Err(_) => return,
        };
        let req = audio_service::PlayRequest {
            format: audio_service::FORMAT_WAV,
            data_len: self.audio_data_len,
            data_offset: 0,
        };
        let mut payload = [0u8; audio_service::PlayRequest::SIZE];

        req.write_to(&mut payload);

        let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
        let _ = ipc::client::call(
            self.audio_ep,
            audio_service::PLAY,
            &payload,
            &[vmo_dup.0],
            &mut [],
            &mut reply_buf,
        );
    }

    fn toggle_video_playback(&mut self) {
        let active = self.workspace.active;

        if let Some(child) = self.workspace.children.get_mut(active) {
            if let handlers::ViewerKind::Video(vid) = &mut child.viewer {
                if vid.decoder_ep.0 == 0 || vid.total_frames == 0 {
                    return;
                }

                if let Ok((0, payload)) =
                    ipc::client::call_simple(vid.decoder_ep, video_decoder::TOGGLE, &[])
                {
                    let reply = video_decoder::ToggleReply::read_from(&payload);

                    vid.playing = reply.playing != 0;
                }
            } else {
                return;
            }
        } else {
            return;
        }

        self.build_scene();
    }
}

// ── Entry point ───────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_NOT_FOUND),
    };

    console::write(console_ep, b"presenter: starting\n");

    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let rtc_va = abi::vmo::map(HANDLE_RTC_VMO, 0, rw).unwrap_or(0);

    // SAFETY: single-threaded, set once before any font access.
    unsafe {
        FONT_VA = abi::vmo::map(HANDLE_FONT_VMO, 0, Rights::READ_MAP).unwrap_or(0);
    }
    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: doc not found\n");

            abi::thread::exit(EXIT_DOC_NOT_FOUND);
        }
    };
    let doc_va =
        match ipc::client::setup_map_vmo(doc_ep, document_service::SETUP, &[], Rights::READ_MAP) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_DOC_SETUP),
        };

    console::write(console_ep, b"presenter: doc connected\n");

    let layout_ep = match name::watch(HANDLE_NS_EP, b"layout") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: layout not found\n");

            abi::thread::exit(EXIT_LAYOUT_NOT_FOUND);
        }
    };
    let viewport_vmo_size = ipc::register::required_size(layout_service::ViewportState::SIZE)
        .next_multiple_of(PAGE_SIZE);
    let viewport_vmo = match abi::vmo::create(viewport_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let viewport_va = match abi::vmo::map(viewport_vmo, 0, Rights::READ_WRITE_MAP) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_MAP),
    };

    ipc::register::init(viewport_va as *mut u8);

    let viewport_dup = match abi::handle::dup(viewport_vmo, Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let results_va = match ipc::client::setup_map_vmo(
        layout_ep,
        layout_service::SETUP,
        &[viewport_dup.0],
        Rights::READ_MAP,
    ) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_LAYOUT_SETUP),
    };

    console::write(console_ep, b"presenter: layout connected\n");

    // SAFETY: results_va is a valid RO mapping of the layout results
    // VMO, 8-byte aligned, at least RESULTS_VALUE_SIZE + HEADER_SIZE
    // bytes. The layout service is the sole writer.
    let results_reader = unsafe {
        ipc::register::Reader::new(results_va as *const u8, layout_service::RESULTS_VALUE_SIZE)
    };
    // Double-buffered scene graph: swap header VMO + 2 scene buffer VMOs.
    let swap_vmo = match abi::vmo::create(PAGE_SIZE, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_SCENE_CREATE),
    };
    let swap_va = match abi::vmo::map(swap_vmo, 0, Rights::READ_WRITE_MAP) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
    };

    // SAFETY: swap_va is page-aligned, at least 16 bytes. Zero-init sets
    // active_index=0, generation=0.
    unsafe {
        core::ptr::write_bytes(swap_va as *mut u8, 0, 16);
    }

    let scene_vmo_size = SCENE_SIZE.next_multiple_of(PAGE_SIZE);
    let scene_vmos = [
        match abi::vmo::create(scene_vmo_size, 0) {
            Ok(h) => h,
            Err(_) => abi::thread::exit(EXIT_SCENE_CREATE),
        },
        match abi::vmo::create(scene_vmo_size, 0) {
            Ok(h) => h,
            Err(_) => abi::thread::exit(EXIT_SCENE_CREATE),
        },
    ];
    let scene_vas = [
        match abi::vmo::map(scene_vmos[0], 0, Rights::READ_WRITE_MAP) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
        },
        match abi::vmo::map(scene_vmos[1], 0, Rights::READ_WRITE_MAP) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
        },
    ];
    // SAFETY: scene_vas are valid RW mappings of at least SCENE_SIZE bytes.
    let scene_bufs: [&'static mut [u8]; 2] = unsafe {
        [
            core::slice::from_raw_parts_mut(scene_vas[0] as *mut u8, SCENE_SIZE),
            core::slice::from_raw_parts_mut(scene_vas[1] as *mut u8, SCENE_SIZE),
        ]
    };
    let _ = SceneWriter::new(scene_bufs[0]);
    let _ = SceneWriter::new(scene_bufs[1]);
    // Connect to compositor — send swap header + both scene buffer VMOs.
    let render_ep = match name::watch(HANDLE_NS_EP, b"render") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: render not found\n");

            abi::thread::exit(EXIT_RENDER_NOT_FOUND);
        }
    };
    let swap_dup = match abi::handle::dup(swap_vmo, Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RENDER_SETUP),
    };
    let scene_dup0 = match abi::handle::dup(scene_vmos[0], Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RENDER_SETUP),
    };
    let scene_dup1 = match abi::handle::dup(scene_vmos[1], Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RENDER_SETUP),
    };
    let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
    let mut recv_handles = [0u32; 4];
    let (display_width, display_height, refresh_hz) = match ipc::client::call(
        render_ep,
        render::comp::SETUP,
        &[],
        &[swap_dup.0, scene_dup0.0, scene_dup1.0],
        &mut recv_handles,
        &mut reply_buf,
    ) {
        Ok(reply) if !reply.is_error() && reply.payload.len() >= render::comp::SetupReply::SIZE => {
            let sr = render::comp::SetupReply::read_from(reply.payload);

            (sr.display_width, sr.display_height, sr.refresh_hz)
        }
        _ => (
            presenter_service::DEFAULT_WIDTH,
            presenter_service::DEFAULT_HEIGHT,
            60,
        ),
    };
    let frame_ns: u64 = 1_000_000_000 / refresh_hz as u64;

    console::write(console_ep, b"presenter: render connected\n");

    let editor_ep = match name::watch(HANDLE_NS_EP, b"editor.text") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: editor not found\n");

            abi::thread::exit(EXIT_EDITOR_NOT_FOUND);
        }
    };

    console::write(console_ep, b"presenter: editor connected\n");

    let mut workspace = handlers::WorkspaceViewer::new();
    let mut text_viewer = handlers::TextViewer::new(
        build_cmap_table(font(init::FONT_IDX_MONO)),
        build_cmap_table(font(init::FONT_IDX_SANS)),
        compute_char_advance_mpt(font(init::FONT_IDX_MONO)),
    );

    text_viewer.blink_start = abi::system::clock_read().unwrap_or(0);

    workspace.children.push(handlers::ChildViewer {
        viewer: handlers::ViewerKind::Text(text_viewer),
        mimetype: b"text/rich",
    });

    if let Some(image_child) = load_image_child(render_ep, console_ep) {
        workspace.children.push(image_child);
    }
    if let Some(video_child) = load_video_child(render_ep, console_ep) {
        workspace.children.push(video_child);
    }

    let mut server = Presenter {
        doc_va,
        doc_ep,
        layout_ep,
        results_reader,
        results_buf: [0u8; layout_service::RESULTS_VALUE_SIZE],
        scene_bufs,
        scene_vmos,
        swap_va,
        swap_gen: 0,
        viewport_va,
        display_width,
        display_height,
        workspace,
        last_line_count: 0,
        last_cursor_line: 0,
        last_cursor_col: 0,
        last_content_len: 0,
        last_clock_secs: u64::MAX,
        sticky_col: None,
        render_ep,
        editor_ep,
        rtc_va,
        pointer_x: 0,
        pointer_y: 0,
        cursor_shape_name: scene::CURSOR_DEFAULT,
        last_click_ms: 0,
        last_click_x: 0,
        last_click_y: 0,
        click_count: 0,
        dragging: false,
        drag_origin_start: 0,
        drag_origin_end: 0,
        frame_stats: FrameStats::new(),
        audio_ep: Handle(0),
        audio_vmo: Handle(0),
        audio_data_len: 0,
        console_ep,
        layout_dirty: true,
    };

    load_audio_clip(&mut server);
    server.write_viewport();
    server.build_scene();

    console::write(console_ep, b"presenter: ready\n");

    const NS_PER_SEC: u64 = 1_000_000_000;
    let mut next_frame: u64 = 0;

    loop {
        let now = abi::system::clock_read().unwrap_or(0);
        let needs_anim = server.workspace.slide_animating;
        let deadline = if needs_anim {
            if next_frame <= now {
                let behind = now - next_frame;

                next_frame += (behind / frame_ns + 1) * frame_ns;
            }

            next_frame
        } else {
            let current_sec = now / NS_PER_SEC;

            (current_sec + 1) * NS_PER_SEC
        };

        let frame_due = match ipc::server::serve_one_timed(HANDLE_SVC_EP, &mut server, deadline) {
            Ok(()) => abi::system::clock_read().unwrap_or(0) >= deadline,
            Err(abi::types::SyscallError::TimedOut) => true,
            Err(_) => break,
        };

        if frame_due {
            if server.workspace.slide_animating {
                let frame_start = abi::system::clock_read().unwrap_or(0);
                let dt_ns = frame_start
                    .saturating_sub(server.workspace.last_anim_tick)
                    .min(33_000_000);

                server.workspace.last_anim_tick = frame_start;
                server.workspace.slide_spring.tick_ns(dt_ns);

                if server.workspace.slide_spring.settled() {
                    server.workspace.slide_animating = false;

                    server
                        .workspace
                        .slide_spring
                        .reset_to(server.workspace.slide_spring.target());
                }

                server.build_scene();

                let frame_end = abi::system::clock_read().unwrap_or(0);

                server
                    .frame_stats
                    .record(frame_end.saturating_sub(frame_start));

                if !server.workspace.slide_animating {
                    server.frame_stats.report(server.console_ep);
                    server.frame_stats.reset();
                }
            }

            server.update_clock();

            let end = abi::system::clock_read().unwrap_or(0);

            if next_frame <= end {
                let behind = end - next_frame;

                next_frame += (behind / frame_ns + 1) * frame_ns;
            }
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
