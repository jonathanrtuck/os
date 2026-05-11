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

pub(crate) const MAX_GLYPHS_PER_LINE: usize = 256;

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

fn compute_char_advance(font_data: &[u8]) -> f32 {
    let gid = fonts::metrics::glyph_id_for_char(font_data, 'M').unwrap_or(0);

    if let (Some((advance_fu, _)), Some(fm)) = (
        fonts::metrics::glyph_h_metrics(font_data, gid),
        fonts::metrics::font_metrics(font_data),
    ) {
        return advance_fu as f32 * presenter_service::FONT_SIZE as f32 / fm.units_per_em as f32;
    }

    presenter_service::CHAR_WIDTH_F32
}

pub(crate) fn shape_text(
    font_data: &[u8],
    text: &str,
    font_size: u16,
    features: &[fonts::Feature],
    out: &mut [ShapedGlyph],
) -> (usize, f32) {
    let upem = fonts::metrics::font_metrics(font_data)
        .map(|m| m.units_per_em)
        .unwrap_or(1000) as f32;
    let scale = font_size as f32 / upem * 65536.0;
    let shaped = fonts::shape(font_data, text, features);
    let count = shaped.len().min(out.len());
    let mut total_width = 0.0f32;

    for (i, sg) in shaped.iter().take(count).enumerate() {
        let adv = sg.x_advance as f32 * scale;

        out[i] = ShapedGlyph {
            glyph_id: sg.glyph_id,
            _pad: 0,
            x_advance: adv as i32,
            x_offset: (sg.x_offset as f32 * scale) as i32,
            y_offset: (sg.y_offset as f32 * scale) as i32,
        };
        total_width += adv / 65536.0;
    }

    (count, total_width)
}

pub(crate) fn copy_into(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());

    dst[..len].copy_from_slice(&src[..len]);

    len
}

pub(crate) fn scale_icon_paths(
    scene: &mut SceneWriter<'_>,
    icon: &icons::Icon,
    size_pt: u32,
) -> (scene::DataRef, u32) {
    let scale = size_pt as f32 / icon.viewbox;
    let mut buf = alloc::vec::Vec::new();

    for icon_path in icon.paths {
        let cmds = icon_path.commands;
        let mut pos = 0;

        while pos + 4 <= cmds.len() {
            let tag = u32::from_le_bytes([cmds[pos], cmds[pos + 1], cmds[pos + 2], cmds[pos + 3]]);

            match tag {
                0 | 1 => {
                    if pos + 12 > cmds.len() {
                        break;
                    }

                    let x = f32::from_le_bytes([
                        cmds[pos + 4],
                        cmds[pos + 5],
                        cmds[pos + 6],
                        cmds[pos + 7],
                    ]) * scale;
                    let y = f32::from_le_bytes([
                        cmds[pos + 8],
                        cmds[pos + 9],
                        cmds[pos + 10],
                        cmds[pos + 11],
                    ]) * scale;

                    buf.extend_from_slice(&tag.to_le_bytes());
                    buf.extend_from_slice(&x.to_le_bytes());
                    buf.extend_from_slice(&y.to_le_bytes());

                    pos += 12;
                }
                2 => {
                    if pos + 28 > cmds.len() {
                        break;
                    }

                    let mut coords = [0f32; 6];

                    for (ci, coord) in coords.iter_mut().enumerate() {
                        let off = pos + 4 + ci * 4;

                        *coord = f32::from_le_bytes([
                            cmds[off],
                            cmds[off + 1],
                            cmds[off + 2],
                            cmds[off + 3],
                        ]) * scale;
                    }

                    buf.extend_from_slice(&2u32.to_le_bytes());

                    for c in &coords {
                        buf.extend_from_slice(&c.to_le_bytes());
                    }

                    pos += 28;
                }
                3 => {
                    buf.extend_from_slice(&3u32.to_le_bytes());

                    pos += 4;
                }
                _ => break,
            }
        }
    }

    let hash = scene::fnv1a(&buf);
    let data_ref = scene.push_path_commands(&buf);

    (data_ref, hash)
}

// ── Layout results parsing (from seqlock-read buffer) ────────────

pub(crate) fn parse_layout_header(buf: &[u8]) -> layout_service::LayoutHeader {
    layout_service::LayoutHeader::read_from(buf)
}

