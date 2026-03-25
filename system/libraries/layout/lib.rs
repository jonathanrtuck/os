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
fn is_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\n' || b == b'\t'
}
