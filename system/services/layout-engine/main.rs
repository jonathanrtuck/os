//! Layout Engine (B) — computes text layout from document content.
//!
//! Pure layout function: reads the document buffer (RO shared memory),
//! reads font data from the Content Region (RO), receives viewport
//! parameters from C via a shared state register, and writes layout
//! results (line breaks, shaped glyphs, content dimensions) to a
//! dedicated shared memory region.
//!
//! Pure data transformation — no view state, no input handling, no UI concerns.
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: B ↔ C (receives MSG_LAYOUT_RECOMPUTE, sends MSG_LAYOUT_READY)

#![no_std]
#![no_main]

extern crate alloc;
extern crate fonts;
extern crate layout as layout_lib;
extern crate piecetable;
extern crate scene;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use protocol::layout::{
    self, LayoutEngineConfig, LayoutResultsHeader, LineInfo, ViewportState, VisibleRun,
    LAYOUT_HEADER_SIZE, MSG_LAYOUT_ENGINE_CONFIG, MSG_LAYOUT_READY, MSG_LAYOUT_RECOMPUTE,
};

const DOC_HEADER_SIZE: usize = 64;

// ── Font state ──────────────────────────────────────────────────────

struct FontState {
    mono_data_ptr: *const u8,
    mono_data_len: usize,
    mono_upem: u16,
    mono_ascender: i16,
    mono_descender: i16,
    mono_line_gap: i16,
    mono_content_id: u32,
    sans_data_ptr: *const u8,
    sans_data_len: usize,
    sans_upem: u16,
    sans_ascender: i16,
    sans_descender: i16,
    sans_line_gap: i16,
    sans_content_id: u32,
    serif_data_ptr: *const u8,
    serif_data_len: usize,
    serif_upem: u16,
    serif_ascender: i16,
    serif_descender: i16,
    serif_line_gap: i16,
    serif_content_id: u32,
    // Italic variants.
    mono_italic_data_ptr: *const u8,
    mono_italic_data_len: usize,
    mono_italic_upem: u16,
    mono_italic_ascender: i16,
    mono_italic_descender: i16,
    mono_italic_line_gap: i16,
    mono_italic_content_id: u32,
    sans_italic_data_ptr: *const u8,
    sans_italic_data_len: usize,
    sans_italic_upem: u16,
    sans_italic_ascender: i16,
    sans_italic_descender: i16,
    sans_italic_line_gap: i16,
    sans_italic_content_id: u32,
    serif_italic_data_ptr: *const u8,
    serif_italic_data_len: usize,
    serif_italic_upem: u16,
    serif_italic_ascender: i16,
    serif_italic_descender: i16,
    serif_italic_line_gap: i16,
    serif_italic_content_id: u32,
    // Caret skew factors (from hhea caretSlopeRise/Run). Cached per font.
    mono_caret_skew: f32,
    sans_caret_skew: f32,
    serif_caret_skew: f32,
    mono_italic_caret_skew: f32,
    sans_italic_caret_skew: f32,
    serif_italic_caret_skew: f32,
}

impl FontState {
    const fn new() -> Self {
        Self {
            mono_data_ptr: core::ptr::null(),
            mono_data_len: 0,
            mono_upem: 1000,
            mono_ascender: 800,
            mono_descender: -200,
            mono_line_gap: 0,
            mono_content_id: 0,
            sans_data_ptr: core::ptr::null(),
            sans_data_len: 0,
            sans_upem: 1000,
            sans_ascender: 800,
            sans_descender: -200,
            sans_line_gap: 0,
            sans_content_id: 0,
            serif_data_ptr: core::ptr::null(),
            serif_data_len: 0,
            serif_upem: 1000,
            serif_ascender: 800,
            serif_descender: -200,
            serif_line_gap: 0,
            serif_content_id: 0,
            mono_italic_data_ptr: core::ptr::null(),
            mono_italic_data_len: 0,
            mono_italic_upem: 0,
            mono_italic_ascender: 800,
            mono_italic_descender: -200,
            mono_italic_line_gap: 0,
            mono_italic_content_id: 0,
            sans_italic_data_ptr: core::ptr::null(),
            sans_italic_data_len: 0,
            sans_italic_upem: 0,
            sans_italic_ascender: 800,
            sans_italic_descender: -200,
            sans_italic_line_gap: 0,
            sans_italic_content_id: 0,
            serif_italic_data_ptr: core::ptr::null(),
            serif_italic_data_len: 0,
            serif_italic_upem: 0,
            serif_italic_ascender: 800,
            serif_italic_descender: -200,
            serif_italic_line_gap: 0,
            serif_italic_content_id: 0,
            mono_caret_skew: 0.0,
            sans_caret_skew: 0.0,
            serif_caret_skew: 0.0,
            mono_italic_caret_skew: 0.0,
            sans_italic_caret_skew: 0.0,
            serif_italic_caret_skew: 0.0,
        }
    }

    fn mono_data(&self) -> &[u8] {
        if self.mono_data_ptr.is_null() || self.mono_data_len == 0 {
            &[]
        } else {
            // SAFETY: pointer and length set from Content Region mapping.
            unsafe { core::slice::from_raw_parts(self.mono_data_ptr, self.mono_data_len) }
        }
    }

    fn sans_data(&self) -> &[u8] {
        if self.sans_data_ptr.is_null() || self.sans_data_len == 0 {
            self.mono_data()
        } else {
            // SAFETY: pointer and length set from Content Region mapping.
            unsafe { core::slice::from_raw_parts(self.sans_data_ptr, self.sans_data_len) }
        }
    }

    fn serif_data(&self) -> &[u8] {
        if self.serif_data_ptr.is_null() || self.serif_data_len == 0 {
            self.sans_data()
        } else {
            // SAFETY: pointer and length set from Content Region mapping.
            unsafe { core::slice::from_raw_parts(self.serif_data_ptr, self.serif_data_len) }
        }
    }

    fn mono_italic_data(&self) -> &[u8] {
        if self.mono_italic_data_ptr.is_null() || self.mono_italic_data_len == 0 {
            self.mono_data() // fallback to roman
        } else {
            unsafe {
                core::slice::from_raw_parts(self.mono_italic_data_ptr, self.mono_italic_data_len)
            }
        }
    }

    fn sans_italic_data(&self) -> &[u8] {
        if self.sans_italic_data_ptr.is_null() || self.sans_italic_data_len == 0 {
            self.sans_data()
        } else {
            unsafe {
                core::slice::from_raw_parts(self.sans_italic_data_ptr, self.sans_italic_data_len)
            }
        }
    }

    fn serif_italic_data(&self) -> &[u8] {
        if self.serif_italic_data_ptr.is_null() || self.serif_italic_data_len == 0 {
            self.serif_data()
        } else {
            unsafe {
                core::slice::from_raw_parts(self.serif_italic_data_ptr, self.serif_italic_data_len)
            }
        }
    }