pub(crate) fn parse_line_at(buf: &[u8], index: usize) -> layout_service::LineInfo {
    let offset = layout_service::LayoutHeader::SIZE + index * layout_service::LineInfo::SIZE;

    layout_service::LineInfo::read_from(&buf[offset..])
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

// ── Presenter server ──────────────────────────────────────────────

pub(crate) struct Presenter {
    pub(crate) doc_va: usize,
    pub(crate) doc_ep: Handle,
    pub(crate) layout_ep: Handle,

    pub(crate) results_reader: ipc::register::Reader,
    pub(crate) results_buf: [u8; layout_service::RESULTS_VALUE_SIZE],

    pub(crate) scene_buf: &'static mut [u8],
    pub(crate) scene_vmo: Handle,

    pub(crate) viewport_va: usize,

    pub(crate) display_width: u32,
    pub(crate) display_height: u32,

    pub(crate) glyphs: [ShapedGlyph; MAX_GLYPHS_PER_LINE],
    pub(crate) cmap_mono: [u16; 128],
    pub(crate) cmap_sans: [u16; 128],
    pub(crate) char_width: f32,

    pub(crate) blink_start: u64,

    pub(crate) last_line_count: u32,
    pub(crate) last_cursor_line: u32,
    pub(crate) last_cursor_col: u32,
    pub(crate) last_content_len: u32,

    pub(crate) scroll_y: i32,
    pub(crate) sticky_col: Option<u32>,

    pub(crate) clock_node_id: scene::NodeId,
    pub(crate) clock_glyph_ref: scene::DataRef,
    pub(crate) last_clock_secs: u64,

    pub(crate) render_ep: Handle,
    pub(crate) editor_ep: Handle,

    pub(crate) rtc_va: usize,

    pub(crate) pointer_x: f32,
    pub(crate) pointer_y: f32,
    pub(crate) cursor_shape_name: u8,

    pub(crate) last_click_ms: u64,
    pub(crate) last_click_x: u32,
    pub(crate) last_click_y: u32,
    pub(crate) click_count: u8,
    pub(crate) dragging: bool,
    pub(crate) drag_origin_start: usize,
    pub(crate) drag_origin_end: usize,

    pub(crate) active_space: u8,
    pub(crate) num_spaces: u8,
    pub(crate) slide_spring: animation::Spring,
    pub(crate) slide_animating: bool,
    pub(crate) last_anim_tick: u64,

    pub(crate) frame_stats: FrameStats,

    pub(crate) image_content_id: u32,
    pub(crate) image_width: u16,
    pub(crate) image_height: u16,

    pub(crate) audio_ep: Handle,
    pub(crate) audio_vmo: Handle,
    pub(crate) audio_data_len: u32,

    pub(crate) video_decoder_ep: Handle,
    pub(crate) video_frame_vmo: Handle,
    pub(crate) video_frame_va: usize,
    pub(crate) video_content_id: u32,
    pub(crate) video_width: u16,
    pub(crate) video_height: u16,
    pub(crate) video_total_frames: u32,
    pub(crate) video_current_frame: u32,
    pub(crate) video_ns_per_frame: u64,
    pub(crate) video_pixel_size: u32,
    pub(crate) video_playing: bool,
    pub(crate) video_next_frame_ns: u64,
    pub(crate) video_uploaded: bool,

    pub(crate) console_ep: Handle,
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

        if cursor_top < self.scroll_y {
            self.scroll_y = cursor_top;
        } else if cursor_top + line_h > self.scroll_y + vp_h {
            self.scroll_y = cursor_top + line_h - vp_h;
        }
    }

    fn ensure_cursor_visible_at(&mut self, cursor_y: i32, cursor_h: i32) {
        let vp_h = self.viewport_height();

        if cursor_y < self.scroll_y {
            self.scroll_y = cursor_y;
        } else if cursor_y + cursor_h > self.scroll_y + vp_h {
            self.scroll_y = cursor_y + cursor_h - vp_h;
        }
    }

    fn clamp_scroll(&mut self) {
        let header = parse_layout_header(&self.results_buf);
        let total_h = header.total_height;
        let vp_h = self.viewport_height();
        let max_scroll = if total_h > vp_h { total_h - vp_h } else { 0 };

        if self.scroll_y < 0 {
            self.scroll_y = 0;
        }

        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }
    }

    fn write_viewport(&self) {
        let (tw, th) = self.text_area_dims();
        let state = layout_service::ViewportState {
            scroll_y: self.scroll_y,
            viewport_width: tw,
            viewport_height: th,
            char_width_fp: layout_service::ViewportState::encode_char_width(self.char_width),
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
            presenter_service::SETUP => match abi::handle::dup(self.scene_vmo, Rights::READ_MAP) {
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
            },
            presenter_service::BUILD => {
                self.build_scene();
                self.request_render();

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

                    self.scroll_y += event.delta_y;
                    self.clamp_scroll();
                    self.write_viewport();
                    self.build_scene();
                    self.request_render();
                }

                let _ = msg.reply_empty();
            }
            presenter_service::POINTER_EVENT => {
                if msg.payload.len() >= presenter_service::PointerEvent::SIZE {
                    let event = presenter_service::PointerEvent::read_from(msg.payload);
                    let px = (event.abs_x as u64 * self.display_width as u64 / 32768) as f32;
                    let py = (event.abs_y as u64 * self.display_height as u64 / 32768) as f32;

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
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

// ── Image loading from store + decode + upload ──────────────────

const IMAGE_CONTENT_ID: u32 = 1;
const IMAGE_MEDIA_TYPE: &[u8] = b"image/jpeg";

fn load_and_decode_image(server: &mut Presenter, render_ep: Handle) {
    let store_ep = match name::lookup(HANDLE_NS_EP, b"store") {
        Ok(h) => h,
        Err(_) => return,
    };
    // Query store for an image/jpeg document.
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let mut query_buf = [0u8; store_service::QueryTypeRequest::SIZE + 10];
    let qt_req = store_service::QueryTypeRequest {
        media_type_len: IMAGE_MEDIA_TYPE.len() as u16,
    };

    qt_req.write_to(&mut query_buf);
    query_buf[store_service::QueryTypeRequest::SIZE..][..IMAGE_MEDIA_TYPE.len()]
        .copy_from_slice(IMAGE_MEDIA_TYPE);

    let query_len = store_service::QueryTypeRequest::SIZE + IMAGE_MEDIA_TYPE.len();
    let query_reply = match ipc::client::call(
        store_ep,
        store_service::QUERY_TYPE,
        &query_buf[..query_len],
        &[],
        &mut [],
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return,
    };

    if query_reply.is_error() || query_reply.payload.len() < store_service::QueryTypeReply::SIZE {
        return;
    }

    let qt = store_service::QueryTypeReply::read_from(query_reply.payload);
    let file_id = qt.file_id;
    let file_size = qt.size as usize;

    if file_size == 0 {
        return;
    }

    // Set up a shared VMO large enough for the JPEG data.
    let shared_vmo_size = file_size.next_multiple_of(PAGE_SIZE);
    let shared_vmo = match abi::vmo::create(shared_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => return,
    };
    let shared_va = match abi::vmo::map(shared_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::handle::close(shared_vmo);

            return;
        }
    };
    let shared_dup = match abi::handle::dup(shared_vmo, rw) {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::vmo::unmap(shared_va);
            let _ = abi::handle::close(shared_vmo);

            return;
        }
    };
    // SETUP: give the store service access to our shared VMO.
    let setup_reply = ipc::client::call(
        store_ep,
        store_service::SETUP,
        &[],
        &[shared_dup.0],
        &mut [],
        &mut call_buf,
    );

    if setup_reply.is_err() {
        let _ = abi::vmo::unmap(shared_va);
        let _ = abi::handle::close(shared_vmo);

        return;
    }

    // READ_DOC: read the JPEG data into the shared VMO.
    let read_req = store_service::ReadRequest {
        file_id,
        offset: 0,
        vmo_offset: 0,
        max_len: file_size as u32,
    };
    let mut read_buf = [0u8; store_service::ReadRequest::SIZE];

    read_req.write_to(&mut read_buf);

    let read_reply = match ipc::client::call(
        store_ep,
        store_service::READ_DOC,
        &read_buf,
        &[],
        &mut [],
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => {
            let _ = abi::vmo::unmap(shared_va);
            let _ = abi::handle::close(shared_vmo);

            return;
        }
    };

    if read_reply.is_error() || read_reply.payload.len() < store_service::ReadReply::SIZE {
        let _ = abi::vmo::unmap(shared_va);
        let _ = abi::handle::close(shared_vmo);

        return;
    }

    let rr = store_service::ReadReply::read_from(read_reply.payload);
    let bytes_read = rr.bytes_read as usize;

    if bytes_read == 0 {
        let _ = abi::vmo::unmap(shared_va);
        let _ = abi::handle::close(shared_vmo);

        return;
    }

    // Send the JPEG data to the decoder service for decoding.
    let decoder_ep = match name::lookup(HANDLE_NS_EP, b"jpeg-decoder") {
        Ok(h) => h,
        Err(_) => {
            console::write(
                server.console_ep,
                b"presenter: jpeg-decoder lookup failed\n",
            );
            let _ = abi::vmo::unmap(shared_va);
            let _ = abi::handle::close(shared_vmo);

            return;
        }
    };
    let _ = abi::vmo::unmap(shared_va);
    let decode_req = jpeg_decoder::DecodeRequest {
        file_size: bytes_read as u32,
    };
    let mut decode_buf = [0u8; jpeg_decoder::DecodeRequest::SIZE];

    decode_req.write_to(&mut decode_buf);

    let mut decode_handles = [0u32; 4];
    let decode_reply = match ipc::client::call(
        decoder_ep,
        jpeg_decoder::DECODE,
        &decode_buf,
        &[shared_vmo.0],
        &mut decode_handles,
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => {
            console::write(server.console_ep, b"presenter: jpeg decode IPC failed\n");

            return;
        }
    };

    if decode_reply.is_error()
        || decode_reply.payload.len() < jpeg_decoder::DecodeReply::SIZE
        || decode_reply.handle_count == 0
    {
        console::write(server.console_ep, b"presenter: jpeg decode error reply\n");

        return;
    }

    let dr = jpeg_decoder::DecodeReply::read_from(decode_reply.payload);
    let pixel_vmo = Handle(decode_handles[0]);
    let width = dr.width as u16;
    let height = dr.height as u16;
    let pixel_size = dr.pixel_size;
    let pixel_dup = match abi::handle::dup(pixel_vmo, Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => return,
    };
    let upload_req = render::comp::UploadImageRequest {
        content_id: IMAGE_CONTENT_ID,
        width,
        height,
        pixel_size,
    };
    let mut upload_buf = [0u8; render::comp::UploadImageRequest::SIZE];

    upload_req.write_to(&mut upload_buf);

    let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
    let mut upload_handles = [0u32; 4];
    let _ = ipc::client::call(
        render_ep,
        render::comp::UPLOAD_IMAGE,
        &upload_buf,
        &[pixel_dup.0],
        &mut upload_handles,
        &mut reply_buf,
    );

    server.image_content_id = IMAGE_CONTENT_ID;
    server.image_width = width;
    server.image_height = height;

    console::write(server.console_ep, b"presenter: image loaded from store\n");
}

// ── Video loading from filesystem ───────────────────────────────

const VIDEO_CONTENT_ID: u32 = 2;
const VIDEO_PATH: &[u8] = b"video.avi";

fn load_video(server: &mut Presenter, render_ep: Handle) {
    let fs_ep = match name::lookup(HANDLE_NS_EP, b"fs") {
        Ok(h) => h,
        Err(_) => return,
    };
    let decoder_ep = match name::lookup(HANDLE_NS_EP, b"video-decoder") {
        Ok(h) => h,
        Err(_) => return,
    };
    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let stat_req = fs_service::StatRequest::new(VIDEO_PATH);
    let mut stat_buf = [0u8; fs_service::StatRequest::SIZE];

    stat_req.write_to(&mut stat_buf);

    let stat_reply = match ipc::client::call(
        fs_ep,
        fs_service::STAT,
        &stat_buf,
        &[],
        &mut [],
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return,
    };

    if stat_reply.is_error() || stat_reply.payload.len() < fs_service::StatReply::SIZE {
        return;
    }

    let sr = fs_service::StatReply::read_from(stat_reply.payload);

    if sr.exists == 0 || sr.size == 0 {
        return;
    }

    let read_req = fs_service::ReadFileRequest::new(VIDEO_PATH);
    let mut read_buf = [0u8; fs_service::ReadFileRequest::SIZE];

    read_req.write_to(&mut read_buf);

    let mut read_handles = [0u32; 4];
    let read_reply = match ipc::client::call(
        fs_ep,
        fs_service::READ_FILE,
        &read_buf,
        &[],
        &mut read_handles,
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return,
    };

    if read_reply.is_error()
        || read_reply.payload.len() < fs_service::ReadFileReply::SIZE
        || read_handles[0] == 0
    {
        return;
    }

    let rr = fs_service::ReadFileReply::read_from(read_reply.payload);
    let file_vmo = Handle(read_handles[0]);
    let file_size = rr.bytes_read;

    if file_size == 0 {
        let _ = abi::handle::close(file_vmo);

        return;
    }

    let open_req = video_decoder::OpenRequest { file_size };
    let mut open_buf = [0u8; video_decoder::OpenRequest::SIZE];

    open_req.write_to(&mut open_buf);

    let mut open_handles = [0u32; 4];
    let open_reply = match ipc::client::call(
        decoder_ep,
        video_decoder::OPEN,
        &open_buf,
        &[file_vmo.0],
        &mut open_handles,
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return,
    };

    if open_reply.is_error()
        || open_reply.payload.len() < video_decoder::OpenReply::SIZE
        || open_handles[0] == 0
    {
        return;
    }

    let or = video_decoder::OpenReply::read_from(open_reply.payload);
    let frame_vmo = Handle(open_handles[0]);

    if or.total_frames == 0 {
        let _ = abi::handle::close(frame_vmo);

        return;
    }

    let frame_va = match abi::vmo::map(frame_vmo, 0, Rights(Rights::READ.0 | Rights::MAP.0)) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::handle::close(frame_vmo);

            return;
        }
    };

    server.video_decoder_ep = decoder_ep;
    server.video_frame_vmo = frame_vmo;
    server.video_frame_va = frame_va;
    server.video_total_frames = or.total_frames;
    server.video_ns_per_frame = or.ns_per_frame;
    server.video_width = or.width as u16;
    server.video_height = or.height as u16;

    decode_video_frame(server, 0);

    if server.video_pixel_size == 0 {
        console::write(server.console_ep, b"presenter: video decode failed\n");

        return;
    }

    upload_video_frame(server, render_ep);

    server.num_spaces += 1;

    console::write(
        server.console_ep,
        b"presenter: video loaded from filesystem\n",
    );
}

