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

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};
use scene::{Color, Content, NodeFlags, SCENE_SIZE, SceneWriter, ShapedGlyph, pt, upt};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_RTC_VMO: Handle = Handle(3);

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
const EXIT_ENDPOINT_CREATE: u32 = 0xE20C;
const EXIT_RENDER_NOT_FOUND: u32 = 0xE20D;
const EXIT_RENDER_SETUP: u32 = 0xE20E;
const EXIT_EDITOR_NOT_FOUND: u32 = 0xE20F;

const MAX_GLYPHS_PER_LINE: usize = 256;

fn read_rtc_seconds(rtc_va: usize) -> u64 {
    if rtc_va == 0 {
        return 0;
    }

    // SAFETY: rtc_va is a valid device VMO mapping of the PL031 RTC.
    // Register 0 (RTCDR) contains the current time as a Unix epoch.
    let val = unsafe { core::ptr::read_volatile(rtc_va as *const u32) };

    val as u64
}

// ── Font data — well-known style IDs ──────────────────────────────
//
// Style IDs shared between the presenter and compositor. The presenter
// assigns style_id to each Content::Glyphs node; the compositor selects
// font data for rasterization based on the same mapping.

const STYLE_MONO: u32 = 0;
const STYLE_SANS: u32 = 1;
#[allow(dead_code)]
const STYLE_SERIF: u32 = 2;

static FONT_MONO: &[u8] = include_bytes!("../../../../assets/jetbrains-mono.ttf");
static FONT_SANS: &[u8] = include_bytes!("../../../../assets/inter.ttf");
#[allow(dead_code)]
static FONT_SERIF: &[u8] = include_bytes!("../../../../assets/source-serif-4.ttf");
static TEST_PNG: &[u8] = include_bytes!("../../../../assets/test.png");

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

fn shape_text(
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

fn copy_into(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());

    dst[..len].copy_from_slice(&src[..len]);

    len
}

// ── Layout results parsing (from seqlock-read buffer) ────────────

fn parse_layout_header(buf: &[u8]) -> layout_service::LayoutHeader {
    layout_service::LayoutHeader::read_from(buf)
}

fn parse_line_at(buf: &[u8], index: usize) -> layout_service::LineInfo {
    let offset = layout_service::LayoutHeader::SIZE + index * layout_service::LineInfo::SIZE;

    layout_service::LineInfo::read_from(&buf[offset..])
}

fn parse_visible_run_at(buf: &[u8], index: usize) -> layout_service::VisibleRun {
    let offset = layout_service::VISIBLE_RUNS_OFFSET + index * layout_service::VisibleRun::SIZE;

    layout_service::VisibleRun::read_from(&buf[offset..])
}

fn font_data_for_family(family: u8) -> &'static [u8] {
    match family {
        piecetable::FONT_MONO => FONT_MONO,
        piecetable::FONT_SERIF => FONT_SERIF,
        _ => FONT_SANS,
    }
}

fn style_id_for_family(family: u8) -> u32 {
    match family {
        piecetable::FONT_MONO => STYLE_MONO,
        piecetable::FONT_SERIF => STYLE_SERIF,
        _ => STYLE_SANS,
    }
}

// ── Selection geometry ────────────────────────────────────────────

struct SelectionSpan {
    start: usize,
    end: usize,
    color: Color,
    char_width: f32,
}

fn build_selection_nodes(
    scene: &mut SceneWriter,
    results_buf: &[u8],
    viewport: scene::NodeId,
    line_count: usize,
    sel: &SelectionSpan,
) {
    let line_height = presenter_service::LINE_HEIGHT;

    for i in 0..line_count {
        let line_info = parse_line_at(results_buf, i);
        let line_byte_start = line_info.byte_offset as usize;
        let line_byte_end = line_byte_start + line_info.byte_length as usize;

        if sel.end <= line_byte_start || sel.start >= line_byte_end {
            continue;
        }

        let col_start = sel.start.saturating_sub(line_byte_start);
        let col_end = if sel.end < line_byte_end {
            sel.end - line_byte_start
        } else {
            line_info.byte_length as usize
        };

        if col_start >= col_end {
            continue;
        }

        let x = col_start as f32 * sel.char_width;
        let w = (col_end - col_start) as f32 * sel.char_width;
        let sel_node = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(sel_node);

            n.x = scene::f32_to_mpt(x);
            n.y = pt(line_info.y);
            n.width = scene::f32_to_mpt(w) as u32;
            n.height = upt(line_height);
            n.background = sel.color;
            n.role = scene::ROLE_SELECTION;
        }

        scene.add_child(viewport, sel_node);
    }
}

// ── Presenter server ──────────────────────────────────────────────

struct Presenter {
    doc_va: usize,
    doc_ep: Handle,
    layout_ep: Handle,

    results_reader: ipc::register::Reader,
    results_buf: [u8; layout_service::RESULTS_VALUE_SIZE],

    scene_buf: &'static mut [u8],
    scene_vmo: Handle,

    viewport_va: usize,

    display_width: u32,
    display_height: u32,

    glyphs: [ShapedGlyph; MAX_GLYPHS_PER_LINE],
    cmap_mono: [u16; 128],
    cmap_sans: [u16; 128],
    char_width: f32,

    blink_start: u64,

    last_line_count: u32,
    last_cursor_line: u32,
    last_cursor_col: u32,
    last_content_len: u32,

    scroll_y: i32,
    sticky_col: Option<u32>,

    clock_node_id: scene::NodeId,
    clock_glyph_ref: scene::DataRef,
    last_clock_secs: u64,

    render_ep: Handle,
    editor_ep: Handle,

    rtc_va: usize,

    pointer_x: f32,
    pointer_y: f32,
    cursor_shape_name: u8,

    last_click_ms: u64,
    last_click_x: u32,
    last_click_y: u32,
    click_count: u8,
    dragging: bool,
    drag_origin_start: usize,
    drag_origin_end: usize,

    active_space: u8,
    num_spaces: u8,
    slide_spring: animation::Spring,
    slide_animating: bool,
    last_anim_tick: u64,

    frame_stats: FrameStats,

    image_content_id: u32,
    image_width: u16,
    image_height: u16,

    console_ep: Handle,
}

struct FrameStats {
    frame_count: u32,
    total_ns: u64,
    min_ns: u64,
    max_ns: u64,
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

    fn build_scene(&mut self) {
        let _ = ipc::client::call_simple(self.layout_ep, layout_service::RECOMPUTE, &[]);
        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };

        self.results_reader.read(&mut self.results_buf);

        let layout_header = parse_layout_header(&self.results_buf);
        let line_count = layout_header.line_count as usize;
        // Compute cursor line, then auto-scroll to keep it visible.
        let (cursor_line_idx, cursor_col_in_line) = self.find_cursor_line(cursor_pos, line_count);

        self.ensure_cursor_visible(cursor_line_idx as u32);
        self.clamp_scroll();
        self.write_viewport();

