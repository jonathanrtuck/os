//! Tests for the mixed-style line breaking (`break_measured_lines`).

use layout::{break_measured_lines, BreakMode, MeasuredChar};

// ── Helpers ──────────────────────────────────────────────────────────

/// Build a MeasuredChar stream from ASCII text with uniform width.
fn uniform_chars(text: &[u8], width: f32) -> Vec<MeasuredChar> {
    text.iter()
        .enumerate()
        .map(|(i, &b)| MeasuredChar {
            byte_offset: i as u32,
            byte_len: 1,
            width,
            run_index: 0,
            is_whitespace: b == b' ' || b == b'\t',
            is_newline: b == b'\n',
        })
        .collect()
}

/// Build a MeasuredChar stream from ASCII text with per-char widths.
fn varied_chars(text: &[u8], widths: &[f32]) -> Vec<MeasuredChar> {
    assert_eq!(text.len(), widths.len());
    text.iter()
        .zip(widths.iter())
        .enumerate()
        .map(|(i, (&b, &w))| MeasuredChar {
            byte_offset: i as u32,
            byte_len: 1,
            width: w,
            run_index: 0,
            is_whitespace: b == b' ' || b == b'\t',
            is_newline: b == b'\n',
        })
        .collect()
}

/// Build a MeasuredChar stream with per-char run indices.
fn chars_with_runs(text: &[u8], width: f32, runs: &[u16]) -> Vec<MeasuredChar> {
    assert_eq!(text.len(), runs.len());
    text.iter()
        .zip(runs.iter())
        .enumerate()
        .map(|(i, (&b, &r))| MeasuredChar {
            byte_offset: i as u32,
            byte_len: 1,
            width,
            run_index: r,
            is_whitespace: b == b' ' || b == b'\t',
            is_newline: b == b'\n',
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────

#[test]
fn uniform_width_char_mode() {
    // 10 chars, width 1.0 each, line_width 3.0 → 4 lines (3,3,3,1)
    let text = b"abcdefghij";
    let chars = uniform_chars(text, 1.0);
    let lines = break_measured_lines(&chars, 3.0, BreakMode::Char);

    assert_eq!(lines.len(), 4);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 3));
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (3, 6));
    assert_eq!((lines[2].byte_start, lines[2].byte_end), (6, 9));
    assert_eq!((lines[3].byte_start, lines[3].byte_end), (9, 10));
}

#[test]
fn mixed_width_respects_limit() {
    // Chars: widths 5, 10, 5, 10, 5, 10 → line_width 30
    // Cumulative: 5, 15, 20, 30, 35 (exceeds at index 4)
    let text = b"abcdef";
    let widths = [5.0, 10.0, 5.0, 10.0, 5.0, 10.0];
    let chars = varied_chars(text, &widths);
    let lines = break_measured_lines(&chars, 30.0, BreakMode::Char);

    // First line: a(5)+b(10)+c(5)+d(10) = 30, then e(5) would be 35 → break
    assert_eq!(lines.len(), 2);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 4));
    assert!((lines[0].width - 30.0).abs() < 0.001);
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (4, 6));
    assert!((lines[1].width - 15.0).abs() < 0.001);
}

#[test]
fn word_break_at_whitespace() {
    // "hello world foo" — 3 words
    let text = b"hello world foo";
    let chars = uniform_chars(text, 1.0);
    // line_width = 8 → "hello " fits (6), "wo" would be 8 but "world " is 6 more
    // "hello " = 6, + "w" = 7, + "o" = 8, + "r" = 9 > 8 → break after "hello "
    let lines = break_measured_lines(&chars, 8.0, BreakMode::Word);

    // Line 1: "hello" (5 chars, break after space), Line 2: "world" (5), Line 3: "foo" (3)
    assert_eq!(lines.len(), 3);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 5));
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (6, 11));
    assert_eq!((lines[2].byte_start, lines[2].byte_end), (12, 15));
}

#[test]
fn long_word_fallback() {
    // One word that exceeds line width — must break at character level.
    let text = b"abcdefghij";
    let chars = uniform_chars(text, 1.0);
    let lines = break_measured_lines(&chars, 4.0, BreakMode::Word);

    // No word breaks available → character-level fallback
    assert_eq!(lines.len(), 3);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 4));
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (4, 8));
    assert_eq!((lines[2].byte_start, lines[2].byte_end), (8, 10));
}

#[test]
fn newline_forces_break() {
    let text = b"ab\ncd\nef";
    let chars = uniform_chars(text, 1.0);
    let lines = break_measured_lines(&chars, 100.0, BreakMode::Word);

    assert_eq!(lines.len(), 3);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 2));
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (3, 5));
    assert_eq!((lines[2].byte_start, lines[2].byte_end), (6, 8));
}

#[test]
fn empty_input() {
    let lines = break_measured_lines(&[], 100.0, BreakMode::Word);
    assert!(lines.is_empty());
}

#[test]
fn run_index_preserved() {
    // "aaabbb" — first 3 chars in run 0, next 3 in run 1, line_width large enough for all.
    let text = b"aaabbb";
    let runs = [0u16, 0, 0, 1, 1, 1];
    let chars = chars_with_runs(text, 1.0, &runs);
    let lines = break_measured_lines(&chars, 100.0, BreakMode::Char);

    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 6));
    assert!((lines[0].width - 6.0).abs() < 0.001);
}

#[test]
fn style_change_no_spurious_break() {
    // "helloworld" — style changes at index 5, but no whitespace → no break.
    let text = b"helloworld";
    let runs: Vec<u16> = (0..10).map(|i| if i < 5 { 0 } else { 1 }).collect();
    let chars = chars_with_runs(text, 1.0, &runs);
    let lines = break_measured_lines(&chars, 100.0, BreakMode::Word);

    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 10));
}

#[test]
fn word_break_trims_trailing_whitespace() {
    // "abc def" with line_width=4 → "abc" should have width 3.0, not 4.0
    let text = b"abc def";
    let chars = uniform_chars(text, 1.0);
    let lines = break_measured_lines(&chars, 4.0, BreakMode::Word);

    assert_eq!(lines.len(), 2);
    assert!((lines[0].width - 3.0).abs() < 0.001, "got {}", lines[0].width);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 3));
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (4, 7));
}

#[test]
fn line_width_exact_fit() {
    // 4 chars at 2.5 each = 10.0, line_width = 10.0 → single line
    let text = b"abcd";
    let chars = uniform_chars(text, 2.5);
    let lines = break_measured_lines(&chars, 10.0, BreakMode::Char);

    assert_eq!(lines.len(), 1);
    assert!((lines[0].width - 10.0).abs() < 0.001);
}

#[test]
fn trailing_newline_produces_no_extra_content() {
    let text = b"ab\n";
    let chars = uniform_chars(text, 1.0);
    let lines = break_measured_lines(&chars, 100.0, BreakMode::Word);

    // "ab" then newline → one line with "ab"
    // (No trailing empty line — that's the layout engine's job, not the breaker's.)
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 2));
}

#[test]
fn multiple_spaces_between_words() {
    let text = b"ab   cd";
    let chars = uniform_chars(text, 1.0);
    // line_width=5: "ab   " = 5, + "c" = 6 > 5 → break after spaces
    let lines = break_measured_lines(&chars, 5.0, BreakMode::Word);

    assert_eq!(lines.len(), 2);
    // Trailing whitespace trimmed from first line
    assert_eq!((lines[0].byte_start, lines[0].byte_end), (0, 2));
    assert_eq!((lines[1].byte_start, lines[1].byte_end), (5, 7));
}