    /// Select font data/upem/content_id for a given family + italic combination.
    fn resolve_font(&self, family: u8, italic: bool) -> (&[u8], u16, u32) {
        if italic {
            match family {
                piecetable::FONT_MONO => (
                    self.mono_italic_data(),
                    self.mono_italic_upem,
                    self.mono_italic_content_id,
                ),
                piecetable::FONT_SERIF => (
                    self.serif_italic_data(),
                    self.serif_italic_upem,
                    self.serif_italic_content_id,
                ),
                _ => (
                    self.sans_italic_data(),
                    self.sans_italic_upem,
                    self.sans_italic_content_id,
                ),
            }
        } else {
            match family {
                piecetable::FONT_MONO => (self.mono_data(), self.mono_upem, self.mono_content_id),
                piecetable::FONT_SERIF => {
                    (self.serif_data(), self.serif_upem, self.serif_content_id)
                }
                _ => (self.sans_data(), self.sans_upem, self.sans_content_id),
            }
        }
    }

    /// Cached caret skew factor for a given family + italic combination.
    fn resolve_caret_skew(&self, family: u8, italic: bool) -> f32 {
        if italic {
            match family {
                piecetable::FONT_MONO => self.mono_italic_caret_skew,
                piecetable::FONT_SERIF => self.serif_italic_caret_skew,
                _ => self.sans_italic_caret_skew,
            }
        } else {
            match family {
                piecetable::FONT_MONO => self.mono_caret_skew,
                piecetable::FONT_SERIF => self.serif_caret_skew,
                _ => self.sans_caret_skew,
            }
        }
    }
}

// ── Global state ────────────────────────────────────────────────────

static mut STATE: LayoutEngineState = LayoutEngineState::new();

struct LayoutEngineState {
    doc_va: usize,
    doc_capacity: usize,
    layout_results_va: usize,
    layout_results_capacity: usize,
    viewport_state_va: usize,
    generation: u32,
    core_handle: sys::ChannelHandle,
    fonts: FontState,
}

impl LayoutEngineState {
    const fn new() -> Self {
        Self {
            doc_va: 0,
            doc_capacity: 0,
            layout_results_va: 0,
            layout_results_capacity: 0,
            viewport_state_va: 0,
            generation: 0,
            core_handle: sys::ChannelHandle(u8::MAX),
            fonts: FontState::new(),
        }
    }
}

fn state() -> &'static mut LayoutEngineState {
    // SAFETY: single-threaded bare-metal process.
    unsafe { &mut STATE }
}

// ── Document buffer access ──────────────────────────────────────────

fn doc_text(doc_len: usize) -> &'static [u8] {
    let s = state();
    if s.doc_va == 0 || doc_len == 0 {
        return &[];
    }
    let ptr = (s.doc_va + DOC_HEADER_SIZE) as *const u8;
    let len = doc_len.min(s.doc_capacity);
    // SAFETY: doc_va + DOC_HEADER_SIZE points to content area of mapped shared memory.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

// ── Viewport state access ───────────────────────────────────────────

fn read_viewport_state() -> ViewportState {
    let s = state();
    let ptr = s.viewport_state_va as *const ViewportState;
    // SAFETY: viewport_state_va points to a mapped page; ViewportState is 64 bytes.
    unsafe { core::ptr::read_volatile(ptr) }
}

// ── Layout computation ──────────────────────────────────────────────

/// Monospace font metrics for the layout library.
struct UnitMetrics {
    line_height: f32,
}

impl layout_lib::FontMetrics for UnitMetrics {
    fn char_width(&self, _ch: char) -> f32 {
        1.0
    }
    fn line_height(&self) -> f32 {
        self.line_height
    }
}

/// A local layout run (line break result).
struct LayoutRun {
    byte_offset: u32,
    byte_length: u32,
    y: i32,
    /// Actual line height in points (computed from tallest run on the line).
    height: u32,
    /// Max ascender on this line (exact fractional points) — for baseline alignment.
    /// Kept as f32 to avoid rounding errors when computing per-run offsets.
    max_ascender_f: f32,
}