fn decode_video_frame(server: &mut Presenter, index: u32) {
    let req = video_decoder::DecodeFrameRequest { frame_index: index };
    let mut req_buf = [0u8; video_decoder::DecodeFrameRequest::SIZE];

    req.write_to(&mut req_buf);

    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let reply = match ipc::client::call(
        server.video_decoder_ep,
        video_decoder::DECODE_FRAME,
        &req_buf,
        &[],
        &mut [],
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return,
    };

    if reply.is_error() || reply.payload.len() < video_decoder::DecodeFrameReply::SIZE {
        return;
    }

    let dr = video_decoder::DecodeFrameReply::read_from(reply.payload);

    server.video_pixel_size = dr.pixel_size;
    server.video_current_frame = index;
}

fn upload_video_frame(server: &mut Presenter, render_ep: Handle) {
    if server.video_pixel_size == 0 || server.video_frame_va == 0 {
        return;
    }

    if !server.video_uploaded {
        let frame_dup = match abi::handle::dup(
            server.video_frame_vmo,
            Rights(Rights::READ.0 | Rights::MAP.0),
        ) {
            Ok(h) => h,
            Err(_) => return,
        };
        let upload_req = render::comp::UploadImageRequest {
            content_id: VIDEO_CONTENT_ID,
            width: server.video_width,
            height: server.video_height,
            pixel_size: server.video_pixel_size,
        };
        let mut upload_buf = [0u8; render::comp::UploadImageRequest::SIZE];

        upload_req.write_to(&mut upload_buf);

        let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
        let mut upload_handles = [0u32; 4];
        let _ = ipc::client::call(
            render_ep,
            render::comp::UPLOAD_IMAGE,
            &upload_buf,
            &[frame_dup.0],
            &mut upload_handles,
            &mut reply_buf,
        );

        server.video_content_id = VIDEO_CONTENT_ID;
        server.video_uploaded = true;
    } else {
        let mut payload = [0u8; 4];

        payload.copy_from_slice(&VIDEO_CONTENT_ID.to_le_bytes());

        let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
        let _ = ipc::client::call(
            render_ep,
            render::comp::REFRESH_IMAGE,
            &payload,
            &[],
            &mut [],
            &mut reply_buf,
        );
    }
}

