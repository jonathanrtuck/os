//! Unified text layout engine.
//!
//! One function for both monospace and proportional text. The layout
//! algorithm is the same — accumulate glyph widths, wrap when exceeding
//! `max_width`. The only difference is where widths come from: a monospace
//! `FontMetrics` impl returns a constant; a proportional impl returns
//! per-character widths.
//!
//! No dependencies on scene graph, rendering, or core.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

// ── Traits ────────────────────────────────────────────────────────────

/// Font metrics provider. The single parameter that determines whether
/// layout is monospace or proportional.
pub trait FontMetrics {
    /// Advance width of a character in points.
    fn char_width(&self, ch: char) -> f32;

    /// Line height in points (ascender - descender + line gap).
    fn line_height(&self) -> f32;
}

/// Determines valid line break positions within text.
///
/// The layout engine calls `can_break_before` while accumulating glyph
/// widths. When a line exceeds `max_width`, it breaks at the last
/// accepted position.
pub trait LineBreaker {
    /// Returns true if a line break is allowed before byte position `pos`.
    /// Called with `0 < pos < text.len()`.
    fn can_break_before(&self, text: &[u8], pos: usize) -> bool;

    /// Whether to trim trailing whitespace from lines at soft breaks
    /// and skip leading whitespace on continuation lines.
    fn trim_whitespace(&self) -> bool {
        false
    }
}

// ── Built-in line breakers ────────────────────────────────────────────

/// Break at any character boundary. No whitespace trimming.
/// Produces identical results to fixed-column character wrapping.
pub struct CharBreaker;

impl LineBreaker for CharBreaker {
    fn can_break_before(&self, _text: &[u8], _pos: usize) -> bool {
        true
    }
}

/// Break at word boundaries (after spaces, tabs, hyphens). Trims
/// whitespace at soft breaks and skips leading whitespace on
/// continuation lines.
pub struct WordBreaker;

impl LineBreaker for WordBreaker {
    fn can_break_before(&self, text: &[u8], pos: usize) -> bool {
        if pos == 0 || pos >= text.len() {
            return false;
        }

        matches!(text[pos - 1], b' ' | b'\t' | b'-')
    }

    fn trim_whitespace(&self) -> bool {
        true
    }
}

// ── Output types ──────────────────────────────────────────────────────

/// Text alignment within the available width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

/// A single laid-out line within a paragraph.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct LayoutLine {
    /// Start byte offset in source text.
    pub byte_offset: u32,
    /// Byte count of this line's content (excludes trailing whitespace
    /// when trimmed, excludes newline characters).
    pub byte_length: u32,
    /// Horizontal offset in points, determined by alignment.
    pub x: f32,
    /// Vertical position in points, relative to paragraph start.
    pub y: i32,
    /// Rendered width of this line's content in points.
    pub width: f32,
}

/// Result of laying out a paragraph of text.
pub struct ParagraphLayout {
    pub lines: Vec<LayoutLine>,
    pub total_height: i32,
}

impl ParagraphLayout {
    /// Find which line and column a byte offset falls on.
    ///
    /// Returns `(line_index, column)` where column is the byte offset
    /// from the start of the line. For bytes that fall in trimmed
    /// whitespace between lines, they are assigned to the preceding line
    /// (column may exceed `byte_length`).
    pub fn byte_to_line_col(&self, byte_offset: usize) -> (usize, usize) {
        if self.lines.is_empty() {
            return (0, 0);
        }

        for (i, line) in self.lines.iter().enumerate() {
            let start = line.byte_offset as usize;
            let next_start = if i + 1 < self.lines.len() {
                self.lines[i + 1].byte_offset as usize
            } else {
                usize::MAX
            };

            if byte_offset < next_start {
                return (i, byte_offset.saturating_sub(start));
            }
        }

        // Past end — last line.
        let last = self.lines.len() - 1;
        let start = self.lines[last].byte_offset as usize;

        (last, byte_offset.saturating_sub(start))
    }