        // SAFETY: doc_va is valid and content_len comes from read_doc_header.
        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };
        let bg = Color::rgb(
            presenter_service::BG_R,
            presenter_service::BG_G,
            presenter_service::BG_B,
        );
        let text_color = Color::rgb(
            presenter_service::TEXT_R,
            presenter_service::TEXT_G,
            presenter_service::TEXT_B,
        );
        let cursor_color = Color::rgb(
            presenter_service::CURSOR_R,
            presenter_service::CURSOR_G,
            presenter_service::CURSOR_B,
        );
        let sel_color = Color::rgba(
            presenter_service::SEL_R,
            presenter_service::SEL_G,
            presenter_service::SEL_B,
            presenter_service::SEL_A,
        );
        let page_bg = Color::rgb(
            presenter_service::PAGE_BG_R,
            presenter_service::PAGE_BG_G,
            presenter_service::PAGE_BG_B,
        );
        let title_color = Color::rgb(
            presenter_service::CHROME_TITLE_R,
            presenter_service::CHROME_TITLE_G,
            presenter_service::CHROME_TITLE_B,
        );
        let clock_color = Color::rgb(
            presenter_service::CHROME_CLOCK_R,
            presenter_service::CHROME_CLOCK_G,
            presenter_service::CHROME_CLOCK_B,
        );
        let has_selection = sel_anchor != cursor_pos;
        let sel_start = sel_anchor.min(cursor_pos);
        let sel_end = sel_anchor.max(cursor_pos);
        let mut scene = SceneWriter::from_existing(self.scene_buf);

        scene.clear();

        let title_bar_h = presenter_service::TITLE_BAR_H;
        let page_margin = presenter_service::PAGE_MARGIN_V;
        let page_padding = presenter_service::PAGE_PADDING;
        let content_h = self.display_height.saturating_sub(title_bar_h);
        let page_h = content_h.saturating_sub(2 * page_margin);
        let page_w = (page_h as u64 * 210 / 297) as u32;
        let page_w = page_w.min(self.display_width.saturating_sub(2 * page_margin));
        let page_x = ((self.display_width - page_w) / 2) as i32;
        let page_y = page_margin as i32;
        let text_area_w = page_w.saturating_sub(2 * page_padding);
        let text_area_h = page_h.saturating_sub(2 * page_padding);
        // Root node — full screen background.
        let root = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(root);

            n.width = upt(self.display_width);
            n.height = upt(self.display_height);
            n.background = bg;
        }

        scene.set_root(root);

        // Title bar text — "untitled" label, shaped with Inter.
        let title_text_y = (title_bar_h.saturating_sub(presenter_service::LINE_HEIGHT)) / 2;
        let (title_glyphs_count, title_width) = shape_text(
            FONT_SANS,
            "untitled",
            presenter_service::FONT_SIZE,
            &[],
            &mut self.glyphs,
        );
        let title_glyph_ref = scene.push_shaped_glyphs(&self.glyphs[..title_glyphs_count]);

        if let Some(title_node) = scene.alloc_node() {
            let n = scene.node_mut(title_node);

            n.x = pt(36);
            n.y = pt(title_text_y as i32);
            n.width = upt(title_width as u32 + 1);
            n.height = upt(presenter_service::LINE_HEIGHT);
            n.content = Content::Glyphs {
                color: title_color,
                glyphs: title_glyph_ref,
                glyph_count: title_glyphs_count as u16,
                font_size: presenter_service::FONT_SIZE,
                style_id: STYLE_SANS,
            };
            n.role = scene::ROLE_LABEL;

            scene.add_child(root, title_node);
        }

        // Clock text — right-aligned, HH:MM:SS format.
        // Use PL031 RTC for wall-clock time; fall back to monotonic uptime.
        let clock_secs = {
            let rtc = read_rtc_seconds(self.rtc_va);

            if rtc > 0 {
                rtc % 86400
            } else {
                let ns = abi::system::clock_read().unwrap_or(0);

                (ns / 1_000_000_000) % 86400
            }
        };
        let hours = (clock_secs / 3600) % 24;
        let minutes = (clock_secs / 60) % 60;
        let seconds = clock_secs % 60;
        let clock_chars: [u8; 8] = [
            b'0' + (hours / 10) as u8,
            b'0' + (hours % 10) as u8,
            b':',
            b'0' + (minutes / 10) as u8,
            b'0' + (minutes % 10) as u8,
            b':',
            b'0' + (seconds / 10) as u8,
            b'0' + (seconds % 10) as u8,
        ];

        let mut clock_str = [0u8; 8];

        clock_str.copy_from_slice(&clock_chars);

        let clock_text = core::str::from_utf8(&clock_str).unwrap_or("00:00:00");
        let tnum = fonts::Feature::new(fonts::Tag::new(b"tnum"), 1, ..);
        let (clock_count, clock_width) = shape_text(
            FONT_SANS,
            clock_text,
            presenter_service::FONT_SIZE,
            &[tnum],
            &mut self.glyphs,
        );
        let clock_glyph_ref = scene.push_shaped_glyphs(&self.glyphs[..clock_count]);
        let clock_text_w = clock_width as u32 + 1;
        let clock_x = (self.display_width - 12 - clock_text_w) as i32;

        if let Some(clock_node) = scene.alloc_node() {
            let n = scene.node_mut(clock_node);

            n.x = pt(clock_x);
            n.y = pt(title_text_y as i32);
            n.width = upt(clock_text_w);
            n.height = upt(presenter_service::LINE_HEIGHT);
            n.content = Content::Glyphs {
                color: clock_color,
                glyphs: clock_glyph_ref,
                glyph_count: clock_count as u16,
                font_size: presenter_service::FONT_SIZE,
                style_id: STYLE_SANS,
            };
            n.role = scene::ROLE_LABEL;

            scene.add_child(root, clock_node);

            self.clock_node_id = clock_node;
            self.clock_glyph_ref = clock_glyph_ref;
        }

        // Content area — below title bar, clips children.
        let content_area = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(content_area);

            n.y = pt(title_bar_h as i32);
            n.width = upt(self.display_width);
            n.height = upt(content_h);
            n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
        }

        scene.add_child(root, content_area);

        // Strip node — holds all document spaces side by side.
        // child_offset_x slides to reveal the active space.
        let strip = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(strip);

            n.width = upt(self.display_width * self.num_spaces as u32);
            n.height = upt(content_h);
            n.child_offset_x = -self.slide_spring.value();
        }

        scene.add_child(content_area, strip);

        // Space 0: Page surface — white, centered, with shadow.
        let page = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(page);

            n.x = pt(page_x);
            n.y = pt(page_y);
            n.width = upt(page_w);
            n.height = upt(page_h);
            n.background = page_bg;
            n.shadow_color = Color::rgba(0, 0, 0, 255);
            n.shadow_blur_radius = presenter_service::SHADOW_BLUR_RADIUS;
            n.shadow_spread = presenter_service::SHADOW_SPREAD;
            n.cursor_shape = scene::CURSOR_TEXT;
        }

        scene.add_child(strip, page);

        // Viewport node — clips children, scroll offset, inside page.
        let viewport = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(viewport);

            n.x = pt(page_padding as i32);
            n.y = pt(page_padding as i32);
            n.width = upt(text_area_w);
            n.height = upt(text_area_h);
            n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
            n.child_offset_y = -(self.scroll_y as f32);
            n.role = scene::ROLE_DOCUMENT;
        }

        scene.add_child(page, viewport);

        // Selection rectangles — rendered behind text, before glyph nodes.
        if has_selection && line_count > 0 {
            let span = SelectionSpan {
                start: sel_start,
                end: sel_end,
                color: sel_color,
                char_width: self.char_width,
            };

            build_selection_nodes(&mut scene, &self.results_buf, viewport, line_count, &span);
        }

        // Per-line glyph nodes (plain) or per-run glyph nodes (rich).
        let mut cursor_line = cursor_line_idx as u32;
        let mut cursor_col = cursor_col_in_line as u32;
        let char_advance = (self.char_width * 65536.0) as i32;
        let is_rich = layout_header.format == 1;

        if is_rich {
            build_rich_text_nodes(
                &mut scene,
                viewport,
                &layout_header,
                &self.results_buf,
                self.doc_va,
                self.display_width,
                &mut self.glyphs,
            );
        }

        if !is_rich {
            for i in 0..line_count.min(scene::MAX_NODES - 4) {
                let line_info = parse_line_at(&self.results_buf, i);
                let line_start = line_info.byte_offset as usize;
                let line_len = line_info.byte_length as usize;

                if line_len == 0 {
                    continue;
                }

                let line_bytes = if line_start + line_len <= content_len {
                    &content[line_start..line_start + line_len]
                } else {
                    continue;
                };
                let glyph_count = line_len.min(MAX_GLYPHS_PER_LINE);
                let mut needs_fallback = false;

                for (j, &byte) in line_bytes.iter().enumerate().take(glyph_count) {
                    let mono_gid = if byte < 128 {
                        self.cmap_mono[byte as usize]
                    } else {
                        0
                    };

                    if mono_gid > 0 {
                        self.glyphs[j] = ShapedGlyph {
                            glyph_id: mono_gid,
                            _pad: STYLE_MONO as u16,
                            x_advance: char_advance,
                            x_offset: 0,
                            y_offset: 0,
                        };
                    } else {
                        let sans_gid = if byte < 128 {
                            self.cmap_sans[byte as usize]
                        } else {
                            0
                        };

                        if sans_gid > 0 {
                            self.glyphs[j] = ShapedGlyph {
                                glyph_id: sans_gid,
                                _pad: STYLE_SANS as u16,
                                x_advance: char_advance,
                                x_offset: 0,
                                y_offset: 0,
                            };

                            needs_fallback = true;
                        } else {
                            self.glyphs[j] = ShapedGlyph {
                                glyph_id: 0,
                                _pad: STYLE_MONO as u16,
                                x_advance: char_advance,
                                x_offset: 0,
                                y_offset: 0,
                            };
                        }
                    }
                }

                if !needs_fallback {
                    // Fast path: all glyphs from primary font.
                    for j in 0..glyph_count {
                        self.glyphs[j]._pad = 0;
                    }

                    let glyph_ref = scene.push_shaped_glyphs(&self.glyphs[..glyph_count]);
                    let line_node = match scene.alloc_node() {
                        Some(id) => id,
                        None => break,
                    };

                    {
                        let n = scene.node_mut(line_node);

                        n.x = scene::f32_to_mpt(line_info.x);
                        n.y = pt(line_info.y);
                        n.width = upt(line_info.width as u32 + 1);
                        n.height = upt(presenter_service::LINE_HEIGHT);
                        n.content = Content::Glyphs {
                            color: text_color,
                            glyphs: glyph_ref,
                            glyph_count: glyph_count as u16,
                            font_size: presenter_service::FONT_SIZE,
                            style_id: STYLE_MONO,
                        };
                        n.role = scene::ROLE_PARAGRAPH;
                    }

                    scene.add_child(viewport, line_node);
                } else {
                    // Slow path: split into runs by font for fallback.
                    let mut run_start = 0;

                    while run_start < glyph_count {
                        let run_style = self.glyphs[run_start]._pad as u32;
                        let mut run_end = run_start + 1;

                        while run_end < glyph_count && self.glyphs[run_end]._pad as u32 == run_style
                        {
                            run_end += 1;
                        }

                        let run_len = run_end - run_start;

                        for j in run_start..run_end {
                            self.glyphs[j]._pad = 0;
                        }

                        let glyph_ref = scene.push_shaped_glyphs(&self.glyphs[run_start..run_end]);
                        let run_node = match scene.alloc_node() {
                            Some(id) => id,
                            None => break,
                        };
                        let run_x = line_info.x + run_start as f32 * self.char_width;

                        {
                            let n = scene.node_mut(run_node);

                            n.x = scene::f32_to_mpt(run_x);
                            n.y = pt(line_info.y);
                            n.width = upt((run_len as f32 * self.char_width) as u32 + 1);
                            n.height = upt(presenter_service::LINE_HEIGHT);
                            n.content = Content::Glyphs {
                                color: text_color,
                                glyphs: glyph_ref,
                                glyph_count: run_len as u16,
                                font_size: presenter_service::FONT_SIZE,
                                style_id: run_style,
                            };
                            n.role = scene::ROLE_PARAGRAPH;
                        }

                        scene.add_child(viewport, run_node);

                        run_start = run_end;
                    }
                }
            }
        } // end if !is_rich

        // Handle cursor past last line.
        if line_count > 0 && cursor_pos >= content_len {
            let last = parse_line_at(&self.results_buf, line_count - 1);
            let last_end = last.byte_offset as usize + last.byte_length as usize;

            if cursor_pos >= last_end && cursor_pos > last.byte_offset as usize {
                cursor_line = (line_count - 1) as u32;
                cursor_col = (cursor_pos - last.byte_offset as usize) as u32;
            }
        }

        // Cursor position — proportional for rich text, monospace for plain.
        let (cursor_x, cursor_y, cursor_h) = if is_rich {
            compute_rich_cursor(&self.results_buf, &layout_header, cursor_pos, self.doc_va)
        } else {
            (
                cursor_col as f32 * self.char_width,
                cursor_line as i32 * presenter_service::LINE_HEIGHT as i32,
                presenter_service::LINE_HEIGHT,
            )
        };

        if let Some(cursor_node) = scene.alloc_node() {
            let n = scene.node_mut(cursor_node);

            n.x = scene::f32_to_mpt(cursor_x);
            n.y = pt(cursor_y);
            n.width = upt(presenter_service::CURSOR_WIDTH);
            n.height = upt(cursor_h);
            n.background = cursor_color;

            if !has_selection {
                n.animation = scene::Animation::cursor_blink(self.blink_start);
            }

            n.role = scene::ROLE_CARET;

            scene.add_child(viewport, cursor_node);
        }

        // Space 1: Image document node — positioned at x=display_width within the strip.
        if self.image_content_id != 0 && self.image_width > 0 && self.image_height > 0 {
            let max_w = self.display_width.saturating_sub(48);
            let max_h = content_h.saturating_sub(48);
            let src_w = self.image_width as u32;
            let src_h = self.image_height as u32;
            let scale_w = max_w as f32 / src_w as f32;
            let scale_h = max_h as f32 / src_h as f32;
            let fit_scale = if scale_w < scale_h { scale_w } else { scale_h };
            let img_display_w = if fit_scale < 1.0 {
                (src_w as f32 * fit_scale) as u32
            } else {
                src_w
            };
            let img_display_h = if fit_scale < 1.0 {
                (src_h as f32 * fit_scale) as u32
            } else {
                src_h
            };
            let img_x = self.display_width as i32
                + ((self.display_width as i32 - img_display_w as i32) / 2);
            let img_y = ((content_h as i32 - img_display_h as i32) / 2).max(0);

            if let Some(image_node) = scene.alloc_node() {
                let n = scene.node_mut(image_node);

                n.x = pt(img_x);
                n.y = pt(img_y);
                n.width = upt(img_display_w);
                n.height = upt(img_display_h);
                n.content = Content::Image {
                    content_id: self.image_content_id,
                    src_width: self.image_width,
                    src_height: self.image_height,
                };
                n.shadow_color = Color::rgba(0, 0, 0, 255);
                n.shadow_blur_radius = presenter_service::SHADOW_BLUR_RADIUS;
                n.shadow_spread = presenter_service::SHADOW_SPREAD;

                scene.add_child(strip, image_node);
            }
        }

        // Space 2: Rendering showcase.
        let now_ns = abi::system::clock_read().unwrap_or(0);

        build_showcase_nodes(&mut scene, strip, self.display_width, content_h, now_ns);

        scene.commit();

        self.last_line_count = line_count as u32;
        self.last_cursor_line = cursor_line;
        self.last_cursor_col = cursor_col;
        self.last_content_len = content_len as u32;
    }

    fn current_clock_secs(&self) -> u64 {
        let rtc = read_rtc_seconds(self.rtc_va);

        if rtc > 0 {
            rtc % 86400
        } else {
            let ns = abi::system::clock_read().unwrap_or(0);

            (ns / 1_000_000_000) % 86400
        }
    }

    fn update_clock(&mut self) -> bool {
        if self.clock_node_id == scene::NULL {
            return false;
        }

        let clock_secs = self.current_clock_secs();

        if clock_secs == self.last_clock_secs {
            return false;
        }

        self.last_clock_secs = clock_secs;

        let hours = (clock_secs / 3600) % 24;
        let minutes = (clock_secs / 60) % 60;
        let seconds = clock_secs % 60;
        let clock_chars: [u8; 8] = [
            b'0' + (hours / 10) as u8,
            b'0' + (hours % 10) as u8,
            b':',
            b'0' + (minutes / 10) as u8,
            b'0' + (minutes % 10) as u8,
            b':',
            b'0' + (seconds / 10) as u8,
            b'0' + (seconds % 10) as u8,
        ];
        let clock_text = core::str::from_utf8(&clock_chars).unwrap_or("00:00:00");
        let tnum = fonts::Feature::new(fonts::Tag::new(b"tnum"), 1, ..);
        let (clock_count, _) = shape_text(
            FONT_SANS,
            clock_text,
            presenter_service::FONT_SIZE,
            &[tnum],
            &mut self.glyphs,
        );
        let _ = clock_count;
        let mut scene = SceneWriter::from_existing(self.scene_buf);

        scene.write_shaped_glyphs_at(self.clock_glyph_ref, &self.glyphs[..8]);
        scene.commit();

        true
    }

    // ── Navigation ─────────────────────────────────────────────

    fn find_cursor_line(&self, cursor_pos: usize, line_count: usize) -> (usize, usize) {
        for i in 0..line_count {
            let line = parse_line_at(&self.results_buf, i);
            let start = line.byte_offset as usize;
            let next_start = if i + 1 < line_count {
                parse_line_at(&self.results_buf, i + 1).byte_offset as usize
            } else {
                usize::MAX
            };

            if cursor_pos < next_start {
                return (i, cursor_pos.saturating_sub(start));
            }
        }

        if line_count > 0 {
            let last = parse_line_at(&self.results_buf, line_count - 1);

            (
                line_count - 1,
                cursor_pos.saturating_sub(last.byte_offset as usize),
            )
        } else {
            (0, 0)
        }
    }

    fn line_start_byte(&self, line_idx: usize) -> usize {
        parse_line_at(&self.results_buf, line_idx).byte_offset as usize
    }

    fn line_end_byte(&self, line_idx: usize) -> usize {
        let line = parse_line_at(&self.results_buf, line_idx);

        (line.byte_offset + line.byte_length) as usize
    }

    fn visible_lines(&self) -> usize {
        let viewport_height = self
            .display_height
            .saturating_sub(presenter_service::MARGIN_TOP as u32 * 2);

        (viewport_height / presenter_service::LINE_HEIGHT).max(1) as usize
    }

    fn nav_target(
        &self,
        key_code: u16,
        modifiers: u8,
        cursor_pos: usize,
        content_len: usize,
    ) -> Option<usize> {
        let alt = modifiers & text_editor::MOD_ALT != 0;
        let cmd = modifiers & text_editor::MOD_SUPER != 0;
        let header = parse_layout_header(&self.results_buf);
        let line_count = header.line_count as usize;

        if line_count == 0 {
            return Some(0);
        }

        let (cur_line, cur_col) = self.find_cursor_line(cursor_pos, line_count);

        match key_code {
            text_editor::HID_KEY_LEFT => {
                if cmd {
                    Some(self.line_start_byte(cur_line))
                } else if alt {
                    // SAFETY: doc_va is a valid RO mapping of the document buffer.
                    let text =
                        unsafe { document_service::doc_content_slice(self.doc_va, content_len) };

                    Some(layout::word_boundary_backward(text, cursor_pos))
                } else {
                    Some(cursor_pos.saturating_sub(1))
                }
            }
            text_editor::HID_KEY_RIGHT => {
                if cmd {
                    Some(self.line_end_byte(cur_line))
                } else if alt {
                    let text =
                        unsafe { document_service::doc_content_slice(self.doc_va, content_len) };

                    Some(layout::word_boundary_forward(text, cursor_pos))
                } else {
                    Some((cursor_pos + 1).min(content_len))
                }
            }
            text_editor::HID_KEY_UP => {
                if cmd {
                    Some(0)
                } else {
                    let col = self.sticky_col.unwrap_or(cur_col as u32) as usize;

                    if cur_line == 0 {
                        return Some(self.line_start_byte(0));
                    }

                    let target_line = cur_line - 1;
                    let target_line_info = parse_line_at(&self.results_buf, target_line);
                    let target_col = col.min(target_line_info.byte_length as usize);

                    Some(target_line_info.byte_offset as usize + target_col)
                }
            }
            text_editor::HID_KEY_DOWN => {
                if cmd {
                    Some(content_len)
                } else {
                    let col = self.sticky_col.unwrap_or(cur_col as u32) as usize;

                    if cur_line + 1 >= line_count {
                        return Some(self.line_end_byte(cur_line));
                    }

                    let target_line = cur_line + 1;
                    let target_line_info = parse_line_at(&self.results_buf, target_line);
                    let target_col = col.min(target_line_info.byte_length as usize);

                    Some(target_line_info.byte_offset as usize + target_col)
                }
            }
            text_editor::HID_KEY_HOME => Some(self.line_start_byte(cur_line)),
            text_editor::HID_KEY_END => Some(self.line_end_byte(cur_line)),
            text_editor::HID_KEY_PAGE_UP => {
                let page = self.visible_lines();

                if cur_line == 0 {
                    return Some(self.line_start_byte(0));
                }

                let col = self.sticky_col.unwrap_or(cur_col as u32) as usize;
                let target_line = cur_line.saturating_sub(page);
                let target_line_info = parse_line_at(&self.results_buf, target_line);
                let target_col = col.min(target_line_info.byte_length as usize);

                Some(target_line_info.byte_offset as usize + target_col)
            }
            text_editor::HID_KEY_PAGE_DOWN => {
                let page = self.visible_lines();

                if cur_line + 1 >= line_count {
                    return Some(self.line_end_byte(cur_line));
                }

                let col = self.sticky_col.unwrap_or(cur_col as u32) as usize;
                let target_line = (cur_line + page).min(line_count - 1);
                let target_line_info = parse_line_at(&self.results_buf, target_line);
                let target_col = col.min(target_line_info.byte_length as usize);

                Some(target_line_info.byte_offset as usize + target_col)
            }
            _ => None,
        }
    }

    fn is_vertical_nav(key_code: u16) -> bool {
        matches!(
            key_code,
            text_editor::HID_KEY_UP
                | text_editor::HID_KEY_DOWN
                | text_editor::HID_KEY_PAGE_UP
                | text_editor::HID_KEY_PAGE_DOWN
        )
    }

    fn handle_key_event(&mut self, dispatch: text_editor::KeyDispatch) {
        let ctrl = dispatch.modifiers & text_editor::MOD_CONTROL != 0;

        if dispatch.key_code == text_editor::HID_KEY_TAB && ctrl {
            if self.num_spaces <= 1 {
                return;
            }

            let new_space = (self.active_space + 1) % self.num_spaces;

            self.active_space = new_space;

            let target = new_space as f32 * self.display_width as f32;

            self.slide_spring.set_target(target);
            self.slide_animating = true;
            self.last_anim_tick = abi::system::clock_read().unwrap_or(0);
            self.blink_start = self.last_anim_tick;
            self.build_scene();
            self.request_render();

            return;
        }

        if self.active_space != 0 {
            return;
        }

        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        let has_selection = sel_anchor != cursor_pos;
        let shift = dispatch.modifiers & text_editor::MOD_SHIFT != 0;
        let cmd = dispatch.modifiers & text_editor::MOD_SUPER != 0;

        // Cmd+A: select all.
        if cmd && dispatch.character == b'a' {
            self.doc_select(0, content_len);
            self.sticky_col = None;
            self.blink_start = abi::system::clock_read().unwrap_or(0);
            self.build_scene();
            self.request_render();

            return;
        }

        if let Some(mut target) = self.nav_target(
            dispatch.key_code,
            dispatch.modifiers,
            cursor_pos,
            content_len,
        ) {
            let is_vertical = Self::is_vertical_nav(dispatch.key_code);

            if !shift && has_selection {
                let sel_start = sel_anchor.min(cursor_pos);
                let sel_end = sel_anchor.max(cursor_pos);

                match dispatch.key_code {
                    text_editor::HID_KEY_LEFT => target = sel_start,
                    text_editor::HID_KEY_RIGHT => target = sel_end,
                    _ => {}
                }
            }

            if shift {
                let anchor = if has_selection {
                    sel_anchor
                } else {
                    cursor_pos
                };

                self.doc_select(anchor, target);
            } else {
                self.doc_cursor_move(target);
            }

            if is_vertical {
                if self.sticky_col.is_none() {
                    let header = parse_layout_header(&self.results_buf);
                    let (_, col) = self.find_cursor_line(cursor_pos, header.line_count as usize);

                    self.sticky_col = Some(col as u32);
                }
            } else {
                self.sticky_col = None;
            }
        } else {
            let mut data = [0u8; text_editor::KeyDispatch::SIZE];

            dispatch.write_to(&mut data);

            let _ = ipc::client::call_simple(self.editor_ep, text_editor::DISPATCH_KEY, &data);

            self.sticky_col = None;
        }

        self.blink_start = abi::system::clock_read().unwrap_or(0);
        self.build_scene();
        self.request_render();
    }

    fn doc_select(&self, anchor: usize, cursor: usize) {
        let sel = document_service::Selection {
            anchor: anchor as u64,
            cursor: cursor as u64,
        };
        let mut data = [0u8; document_service::Selection::SIZE];

        sel.write_to(&mut data);

        let _ = ipc::client::call_simple(self.doc_ep, document_service::SELECT, &data);
    }

    fn doc_cursor_move(&self, pos: usize) {
        let req = document_service::CursorMove {
            position: pos as u64,
        };
        let mut data = [0u8; document_service::CursorMove::SIZE];

        req.write_to(&mut data);

        let _ = ipc::client::call_simple(self.doc_ep, document_service::CURSOR_MOVE, &data);
    }

    fn request_render(&self) {
        let _ = ipc::client::call_simple(self.render_ep, render::comp::RENDER, &[]);
    }

    // ── Cursor shape resolution ──────────────────────────────────

    fn resolve_cursor_shape(&self) -> u8 {
        let scene = SceneWriter::from_existing(unsafe {
            core::slice::from_raw_parts_mut(self.scene_buf.as_ptr() as *mut u8, SCENE_SIZE)
        });
        let node_count = scene.node_count() as usize;

        if node_count == 0 {
            return scene::CURSOR_POINTER;
        }

        let test_x = (self.pointer_x as i64) * (scene::MPT_PER_PT as i64);
        let test_y = (self.pointer_y as i64) * (scene::MPT_PER_PT as i64);
        let mut parent = [scene::NULL; 64];
        let mut hit: scene::NodeId = scene::NULL;
        let mut stack: [(scene::NodeId, i64, i64); 48] = [(scene::NULL, 0, 0); 48];
        let mut sp: usize = 0;
        let root = scene.node(0);

        if root.flags.contains(NodeFlags::VISIBLE) {
            stack[0] = (0, 0, 0);
            sp = 1;
        }

        while sp > 0 {
            sp -= 1;

            let (id, ox, oy) = stack[sp];
            let node = scene.node(id);
            let abs_x = ox + node.x as i64;
            let abs_y = oy + node.y as i64;
            let inside = test_x >= abs_x
                && test_x < abs_x + node.width as i64
                && test_y >= abs_y
                && test_y < abs_y + node.height as i64;

            if inside {
                hit = id;
            }

            if node.clips_children() && !inside {
                continue;
            }

            let child_ox = abs_x - (node.child_offset_x * scene::MPT_PER_PT as f32) as i64;
            let child_oy = abs_y - (node.child_offset_y * scene::MPT_PER_PT as f32) as i64;
            let mut children: [scene::NodeId; 16] = [scene::NULL; 16];
            let mut nc: usize = 0;
            let mut c = node.first_child;

            while c != scene::NULL && (c as usize) < node_count && nc < 16 {
                children[nc] = c;
                parent[c as usize & 63] = id;
                nc += 1;
                c = scene.node(c).next_sibling;
            }

            for i in (0..nc).rev() {
                let cid = children[i];

                if scene.node(cid).flags.contains(NodeFlags::VISIBLE) && sp < stack.len() {
                    stack[sp] = (cid, child_ox, child_oy);
                    sp += 1;
                }
            }
        }

        let mut cursor_node = hit;

        while cursor_node != scene::NULL && (cursor_node as usize) < node_count {
            let shape = scene.node(cursor_node).cursor_shape;

            if shape != scene::CURSOR_INHERIT {
                return shape;
            }

            cursor_node = parent[cursor_node as usize & 63];
        }

        scene::CURSOR_POINTER
    }

    fn update_cursor_shape(&mut self) {
        let shape = self.resolve_cursor_shape();

        if shape != self.cursor_shape_name {
            self.cursor_shape_name = shape;

            let payload = [shape];
            let _ =
                ipc::client::call_simple(self.render_ep, render::comp::SET_CURSOR_SHAPE, &payload);
        }
    }

    // ── Pixel-to-byte hit testing ────────────────────────────────

    fn page_rect(&self) -> (u32, u32, u32, u32) {
        let title_bar_h = presenter_service::TITLE_BAR_H;
        let page_margin = presenter_service::PAGE_MARGIN_V;
        let content_h = self.display_height.saturating_sub(title_bar_h);
        let page_h = content_h.saturating_sub(2 * page_margin);
        let page_w = ((page_h as u64 * 210 / 297) as u32)
            .min(self.display_width.saturating_sub(2 * page_margin));
        let page_x = (self.display_width - page_w) / 2;
        let page_y = title_bar_h + page_margin;

        (page_x, page_y, page_w, page_h)
    }

    fn is_on_page(&self, px: u32, py: u32) -> bool {
        let (page_x, page_y, page_w, page_h) = self.page_rect();

        px >= page_x && px < page_x + page_w && py >= page_y && py < page_y + page_h
    }

    fn text_origin(&self) -> (u32, u32) {
        let (page_x, page_y, _, _) = self.page_rect();
        let page_padding = presenter_service::PAGE_PADDING;

        (page_x + page_padding, page_y + page_padding)
    }

    fn xy_to_byte(&self, px: u32, py: u32, content_len: usize) -> usize {
        let (text_x, text_y) = self.text_origin();
        let rel_x = px.saturating_sub(text_x);
        let rel_y = py.saturating_sub(text_y) as i32 + self.scroll_y;

        if rel_y < 0 {
            return 0;
        }

        let header = parse_layout_header(&self.results_buf);
        let line_count = header.line_count as usize;

        if line_count == 0 {
            return 0;
        }

        let line_h = presenter_service::LINE_HEIGHT as i32;
        let target_line = (rel_y / line_h) as usize;
        let cw = self.char_width;
        let target_col = if cw > 0.0 {
            ((rel_x as f32 + cw * 0.5) / cw) as usize
        } else {
            0
        };

        if target_line >= line_count {
            return content_len;
        }

        let line = parse_line_at(&self.results_buf, target_line);
        let line_start = line.byte_offset as usize;
        let line_len = line.byte_length as usize;
        let pos = line_start + target_col.min(line_len);

        pos.min(content_len)
    }

    // ── Pointer button handling ──────────────────────────────────

    fn handle_pointer_button(&mut self, btn: presenter_service::PointerButton) {
        if btn.button != 0 {
            return;
        }

        if btn.pressed == 0 {
            self.dragging = false;

            return;
        }

        let click_x = (btn.abs_x as u64 * self.display_width as u64 / 32768) as u32;
        let click_y = (btn.abs_y as u64 * self.display_height as u64 / 32768) as u32;

        if !self.is_on_page(click_x, click_y) {
            return;
        }

        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, _cursor_pos, _sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        let byte_pos = self.xy_to_byte(click_x, click_y, content_len);

        let now_ns = abi::system::clock_read().unwrap_or(0);
        let now_ms = now_ns / 1_000_000;

        let mpt = scene::MPT_PER_PT as u32;
        let dx_mpt = click_x.abs_diff(self.last_click_x) * mpt;
        let dy_mpt = click_y.abs_diff(self.last_click_y) * mpt;
        let dt = now_ms.saturating_sub(self.last_click_ms);
        let click_tolerance_mpt = 4 * mpt;
        let same_spot = dx_mpt <= click_tolerance_mpt && dy_mpt <= click_tolerance_mpt && dt <= 400;

        let click_count = if same_spot {
            (self.click_count % 3) + 1
        } else {
            1
        };

        self.last_click_ms = now_ms;
        self.last_click_x = click_x;
        self.last_click_y = click_y;
        self.click_count = click_count;

        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };

        match click_count {
            2 => {
                let at_word = byte_pos < content_len && !layout::is_whitespace(content[byte_pos]);
                let back_pos = if at_word { byte_pos + 1 } else { byte_pos };
                let lo = layout::word_boundary_backward(content, back_pos);
                let mut hi = byte_pos;

                while hi < content_len && !layout::is_whitespace(content[hi]) {
                    hi += 1;
                }

                if hi > lo {
                    self.doc_select(lo, hi);
                } else {
                    self.doc_cursor_move(byte_pos);
                }
            }
            3 => {
                let header = parse_layout_header(&self.results_buf);
                let line_count = header.line_count as usize;
                let (line_idx, _) = self.find_cursor_line(byte_pos, line_count);
                let lo = self.line_start_byte(line_idx);
                let mut hi = self.line_end_byte(line_idx);

                if hi < content_len && content[hi] == b'\n' {
                    hi += 1;
                }

                self.doc_select(lo, hi);
            }
            _ => {
                self.doc_select(byte_pos, byte_pos);
            }
        }

        self.dragging = true;
        // SAFETY: re-read header after doc_select may have changed it.
        let (_cl, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        self.drag_origin_start = sel_anchor;
        self.drag_origin_end = cursor_pos;
        self.sticky_col = None;
        self.blink_start = abi::system::clock_read().unwrap_or(0);
        self.build_scene();
        self.request_render();
    }

    fn handle_pointer_drag(&mut self, abs_x: u32, abs_y: u32) {
        if !self.dragging {
            return;
        }

        let drag_x = (abs_x as u64 * self.display_width as u64 / 32768) as u32;
        let drag_y = (abs_y as u64 * self.display_height as u64 / 32768) as u32;

        if !self.is_on_page(drag_x, drag_y) {
            return;
        }

        // SAFETY: doc_va is a valid RO mapping.
        let (content_len, _, _, _) = unsafe { document_service::read_doc_header(self.doc_va) };
        let byte_pos = self.xy_to_byte(drag_x, drag_y, content_len);
        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };

        match self.click_count {
            2 => {
                if byte_pos < self.drag_origin_start {
                    let lo = layout::word_boundary_backward(content, byte_pos);

                    self.doc_select(self.drag_origin_end, lo);
                } else if byte_pos >= self.drag_origin_end {
                    let mut hi = byte_pos;

                    while hi < content_len && !layout::is_whitespace(content[hi]) {
                        hi += 1;
                    }

                    self.doc_select(self.drag_origin_start, hi);
                } else {
                    self.doc_select(self.drag_origin_start, self.drag_origin_end);
                }
            }
            3 => {
                let header = parse_layout_header(&self.results_buf);
                let line_count = header.line_count as usize;

                if byte_pos < self.drag_origin_start {
                    let (line_idx, _) = self.find_cursor_line(byte_pos, line_count);
                    let lo = self.line_start_byte(line_idx);

                    self.doc_select(self.drag_origin_end, lo);
                } else if byte_pos >= self.drag_origin_end {
                    let (line_idx, _) = self.find_cursor_line(byte_pos, line_count);
                    let mut hi = self.line_end_byte(line_idx);

                    if hi < content_len && content[hi] == b'\n' {
                        hi += 1;
                    }

                    self.doc_select(self.drag_origin_start, hi);
                } else {
                    self.doc_select(self.drag_origin_start, self.drag_origin_end);
                }
            }
            _ => {
                self.doc_select(self.drag_origin_start, byte_pos);
            }
        }

        self.blink_start = abi::system::clock_read().unwrap_or(0);
        self.build_scene();
        self.request_render();
    }

    fn make_info_reply(&self) -> presenter_service::InfoReply {
        let scene = SceneWriter::from_existing(unsafe {
            core::slice::from_raw_parts_mut(self.scene_buf.as_ptr() as *mut u8, SCENE_SIZE)
        });

        presenter_service::InfoReply {
            node_count: scene.node_count(),
            generation: scene.generation(),
            line_count: self.last_line_count,
            cursor_line: self.last_cursor_line,
            cursor_col: self.last_cursor_col,
            content_len: self.last_content_len,
            scroll_y: self.scroll_y,
        }
    }
}