// ── Audio clip loading from store ─────────────────────────────────

const AUDIO_MEDIA_TYPE: &[u8] = b"audio/wav";

fn load_audio_clip(server: &mut Presenter) {
    let store_ep = match name::lookup(HANDLE_NS_EP, b"store") {
        Ok(h) => h,
        Err(_) => return,
    };
    let audio_ep = match name::lookup(HANDLE_NS_EP, b"audio") {
        Ok(h) => h,
        Err(_) => return,
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let mut call_buf = [0u8; ipc::message::MSG_SIZE];
    let mut query_buf = [0u8; store_service::QueryTypeRequest::SIZE + 10];
    let qt_req = store_service::QueryTypeRequest {
        media_type_len: AUDIO_MEDIA_TYPE.len() as u16,
    };

    qt_req.write_to(&mut query_buf);
    query_buf[store_service::QueryTypeRequest::SIZE..][..AUDIO_MEDIA_TYPE.len()]
        .copy_from_slice(AUDIO_MEDIA_TYPE);

    let query_len = store_service::QueryTypeRequest::SIZE + AUDIO_MEDIA_TYPE.len();
    let query_reply = match ipc::client::call(
        store_ep,
        store_service::QUERY_TYPE,
        &query_buf[..query_len],
        &[],
        &mut [],
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => return,
    };

    if query_reply.is_error() || query_reply.payload.len() < store_service::QueryTypeReply::SIZE {
        return;
    }

    let qt = store_service::QueryTypeReply::read_from(query_reply.payload);
    let file_id = qt.file_id;
    let file_size = qt.size as usize;

    if file_size == 0 {
        return;
    }

    let vmo_size = file_size.next_multiple_of(PAGE_SIZE);
    let data_vmo = match abi::vmo::create(vmo_size, 0) {
        Ok(h) => h,
        Err(_) => return,
    };
    let data_va = match abi::vmo::map(data_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::handle::close(data_vmo);

            return;
        }
    };
    let shared_dup = match abi::handle::dup(data_vmo, rw) {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::vmo::unmap(data_va);
            let _ = abi::handle::close(data_vmo);

            return;
        }
    };
    let setup_reply = ipc::client::call(
        store_ep,
        store_service::SETUP,
        &[],
        &[shared_dup.0],
        &mut [],
        &mut call_buf,
    );

    if setup_reply.is_err() {
        let _ = abi::vmo::unmap(data_va);
        let _ = abi::handle::close(data_vmo);

        return;
    }

    let read_req = store_service::ReadRequest {
        file_id,
        offset: 0,
        vmo_offset: 0,
        max_len: file_size as u32,
    };
    let mut read_buf = [0u8; store_service::ReadRequest::SIZE];

    read_req.write_to(&mut read_buf);

    let read_reply = match ipc::client::call(
        store_ep,
        store_service::READ_DOC,
        &read_buf,
        &[],
        &mut [],
        &mut call_buf,
    ) {
        Ok(r) => r,
        Err(_) => {
            let _ = abi::vmo::unmap(data_va);
            let _ = abi::handle::close(data_vmo);

            return;
        }
    };

    if read_reply.is_error() || read_reply.payload.len() < store_service::ReadReply::SIZE {
        let _ = abi::vmo::unmap(data_va);
        let _ = abi::handle::close(data_vmo);

        return;
    }

    let rr = store_service::ReadReply::read_from(read_reply.payload);
    let bytes_read = rr.bytes_read as usize;

    if bytes_read == 0 {
        let _ = abi::vmo::unmap(data_va);
        let _ = abi::handle::close(data_vmo);

        return;
    }

    let _ = abi::vmo::unmap(data_va);

    server.audio_ep = audio_ep;
    server.audio_vmo = data_vmo;
    server.audio_data_len = bytes_read as u32;

    console::write(server.console_ep, b"presenter: audio clip loaded\n");
}