    /// Inverse of `byte_to_line_col` — convert (line, column) to byte offset.
    ///
    /// If `target_col` exceeds the line's byte length, snaps to the end
    /// of the line. If `target_line` exceeds the line count, returns
    /// the total text length (sum of all line offsets + lengths).
    pub fn line_col_to_byte(&self, target_line: usize, target_col: usize) -> usize {
        if self.lines.is_empty() {
            return 0;
        }

        if target_line >= self.lines.len() {
            // Past last line — return end of text.
            let last = &self.lines[self.lines.len() - 1];

            return (last.byte_offset + last.byte_length) as usize;
        }

        let line = &self.lines[target_line];
        let start = line.byte_offset as usize;
        let len = line.byte_length as usize;
        let col = if target_col > len { len } else { target_col };

        start + col
    }
}

// ── Layout function ───────────────────────────────────────────────────

/// Lay out text into lines that fit within `max_width` points.
///
/// The `metrics` provider determines character widths — monospace fonts
/// return a constant, proportional fonts return per-character widths.
/// The `breaker` determines where lines may break.
pub fn layout_paragraph(
    text: &[u8],
    metrics: &dyn FontMetrics,
    max_width: f32,
    alignment: Alignment,
    breaker: &dyn LineBreaker,
) -> ParagraphLayout {
    let line_height = metrics.line_height() as i32;
    let trim = breaker.trim_whitespace();
    let mut lines = Vec::new();
    let mut pos: usize = 0;
    let mut y: i32 = 0;

    while pos < text.len() {
        // Skip leading whitespace on continuation lines (soft breaks only).
        if trim && !lines.is_empty() {
            let before_skip = pos;

            while pos < text.len() && matches!(text[pos], b' ' | b'\t') {
                pos += 1;
            }

            // If we skipped to end-of-text, all remaining was whitespace.
            // Don't emit a blank line for it.
            if pos >= text.len() && before_skip < text.len() {
                break;
            }
        }

        let line_start = pos;
        let mut width: f32 = 0.0;
        // Last valid break: (byte_pos_after_break, width_at_that_point).
        let mut best_break: Option<(usize, f32)> = None;
        let mut broke_soft = false;

        while pos < text.len() {
            let b = text[pos];

            // Hard line break — stop without including the newline.
            if b == b'\n' {
                break;
            }

            let cw = metrics.char_width(b as char);

            // Would this character exceed the line width?
            if width + cw > max_width && pos > line_start {
                broke_soft = true;

                if let Some((bp, bw)) = best_break {
                    pos = bp;
                    width = bw;
                }

                // else: no break opportunity found, break at current pos
                // (character-level fallback for very long words).
                break;
            }

            width += cw;
            pos += 1;

            if pos < text.len() && breaker.can_break_before(text, pos) {
                best_break = Some((pos, width));
            }
        }

        // Determine displayed content boundaries.
        let mut line_end = pos;
        let mut line_width = width;

        if trim && broke_soft {
            // Trim trailing whitespace from soft-broken lines.
            while line_end > line_start && matches!(text[line_end - 1], b' ' | b'\t') {
                line_end -= 1;
            }

            if line_end < pos {
                // Recalculate width without trailing whitespace.
                line_width = 0.0;

                for &b in &text[line_start..line_end] {
                    line_width += metrics.char_width(b as char);
                }
            }
        }

        let x = match alignment {
            Alignment::Left => 0.0,
            Alignment::Center => (max_width - line_width) * 0.5,
            Alignment::Right => max_width - line_width,
        };

        lines.push(LayoutLine {
            byte_offset: line_start as u32,
            byte_length: (line_end - line_start) as u32,
            x,
            y,
            width: line_width,
        });

        y = y.saturating_add(line_height);

        // Skip newline character.
        if pos < text.len() && text[pos] == b'\n' {
            pos += 1;
        }
    }

    // Empty text: one empty line (for cursor positioning).
    if lines.is_empty() {
        let x = match alignment {
            Alignment::Left => 0.0,
            Alignment::Center => max_width * 0.5,
            Alignment::Right => max_width,
        };

        lines.push(LayoutLine {
            byte_offset: 0,
            byte_length: 0,
            x,
            y: 0,
            width: 0.0,
        });
    }

    // Trailing newline: emit an empty line so the cursor can sit there.
    if !text.is_empty() && text[text.len() - 1] == b'\n' {
        let x = match alignment {
            Alignment::Left => 0.0,
            Alignment::Center => max_width * 0.5,
            Alignment::Right => max_width,
        };

        lines.push(LayoutLine {
            byte_offset: text.len() as u32,
            byte_length: 0,
            x,
            y,
            width: 0.0,
        });
    }

    ParagraphLayout {
        lines,
        total_height: y,
    }
}