// ── Rich text cursor ─────────────────────────────────────────────

fn compute_rich_cursor(
    results_buf: &[u8],
    layout_header: &layout_service::LayoutHeader,
    cursor_pos: usize,
    doc_va: usize,
) -> (f32, i32, u32) {
    let run_count = layout_header.visible_run_count as usize;

    if run_count == 0 {
        return (0.0, 0, 20);
    }

    // SAFETY: doc_va is a valid RO mapping.
    let (content_len, _, _, _) = unsafe { document_service::read_doc_header(doc_va) };
    let doc_buf = unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            content_len,
        )
    };

    if !piecetable::validate(doc_buf) {
        return (0.0, 0, 20);
    }

    // Find the run containing cursor_pos and compute x offset within it.
    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);
        let run_start = vr.byte_offset as usize;
        let run_end = run_start + vr.byte_length as usize;

        if cursor_pos < run_start || cursor_pos > run_end {
            continue;
        }

        let font_data = font_data_for_family(vr.font_family);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);

        let mut axes_buf = [fonts::metrics::AxisValue {
            tag: [0; 4],
            value: 0.0,
        }; 2];
        let mut axis_count = 0;

        if vr.weight != 400 {
            axes_buf[axis_count] = fonts::metrics::AxisValue {
                tag: *b"wght",
                value: vr.weight as f32,
            };
            axis_count += 1;
        }

        axes_buf[axis_count] = fonts::metrics::AxisValue {
            tag: *b"opsz",
            value: vr.font_size as f32,
        };
        axis_count += 1;

        let axes = &axes_buf[..axis_count];

        // Extract run text from piecetable.
        let text_len = piecetable::text_len(doc_buf) as usize;
        let chars_before = cursor_pos - run_start;
        let extract_len = chars_before.min(text_len.saturating_sub(run_start));
        let mut text_buf = alloc::vec![0u8; extract_len + 1];
        let copied =
            piecetable::text_slice(doc_buf, run_start as u32, cursor_pos as u32, &mut text_buf);

        let mut x = vr.x;

        for ch in core::str::from_utf8(&text_buf[..copied])
            .unwrap_or("")
            .chars()
        {
            let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
            let advance_fu = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                .unwrap_or(upem as i32 / 2);

            x += (advance_fu as f32 * vr.font_size as f32) / upem as f32;
        }

        let line_height = (vr.font_size as u32 * 14) / 10;

        return (x, vr.y, line_height);
    }

    // Cursor past last run — position at end of last run.
    if run_count > 0 {
        let last = parse_visible_run_at(results_buf, run_count - 1);
        let line_height = (last.font_size as u32 * 14) / 10;

        // Approximate: use the last run's x + estimated width.
        return (
            last.x + last.byte_length as f32 * last.font_size as f32 * 0.5,
            last.y,
            line_height,
        );
    }

    (0.0, 0, 20)
}

