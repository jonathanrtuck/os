//! Tests for the unified text layout library.
//!
//! Verifies that:
//! - Monospace layout (CharBreaker) matches the old `layout_mono_lines` behavior
//! - Proportional layout (WordBreaker) produces correct word wrapping
//! - Alignment offsets are calculated correctly
//! - Edge cases are handled (empty text, trailing newline, very long words)
//! - `byte_to_line_col` agrees with layout output

use layout::{
    byte_to_line_col, layout_paragraph, Alignment, CharBreaker, FontMetrics, LayoutLine,
    LineBreaker, ParagraphLayout, WordBreaker,
};

// ── Test font metrics ─────────────────────────────────────────────────

/// Monospace metrics: every character has the same width.
struct MonoMetrics {
    char_width: f32,
    line_height: f32,
}

impl MonoMetrics {
    fn new(char_width: f32, line_height: f32) -> Self {
        Self {
            char_width,
            line_height,
        }
    }
}

impl FontMetrics for MonoMetrics {
    fn char_width(&self, _ch: char) -> f32 {
        self.char_width
    }
    fn line_height(&self) -> f32 {
        self.line_height
    }
}

/// Proportional metrics: different widths per character class.
struct ProportionalMetrics;

impl FontMetrics for ProportionalMetrics {
    fn char_width(&self, ch: char) -> f32 {
        match ch {
            'i' | 'l' | '!' | '.' | ',' | ':' | ';' | '\'' => 4.0,
            'm' | 'w' | 'M' | 'W' => 12.0,
            ' ' => 5.0,
            '-' => 5.0,
            _ => 8.0, // default for most characters
        }
    }
    fn line_height(&self) -> f32 {
        20.0
    }
}

// ── Helper ────────────────────────────────────────────────────────────

/// Extract the text content of a layout line from the source.
fn line_text<'a>(text: &'a [u8], line: &LayoutLine) -> &'a str {
    let start = line.byte_offset as usize;
    let end = start + line.byte_length as usize;
    core::str::from_utf8(&text[start..end]).unwrap_or("<invalid>")
}

// ═══════════════════════════════════════════════════════════════════════
// Monospace + CharBreaker — must match old layout_mono_lines behavior
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn mono_single_line_fits() {
    let text = b"hello";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 1);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(layout.lines[0].y, 0);
    assert_eq!(layout.lines[0].width, 40.0); // 5 * 8
    assert_eq!(layout.lines[0].x, 0.0);
}

#[test]
fn mono_wraps_at_max_width() {
    // 5 chars fit per line (5 * 8 = 40 = max_width).
    let text = b"helloworld";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 40.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(line_text(text, &layout.lines[1]), "world");
    assert_eq!(layout.lines[0].y, 0);
    assert_eq!(layout.lines[1].y, 18);
}

#[test]
fn mono_wraps_preserves_spaces() {
    // CharBreaker doesn't trim — space stays as first char of next line.
    let text = b"hello world";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 40.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 3);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(line_text(text, &layout.lines[1]), " worl");
    assert_eq!(line_text(text, &layout.lines[2]), "d");
}

#[test]
fn mono_newline_creates_lines() {
    let text = b"ab\ncd\nef";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 3);
    assert_eq!(line_text(text, &layout.lines[0]), "ab");
    assert_eq!(line_text(text, &layout.lines[1]), "cd");
    assert_eq!(line_text(text, &layout.lines[2]), "ef");
    assert_eq!(layout.lines[0].y, 0);
    assert_eq!(layout.lines[1].y, 18);
    assert_eq!(layout.lines[2].y, 36);
}

#[test]
fn mono_trailing_newline_adds_empty_line() {
    let text = b"hello\n";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(layout.lines[1].byte_length, 0);
    assert_eq!(layout.lines[1].byte_offset, 6); // past the newline
}

#[test]
fn mono_empty_text_produces_one_line() {
    let text = b"";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].byte_offset, 0);
    assert_eq!(layout.lines[0].byte_length, 0);
    assert_eq!(layout.lines[0].width, 0.0);
}

#[test]
fn mono_wrap_plus_newline() {
    // Wrap at 3 chars, then a newline in the middle.
    let text = b"abcde\nfg";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 24.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 3);
    assert_eq!(line_text(text, &layout.lines[0]), "abc");
    assert_eq!(line_text(text, &layout.lines[1]), "de");
    assert_eq!(line_text(text, &layout.lines[2]), "fg");
}

#[test]
fn mono_newline_before_wrap_point() {
    // Newline at position 2 with wrap at 5 — newline takes priority.
    let text = b"ab\ncdefghij";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 40.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 3);
    assert_eq!(line_text(text, &layout.lines[0]), "ab");
    assert_eq!(line_text(text, &layout.lines[1]), "cdefg");
    assert_eq!(line_text(text, &layout.lines[2]), "hij");
}