/// Shape text through HarfBuzz and convert from font units to points.
fn shape_text(
    font_data: &[u8],
    text: &[u8],
    point_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> Vec<scene::ShapedGlyph> {
    let s = alloc::string::String::from_utf8_lossy(text);
    if s.is_empty() || font_data.is_empty() || upem == 0 {
        return Vec::new();
    }
    let shaped = fonts::shape_with_variations(font_data, &s, &[], axes);
    let ps = point_size as i64;
    let u = upem as i64;
    shaped
        .iter()
        .map(|g| scene::ShapedGlyph {
            glyph_id: g.glyph_id,
            _pad: 0,
            x_advance: ((g.x_advance as i64 * ps * 65536) / u) as i32,
            x_offset: ((g.x_offset as i64 * ps * 65536) / u) as i32,
            y_offset: ((g.y_offset as i64 * ps * 65536) / u) as i32,
        })
        .collect()
}

/// Compute chars_per_line from page geometry.
fn chars_per_line(page_width: u32, text_inset_x: u32, char_width_fx: i32) -> u32 {
    let doc_w = page_width.saturating_sub(2 * text_inset_x);
    if char_width_fx > 0 {
        ((doc_w as i64 * 65536) / char_width_fx as i64).max(1) as u32
    } else {
        80
    }
}

/// Compute monospace layout and write results to shared memory.
fn compute_plain_layout(vp: &ViewportState, text: &[u8]) {
    let s = state();
    let fonts = &s.fonts;

    let cpl = chars_per_line(vp.page_width_pt, vp.text_inset_x, vp.char_width_fx) as usize;
    let line_height = vp.line_height;
    let doc_w = vp.page_width_pt.saturating_sub(2 * vp.text_inset_x);

    // Viewport geometry.
    let scroll_pt = vp.scroll_y_mpt >> 10;
    let viewport_h = vp.viewport_height_pt as i32;

    // Line breaking via layout library.
    let metrics = UnitMetrics {
        line_height: line_height as f32,
    };
    let max_width = cpl as f32;
    let para = layout_lib::layout_paragraph(
        text,
        &metrics,
        max_width,
        layout_lib::Alignment::Left,
        &layout_lib::CharBreaker,
    );

    // Convert to local runs.
    let all_runs: Vec<LayoutRun> = para
        .lines
        .iter()
        .map(|line| LayoutRun {
            byte_offset: line.byte_offset,
            byte_length: line.byte_length,
            y: line.y,
            height: line_height,
            max_ascender_f: 0.0, // uniform for plain text
        })
        .collect();

    let total_line_count = all_runs.len();
    let content_height = if total_line_count > 0 {
        all_runs[total_line_count - 1].y + line_height as i32
    } else {
        0
    };

    // Filter visible runs.
    let visible_runs: Vec<&LayoutRun> = all_runs
        .iter()
        .filter(|r| r.y + r.height as i32 > scroll_pt && r.y < scroll_pt + viewport_h)
        .collect();

    // Shape visible runs.
    let font_data = fonts.mono_data();
    let font_size = vp.font_size;
    let upem = fonts.mono_upem;
    let axes: &[fonts::rasterize::AxisValue] = &[];

    let mut shaped_runs: Vec<(Vec<scene::ShapedGlyph>, i32)> =
        Vec::with_capacity(visible_runs.len());
    for run in &visible_runs {
        let start = run.byte_offset as usize;
        let len = run.byte_length as usize;
        let line_text = if start + len <= text.len() {
            &text[start..start + len]
        } else {
            &[]
        };
        let glyphs = shape_text(font_data, line_text, font_size, upem, axes);
        shaped_runs.push((glyphs, run.y));
    }

    // Build style table. For monospace, one entry: the mono font.
    let mono_ascent_fu = fonts.mono_ascender as u16;
    let mono_descent_fu = (-fonts.mono_descender) as u16;
    let mut style_entries = [protocol::content::StyleRegistryEntry {
        style_id: 0,
        content_id: 0,
        ascent_fu: 0,
        descent_fu: 0,
        upem: 0,
        axis_count: 0,
        _pad: 0,
        weight: 400,
        caret_skew: 0,
        axes: [protocol::content::StyleAxisValue {
            tag: [0; 4],
            value: 0.0,
        }; protocol::content::MAX_STYLE_AXES],
    }; 2];

    // Style 0: mono font.
    style_entries[0].style_id = 0;
    style_entries[0].content_id = fonts.mono_content_id;
    style_entries[0].ascent_fu = mono_ascent_fu;
    style_entries[0].descent_fu = mono_descent_fu;
    style_entries[0].upem = fonts.mono_upem;
    style_entries[0].caret_skew = (fonts.mono_caret_skew * 10_000.0) as i16;

    // Style 1: sans font (for chrome text).
    style_entries[1].style_id = 1;
    style_entries[1].content_id = fonts.sans_content_id;
    style_entries[1].ascent_fu = fonts.sans_ascender as u16;
    style_entries[1].descent_fu = (-fonts.sans_descender) as u16;
    style_entries[1].upem = fonts.sans_upem;
    style_entries[1].caret_skew = (fonts.sans_caret_skew * 10_000.0) as i16;

    let mut style_buf = [0u8; 8192];
    let style_size = protocol::content::write_style_registry(&mut style_buf, &style_entries[..2]);

    // Pack color for monospace text (will be overridden by C, but provide a default).
    // C reads the color from the VisibleRun; use a neutral value.
    let text_color_rgba: u32 = 0x00_00_00_FF; // black, full alpha

    // Write results to shared memory.
    write_layout_results(
        &all_runs,
        &shaped_runs,
        total_line_count as u32,
        content_height,
        cpl as u32,
        doc_w,
        line_height,
        0, // doc_format = Plain
        text_color_rgba,
        &style_buf[..style_size],
        style_entries.len() as u32,
    );
}

/// Compute rich text layout and write results to shared memory.
fn compute_rich_layout(vp: &ViewportState, doc_buf: &[u8]) {
    let s = state();
    let fonts = &s.fonts;

    let doc_w = vp.page_width_pt.saturating_sub(2 * vp.text_inset_x);
    let line_width_pt = doc_w as f32;
    let line_height = vp.line_height;
    let scroll_pt = vp.scroll_y_mpt >> 10;
    let viewport_h = vp.viewport_height_pt as i32;

    // Extract text from piece table.
    let text_len = piecetable::text_len(doc_buf);
    let mut scratch = alloc::vec![0u8; (text_len as usize) + 1];
    let copied = piecetable::text_slice(doc_buf, 0, text_len, &mut scratch);
    let text = &scratch[..copied];

    let run_count = piecetable::styled_run_count(doc_buf);
    if run_count == 0 {
        // Empty rich document — write empty results.
        write_layout_results(&[], &[], 0, 0, 0, doc_w, line_height, 1, 0, &[], 0);
        return;
    }

    // Build MeasuredChar stream.
    let mut measured: Vec<layout_lib::MeasuredChar> = Vec::new();

    for run_idx in 0..run_count {
        let Some(run) = piecetable::styled_run(doc_buf, run_idx) else {
            continue;
        };
        let Some(style) = piecetable::style(doc_buf, run.style_id) else {
            continue;
        };

        let fi_data = match style.font_family {
            piecetable::FONT_MONO => fonts.mono_data(),
            piecetable::FONT_SERIF => fonts.serif_data(),
            _ => fonts.sans_data(),
        };
        let fi_upem = match style.font_family {
            piecetable::FONT_MONO => fonts.mono_upem,
            piecetable::FONT_SERIF => fonts.serif_upem,
            _ => fonts.sans_upem,
        };

        let mut axes_buf = [fonts::rasterize::AxisValue {
            tag: [0; 4],
            value: 0.0,
        }; 3];
        let mut axis_count = 0;
        if style.weight != 400 {
            axes_buf[axis_count] = fonts::rasterize::AxisValue {
                tag: *b"wght",
                value: style.weight as f32,
            };
            axis_count += 1;
        }
        axes_buf[axis_count] = fonts::rasterize::AxisValue {
            tag: *b"opsz",
            value: style.font_size_pt as f32,
        };
        axis_count += 1;
        let axes = &axes_buf[..axis_count];

        let run_start = run.byte_offset as usize;
        let run_end = run_start + run.byte_len as usize;
        let run_text = if run_end <= text.len() {
            &text[run_start..run_end]
        } else {
            continue;
        };

        let mut offset = run_start;
        for ch in core::str::from_utf8(run_text).unwrap_or("").chars() {
            let byte_len = ch.len_utf8() as u8;
            let width = if ch == '\n' {
                0.0
            } else {
                char_advance_pt(fi_data, ch, style.font_size_pt as u16, fi_upem, axes)
            };
            measured.push(layout_lib::MeasuredChar {
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
        layout_lib::break_measured_lines(&measured, line_width_pt, layout_lib::BreakMode::Word);

    // Build all_runs and rich line info.
    let mut all_runs: Vec<LayoutRun> = Vec::with_capacity(line_breaks.len());
    let mut y = 0i32;

    for lb in &line_breaks {
        let (max_line_h, max_asc) =
            compute_rich_line_metrics(doc_buf, &measured, lb, fonts, line_height as i32);
        all_runs.push(LayoutRun {
            byte_offset: lb.byte_start,
            byte_length: lb.byte_end - lb.byte_start,
            y,
            height: max_line_h as u32,
            max_ascender_f: max_asc,
        });
        y += max_line_h;
    }

    // Trailing empty line: if the document ends with '\n', emit a zero-length
    // LayoutRun so C has a line for every cursor-occupiable position.
    // This is the SSOT — click and Cmd+Down both resolve through B's lines.
    if !text.is_empty() && text.last() == Some(&b'\n') {
        let trailing_h = all_runs.last().map_or(line_height, |r| r.height);
        let trailing_asc = all_runs.last().map_or(0.0, |r| r.max_ascender_f);
        all_runs.push(LayoutRun {
            byte_offset: text.len() as u32,
            byte_length: 0,
            y,
            height: trailing_h,
            max_ascender_f: trailing_asc,
        });
        y += trailing_h as i32;
    }

    let total_line_count = all_runs.len() as u32;
    let content_height = y;

    // Shape visible runs (segments within visible lines).
    let mut shaped_runs: Vec<(Vec<scene::ShapedGlyph>, i32)> = Vec::new();
    let mut visible_run_metas: Vec<RichRunMeta> = Vec::new();
    let mut style_table = StyleTable::new();

    for (line_idx, run) in all_runs.iter().enumerate() {
        if run.y + run.height as i32 <= scroll_pt || run.y > scroll_pt + viewport_h {
            continue;
        }
        // Trailing empty line has no line_break entry — nothing to shape.
        if line_idx >= line_breaks.len() {
            break;
        }

        // Find segments in this line.
        let lb = &line_breaks[line_idx];
        let mut seg_start: Option<(usize, u8)> = None; // (byte_start, style_id)
        let mut pen_x_mpt: i32 = 0; // reset pen per line

        for mc in &measured {
            if mc.byte_offset < lb.byte_start || mc.byte_offset >= lb.byte_end {
                if mc.byte_offset >= lb.byte_end {
                    break;
                }
                continue;
            }
            if mc.is_newline {
                continue;
            }

            let run_idx = mc.run_index as usize;
            let Some(styled_run) = piecetable::styled_run(doc_buf, run_idx) else {
                continue;
            };

            match seg_start {
                Some((start, sid)) if sid == styled_run.style_id => {
                    // Same segment, continue.
                }
                Some((start, sid)) => {
                    // New segment — shape the previous one.
                    let seg_end = mc.byte_offset as usize;
                    shape_rich_segment_into(
                        doc_buf,
                        fonts,
                        text,
                        start,
                        seg_end,
                        sid,
                        run.y,
                        run.max_ascender_f,
                        &mut pen_x_mpt,
                        &mut shaped_runs,
                        &mut visible_run_metas,
                        &mut style_table,
                    );
                    seg_start = Some((mc.byte_offset as usize, styled_run.style_id));
                }
                None => {
                    seg_start = Some((mc.byte_offset as usize, styled_run.style_id));
                }
            }
        }

        // Flush last segment.
        if let Some((start, sid)) = seg_start {
            let seg_end = (lb.byte_end as usize).min(text.len());
            // Trim trailing newlines.
            let mut end = seg_end;
            while end > start
                && (end > text.len() || (end <= text.len() && end > 0 && text[end - 1] == b'\n'))
            {
                end -= 1;
            }
            if end > start {
                shape_rich_segment_into(
                    doc_buf,
                    fonts,
                    text,
                    start,
                    end,
                    sid,
                    run.y,
                    run.max_ascender_f,
                    &mut pen_x_mpt,
                    &mut shaped_runs,
                    &mut visible_run_metas,
                    &mut style_table,
                );
            }
        }
    }

    // Build style registry from the dynamic style table. Each unique
    // (content_id, weight, opsz) combination gets its own entry.
    let mut style_buf = [0u8; 8192];
    let entries = style_table.to_registry_entries();
    let style_size = protocol::content::write_style_registry(&mut style_buf, &entries);

    // Write results. For rich text, we write all_runs as line info and
    // shaped_runs as visible runs with style metadata.
    write_rich_layout_results(
        &all_runs,
        &shaped_runs,
        &visible_run_metas,
        total_line_count,
        content_height,
        doc_w,
        line_height,
        &style_buf[..style_size],
        entries.len() as u32,
    );
}

/// Dynamic style table — assigns sequential style_ids to unique
/// (content_id, axes) combinations. Each weight/opsz/italic variant
/// gets its own style_id so the renderer can apply the correct axes.
struct StyleTable {
    entries: Vec<StyleTableEntry>,
}

struct StyleTableEntry {
    content_id: u32,
    ascent_fu: u16,
    descent_fu: u16,
    upem: u16,
    weight: u16,
    caret_skew: i16,
    axes: [protocol::content::StyleAxisValue; protocol::content::MAX_STYLE_AXES],
    axis_count: u8,
}

impl StyleTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Get or assign a style_id for the given font + axes combination.
    fn style_id_for(
        &mut self,
        content_id: u32,
        axes: &[protocol::content::StyleAxisValue],
        ascent_fu: u16,
        descent_fu: u16,
        upem: u16,
        weight: u16,
        caret_skew: i16,
    ) -> u32 {
        // Linear scan for dedup (typically < 20 entries).
        for (i, e) in self.entries.iter().enumerate() {
            if e.content_id == content_id && e.axis_count == axes.len() as u8 {
                let mut match_axes = true;
                for j in 0..axes.len() {
                    if e.axes[j].tag != axes[j].tag
                        || (e.axes[j].value - axes[j].value).abs() > 0.01
                    {
                        match_axes = false;
                        break;
                    }
                }
                if match_axes {
                    return i as u32;
                }
            }
        }
        let id = self.entries.len() as u32;
        let mut entry_axes = [protocol::content::StyleAxisValue {
            tag: [0; 4],
            value: 0.0,
        }; protocol::content::MAX_STYLE_AXES];
        let axis_count = axes.len().min(protocol::content::MAX_STYLE_AXES);
        for (j, a) in axes.iter().take(axis_count).enumerate() {
            entry_axes[j] = *a;
        }
        self.entries.push(StyleTableEntry {
            content_id,
            ascent_fu,
            descent_fu,
            upem,
            weight,
            caret_skew,
            axes: entry_axes,
            axis_count: axis_count as u8,
        });
        id
    }

    fn to_registry_entries(&self) -> Vec<protocol::content::StyleRegistryEntry> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| protocol::content::StyleRegistryEntry {
                style_id: i as u32,
                content_id: e.content_id,
                ascent_fu: e.ascent_fu,
                descent_fu: e.descent_fu,
                upem: e.upem,
                axis_count: e.axis_count,
                _pad: 0,
                weight: e.weight,
                caret_skew: e.caret_skew,
                axes: e.axes,
            })
            .collect()
    }
}

fn shape_rich_segment_into(
    doc_buf: &[u8],
    fonts: &FontState,
    text: &[u8],
    start: usize,
    end: usize,
    style_id: u8,
    line_y: i32,
    line_max_asc_f: f32,
    pen_x_mpt: &mut i32,
    shaped_runs: &mut Vec<(Vec<scene::ShapedGlyph>, i32)>,
    metas: &mut Vec<RichRunMeta>,
    style_table: &mut StyleTable,
) {
    let Some(style) = piecetable::style(doc_buf, style_id) else {
        return;
    };
    let italic = style.flags & piecetable::FLAG_ITALIC != 0;
    let (fi_data, fi_upem, fi_content_id) = fonts.resolve_font(style.font_family, italic);

    // Baseline alignment: compute exact offset in millipoints.
    // By subtracting exact f32 ascenders before rounding, mixed-size baselines align
    // to within 1 millipoint (0.001pt) instead of ~0.3pt with integer rounding.
    let this_asc_f = {
        let asc_fu = if italic {
            match style.font_family {
                piecetable::FONT_MONO => fonts.mono_italic_ascender.abs(),
                piecetable::FONT_SERIF => fonts.serif_italic_ascender.abs(),
                _ => fonts.sans_italic_ascender.abs(),
            }
        } else {
            match style.font_family {
                piecetable::FONT_MONO => fonts.mono_ascender.abs(),
                piecetable::FONT_SERIF => fonts.serif_ascender.abs(),
                _ => fonts.sans_ascender.abs(),
            }
        };
        if fi_upem > 0 {
            asc_fu as f32 * style.font_size_pt as f32 / fi_upem as f32
        } else {
            0.0
        }
    };
    // Compute y in millipoints: line_y (points) * 1024 + baseline_offset (millipoints).
    let baseline_offset_mpt = if line_max_asc_f > 0.0 {
        ((line_max_asc_f - this_asc_f) * 1024.0 + 0.5) as i32
    } else {
        0
    };
    let y_mpt = line_y * 1024 + baseline_offset_mpt;
    let font_size = style.font_size_pt as u16;

    let mut axes_buf = [fonts::rasterize::AxisValue {
        tag: *b"wght",
        value: 0.0,
    }; 3];
    let mut axis_count = 0;
    if style.weight != 400 {
        axes_buf[axis_count] = fonts::rasterize::AxisValue {
            tag: *b"wght",
            value: style.weight as f32,
        };
        axis_count += 1;
    }
    axes_buf[axis_count] = fonts::rasterize::AxisValue {
        tag: *b"opsz",
        value: font_size as f32,
    };
    axis_count += 1;
    let axes = &axes_buf[..axis_count];

    let seg_text = if end <= text.len() {
        &text[start..end]
    } else {
        &[]
    };
    let glyphs = shape_text(fi_data, seg_text, font_size, fi_upem, axes);

    // Dynamic style_id: unique per (content_id, weight, opsz) combination.
    // Convert shaping axes to protocol axes for the style registry.
    let mut proto_axes = [protocol::content::StyleAxisValue {
        tag: [0; 4],
        value: 0.0,
    }; protocol::content::MAX_STYLE_AXES];
    for (j, a) in axes
        .iter()
        .enumerate()
        .take(protocol::content::MAX_STYLE_AXES)
    {
        proto_axes[j] = protocol::content::StyleAxisValue {
            tag: a.tag,
            value: a.value,
        };
    }
    let asc_fu = if italic {
        match style.font_family {
            piecetable::FONT_MONO => fonts.mono_italic_ascender.unsigned_abs(),
            piecetable::FONT_SERIF => fonts.serif_italic_ascender.unsigned_abs(),
            _ => fonts.sans_italic_ascender.unsigned_abs(),
        }
    } else {
        match style.font_family {
            piecetable::FONT_MONO => fonts.mono_ascender.unsigned_abs(),
            piecetable::FONT_SERIF => fonts.serif_ascender.unsigned_abs(),
            _ => fonts.sans_ascender.unsigned_abs(),
        }
    };
    let desc_fu = if italic {
        match style.font_family {
            piecetable::FONT_MONO => fonts.mono_italic_descender.unsigned_abs(),
            piecetable::FONT_SERIF => fonts.serif_italic_descender.unsigned_abs(),
            _ => fonts.sans_italic_descender.unsigned_abs(),
        }
    } else {
        match style.font_family {
            piecetable::FONT_MONO => fonts.mono_descender.unsigned_abs(),
            piecetable::FONT_SERIF => fonts.serif_descender.unsigned_abs(),
            _ => fonts.sans_descender.unsigned_abs(),
        }
    };
    let caret_skew_fp = (fonts.resolve_caret_skew(style.font_family, italic) * 10_000.0) as i16;
    let sid = style_table.style_id_for(
        fi_content_id,
        &proto_axes[..axis_count],
        asc_fu,
        desc_fu,
        fi_upem,
        style.weight,
        caret_skew_fp,
    );
    let color = pack_style_color(style);

    // Compute pen advance for this run (sum of glyph x_advances).
    let run_x_mpt = *pen_x_mpt;
    let advance_mpt: i32 = glyphs
        .iter()
        .map(|g| {
            // x_advance is 16.16 fixed-point → convert to millipoints (>> 6).
            g.x_advance >> 6
        })
        .sum();
    *pen_x_mpt += advance_mpt;

    shaped_runs.push((glyphs, y_mpt));
    metas.push(RichRunMeta {
        style_id: sid,
        color_rgba: color,
        byte_offset: start as u32,
        byte_length: (end - start) as u16,
        flags: style.flags,
        font_size,
        x_mpt: run_x_mpt,
    });
}

fn pack_style_color(style: &piecetable::Style) -> u32 {
    let r = style.color[0];
    let g = style.color[1];
    let b = style.color[2];
    let a = style.color[3];
    ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32)
}

/// Returns (line_height, max_ascender_f) for a rich text line.
/// max_ascender_f is the largest ascender (exact fractional points) on the line — used
/// for baseline alignment of mixed-size runs.
fn compute_rich_line_metrics(
    doc_buf: &[u8],
    measured: &[layout_lib::MeasuredChar],
    lb: &layout_lib::LineBreak,
    fonts: &FontState,
    default_line_height: i32,
) -> (i32, f32) {
    let mut max_h = default_line_height;
    let mut max_asc_f: f32 = 0.0;
    for mc in measured {
        if mc.byte_offset < lb.byte_start {
            continue;
        }
        if mc.byte_offset >= lb.byte_end {
            break;
        }
        let run_idx = mc.run_index as usize;
        let Some(run) = piecetable::styled_run(doc_buf, run_idx) else {
            continue;
        };
        let Some(style) = piecetable::style(doc_buf, run.style_id) else {
            continue;
        };
        let italic = style.flags & piecetable::FLAG_ITALIC != 0;
        let (asc, desc, gap, upem) = if italic {
            match style.font_family {
                piecetable::FONT_MONO => (
                    fonts.mono_italic_ascender as i32,
                    fonts.mono_italic_descender as i32,
                    fonts.mono_italic_line_gap as i32,
                    fonts.mono_italic_upem,
                ),
                piecetable::FONT_SERIF => (
                    fonts.serif_italic_ascender as i32,
                    fonts.serif_italic_descender as i32,
                    fonts.serif_italic_line_gap as i32,
                    fonts.serif_italic_upem,
                ),
                _ => (
                    fonts.sans_italic_ascender as i32,
                    fonts.sans_italic_descender as i32,
                    fonts.sans_italic_line_gap as i32,
                    fonts.sans_italic_upem,
                ),
            }
        } else {
            match style.font_family {
                piecetable::FONT_MONO => (
                    fonts.mono_ascender as i32,
                    fonts.mono_descender as i32,
                    fonts.mono_line_gap as i32,
                    fonts.mono_upem,
                ),
                piecetable::FONT_SERIF => (
                    fonts.serif_ascender as i32,
                    fonts.serif_descender as i32,
                    fonts.serif_line_gap as i32,
                    fonts.serif_upem,
                ),
                _ => (
                    fonts.sans_ascender as i32,
                    fonts.sans_descender as i32,
                    fonts.sans_line_gap as i32,
                    fonts.sans_upem,
                ),
            }
        };
        if upem == 0 {
            continue;
        }
        let h = ((asc.abs() + desc.abs() + gap.max(0)) as f32 * style.font_size_pt as f32)
            / upem as f32;
        let h_i = (h + 0.5) as i32;
        if h_i > max_h {
            max_h = h_i;
        }
        // Track the max ascender (exact fractional points) for baseline alignment.
        let asc_exact = asc.abs() as f32 * style.font_size_pt as f32 / upem as f32;
        if asc_exact > max_asc_f {
            max_asc_f = asc_exact;
        }
    }
    (max_h, max_asc_f)
}

fn char_advance_pt(
    font_data: &[u8],
    ch: char,
    font_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> f32 {
    if upem == 0 || font_data.is_empty() {
        return 8.0;
    }
    let gid = fonts::rasterize::glyph_id_for_char(font_data, ch).unwrap_or(0);
    let advance_fu = if !axes.is_empty() {
        fonts::rasterize::glyph_h_advance_with_axes(font_data, gid, axes).unwrap_or(0) as f32
    } else {
        let (adv, _) = fonts::rasterize::glyph_h_metrics(font_data, gid).unwrap_or((0, 0));
        adv as f32
    };
    (advance_fu * font_size as f32) / upem as f32
}

// ── Write layout results to shared memory ───────────────────────────

fn write_layout_results(
    all_runs: &[LayoutRun],
    shaped_runs: &[(Vec<scene::ShapedGlyph>, i32)],
    total_line_count: u32,
    content_height: i32,
    chars_per_line: u32,
    doc_width: u32,
    line_height: u32,
    doc_format: u32,
    text_color_rgba: u32,
    style_registry: &[u8],
    style_count: u32,
) {
    let s = state();
    let base = s.layout_results_va as *mut u8;
    let capacity = s.layout_results_capacity;

    let visible_run_count = shaped_runs.len() as u32;
    let line_info_off = layout::line_info_offset();
    let vr_off = layout::visible_run_offset(total_line_count);
    let gd_off = layout::glyph_data_offset(total_line_count, visible_run_count);

    // Pack glyph data.
    let mut glyph_data_used: u32 = 0;
    let glyph_size = core::mem::size_of::<scene::ShapedGlyph>() as u32;

    for (glyphs, _y) in shaped_runs {
        glyph_data_used += (glyphs.len() as u32) * glyph_size;
    }

    let style_off =
        layout::style_registry_offset(total_line_count, visible_run_count, glyph_data_used);
    let total_needed = style_off + style_registry.len();

    if total_needed > capacity {
        sys::print(b"layout-engine: results exceed capacity\n");
        return;
    }

    // Write line info.
    for (i, run) in all_runs.iter().enumerate() {
        let off = line_info_off + i * core::mem::size_of::<LineInfo>();
        if off + core::mem::size_of::<LineInfo>() > capacity {
            break;
        }
        let info = LineInfo {
            byte_offset: run.byte_offset,
            byte_length: run.byte_length,
            y_pt: run.y,
            line_height_pt: run.height,
        };
        // SAFETY: offset within allocated region, LineInfo is repr(C).
        unsafe {
            core::ptr::write(base.add(off) as *mut LineInfo, info);
        }
    }

    // Write visible runs + glyph data.
    let mut glyph_cursor: u32 = 0;
    for (i, (glyphs, y)) in shaped_runs.iter().enumerate() {
        // For plain text, byte_offset matches the corresponding LineInfo entry.
        let (bo, bl) = if i < all_runs.len() {
            (all_runs[i].byte_offset, all_runs[i].byte_length as u16)
        } else {
            (0, 0)
        };
        let run_off = vr_off + i * core::mem::size_of::<VisibleRun>();
        let vr = VisibleRun {
            glyph_data_offset: glyph_cursor,
            glyph_count: glyphs.len() as u16,
            font_size: 0, // mono: font_size comes from viewport state
            y_mpt: *y * 1024, // plain text: y is in points, convert to mpt
            style_id: 0, // mono style
            color_rgba: text_color_rgba,
            byte_offset: bo,
            byte_length: bl,
            flags: 0,
            _pad: 0,
            x_mpt: 0, // plain text always starts at x=0
        };
        // SAFETY: offset within allocated region.
        unsafe {
            core::ptr::write(base.add(run_off) as *mut VisibleRun, vr);
        }

        // Write glyph data.
        let glyph_bytes = (glyphs.len() as u32) * glyph_size;
        let data_off = gd_off + glyph_cursor as usize;
        if data_off + glyph_bytes as usize <= capacity {
            // SAFETY: source is valid ShapedGlyph slice, dest is within region.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    glyphs.as_ptr() as *const u8,
                    base.add(data_off),
                    glyph_bytes as usize,
                );
            }
        }
        glyph_cursor += glyph_bytes;
    }

    // Write style registry.
    if !style_registry.is_empty() && style_off + style_registry.len() <= capacity {
        // SAFETY: within allocated region.
        unsafe {
            core::ptr::copy_nonoverlapping(
                style_registry.as_ptr(),
                base.add(style_off),
                style_registry.len(),
            );
        }
    }

    // Write header last (with store-release on generation).
    s.generation += 1;
    let header = LayoutResultsHeader {
        generation: s.generation,
        total_line_count,
        visible_run_count,
        content_height_pt: content_height,
        chars_per_line,
        doc_width_pt: doc_width,
        line_height_pt: line_height,
        glyph_data_used,
        doc_format,
        style_registry_size: style_registry.len() as u32,
        style_count,
        _reserved: [0; 5],
    };

    // SAFETY: base points to mapped shared memory, header fits at offset 0.
    unsafe {
        // Write all header fields except generation first.
        let hdr_ptr = base as *mut LayoutResultsHeader;
        core::ptr::write(hdr_ptr, header);
        // Store-release on generation to ensure all data is visible.
        let gen_ptr = base as *const AtomicU32;
        (*gen_ptr).store(s.generation, Ordering::Release);
    }
}