// ── Rich text rendering ──────────────────────────────────────────

fn build_rich_text_nodes(
    scene: &mut SceneWriter,
    viewport: scene::NodeId,
    layout_header: &layout_service::LayoutHeader,
    results_buf: &[u8],
    doc_va: usize,
    display_width: u32,
    glyphs: &mut [ShapedGlyph; MAX_GLYPHS_PER_LINE],
) {
    let run_count = layout_header.visible_run_count as usize;

    if run_count == 0 {
        return;
    }

    // SAFETY: doc_va is a valid RO mapping of the document buffer.
    let (content_len, _, _, _) = unsafe { document_service::read_doc_header(doc_va) };
    let doc_buf = unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            content_len,
        )
    };

    if !piecetable::validate(doc_buf) {
        return;
    }

    let text_len = piecetable::text_len(doc_buf) as usize;
    let mut text_scratch = alloc::vec![0u8; text_len + 1];
    let copied = piecetable::text_slice(doc_buf, 0, text_len as u32, &mut text_scratch);

    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);
        let byte_start = vr.byte_offset as usize;
        let byte_len = vr.byte_length as usize;

        if byte_start + byte_len > copied || byte_len == 0 {
            continue;
        }

        let run_text = &text_scratch[byte_start..byte_start + byte_len];
        let font_data = font_data_for_family(vr.font_family);
        let sid = style_id_for_family(vr.font_family);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let mut axes_buf = [fonts::metrics::AxisValue {
            tag: [0; 4],
            value: 0.0,
        }; 2];
        let mut axis_count = 0;

        if vr.weight != 400 {
            axes_buf[axis_count] = fonts::metrics::AxisValue {
                tag: *b"wght",
                value: vr.weight as f32,
            };
            axis_count += 1;
        }

        axes_buf[axis_count] = fonts::metrics::AxisValue {
            tag: *b"opsz",
            value: vr.font_size as f32,
        };
        axis_count += 1;

        let axes = &axes_buf[..axis_count];
        let mut glyph_count = 0usize;

        for ch in core::str::from_utf8(run_text).unwrap_or("").chars() {
            if glyph_count >= MAX_GLYPHS_PER_LINE {
                break;
            }

            let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
            let advance_fu = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                .unwrap_or(upem as i32 / 2);
            let advance_fp = (advance_fu as i64 * vr.font_size as i64 * 65536 / upem as i64) as i32;

            glyphs[glyph_count] = ShapedGlyph {
                glyph_id: gid,
                _pad: 0,
                x_advance: advance_fp,
                x_offset: 0,
                y_offset: 0,
            };
            glyph_count += 1;
        }

        if glyph_count == 0 {
            continue;
        }

        let glyph_ref = scene.push_shaped_glyphs(&glyphs[..glyph_count]);
        let run_node = match scene.alloc_node() {
            Some(id) => id,
            None => break,
        };
        let color = Color::rgba(
            ((vr.color_rgba >> 24) & 0xFF) as u8,
            ((vr.color_rgba >> 16) & 0xFF) as u8,
            ((vr.color_rgba >> 8) & 0xFF) as u8,
            (vr.color_rgba & 0xFF) as u8,
        );
        let line_height = (vr.font_size as u32 * 14) / 10;

        {
            let n = scene.node_mut(run_node);

            n.x = scene::f32_to_mpt(vr.x);
            n.y = pt(vr.y);
            n.width = upt(display_width);
            n.height = upt(line_height);
            n.content = Content::Glyphs {
                color,
                glyphs: glyph_ref,
                glyph_count: glyph_count as u16,
                font_size: vr.font_size,
                style_id: sid,
            };
            n.role = scene::ROLE_PARAGRAPH;
        }

        scene.add_child(viewport, run_node);

        if vr.flags & piecetable::FLAG_UNDERLINE != 0 {
            let run_width: f32 = glyphs[..glyph_count]
                .iter()
                .map(|g| (g.x_advance as f32) / 65536.0)
                .sum();
            let baseline_y = vr.y + (vr.font_size as i32 * 11) / 10;
            let thickness = (vr.font_size as u32 / 14).max(1);

            if let Some(ul_id) = scene.alloc_node() {
                let n = scene.node_mut(ul_id);

                n.x = scene::f32_to_mpt(vr.x);
                n.y = pt(baseline_y);
                n.width = upt(run_width as u32 + 1);
                n.height = upt(thickness);
                n.background = color;

                scene.add_child(viewport, ul_id);
            }
        }
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