#[test]
fn mono_total_height() {
    let text = b"a\nb\nc";
    let metrics = MonoMetrics::new(8.0, 20.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 3);
    // total_height = 3 lines * 20 = 60
    assert_eq!(layout.total_height, 60);
}

#[test]
fn mono_exact_fit_no_wrap() {
    // Exactly max_width — should NOT wrap.
    let text = b"12345";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 40.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 1);
    assert_eq!(line_text(text, &layout.lines[0]), "12345");
    assert_eq!(layout.lines[0].width, 40.0);
}

// ═══════════════════════════════════════════════════════════════════════
// Word wrapping + WordBreaker
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn word_wrap_basic() {
    // "hello world" with max_width=48 (6 chars × 8) — "hello" fits, " world" doesn't.
    let text = b"hello world";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 48.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(line_text(text, &layout.lines[1]), "world");
}

#[test]
fn word_wrap_multiple_words() {
    let text = b"the quick brown fox";
    let metrics = MonoMetrics::new(8.0, 18.0);
    // max_width = 80 (10 chars). "the quick " = 10 chars, but "the quick b" > 10.
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "the quick");
    assert_eq!(line_text(text, &layout.lines[1]), "brown fox");
}

#[test]
fn word_wrap_long_word_falls_back_to_char() {
    // A word longer than max_width — must break at character level.
    let text = b"superlongword";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 40.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 3);
    assert_eq!(line_text(text, &layout.lines[0]), "super");
    assert_eq!(line_text(text, &layout.lines[1]), "longw");
    assert_eq!(line_text(text, &layout.lines[2]), "ord");
}

#[test]
fn word_wrap_preserves_hard_newlines() {
    let text = b"hello\nworld";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 200.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(line_text(text, &layout.lines[1]), "world");
}

#[test]
fn word_wrap_trailing_newline() {
    let text = b"hello\n";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 200.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "hello");
    assert_eq!(layout.lines[1].byte_length, 0);
}

#[test]
fn word_wrap_hyphen_break() {
    // Break after hyphen.
    let text = b"self-contained box";
    let metrics = MonoMetrics::new(8.0, 18.0);
    // max_width = 48 (6 chars). "self-c" fits (48), but next 'o' overflows.
    // Break after hyphen: "self-" (5 chars), "contained box" continues.
    let layout = layout_paragraph(text, &metrics, 48.0, Alignment::Left, &WordBreaker);

    assert_eq!(line_text(text, &layout.lines[0]), "self-");
    assert_eq!(layout.lines.len() >= 2, true);
}

// ═══════════════════════════════════════════════════════════════════════
// Proportional font metrics
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn proportional_variable_widths() {
    let text = b"mill";
    let metrics = ProportionalMetrics;
    let layout = layout_paragraph(text, &metrics, 200.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 1);
    // m=12 + i=4 + l=4 + l=4 = 24
    assert_eq!(layout.lines[0].width, 24.0);
}

#[test]
fn proportional_word_wrap() {
    // "mm ii" — m=12, m=12, space=5, i=4, i=4 = 37 total.
    // max_width=30: "mm" (24) fits, space+ii would be 24+5+4=33 > 30.
    let text = b"mm ii";
    let metrics = ProportionalMetrics;
    let layout = layout_paragraph(text, &metrics, 30.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(line_text(text, &layout.lines[0]), "mm");
    assert_eq!(line_text(text, &layout.lines[1]), "ii");
}

#[test]
fn proportional_narrow_chars_fit_more() {
    // "iiii" = 4*4 = 16, "mmmm" = 4*12 = 48.
    // With max_width=20, "iiii" fits on one line, "mmmm" wraps.
    let text_narrow = b"iiii";
    let text_wide = b"mmmm";
    let metrics = ProportionalMetrics;

    let layout_narrow =
        layout_paragraph(text_narrow, &metrics, 20.0, Alignment::Left, &WordBreaker);
    let layout_wide = layout_paragraph(text_wide, &metrics, 20.0, Alignment::Left, &WordBreaker);

    assert_eq!(layout_narrow.lines.len(), 1);
    assert!(layout_wide.lines.len() > 1); // "mmmm" must wrap
}

// ═══════════════════════════════════════════════════════════════════════
// Alignment
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn alignment_left() {
    let text = b"hi";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines[0].x, 0.0);
}

#[test]
fn alignment_center() {
    let text = b"hi";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Center, &CharBreaker);

    // width = 16, max = 80, center = (80 - 16) / 2 = 32
    assert_eq!(layout.lines[0].x, 32.0);
}

#[test]
fn alignment_right() {
    let text = b"hi";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Right, &CharBreaker);

    // width = 16, max = 80, right = 80 - 16 = 64
    assert_eq!(layout.lines[0].x, 64.0);
}