/// Per-run metadata for rich text visible runs.
struct RichRunMeta {
    style_id: u32,
    color_rgba: u32,
    byte_offset: u32,
    byte_length: u16,
    flags: u8,
    font_size: u16,
    x_mpt: i32,
}

fn write_rich_layout_results(
    all_runs: &[LayoutRun],
    shaped_runs: &[(Vec<scene::ShapedGlyph>, i32)],
    metas: &[RichRunMeta],
    total_line_count: u32,
    content_height: i32,
    doc_width: u32,
    line_height: u32,
    style_registry: &[u8],
    style_count: u32,
) {
    let s = state();
    let base = s.layout_results_va as *mut u8;
    let capacity = s.layout_results_capacity;

    let visible_run_count = shaped_runs.len() as u32;
    let line_info_off = layout::line_info_offset();
    let vr_off = layout::visible_run_offset(total_line_count);
    let gd_off = layout::glyph_data_offset(total_line_count, visible_run_count);

    let glyph_size = core::mem::size_of::<scene::ShapedGlyph>() as u32;
    let mut glyph_data_used: u32 = 0;
    for (glyphs, _) in shaped_runs {
        glyph_data_used += (glyphs.len() as u32) * glyph_size;
    }

    let style_off =
        layout::style_registry_offset(total_line_count, visible_run_count, glyph_data_used);
    let total_needed = style_off + style_registry.len();

    if total_needed > capacity {
        sys::print(b"layout-engine: rich results exceed capacity\n");
        return;
    }

    // Write line info.
    for (i, run) in all_runs.iter().enumerate() {
        let off = line_info_off + i * core::mem::size_of::<LineInfo>();
        if off + core::mem::size_of::<LineInfo>() > capacity {
            break;
        }
        let info = LineInfo {
            byte_offset: run.byte_offset,
            byte_length: run.byte_length,
            y_pt: run.y,
            line_height_pt: run.height,
        };
        // SAFETY: within allocated region.
        unsafe {
            core::ptr::write(base.add(off) as *mut LineInfo, info);
        }
    }

    // Write visible runs + glyph data.
    let mut glyph_cursor: u32 = 0;
    for (i, (glyphs, y)) in shaped_runs.iter().enumerate() {
        let meta = if i < metas.len() {
            &metas[i]
        } else {
            &RichRunMeta {
                style_id: 0,
                color_rgba: 0x000000FF,
                byte_offset: 0,
                byte_length: 0,
                flags: 0,
                font_size: 0,
                x_mpt: 0,
            }
        };
        let run_off = vr_off + i * core::mem::size_of::<VisibleRun>();
        let vr = VisibleRun {
            glyph_data_offset: glyph_cursor,
            glyph_count: glyphs.len() as u16,
            font_size: meta.font_size,
            y_mpt: *y, // rich text: already in millipoints from shape_rich_segment_into
            style_id: meta.style_id,
            color_rgba: meta.color_rgba,
            byte_offset: meta.byte_offset,
            byte_length: meta.byte_length,
            flags: meta.flags,
            _pad: 0,
            x_mpt: meta.x_mpt,
        };
        // SAFETY: within allocated region.
        unsafe {
            core::ptr::write(base.add(run_off) as *mut VisibleRun, vr);
        }

        let glyph_bytes = (glyphs.len() as u32) * glyph_size;
        let data_off = gd_off + glyph_cursor as usize;
        if data_off + glyph_bytes as usize <= capacity {
            // SAFETY: within allocated region.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    glyphs.as_ptr() as *const u8,
                    base.add(data_off),
                    glyph_bytes as usize,
                );
            }
        }
        glyph_cursor += glyph_bytes;
    }

    // Write style registry.
    if !style_registry.is_empty() && style_off + style_registry.len() <= capacity {
        // SAFETY: within allocated region.
        unsafe {
            core::ptr::copy_nonoverlapping(
                style_registry.as_ptr(),
                base.add(style_off),
                style_registry.len(),
            );
        }
    }

    // Write header last with store-release.
    s.generation += 1;
    let header = LayoutResultsHeader {
        generation: s.generation,
        total_line_count,
        visible_run_count,
        content_height_pt: content_height,
        chars_per_line: 0, // rich text doesn't use chars_per_line
        doc_width_pt: doc_width,
        line_height_pt: line_height,
        glyph_data_used,
        doc_format: 1, // Rich
        style_registry_size: style_registry.len() as u32,
        style_count,
        _reserved: [0; 5],
    };

    // SAFETY: base points to mapped shared memory.
    unsafe {
        let hdr_ptr = base as *mut LayoutResultsHeader;
        core::ptr::write(hdr_ptr, header);
        let gen_ptr = base as *const AtomicU32;
        (*gen_ptr).store(s.generation, Ordering::Release);
    }
}