// ── Embedded image decode + upload ───────────────────────────────

const IMAGE_CONTENT_ID: u32 = 1;

fn decode_embedded_image(server: &mut Presenter, render_ep: Handle) {
    let header = match png_lib::png_header(TEST_PNG) {
        Ok(h) => h,
        Err(_) => return,
    };
    let buf_size = match png_lib::png_decode_buf_size(TEST_PNG) {
        Ok(s) => s,
        Err(_) => return,
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let decode_vmo_size = buf_size.next_multiple_of(PAGE_SIZE);
    let decode_vmo = match abi::vmo::create(decode_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => return,
    };
    let decode_va = match abi::vmo::map(decode_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::handle::close(decode_vmo);

            return;
        }
    };
    // SAFETY: decode_va is a valid RW mapping of at least buf_size bytes.
    let output = unsafe { core::slice::from_raw_parts_mut(decode_va as *mut u8, buf_size) };

    if png_lib::png_decode(TEST_PNG, output).is_err() {
        let _ = abi::vmo::unmap(decode_va);
        let _ = abi::handle::close(decode_vmo);

        return;
    }

    let width = header.width as u16;
    let height = header.height as u16;
    let pixel_size = header.width as u32 * header.height as u32 * 4;
    let pixel_vmo_size = (pixel_size as usize).next_multiple_of(PAGE_SIZE);
    let pixel_vmo = match abi::vmo::create(pixel_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => {
            let _ = abi::vmo::unmap(decode_va);
            let _ = abi::handle::close(decode_vmo);

            return;
        }
    };
    let pixel_va = match abi::vmo::map(pixel_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => {
            let _ = abi::vmo::unmap(decode_va);
            let _ = abi::handle::close(decode_vmo);
            let _ = abi::handle::close(pixel_vmo);

            return;
        }
    };

    // SAFETY: both mappings are valid and non-overlapping.
    unsafe {
        core::ptr::copy_nonoverlapping(
            decode_va as *const u8,
            pixel_va as *mut u8,
            pixel_size as usize,
        );
    }

    let _ = abi::vmo::unmap(decode_va);
    let _ = abi::handle::close(decode_vmo);
    let _ = abi::vmo::unmap(pixel_va);

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
    let mut recv_handles = [0u32; 4];
    let _ = ipc::client::call(
        render_ep,
        render::comp::UPLOAD_IMAGE,
        &upload_buf,
        &[pixel_dup.0],
        &mut recv_handles,
        &mut reply_buf,
    );

    server.image_content_id = IMAGE_CONTENT_ID;
    server.image_width = width;
    server.image_height = height;

    console::write(server.console_ep, b"presenter: embedded image ready\n");
}

