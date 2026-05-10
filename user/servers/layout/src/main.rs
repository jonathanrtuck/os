//! Layout service — pure function from document content to positioned
//! text runs.
//!
//! Reads the document buffer (RO shared VMO from the document service)
//! and viewport state (seqlock register from the presenter). Computes
//! line breaks and positions using the layout library. Writes results
//! to a seqlock-protected VMO.
//!
//! For rich text (FORMAT_RICH), reads the piecetable from the document
//! buffer, iterates styled runs, measures character widths using
//! embedded font data, and outputs VisibleRun entries alongside lines.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;
extern crate piecetable;

use alloc::vec::Vec;
use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_FONT_VMO: Handle = Handle(3);
const HANDLE_SVC_EP: Handle = Handle(4);

const PAGE_SIZE: usize = 16384;
const RESULTS_VMO_SIZE: usize = PAGE_SIZE * 2;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE101;
const EXIT_DOC_NOT_FOUND: u32 = 0xE102;
const EXIT_DOC_SETUP: u32 = 0xE103;
const EXIT_RESULTS_CREATE: u32 = 0xE105;
const EXIT_RESULTS_MAP: u32 = 0xE106;

// ── Font data (shared VMO from init) ─────────────────────────────

static mut FONT_VA: usize = 0;

fn font(index: usize) -> &'static [u8] {
    unsafe { init::font_data(FONT_VA, index) }
}

fn font_data_for_style(family: u8, flags: u8) -> &'static [u8] {
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

fn font_upem(font_data: &[u8]) -> u16 {
    fonts::metrics::font_metrics(font_data)
        .map(|m| m.units_per_em)
        .unwrap_or(1000)
}

fn font_ascent_fu(font_data: &[u8]) -> i16 {
    fonts::metrics::font_metrics(font_data)
        .map(|m| m.ascent)
        .unwrap_or(800)
}

fn ascent_pt(font_data: &[u8], font_size_pt: u16, upem: u16) -> f32 {
    let ascent_fu = font_ascent_fu(font_data);

    ascent_fu as f32 * font_size_pt as f32 / upem as f32
}

fn char_advance_pt(
    font_data: &[u8],
    ch: char,
    font_size_pt: u16,
    upem: u16,
    axes: &[fonts::metrics::AxisValue],
) -> f32 {
    let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);

    if gid == 0 {
        return font_size_pt as f32 * 0.5;
    }

    let advance_fu =
        fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes).unwrap_or(upem as i32 / 2);

    (advance_fu as f32 * font_size_pt as f32) / upem as f32
}

// ── Font metrics adapter ──────────────────────────────────────────

struct MonoMetrics {
    char_width: f32,
    line_height: f32,
}

impl layout::FontMetrics for MonoMetrics {
    fn char_width(&self, _ch: char) -> f32 {
        self.char_width
    }

    fn line_height(&self) -> f32 {
        self.line_height
    }
}

// ── Layout server ─────────────────────────────────────────────────

struct LayoutServer {
    doc_va: usize,
    results_va: usize,
    results_vmo: Handle,

    viewport_va: usize,
    viewport_ready: bool,

    last_doc_gen: u32,
    last_line_count: u32,
    last_total_height: i32,
    last_content_len: u32,
    last_viewport_width: u32,
    last_line_height: u32,

    #[allow(dead_code)]
    console_ep: Handle,
}

impl LayoutServer {
    // ── Viewport state reading ────────────────────────────────────

    fn read_viewport(&self) -> layout_service::ViewportState {
        let mut buf = [0u8; layout_service::ViewportState::SIZE];
        // SAFETY: viewport_va is a valid RO mapping of the viewport
        // state register. Use the seqlock reader protocol.
        let mut reader = unsafe {
            ipc::register::Reader::new(
                self.viewport_va as *const u8,
                layout_service::ViewportState::SIZE,
            )
        };

        reader.read(&mut buf);

        layout_service::ViewportState::read_from(&buf)
    }

    // ── Layout computation ────────────────────────────────────────