// ── Font discovery from Content Region ──────────────────────────────

fn discover_fonts(content_va: usize, content_size: u32) {
    if content_va == 0 || content_size == 0 {
        return;
    }

    // SAFETY: content_va is page-aligned mapped shared memory with valid ContentRegionHeader.
    let header = unsafe { &*(content_va as *const protocol::content::ContentRegionHeader) };

    let s = state();

    // Mono font.
    if let Some(entry) =
        protocol::content::find_entry(&header, protocol::content::CONTENT_ID_FONT_MONO)
    {
        let ptr = (content_va + entry.offset as usize) as *const u8;
        // SAFETY: entry bounds validated by init; content_va region is mapped.
        let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
        s.fonts.mono_data_ptr = ptr;
        s.fonts.mono_data_len = entry.length as usize;
        s.fonts.mono_content_id = protocol::content::CONTENT_ID_FONT_MONO;
        if let Some(fm) = fonts::rasterize::font_metrics(data) {
            s.fonts.mono_upem = fm.units_per_em;
            s.fonts.mono_ascender = fm.ascent;
            s.fonts.mono_descender = fm.descent;
            s.fonts.mono_line_gap = fm.line_gap;
        }
        s.fonts.mono_caret_skew = fonts::rasterize::caret_skew(data);
    }

    // Sans font.
    if let Some(entry) =
        protocol::content::find_entry(&header, protocol::content::CONTENT_ID_FONT_SANS)
    {
        let ptr = (content_va + entry.offset as usize) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
        s.fonts.sans_data_ptr = ptr;
        s.fonts.sans_data_len = entry.length as usize;
        s.fonts.sans_content_id = protocol::content::CONTENT_ID_FONT_SANS;
        if let Some(fm) = fonts::rasterize::font_metrics(data) {
            s.fonts.sans_upem = fm.units_per_em;
            s.fonts.sans_ascender = fm.ascent;
            s.fonts.sans_descender = fm.descent;
            s.fonts.sans_line_gap = fm.line_gap;
        }
        s.fonts.sans_caret_skew = fonts::rasterize::caret_skew(data);
    }

    // Serif font.
    if let Some(entry) =
        protocol::content::find_entry(&header, protocol::content::CONTENT_ID_FONT_SERIF)
    {
        let ptr = (content_va + entry.offset as usize) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
        s.fonts.serif_data_ptr = ptr;
        s.fonts.serif_data_len = entry.length as usize;
        s.fonts.serif_content_id = protocol::content::CONTENT_ID_FONT_SERIF;
        if let Some(fm) = fonts::rasterize::font_metrics(data) {
            s.fonts.serif_upem = fm.units_per_em;
            s.fonts.serif_ascender = fm.ascent;
            s.fonts.serif_descender = fm.descent;
            s.fonts.serif_line_gap = fm.line_gap;
        }
        s.fonts.serif_caret_skew = fonts::rasterize::caret_skew(data);
    }

    // Italic variants (with full metrics + content_id).
    if let Some(entry) =
        protocol::content::find_entry(&header, protocol::content::CONTENT_ID_FONT_MONO_ITALIC)
    {
        let ptr = (content_va + entry.offset as usize) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
        s.fonts.mono_italic_data_ptr = ptr;
        s.fonts.mono_italic_data_len = entry.length as usize;
        s.fonts.mono_italic_content_id = protocol::content::CONTENT_ID_FONT_MONO_ITALIC;
        if let Some(fm) = fonts::rasterize::font_metrics(data) {
            s.fonts.mono_italic_upem = fm.units_per_em;
            s.fonts.mono_italic_ascender = fm.ascent;
            s.fonts.mono_italic_descender = fm.descent;
            s.fonts.mono_italic_line_gap = fm.line_gap;
        }
        s.fonts.mono_italic_caret_skew = fonts::rasterize::caret_skew(data);
    }
    if let Some(entry) =
        protocol::content::find_entry(&header, protocol::content::CONTENT_ID_FONT_SANS_ITALIC)
    {
        let ptr = (content_va + entry.offset as usize) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
        s.fonts.sans_italic_data_ptr = ptr;
        s.fonts.sans_italic_data_len = entry.length as usize;
        s.fonts.sans_italic_content_id = protocol::content::CONTENT_ID_FONT_SANS_ITALIC;
        if let Some(fm) = fonts::rasterize::font_metrics(data) {
            s.fonts.sans_italic_upem = fm.units_per_em;
            s.fonts.sans_italic_ascender = fm.ascent;
            s.fonts.sans_italic_descender = fm.descent;
            s.fonts.sans_italic_line_gap = fm.line_gap;
        }
        s.fonts.sans_italic_caret_skew = fonts::rasterize::caret_skew(data);
    }
    if let Some(entry) =
        protocol::content::find_entry(&header, protocol::content::CONTENT_ID_FONT_SERIF_ITALIC)
    {
        let ptr = (content_va + entry.offset as usize) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
        s.fonts.serif_italic_data_ptr = ptr;
        s.fonts.serif_italic_data_len = entry.length as usize;
        s.fonts.serif_italic_content_id = protocol::content::CONTENT_ID_FONT_SERIF_ITALIC;
        if let Some(fm) = fonts::rasterize::font_metrics(data) {
            s.fonts.serif_italic_upem = fm.units_per_em;
            s.fonts.serif_italic_ascender = fm.ascent;
            s.fonts.serif_italic_descender = fm.descent;
            s.fonts.serif_italic_line_gap = fm.line_gap;
        }
        s.fonts.serif_italic_caret_skew = fonts::rasterize::caret_skew(data);
    }

    sys::print(b"  layout-engine: fonts discovered\n");
}