#[test]
fn alignment_empty_line() {
    let text = b"";
    let metrics = MonoMetrics::new(8.0, 18.0);

    let center = layout_paragraph(text, &metrics, 80.0, Alignment::Center, &CharBreaker);
    assert_eq!(center.lines[0].x, 40.0); // (80 - 0) / 2

    let right = layout_paragraph(text, &metrics, 80.0, Alignment::Right, &CharBreaker);
    assert_eq!(right.lines[0].x, 80.0); // 80 - 0
}

// ═══════════════════════════════════════════════════════════════════════
// byte_to_line_col
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn btlc_basic() {
    let text = b"ab\ncd";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.byte_to_line_col(0), (0, 0)); // 'a'
    assert_eq!(layout.byte_to_line_col(1), (0, 1)); // 'b'
    assert_eq!(layout.byte_to_line_col(2), (0, 2)); // '\n' (end of line 0)
    assert_eq!(layout.byte_to_line_col(3), (1, 0)); // 'c'
    assert_eq!(layout.byte_to_line_col(4), (1, 1)); // 'd'
    assert_eq!(layout.byte_to_line_col(5), (1, 2)); // past end
}

#[test]
fn btlc_with_wrap() {
    let text = b"abcdef"; // wraps at 3
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 24.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.byte_to_line_col(0), (0, 0)); // 'a'
    assert_eq!(layout.byte_to_line_col(2), (0, 2)); // 'c'
    assert_eq!(layout.byte_to_line_col(3), (1, 0)); // 'd' — start of line 2
    assert_eq!(layout.byte_to_line_col(5), (1, 2)); // 'f'
}

#[test]
fn btlc_standalone_agrees_with_method() {
    let text = b"the quick brown fox jumps";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 40.0, Alignment::Left, &CharBreaker);

    for offset in 0..=text.len() {
        let method_result = layout.byte_to_line_col(offset);
        let standalone_result = byte_to_line_col(text, offset, &metrics, 40.0, &CharBreaker);
        assert_eq!(
            method_result, standalone_result,
            "disagreement at offset {}",
            offset
        );
    }
}

#[test]
fn btlc_empty_text() {
    let text = b"";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.byte_to_line_col(0), (0, 0));
}

// ═══════════════════════════════════════════════════════════════════════
// Regression: match layout_mono_lines behavior exactly
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn regression_empty_lines_between_newlines() {
    let text = b"a\n\nb";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 3);
    assert_eq!(line_text(text, &layout.lines[0]), "a");
    assert_eq!(line_text(text, &layout.lines[1]), ""); // empty line
    assert_eq!(line_text(text, &layout.lines[2]), "b");
}

#[test]
fn regression_only_newlines() {
    let text = b"\n\n";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    // "\n\n" → empty line, empty line, then trailing-newline adds a third.
    // Current layout_mono_lines: 3 lines (first \n → empty, second \n → empty,
    // trailing \n → empty cursor line).
    assert_eq!(layout.lines.len(), 3);
    for line in &layout.lines {
        assert_eq!(line.byte_length, 0);
    }
}

#[test]
fn regression_single_char() {
    let text = b"x";
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 80.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 1);
    assert_eq!(line_text(text, &layout.lines[0]), "x");
    assert_eq!(layout.lines[0].width, 8.0);
}

#[test]
fn regression_width_per_line_accurate() {
    // Each line should report its exact width, not max_width.
    let text = b"abcdefg"; // wraps at 4: "abcd" (32) + "efg" (24)
    let metrics = MonoMetrics::new(8.0, 18.0);
    let layout = layout_paragraph(text, &metrics, 32.0, Alignment::Left, &CharBreaker);

    assert_eq!(layout.lines.len(), 2);
    assert_eq!(layout.lines[0].width, 32.0);
    assert_eq!(layout.lines[1].width, 24.0);
}

// ═══════════════════════════════════════════════════════════════════════
// Custom LineBreaker
// ═══════════════════════════════════════════════════════════════════════

/// Only break at commas.
struct CommaBreaker;

impl LineBreaker for CommaBreaker {
    fn can_break_before(&self, text: &[u8], pos: usize) -> bool {
        pos > 0 && text[pos - 1] == b','
    }
    fn trim_whitespace(&self) -> bool {
        true
    }
}

#[test]
fn custom_breaker_comma() {
    let text = b"one,two,three";
    let metrics = MonoMetrics::new(8.0, 18.0);
    // max_width=48 (6 chars). "one,tw" fits (48), "one,two" doesn't (56).
    // Break after comma at pos 4: "one," (4 chars, 32), then "two,three".
    let layout = layout_paragraph(text, &metrics, 48.0, Alignment::Left, &CommaBreaker);

    assert_eq!(line_text(text, &layout.lines[0]), "one,");
    assert!(layout.lines.len() >= 2);
}