// ── Audio playback (threaded to avoid blocking the event loop) ───

#[repr(C)]
struct PlayArgs {
    audio_ep: u32,
    vmo_dup: u32,
    data_len: u32,
}

extern "C" fn play_thread_entry(arg: usize) -> ! {
    // SAFETY: arg is a pointer to a heap-allocated PlayArgs.
    let args = unsafe { &*(arg as *const PlayArgs) };
    let ep = Handle(args.audio_ep);
    let vmo_dup = Handle(args.vmo_dup);
    let data_len = args.data_len;
    let req = audio_service::PlayRequest {
        format: audio_service::FORMAT_WAV,
        data_len,
    };
    let mut payload = [0u8; audio_service::PlayRequest::SIZE];

    req.write_to(&mut payload);

    let mut buf = [0u8; ipc::message::MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, audio_service::PLAY, &payload);
    let _ = abi::ipc::call(ep, &mut buf, total, &[vmo_dup.0], &mut []);

    // SAFETY: arg was heap-allocated in play_audio_clip; we own it.
    unsafe {
        let _ = alloc::boxed::Box::from_raw(arg as *mut PlayArgs);
    }

    abi::thread::exit(0);
}

const PLAY_THREAD_STACK_SIZE: usize = PAGE_SIZE;

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
        let args = alloc::boxed::Box::new(PlayArgs {
            audio_ep: self.audio_ep.0,
            vmo_dup: vmo_dup.0,
            data_len: self.audio_data_len,
        });
        let arg_ptr = alloc::boxed::Box::into_raw(args) as usize;
        let stack_layout = alloc::alloc::Layout::from_size_align(PLAY_THREAD_STACK_SIZE, PAGE_SIZE);
        let stack_layout = match stack_layout {
            Ok(l) => l,
            Err(_) => return,
        };

        // SAFETY: layout is non-zero size with valid alignment.
        let stack_base = unsafe { alloc::alloc::alloc(stack_layout) };

        if stack_base.is_null() {
            return;
        }

        let stack_top = stack_base as usize + PLAY_THREAD_STACK_SIZE;
        let entry = play_thread_entry as extern "C" fn(usize) -> ! as usize;
        let _ = abi::thread::create(entry, stack_top, arg_ptr);
    }

    fn toggle_video_playback(&mut self) {
        if self.video_decoder_ep.0 == 0 || self.video_total_frames == 0 {
            return;
        }

        self.video_playing = !self.video_playing;

        if self.video_playing {
            self.video_next_frame_ns = abi::system::clock_read().unwrap_or(0);
        }
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
    let scene_vmo_size = SCENE_SIZE.next_multiple_of(PAGE_SIZE);
    let scene_vmo = match abi::vmo::create(scene_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_SCENE_CREATE),
    };
    let scene_va = match abi::vmo::map(scene_vmo, 0, Rights::READ_WRITE_MAP) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
    };
    // SAFETY: scene_va is a valid RW mapping of at least SCENE_SIZE
    // bytes. The presenter is the sole writer.
    let scene_buf = unsafe { core::slice::from_raw_parts_mut(scene_va as *mut u8, SCENE_SIZE) };
    let _ = SceneWriter::new(scene_buf);
    // Connect to compositor — send scene graph VMO so it can read our output.
    let render_ep = match name::watch(HANDLE_NS_EP, b"render") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: render not found\n");

            abi::thread::exit(EXIT_RENDER_NOT_FOUND);
        }
    };
    let scene_dup = match abi::handle::dup(scene_vmo, Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RENDER_SETUP),
    };
    let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
    let mut recv_handles = [0u32; 4];
    let (display_width, display_height, refresh_hz) = match ipc::client::call(
        render_ep,
        render::comp::SETUP,
        &[],
        &[scene_dup.0],
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

    let mut server = Presenter {
        doc_va,
        doc_ep,
        layout_ep,
        results_reader,
        results_buf: [0u8; layout_service::RESULTS_VALUE_SIZE],
        scene_buf,
        scene_vmo,
        viewport_va,
        display_width,
        display_height,
        glyphs: [ShapedGlyph {
            glyph_id: 0,
            _pad: 0,
            x_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; MAX_GLYPHS_PER_LINE],
        cmap_mono: build_cmap_table(font(init::FONT_IDX_MONO)),
        cmap_sans: build_cmap_table(font(init::FONT_IDX_SANS)),
        char_width: compute_char_advance(font(init::FONT_IDX_MONO)),
        blink_start: abi::system::clock_read().unwrap_or(0),
        last_line_count: 0,
        last_cursor_line: 0,
        last_cursor_col: 0,
        last_content_len: 0,
        scroll_y: 0,
        sticky_col: None,
        clock_node_id: scene::NULL,
        clock_glyph_ref: scene::DataRef {
            offset: 0,
            length: 0,
        },
        last_clock_secs: u64::MAX,
        render_ep,
        editor_ep,
        rtc_va,
        pointer_x: 0.0,
        pointer_y: 0.0,
        cursor_shape_name: scene::CURSOR_DEFAULT,
        last_click_ms: 0,
        last_click_x: 0,
        last_click_y: 0,
        click_count: 0,
        dragging: false,
        drag_origin_start: 0,
        drag_origin_end: 0,
        active_space: 0,
        num_spaces: 1,
        slide_spring: {
            let mut s = animation::Spring::new(0.0, 600.0, 49.0, 1.0);

            s.set_settle_threshold(0.5);

            s
        },
        slide_animating: false,
        last_anim_tick: 0,
        frame_stats: FrameStats::new(),
        image_content_id: 0,
        image_width: 0,
        image_height: 0,
        audio_ep: Handle(0),
        audio_vmo: Handle(0),
        audio_data_len: 0,
        video_decoder_ep: Handle(0),
        video_frame_vmo: Handle(0),
        video_frame_va: 0,
        video_content_id: 0,
        video_width: 0,
        video_height: 0,
        video_total_frames: 0,
        video_current_frame: 0,
        video_ns_per_frame: 0,
        video_pixel_size: 0,
        video_playing: false,
        video_next_frame_ns: 0,
        video_uploaded: false,
        console_ep,
    };

    // Space 0 = text, 1 = image, last = showcase.
    server.num_spaces = 3;

    load_and_decode_image(&mut server, render_ep);
    load_video(&mut server, render_ep);
    load_audio_clip(&mut server);

    // Initial render: write viewport, build scene graph, tell compositor.
    server.write_viewport();
    server.build_scene();
    server.request_render();

    console::write(console_ep, b"presenter: ready\n");

    const NS_PER_SEC: u64 = 1_000_000_000;
    let mut next_frame: u64 = 0;

    loop {
        let now = abi::system::clock_read().unwrap_or(0);
        let showcase_space = server.num_spaces - 1;
        let needs_anim =
            server.slide_animating || server.active_space == showcase_space || server.video_playing;
        let deadline = if needs_anim {
            if next_frame <= now {
                next_frame = now + frame_ns;
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
            let mut needs_render = false;

            if server.slide_animating {
                let frame_start = abi::system::clock_read().unwrap_or(0);
                let dt_ns = frame_start.saturating_sub(server.last_anim_tick);
                let dt = (dt_ns as f32 / 1_000_000_000.0).min(0.033);

                server.last_anim_tick = frame_start;

                server.slide_spring.tick(dt);

                if server.slide_spring.settled() {
                    server.slide_animating = false;

                    server.slide_spring.reset_to(server.slide_spring.target());
                }

                server.build_scene();

                needs_render = true;

                let frame_end = abi::system::clock_read().unwrap_or(0);

                server
                    .frame_stats
                    .record(frame_end.saturating_sub(frame_start));

                if !server.slide_animating {
                    server.frame_stats.report(server.console_ep);
                    server.frame_stats.reset();
                }
            }

            if !server.slide_animating && server.active_space == showcase_space {
                server.build_scene();

                needs_render = true;
            }

            if server.video_playing {
                let now = abi::system::clock_read().unwrap_or(0);

                if now >= server.video_next_frame_ns {
                    let next = (server.video_current_frame + 1) % server.video_total_frames;

                    decode_video_frame(&mut server, next);
                    upload_video_frame(&mut server, render_ep);

                    server.video_next_frame_ns = now + server.video_ns_per_frame;

                    if server.active_space == 2 {
                        server.build_scene();

                        needs_render = true;
                    }
                }
            }

            if server.update_clock() {
                needs_render = true;
            }

            if needs_render {
                server.request_render();
            }

            next_frame = abi::system::clock_read().unwrap_or(0) + frame_ns;
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