/// Standalone `byte_to_line_col` — computes the layout and searches it.
///
/// Convenience for call sites that do not have a `ParagraphLayout` at hand.
/// Allocates internally. For hot paths where layout is already available,
/// use `ParagraphLayout::byte_to_line_col` directly.
pub fn byte_to_line_col(
    text: &[u8],
    byte_offset: usize,
    metrics: &dyn FontMetrics,
    max_width: f32,
    breaker: &dyn LineBreaker,
) -> (usize, usize) {
    let layout = layout_paragraph(text, metrics, max_width, Alignment::Left, breaker);

    layout.byte_to_line_col(byte_offset)
}

/// Inverse of `byte_to_line_col` — convert (line, column) to byte offset.
///
/// Computes the layout and finds the byte offset for the given visual
/// position. If `target_col` exceeds the line length, snaps to the
/// end of the line. If `target_line` exceeds the line count, returns
/// the end of the text.
pub fn line_col_to_byte(
    text: &[u8],
    target_line: usize,
    target_col: usize,
    metrics: &dyn FontMetrics,
    max_width: f32,
    breaker: &dyn LineBreaker,
) -> usize {
    let layout = layout_paragraph(text, metrics, max_width, Alignment::Left, breaker);

    layout.line_col_to_byte(target_line, target_col)
}

/// Find the previous word boundary (for Opt+Left / Opt+Backspace).
///
/// From `pos`, skips whitespace backward, then skips non-whitespace
/// backward. Returns the byte offset of the word start.
pub fn word_boundary_backward(text: &[u8], pos: usize) -> usize {
    if pos == 0 || text.is_empty() {
        return 0;
    }

    let mut i = pos;

    while i > 0 && is_whitespace(text[i - 1]) {
        i -= 1;
    }
    while i > 0 && !is_whitespace(text[i - 1]) {
        i -= 1;
    }

    i
}

/// Find the next word boundary (for Opt+Right / Opt+Delete).
///
/// From `pos`, skips non-whitespace forward, then skips whitespace
/// forward. Returns the byte offset past the word end.
pub fn word_boundary_forward(text: &[u8], pos: usize) -> usize {
    let len = text.len();

    if pos >= len {
        return len;
    }

    let mut i = pos;

    while i < len && !is_whitespace(text[i]) {
        i += 1;
    }
    while i < len && is_whitespace(text[i]) {
        i += 1;
    }

    i
}

#[inline]
pub fn is_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\n' || b == b'\t'
}

// ── Mixed-style line breaking ────────────────────────────────────────

/// A pre-measured character for mixed-style line breaking.
///
/// The caller measures each character using its style's font metrics,
/// then passes the stream to `break_measured_lines`. The breaker does
/// not need to know about fonts, styles, or piece tables.
#[derive(Debug, Clone, Copy)]
pub struct MeasuredChar {
    /// Byte offset in the logical text.
    pub byte_offset: u32,
    /// UTF-8 byte length of this character (1–4).
    pub byte_len: u8,
    /// Advance width in points, from the character's font metrics.
    pub width: f32,
    /// Index of the styled run this character belongs to.
    pub run_index: u16,
    /// Whether this character is whitespace (space, tab).
    pub is_whitespace: bool,
    /// Whether this character is a newline.
    pub is_newline: bool,
}

/// A line produced by `break_measured_lines`.
#[derive(Debug, Clone, Copy)]
pub struct LineBreak {
    /// Start byte offset in logical text.
    pub byte_start: u32,
    /// End byte offset in logical text (exclusive).
    pub byte_end: u32,
    /// Actual rendered width of this line in points.
    pub width: f32,
}

/// Line breaking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakMode {
    /// Break at word boundaries (after whitespace). Falls back to
    /// character-level breaking for words that exceed the line width.
    Word,
    /// Break at any character boundary.
    Char,
}