// ── Entry point ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x90 layout-engine - starting\n");

    // Read config from init channel.
    let init_ch =
        // SAFETY: channel_shm_va(0) is the base of the init channel SHM region.
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_LAYOUT_ENGINE_CONFIG {
        sys::print(b"layout-engine: no config message\n");
        sys::exit();
    }

    let Some(layout::Message::LayoutEngineConfig(config)) =
        layout::decode(msg.msg_type, &msg.payload)
    else {
        sys::print(b"layout-engine: bad config payload\n");
        sys::exit();
    };

    {
        let s = state();
        s.doc_va = config.doc_va as usize;
        s.doc_capacity = config.doc_capacity as usize;
        s.layout_results_va = config.layout_results_va as usize;
        s.layout_results_capacity = config.layout_results_capacity as usize;
        s.viewport_state_va = config.viewport_state_va as usize;
        s.core_handle = sys::ChannelHandle(config.core_handle);
    }

    // Discover fonts from Content Region.
    discover_fonts(config.content_va as usize, config.content_size);

    sys::print(b"  layout-engine: ready, waiting for recompute signals\n");

    // Main loop: wait for MSG_LAYOUT_RECOMPUTE, compute layout, signal back.
    let core_handle = state().core_handle;
    let core_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(core_handle.0 as usize),
            ipc::PAGE_SIZE,
            0,
        )
    };
    let mut recompute_msg = ipc::Message::new(0);

    loop {
        // Wait on core channel for recompute signal.
        let _ = sys::wait(&[core_handle.0], 1_000_000_000); // 1s timeout

        // Drain all recompute messages (coalesce multiple signals).
        let mut got_recompute = false;
        while core_ch.try_recv(&mut recompute_msg) {
            if recompute_msg.msg_type == MSG_LAYOUT_RECOMPUTE {
                got_recompute = true;
            }
        }

        if !got_recompute {
            continue;
        }

        // Read viewport state.
        let vp = read_viewport_state();
        if vp.generation == 0 {
            continue; // Not yet initialized.
        }

        // Read document text.
        let text = doc_text(vp.doc_len as usize);

        // Dispatch based on document format.
        if vp.doc_format == 1 {
            // Rich text: read piece table from doc buffer.
            let s = state();
            // SAFETY: doc_va points to mapped shared memory.
            let doc_buf = unsafe {
                core::slice::from_raw_parts(
                    (s.doc_va + DOC_HEADER_SIZE) as *const u8,
                    vp.doc_len as usize,
                )
            };
            compute_rich_layout(&vp, doc_buf);
        } else {
            compute_plain_layout(&vp, text);
        }

        // Signal C that layout results are ready.
        let ready_msg = ipc::Message::new(MSG_LAYOUT_READY);
        core_ch.send(&ready_msg);
        let _ = sys::channel_signal(core_handle);
    }
}