// ── Showcase scene (space 2) ────────────────────────────────────

fn build_showcase_nodes(
    scene: &mut SceneWriter,
    strip: scene::NodeId,
    display_width: u32,
    content_h: u32,
    now_ns: u64,
) {
    let base_x = (display_width * 2) as i32;
    let container = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };

    {
        let n = scene.node_mut(container);

        n.x = pt(base_x);
        n.width = upt(display_width);
        n.height = upt(content_h);
        n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
    }

    scene.add_child(strip, container);

    // Outer group — translated container with a subtle conical gradient bg.
    let group = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };

    {
        let g = scene.node_mut(group);

        g.x = pt(0);
        g.y = pt(0);
        g.width = upt(400);
        g.height = upt(400);
        g.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
        g.child_offset_x = 48.0;
        g.child_offset_y = 48.0;
        g.content = Content::Gradient {
            color_start: Color::rgba(255, 255, 255, 10),
            color_end: Color::rgba(255, 255, 255, 0),
            kind: scene::GradientKind::Radial,
            _pad: 48,
            angle_fp: scene::angle_to_fp(0.0),
        };
        g.transform = scene::AffineTransform::translate(20.0, 20.0);
    }

    scene.add_child(container, group);

    if let Some(id) = scene.alloc_node() {
        let a = scene.node_mut(id);

        a.x = pt(100);
        a.y = pt(60);
        a.width = upt(120);
        a.height = upt(120);
        a.background = Color::rgb(255, 0, 0);

        scene.add_child(group, id);
    }
    if let Some(id) = scene.alloc_node() {
        let b = scene.node_mut(id);

        b.x = pt(60);
        b.y = pt(100);
        b.width = upt(120);
        b.height = upt(120);
        b.background = Color::rgb(0, 255, 0);
        b.shadow_color = Color::rgba(0, 0, 0, 180);
        b.shadow_blur_radius = 16;
        b.shadow_spread = 4;
        b.corner_radius = 60;
        b.opacity = 128;
        b.transform = scene::AffineTransform::scale(0.8, 0.8);

        scene.add_child(group, id);
    }
    if let Some(id) = scene.alloc_node() {
        let c = scene.node_mut(id);

        c.x = pt(140);
        c.y = pt(140);
        c.width = upt(120);
        c.height = upt(120);
        c.background = Color::rgba(0, 0, 255, 128);
        c.backdrop_blur_radius = 64;
        c.shadow_color = Color::rgba(0, 0, 0, 180);
        c.shadow_blur_radius = 16;
        c.shadow_spread = 4;
        c.corner_radius = 16;
        c.opacity = 240;
        c.transform = scene::AffineTransform::rotate(0.3);

        scene.add_child(group, id);
    }

    // Star filled with conical gradient.
    {
        let mut path_buf = alloc::vec::Vec::new();
        let cx = 100.0f32;
        let cy = 100.0;
        let outer = 90.0;
        let inner = 38.0;

        const STAR_CS: [(f32, f32); 10] = [
            (0.0, -1.0),
            (0.5878, -0.8090),
            (0.9511, -0.3090),
            (0.9511, 0.3090),
            (0.5878, 0.8090),
            (0.0, 1.0),
            (-0.5878, 0.8090),
            (-0.9511, 0.3090),
            (-0.9511, -0.3090),
            (-0.5878, -0.8090),
        ];

        for (i, &(sin_a, cos_a)) in STAR_CS.iter().enumerate() {
            let r = if i % 2 == 0 { outer } else { inner };
            let px = cx + r * sin_a;
            let py = cy + r * cos_a;

            if i == 0 {
                scene::path_move_to(&mut path_buf, px, py);
            } else {
                scene::path_line_to(&mut path_buf, px, py);
            }
        }

        scene::path_close(&mut path_buf);

        let path_ref = scene.push_data(&path_buf);

        if let Some(id) = scene.alloc_node() {
            let n = scene.node_mut(id);

            n.x = pt(80);
            n.y = pt(300);
            n.width = upt(200);
            n.height = upt(200);
            n.content = Content::GradientPath {
                color_start: Color::rgb(241, 196, 15),
                color_end: Color::rgb(231, 76, 60),
                kind: scene::GradientKind::Conical,
                _pad: 0,
                angle_fp: scene::angle_to_fp(4.71239),
                contours: path_ref,
            };

            scene.add_child(group, id);
        }
    }

    // ── Section: Easing sampler ──
    //
    // 5 colored squares travel back and forth horizontally, each with a
    // different easing curve. 2-second ping-pong cycle.
    // Inspired by the Phase 1 demo (commit 9f1b04a5).
    const CYCLE_NS: u64 = 2_000_000_000;
    const FULL_NS: u64 = CYCLE_NS * 2;

    let phase_ns = now_ns % FULL_NS;
    let raw_t = if phase_ns < CYCLE_NS {
        phase_ns as f32 / CYCLE_NS as f32
    } else {
        1.0 - (phase_ns - CYCLE_NS) as f32 / CYCLE_NS as f32
    };
    let travel = 200i32;
    let sq = 16u32;
    let row_h = 22i32;
    let total_w = travel + sq as i32;
    let demo_x = 20 + (400 - total_w) / 2;
    let demo_y = 560i32;
    let easings: [(animation::Easing, Color); 5] = [
        (animation::Easing::Linear, Color::rgb(255, 100, 100)),
        (animation::Easing::EaseOut, Color::rgb(255, 180, 50)),
        (animation::Easing::EaseInOut, Color::rgb(100, 220, 100)),
        (animation::Easing::EaseInBack, Color::rgb(80, 160, 255)),
        (animation::Easing::EaseOutBounce, Color::rgb(220, 80, 220)),
    ];

    for (i, &(easing, color)) in easings.iter().enumerate() {
        let y = demo_y + i as i32 * row_h;

        // Track.
        if let Some(id) = scene.alloc_node() {
            let n = scene.node_mut(id);

            n.x = pt(demo_x);
            n.y = pt(y + sq as i32 / 2 - 1);
            n.width = upt(travel as u32 + sq);
            n.height = upt(2);
            n.background = Color::rgba(255, 255, 255, 20);

            scene.add_child(container, id);
        }

        // Square.
        let eased = animation::ease(easing, raw_t);
        let sq_x = demo_x + (eased * travel as f32) as i32;

        if let Some(id) = scene.alloc_node() {
            let n = scene.node_mut(id);

            n.x = pt(sq_x);
            n.y = pt(y);
            n.width = upt(sq);
            n.height = upt(sq);
            n.background = color;
            n.corner_radius = 3;

            scene.add_child(container, id);
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
    let (display_width, display_height) = match ipc::client::call(
        render_ep,
        render::comp::SETUP,
        &[],
        &[scene_dup.0],
        &mut recv_handles,
        &mut reply_buf,
    ) {
        Ok(reply) if !reply.is_error() && reply.payload.len() >= render::comp::SetupReply::SIZE => {
            let sr = render::comp::SetupReply::read_from(reply.payload);

            (sr.display_width, sr.display_height)
        }
        _ => (
            presenter_service::DEFAULT_WIDTH,
            presenter_service::DEFAULT_HEIGHT,
        ),
    };

    console::write(console_ep, b"presenter: render connected\n");

    let editor_ep = match name::watch(HANDLE_NS_EP, b"editor.text") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: editor not found\n");

            abi::thread::exit(EXIT_EDITOR_NOT_FOUND);
        }
    };

    console::write(console_ep, b"presenter: editor connected\n");

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_ENDPOINT_CREATE),
    };

    name::register(HANDLE_NS_EP, b"presenter", own_ep);

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
        cmap_mono: build_cmap_table(FONT_MONO),
        cmap_sans: build_cmap_table(FONT_SANS),
        char_width: compute_char_advance(FONT_MONO),
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
        cursor_shape_name: scene::CURSOR_POINTER,
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
        console_ep,
    };

    decode_embedded_image(&mut server, render_ep);

    // Space 0 = text, 1 = image, 2 = showcase.
    server.num_spaces = 3;

    // Initial render: write viewport, build scene graph, tell compositor.
    server.write_viewport();
    server.build_scene();
    server.request_render();

    console::write(console_ep, b"presenter: ready\n");

    const NS_PER_SEC: u64 = 1_000_000_000;
    const FRAME_NS: u64 = 16_666_667;

    loop {
        let now = abi::system::clock_read().unwrap_or(0);
        let needs_anim = server.slide_animating || server.active_space == 2;
        let deadline = if needs_anim {
            now + FRAME_NS
        } else {
            let current_sec = now / NS_PER_SEC;

            (current_sec + 1) * NS_PER_SEC
        };

        match ipc::server::serve_one_timed(own_ep, &mut server, deadline) {
            Ok(()) => {
                if server.active_space == 2 {
                    server.build_scene();
                    server.request_render();
                }
            }
            Err(abi::types::SyscallError::TimedOut) => {
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

                if !server.slide_animating && server.active_space == 2 {
                    server.build_scene();
                    needs_render = true;
                }

                if server.update_clock() {
                    needs_render = true;
                }

                if needs_render {
                    server.request_render();
                }
            }
            Err(_) => break,
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