    fn recompute(&mut self) {
        if !self.viewport_ready {
            return;
        }

        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, _, _, doc_gen) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        let format = unsafe { document_service::read_doc_format(self.doc_va) };
        let viewport = self.read_viewport();
        let cw = viewport.char_width();
        let lh = viewport.line_height as f32;

        if cw <= 0.0 || lh <= 0.0 || viewport.viewport_width == 0 {
            return;
        }

        if format == document_service::FORMAT_RICH {
            self.recompute_rich(content_len, viewport, doc_gen);
        } else {
            self.recompute_plain(content_len, viewport, cw, lh, doc_gen);
        }
    }

    fn recompute_plain(
        &mut self,
        content_len: usize,
        viewport: layout_service::ViewportState,
        cw: f32,
        lh: f32,
        doc_gen: u32,
    ) {
        let metrics = MonoMetrics {
            char_width: cw,
            line_height: lh,
        };
        let max_width = viewport.viewport_width as f32;
        // SAFETY: doc_va is valid and content_len comes from read_doc_header.
        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };
        let result = layout::layout_paragraph(
            content,
            &metrics,
            max_width,
            layout::Alignment::Left,
            &layout::WordBreaker,
        );
        let line_count = result.lines.len().min(layout_service::MAX_LINES);

        self.write_results_plain(
            &result.lines[..line_count],
            result.total_height,
            content_len,
        );

        self.last_doc_gen = doc_gen;
        self.last_line_count = line_count as u32;
        self.last_total_height = result.total_height;
        self.last_content_len = content_len as u32;
        self.last_viewport_width = viewport.viewport_width;
        self.last_line_height = viewport.line_height;
    }

    fn recompute_rich(
        &mut self,
        content_len: usize,
        viewport: layout_service::ViewportState,
        doc_gen: u32,
    ) {
        // SAFETY: doc_va + DOC_HEADER_SIZE is the piecetable buffer.
        let doc_buf = unsafe {
            core::slice::from_raw_parts(
                (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                content_len,
            )
        };

        if !piecetable::validate(doc_buf) {
            return;
        }

        let text_len = piecetable::text_len(doc_buf) as usize;
        let run_count = piecetable::styled_run_count(doc_buf);

        if run_count == 0 || text_len == 0 {
            self.write_results_plain(&[], 0, 0);
            self.last_doc_gen = doc_gen;

            return;
        }

        // Extract flat text from piecetable.
        let mut text_buf = alloc::vec![0u8; text_len + 1];
        let copied = piecetable::text_slice(doc_buf, 0, text_len as u32, &mut text_buf);
        let text = &text_buf[..copied];
        // Build MeasuredChar stream from styled runs.
        let max_width = viewport.viewport_width as f32;
        let mut measured: Vec<layout::MeasuredChar> = Vec::with_capacity(text_len);

        for run_idx in 0..run_count {
            let Some(run) = piecetable::styled_run(doc_buf, run_idx) else {
                continue;
            };
            let Some(style) = piecetable::style(doc_buf, run.style_id) else {
                continue;
            };
            let fi_data = font_data_for_style(style.font_family, style.flags);
            let fi_upem = font_upem(fi_data);
            let mut axes_buf = [fonts::metrics::AxisValue {
                tag: [0; 4],
                value: 0.0,
            }; 2];
            let mut axis_count = 0;

            if style.weight != 400 {
                axes_buf[axis_count] = fonts::metrics::AxisValue {
                    tag: *b"wght",
                    value: style.weight as f32,
                };
                axis_count += 1;
            }

            axes_buf[axis_count] = fonts::metrics::AxisValue {
                tag: *b"opsz",
                value: style.font_size_pt as f32,
            };
            axis_count += 1;

            let axes = &axes_buf[..axis_count];
            let run_start = run.byte_offset as usize;
            let run_end = (run_start + run.byte_len as usize).min(copied);
            let run_text = &text[run_start..run_end];
            let mut offset = run_start;

            for ch in core::str::from_utf8(run_text).unwrap_or("").chars() {
                let byte_len = ch.len_utf8() as u8;
                let width = if ch == '\n' {
                    0.0
                } else {
                    char_advance_pt(fi_data, ch, style.font_size_pt as u16, fi_upem, axes)
                };

                measured.push(layout::MeasuredChar {
                    byte_offset: offset as u32,
                    byte_len,
                    width,
                    run_index: run_idx as u16,
                    is_whitespace: ch == ' ' || ch == '\t',
                    is_newline: ch == '\n',
                });

                offset += byte_len as usize;
            }
        }

        // Break into lines.
        let line_breaks =
            layout::break_measured_lines(&measured, max_width, layout::BreakMode::Word);
        let default_line_height = viewport.line_height as i32;
        let mut lines: Vec<layout::LayoutLine> = Vec::with_capacity(line_breaks.len());
        let mut visible_runs: Vec<layout_service::VisibleRun> = Vec::new();
        let mut y = 0i32;
        let mut mc_cursor = 0usize;

        for (line_idx, lb) in line_breaks.iter().enumerate() {
            let line_byte_start = lb.byte_start;
            let line_byte_end = lb.byte_end;
            let byte_length = line_byte_end - line_byte_start;

            // Advance mc_cursor to the first MeasuredChar in this line.
            while mc_cursor < measured.len() && measured[mc_cursor].byte_offset < line_byte_start {
                mc_cursor += 1;
            }

            let runs_start = visible_runs.len();
            let mut max_ascent_pt = 0.0f32;
            let mut max_descent_pt = 0.0f32;
            let mut run_x = 0.0f32;
            let mut current_run_idx: Option<u16> = None;
            let mut run_start_byte = line_byte_start;
            let mut run_start_x = 0.0f32;
            let mut mc_i = mc_cursor;

            while mc_i < measured.len() && measured[mc_i].byte_offset < line_byte_end {
                let mc = &measured[mc_i];

                if let Some(run) = piecetable::styled_run(doc_buf, mc.run_index as usize)
                    && let Some(style) = piecetable::style(doc_buf, run.style_id)
                {
                    let fi_data = font_data_for_style(style.font_family, style.flags);
                    let fi_upem = font_upem(fi_data);
                    let asc = ascent_pt(fi_data, style.font_size_pt as u16, fi_upem);

                    if asc > max_ascent_pt {
                        max_ascent_pt = asc;
                    }

                    if let Some(fm) = fonts::metrics::font_metrics(fi_data) {
                        let desc = -fm.descent as f32 * style.font_size_pt as f32 / fi_upem as f32;

                        if desc > max_descent_pt {
                            max_descent_pt = desc;
                        }
                    }
                }

                if current_run_idx != Some(mc.run_index) {
                    if let Some(prev_run_idx) = current_run_idx
                        && visible_runs.len() < layout_service::MAX_VISIBLE_RUNS
                    {
                        emit_visible_run(
                            &mut visible_runs,
                            doc_buf,
                            prev_run_idx,
                            &RunPosition {
                                byte_offset: run_start_byte,
                                byte_length: mc.byte_offset - run_start_byte,
                                x: run_start_x,
                                y: 0,
                                line_index: line_idx as u16,
                            },
                        );
                    }

                    current_run_idx = Some(mc.run_index);
                    run_start_byte = mc.byte_offset;
                    run_start_x = run_x;
                }

                run_x += mc.width;
                mc_i += 1;
            }

            if let Some(prev_run_idx) = current_run_idx
                && visible_runs.len() < layout_service::MAX_VISIBLE_RUNS
            {
                emit_visible_run(
                    &mut visible_runs,
                    doc_buf,
                    prev_run_idx,
                    &RunPosition {
                        byte_offset: run_start_byte,
                        byte_length: line_byte_end - run_start_byte,
                        x: run_start_x,
                        y: 0,
                        line_index: line_idx as u16,
                    },
                );
            }

            // Adjust each run's y for baseline alignment: runs with smaller
            // ascent get pushed down so all runs share the same baseline.
            for vr in &mut visible_runs[runs_start..] {
                let fi_data = font_data_for_style(vr.font_family, vr.flags);
                let fi_upem = font_upem(fi_data);
                let run_ascent = ascent_pt(fi_data, vr.font_size, fi_upem);

                vr.y = y + (max_ascent_pt - run_ascent) as i32;
            }

            let line_h = if max_ascent_pt > 0.0 {
                let leading = (max_ascent_pt + max_descent_pt) * 0.2;

                (max_ascent_pt + max_descent_pt + leading) as i32
            } else {
                default_line_height
            };

            lines.push(layout::LayoutLine {
                byte_offset: line_byte_start,
                byte_length,
                x: 0.0,
                y,
                width: run_x,
            });

            y += line_h;
        }

        let line_count = lines.len().min(layout_service::MAX_LINES);
        let run_count = visible_runs.len().min(layout_service::MAX_VISIBLE_RUNS);

        self.write_results_rich(
            &lines[..line_count],
            &visible_runs[..run_count],
            y,
            text_len,
        );

        self.last_doc_gen = doc_gen;
        self.last_line_count = line_count as u32;
        self.last_total_height = y;
        self.last_content_len = content_len as u32;
        self.last_viewport_width = viewport.viewport_width;
        self.last_line_height = viewport.line_height;
    }

    // ── Results writing (via ipc::register::Writer) ──────────────

    fn write_results_plain(
        &mut self,
        lines: &[layout::LayoutLine],
        total_height: i32,
        content_len: usize,
    ) {
        let used =
            layout_service::LayoutHeader::SIZE + lines.len() * layout_service::LineInfo::SIZE;
        let mut buf = [0u8; layout_service::RESULTS_VALUE_SIZE];
        let header = layout_service::LayoutHeader {
            line_count: lines.len() as u32,
            total_height,
            content_len: content_len as u32,
            format: 0,
            _pad: 0,
            visible_run_count: 0,
        };

        header.write_to(&mut buf);

        for (i, line) in lines.iter().enumerate() {
            let off = layout_service::LayoutHeader::SIZE + i * layout_service::LineInfo::SIZE;
            let info = layout_service::LineInfo {
                byte_offset: line.byte_offset,
                byte_length: line.byte_length,
                x: line.x,
                y: line.y,
                width: line.width,
            };

            info.write_to(&mut buf[off..]);
        }

        // SAFETY: results_va is a valid RW mapping, 8-byte aligned.
        let mut writer = unsafe { ipc::register::Writer::new(self.results_va as *mut u8, used) };

        writer.write(&buf[..used]);
    }

    fn write_results_rich(
        &mut self,
        lines: &[layout::LayoutLine],
        runs: &[layout_service::VisibleRun],
        total_height: i32,
        content_len: usize,
    ) {
        let header = layout_service::LayoutHeader {
            line_count: lines.len() as u32,
            total_height,
            content_len: content_len as u32,
            format: 1,
            _pad: 0,
            visible_run_count: runs.len() as u16,
        };
        let runs_size = runs.len() * layout_service::VisibleRun::SIZE;
        let total_used = layout_service::LayoutHeader::SIZE
            + layout_service::MAX_LINES * layout_service::LineInfo::SIZE
            + runs_size;
        let mut buf = alloc::vec![0u8; total_used];

        header.write_to(&mut buf);

        for (i, line) in lines.iter().enumerate() {
            let off = layout_service::LayoutHeader::SIZE + i * layout_service::LineInfo::SIZE;
            let info = layout_service::LineInfo {
                byte_offset: line.byte_offset,
                byte_length: line.byte_length,
                x: line.x,
                y: line.y,
                width: line.width,
            };

            info.write_to(&mut buf[off..]);
        }

        let runs_offset = layout_service::VISIBLE_RUNS_OFFSET;

        for (i, run) in runs.iter().enumerate() {
            let off = runs_offset + i * layout_service::VisibleRun::SIZE;

            if off + layout_service::VisibleRun::SIZE <= buf.len() {
                run.write_to(&mut buf[off..]);
            }
        }

        // SAFETY: results_va is a valid RW mapping, 8-byte aligned,
        // of at least RESULTS_VMO_SIZE bytes.
        let mut writer =
            unsafe { ipc::register::Writer::new(self.results_va as *mut u8, total_used) };

        writer.write(&buf);
    }

    fn current_info_reply(&self) -> layout_service::InfoReply {
        layout_service::InfoReply {
            line_count: self.last_line_count,
            total_height: self.last_total_height,
            content_len: self.last_content_len,
            viewport_width: self.last_viewport_width,
            line_height: self.last_line_height,
        }
    }
}