/// Break a stream of pre-measured characters into lines.
///
/// This is the mixed-style equivalent of `layout_paragraph`. Instead of
/// measuring characters internally via `FontMetrics`, the caller
/// pre-measures each character and passes the results here. This
/// decouples the breaker from any font or style knowledge.
///
/// Existing `CharBreaker`/`WordBreaker` paths are unchanged — this
/// function is used only for `text/rich` documents.
pub fn break_measured_lines(
    chars: &[MeasuredChar],
    line_width: f32,
    mode: BreakMode,
) -> Vec<LineBreak> {
    let mut lines = Vec::new();

    if chars.is_empty() {
        return lines;
    }

    let mut i = 0;

    while i < chars.len() {
        let line_start_byte = chars[i].byte_offset;
        let mut width: f32 = 0.0;
        // Last valid word-break: (char index of first char in next word,
        // trimmed byte_end, trimmed width).
        let mut best_break: Option<(usize, u32, f32)> = None;
        let line_start_idx = i;
        let mut line_emitted = false;

        while i < chars.len() {
            let mc = &chars[i];

            // Hard newline — emit line without it, advance past it.
            if mc.is_newline {
                lines.push(LineBreak {
                    byte_start: line_start_byte,
                    byte_end: mc.byte_offset,
                    width,
                });

                i += 1;
                line_emitted = true;

                break;
            }

            // Would this character exceed the line width?
            if width + mc.width > line_width && i > line_start_idx {
                if mode == BreakMode::Word {
                    if let Some((next_idx, trimmed_end, trimmed_w)) = best_break {
                        lines.push(LineBreak {
                            byte_start: line_start_byte,
                            byte_end: trimmed_end,
                            width: trimmed_w,
                        });

                        // Skip any remaining whitespace to find the next word.
                        i = next_idx;

                        while i < chars.len() && chars[i].is_whitespace && !chars[i].is_newline {
                            i += 1;
                        }

                        line_emitted = true;

                        break;
                    }
                }

                // Char mode, or word mode with no break opportunity.
                lines.push(LineBreak {
                    byte_start: line_start_byte,
                    byte_end: mc.byte_offset,
                    width,
                });

                line_emitted = true;

                break;
            }

            width += mc.width;
            i += 1;

            // Record word-break opportunity after whitespace.
            if mode == BreakMode::Word && mc.is_whitespace && !mc.is_newline {
                let (trimmed_end, trimmed_w) = trim_trailing(chars, line_start_idx, i);

                best_break = Some((i, trimmed_end, trimmed_w));
            }
        }

        if !line_emitted {
            // Reached end of input — emit remaining content.
            let end_byte = chars.last().map_or(line_start_byte, |last| {
                last.byte_offset + last.byte_len as u32
            });

            lines.push(LineBreak {
                byte_start: line_start_byte,
                byte_end: end_byte,
                width,
            });

            break;
        }
    }

    lines
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test font metrics ────────────────────────────────────────────

    /// Monospace font: every character is 10pt wide, line height 20pt.
    struct MonoMetrics;
    impl FontMetrics for MonoMetrics {
        fn char_width(&self, _ch: char) -> f32 {
            10.0
        }

        fn line_height(&self) -> f32 {
            20.0
        }
    }

    /// Proportional font: space=5, others=10. Line height 18pt.
    struct ProportionalMetrics;
    impl FontMetrics for ProportionalMetrics {
        fn char_width(&self, ch: char) -> f32 {
            if ch == ' ' { 5.0 } else { 10.0 }
        }

        fn line_height(&self) -> f32 {
            18.0
        }
    }

    // ── CharBreaker tests ────────────────────────────────────────────

    #[test]
    fn char_breaker_allows_any_position() {
        let b = CharBreaker;

        assert!(b.can_break_before(b"abc", 1));
        assert!(b.can_break_before(b"abc", 2));
    }

    #[test]
    fn char_breaker_no_trim() {
        let b = CharBreaker;

        assert!(!b.trim_whitespace());
    }

    // ── WordBreaker tests ────────────────────────────────────────────

    #[test]
    fn word_breaker_after_space() {
        let b = WordBreaker;

        assert!(b.can_break_before(b"a b", 2));
    }

    #[test]
    fn word_breaker_after_hyphen() {
        let b = WordBreaker;

        assert!(b.can_break_before(b"a-b", 2));
    }

    #[test]
    fn word_breaker_not_at_start() {
        let b = WordBreaker;

        assert!(!b.can_break_before(b"abc", 0));
    }

    #[test]
    fn word_breaker_not_at_end() {
        let b = WordBreaker;

        assert!(!b.can_break_before(b"abc", 3));
    }

    #[test]
    fn word_breaker_no_break_between_letters() {
        let b = WordBreaker;

        assert!(!b.can_break_before(b"abc", 1));
    }

    #[test]
    fn word_breaker_trims_whitespace() {
        let b = WordBreaker;

        assert!(b.trim_whitespace());
    }

    // ── Empty text ───────────────────────────────────────────────────

    #[test]
    fn empty_text_produces_one_line() {
        let layout = layout_paragraph(b"", &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].byte_offset, 0);
        assert_eq!(layout.lines[0].byte_length, 0);
        assert_eq!(layout.lines[0].width, 0.0);
    }

    // ── Single line (no wrap) ────────────────────────────────────────

    #[test]
    fn single_line_fits() {
        let text = b"hello";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].byte_length, 5);
        assert_eq!(layout.lines[0].width, 50.0);
    }

    // ── Character wrapping ───────────────────────────────────────────

    #[test]
    fn char_wrap_splits_at_width() {
        // 10pt chars, 30pt width => 3 chars per line.
        let text = b"abcdef";
        let layout = layout_paragraph(text, &MonoMetrics, 30.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[0].byte_length, 3);
        assert_eq!(layout.lines[1].byte_length, 3);
    }

    #[test]
    fn char_wrap_partial_last_line() {
        // 10pt chars, 30pt width => "abcde" = 3 + 2
        let text = b"abcde";
        let layout = layout_paragraph(text, &MonoMetrics, 30.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[0].byte_length, 3);
        assert_eq!(layout.lines[1].byte_length, 2);
    }

    // ── Word wrapping ────────────────────────────────────────────────

    #[test]
    fn word_wrap_breaks_at_space() {
        // "hello world" with 10pt chars and 60pt width.
        // "hello " = 60pt, then "world" on next line.
        let text = b"hello world";
        let layout = layout_paragraph(text, &MonoMetrics, 60.0, Alignment::Left, &WordBreaker);

        assert_eq!(layout.lines.len(), 2);
        // First line: "hello" (trimmed trailing space).
        assert_eq!(layout.lines[0].byte_offset, 0);
        assert_eq!(layout.lines[0].byte_length, 5);
        // Second line: "world".
        assert_eq!(layout.lines[1].byte_length, 5);
    }

    #[test]
    fn word_wrap_long_word_falls_back_to_char() {
        // A word that exceeds the line width. WordBreaker has no break
        // opportunity, so it falls back to char-level breaking.
        let text = b"abcdefghij";
        let layout = layout_paragraph(text, &MonoMetrics, 50.0, Alignment::Left, &WordBreaker);

        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[0].byte_length, 5);
        assert_eq!(layout.lines[1].byte_length, 5);
    }

    // ── Hard line breaks ─────────────────────────────────────────────

    #[test]
    fn hard_newline_splits_lines() {
        let text = b"abc\ndef";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[0].byte_offset, 0);
        assert_eq!(layout.lines[0].byte_length, 3);
        assert_eq!(layout.lines[1].byte_offset, 4);
        assert_eq!(layout.lines[1].byte_length, 3);
    }

    #[test]
    fn trailing_newline_adds_empty_line() {
        let text = b"abc\n";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[1].byte_length, 0);
        assert_eq!(layout.lines[1].byte_offset, 4);
    }

    // ── Y positions ──────────────────────────────────────────────────

    #[test]
    fn y_positions_increment_by_line_height() {
        let text = b"a\nb\nc";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines.len(), 3);
        assert_eq!(layout.lines[0].y, 0);
        assert_eq!(layout.lines[1].y, 20);
        assert_eq!(layout.lines[2].y, 40);
    }

    // ── Alignment ────────────────────────────────────────────────────

    #[test]
    fn alignment_left() {
        let text = b"hi";
        let layout = layout_paragraph(text, &MonoMetrics, 100.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.lines[0].x, 0.0);
    }

    #[test]
    fn alignment_center() {
        let text = b"hi"; // width = 20pt, max = 100pt
        let layout = layout_paragraph(text, &MonoMetrics, 100.0, Alignment::Center, &CharBreaker);

        assert!((layout.lines[0].x - 40.0).abs() < 0.01);
    }

    #[test]
    fn alignment_right() {
        let text = b"hi"; // width = 20pt, max = 100pt
        let layout = layout_paragraph(text, &MonoMetrics, 100.0, Alignment::Right, &CharBreaker);

        assert!((layout.lines[0].x - 80.0).abs() < 0.01);
    }

    // ── ParagraphLayout::byte_to_line_col ────────────────────────────

    #[test]
    fn byte_to_line_col_first_line() {
        let text = b"abc\ndef";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.byte_to_line_col(0), (0, 0));
        assert_eq!(layout.byte_to_line_col(2), (0, 2));
    }

    #[test]
    fn byte_to_line_col_second_line() {
        let text = b"abc\ndef";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.byte_to_line_col(4), (1, 0));
        assert_eq!(layout.byte_to_line_col(6), (1, 2));
    }

    #[test]
    fn byte_to_line_col_past_end() {
        let text = b"abc";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);
        let (line, col) = layout.byte_to_line_col(100);

        assert_eq!(line, 0);
    }

    #[test]
    fn byte_to_line_col_empty() {
        let layout = layout_paragraph(b"", &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.byte_to_line_col(0), (0, 0));
    }

    // ── ParagraphLayout::line_col_to_byte ────────────────────────────

    #[test]
    fn line_col_to_byte_basic() {
        let text = b"abc\ndef";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.line_col_to_byte(0, 0), 0);
        assert_eq!(layout.line_col_to_byte(0, 2), 2);
        assert_eq!(layout.line_col_to_byte(1, 0), 4);
        assert_eq!(layout.line_col_to_byte(1, 2), 6);
    }

    #[test]
    fn line_col_to_byte_clamps_col() {
        let text = b"abc\ndef";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        // Column past end of line snaps to end.
        assert_eq!(layout.line_col_to_byte(0, 100), 3);
    }

    #[test]
    fn line_col_to_byte_past_last_line() {
        let text = b"abc";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.line_col_to_byte(100, 0), 3);
    }

    #[test]
    fn line_col_to_byte_empty() {
        let layout = layout_paragraph(b"", &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.line_col_to_byte(0, 0), 0);
    }

    // ── Standalone helpers ───────────────────────────────────────────

    #[test]
    fn standalone_byte_to_line_col() {
        let text = b"abc\ndef";
        let (line, col) = byte_to_line_col(text, 5, &MonoMetrics, 200.0, &CharBreaker);

        assert_eq!(line, 1);
        assert_eq!(col, 1);
    }

    #[test]
    fn standalone_line_col_to_byte() {
        let text = b"abc\ndef";
        let byte = line_col_to_byte(text, 1, 1, &MonoMetrics, 200.0, &CharBreaker);

        assert_eq!(byte, 5);
    }

    // ── Word boundary helpers ────────────────────────────────────────

    #[test]
    fn word_boundary_backward_from_middle() {
        let text = b"hello world foo";

        assert_eq!(word_boundary_backward(text, 11), 6);
    }

    #[test]
    fn word_boundary_backward_from_start() {
        let text = b"hello";

        assert_eq!(word_boundary_backward(text, 0), 0);
    }

    #[test]
    fn word_boundary_backward_empty() {
        assert_eq!(word_boundary_backward(b"", 0), 0);
    }

    #[test]
    fn word_boundary_forward_from_start() {
        let text = b"hello world";

        assert_eq!(word_boundary_forward(text, 0), 6);
    }

    #[test]
    fn word_boundary_forward_from_end() {
        let text = b"hello";
        assert_eq!(word_boundary_forward(text, 5), 5);
    }

    #[test]
    fn word_boundary_forward_empty() {
        assert_eq!(word_boundary_forward(b"", 0), 0);
    }

    // ── is_whitespace ────────────────────────────────────────────────

    #[test]
    fn whitespace_detection() {
        assert!(is_whitespace(b' '));
        assert!(is_whitespace(b'\n'));
        assert!(is_whitespace(b'\t'));
        assert!(!is_whitespace(b'a'));
        assert!(!is_whitespace(b'0'));
    }

    // ── MeasuredChar / break_measured_lines ──────────────────────────

    fn make_chars(text: &[u8], char_width: f32) -> Vec<MeasuredChar> {
        text.iter()
            .enumerate()
            .map(|(i, &b)| MeasuredChar {
                byte_offset: i as u32,
                byte_len: 1,
                width: if b == b'\n' { 0.0 } else { char_width },
                run_index: 0,
                is_whitespace: b == b' ' || b == b'\t',
                is_newline: b == b'\n',
            })
            .collect()
    }

    #[test]
    fn measured_lines_empty() {
        let lines = break_measured_lines(&[], 100.0, BreakMode::Word);

        assert!(lines.is_empty());
    }

    #[test]
    fn measured_lines_single_line() {
        let chars = make_chars(b"hello", 10.0);
        let lines = break_measured_lines(&chars, 200.0, BreakMode::Char);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].byte_start, 0);
        assert_eq!(lines[0].byte_end, 5);
        assert!((lines[0].width - 50.0).abs() < 0.01);
    }

    #[test]
    fn measured_lines_char_wrap() {
        let chars = make_chars(b"abcdef", 10.0);
        let lines = break_measured_lines(&chars, 30.0, BreakMode::Char);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].byte_end, 3);
        assert_eq!(lines[1].byte_start, 3);
    }

    #[test]
    fn measured_lines_word_wrap() {
        let chars = make_chars(b"hi there world", 10.0);
        // "hi there " has widths: h(10)+i(10)+' '(10)+t(10)+h(10)+e(10)+r(10)+e(10)+' '(10) = 90
        // With line_width=90, "hi there " fits, then "world".
        let lines = break_measured_lines(&chars, 90.0, BreakMode::Word);

        assert!(lines.len() >= 2);
    }

    #[test]
    fn measured_lines_hard_newline() {
        let chars = make_chars(b"ab\ncd", 10.0);
        let lines = break_measured_lines(&chars, 200.0, BreakMode::Char);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].byte_start, 0);
        assert_eq!(lines[0].byte_end, 2);
        assert_eq!(lines[1].byte_start, 3);
        assert_eq!(lines[1].byte_end, 5);
    }

    // ── Proportional metrics ─────────────────────────────────────────

    #[test]
    fn proportional_layout() {
        // "ab cd" => a(10)+b(10)+' '(5)+c(10)+d(10) = 45pt.
        // With 30pt width: "ab " = 25pt fits, next "cd" = 20pt.
        let text = b"ab cd";
        let layout = layout_paragraph(
            text,
            &ProportionalMetrics,
            30.0,
            Alignment::Left,
            &WordBreaker,
        );

        assert_eq!(layout.lines.len(), 2);
    }

    // ── total_height ─────────────────────────────────────────────────

    #[test]
    fn total_height_single_line() {
        let text = b"hello";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        // Single line: y starts at 0, total_height = 0 + line_height = 20.
        assert_eq!(layout.total_height, 20);
    }

    #[test]
    fn total_height_multiple_lines() {
        let text = b"a\nb\nc";
        let layout = layout_paragraph(text, &MonoMetrics, 200.0, Alignment::Left, &CharBreaker);

        assert_eq!(layout.total_height, 60);
    }
}

/// Trim trailing whitespace from `chars[start_idx..end_idx]`.
/// Returns `(trimmed_byte_end, trimmed_width)`.
fn trim_trailing(chars: &[MeasuredChar], start_idx: usize, end_idx: usize) -> (u32, f32) {
    let mut trim_end = end_idx;

    while trim_end > start_idx && chars[trim_end - 1].is_whitespace {
        trim_end -= 1;
    }

    let byte_end = if trim_end > start_idx {
        let last = &chars[trim_end - 1];

        last.byte_offset + last.byte_len as u32
    } else {
        chars[start_idx].byte_offset
    };
    let mut w: f32 = 0.0;

    for mc in &chars[start_idx..trim_end] {
        w += mc.width;
    }

    (byte_end, w)
}