struct RunPosition {
    byte_offset: u32,
    byte_length: u32,
    x: f32,
    y: i32,
    line_index: u16,
}

fn emit_visible_run(
    runs: &mut Vec<layout_service::VisibleRun>,
    doc_buf: &[u8],
    run_idx: u16,
    pos: &RunPosition,
) {
    let Some(run) = piecetable::styled_run(doc_buf, run_idx as usize) else {
        return;
    };
    let Some(style) = piecetable::style(doc_buf, run.style_id) else {
        return;
    };
    let color_rgba = ((style.color[0] as u32) << 24)
        | ((style.color[1] as u32) << 16)
        | ((style.color[2] as u32) << 8)
        | (style.color[3] as u32);

    runs.push(layout_service::VisibleRun {
        x: pos.x,
        y: pos.y,
        byte_offset: pos.byte_offset,
        byte_length: pos.byte_length.min(u16::MAX as u32) as u16,
        font_size: style.font_size_pt as u16,
        style_id: run.style_id,
        font_family: style.font_family,
        flags: style.flags,
        _pad: 0,
        color_rgba,
        weight: style.weight,
        line_index: pos.line_index,
    });
}

// ── Dispatch ──────────────────────────────────────────────────────

impl Dispatch for LayoutServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            layout_service::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let viewport_vmo = Handle(msg.handles[0]);

                match abi::vmo::map(viewport_vmo, 0, Rights::READ_MAP) {
                    Ok(va) => {
                        self.viewport_va = va;
                        self.viewport_ready = true;
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);

                        return;
                    }
                }

                match abi::handle::dup(self.results_vmo, Rights::READ_MAP) {
                    Ok(dup) => {
                        let reply = layout_service::SetupReply {
                            max_lines: layout_service::MAX_LINES as u32,
                        };
                        let mut data = [0u8; layout_service::SetupReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[dup.0]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }
            layout_service::RECOMPUTE => {
                self.recompute();

                let reply = self.current_info_reply();
                let mut data = [0u8; layout_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            layout_service::GET_INFO => {
                let reply = self.current_info_reply();
                let mut data = [0u8; layout_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
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

    console::write(console_ep, b"layout: starting\n");

    unsafe {
        FONT_VA = abi::vmo::map(HANDLE_FONT_VMO, 0, Rights::READ_MAP).unwrap_or(0);
    }

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"layout: document not found\n");

            abi::thread::exit(EXIT_DOC_NOT_FOUND);
        }
    };

    console::write(console_ep, b"layout: document found\n");

    let doc_va =
        match ipc::client::setup_map_vmo(doc_ep, document_service::SETUP, &[], Rights::READ_MAP) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_DOC_SETUP),
        };

    console::write(console_ep, b"layout: doc buffer mapped\n");

    // Create layout results VMO (2 pages for rich text run data).
    let results_vmo = match abi::vmo::create(RESULTS_VMO_SIZE, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RESULTS_CREATE),
    };
    let results_va = match abi::vmo::map(results_vmo, 0, Rights::READ_WRITE_MAP) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_RESULTS_MAP),
    };

    // Initialize seqlock generation to 0.
    ipc::register::init(results_va as *mut u8);

    console::write(console_ep, b"layout: ready\n");

    let mut server = LayoutServer {
        doc_va,
        results_va,
        results_vmo,
        viewport_va: 0,
        viewport_ready: false,
        last_doc_gen: 0,
        last_line_count: 0,
        last_total_height: 0,
        last_content_len: 0,
        last_viewport_width: 0,
        last_line_height: 0,
        console_ep,
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
