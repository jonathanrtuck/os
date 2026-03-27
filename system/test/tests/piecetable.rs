//! Host-side unit tests for the piecetable library.

use piecetable::*;

/// Helper: allocate a buffer and initialize an empty piece table.
fn make_empty(size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    assert!(init(&mut buf, size));
    buf
}

/// Helper: allocate a buffer and initialize with text + default body style.
fn make_with_text(text: &[u8], extra: usize) -> Vec<u8> {
    let size = HEADER_SIZE + 12 + 16 + text.len() + extra;
    let mut buf = vec![0u8; size];
    let style = default_body_style();
    assert!(init_with_text(&mut buf, size, text, &style));
    buf
}

/// Helper: read the full logical text into a Vec.
fn read_text(buf: &[u8]) -> Vec<u8> {
    let len = text_len(buf) as usize;
    let mut out = vec![0u8; len];
    let n = text_slice(buf, 0, len as u32, &mut out);
    out.truncate(n);
    out
}

// ── Initialization ──────────────────────────────────────────────────────

#[test]
fn empty_table() {
    let buf = make_empty(4096);
    assert!(validate(&buf));
    assert_eq!(text_len(&buf), 0);
    assert_eq!(header(&buf).piece_count, 0);
    assert_eq!(cursor_pos(&buf), 0);
}

#[test]
fn init_with_existing_text() {
    let buf = make_with_text(b"hello", 4096);
    assert!(validate(&buf));
    assert_eq!(text_len(&buf), 5);
    assert_eq!(header(&buf).piece_count, 1);
    assert_eq!(read_text(&buf), b"hello");
}

#[test]
fn init_too_small_buffer() {
    let mut buf = vec![0u8; 10];
    assert!(!init(&mut buf, 10));
}

// ── Single character insert ─────────────────────────────────────────────

#[test]
fn insert_single_char() {
    let mut buf = make_empty(4096);
    assert!(insert(&mut buf, 0, b'h'));
    assert_eq!(text_len(&buf), 1);
    assert_eq!(byte_at(&buf, 0), Some(b'h'));
}

// ── Sequential inserts (coalescing) ─────────────────────────────────────

#[test]
fn insert_sequential() {
    let mut buf = make_empty(4096);
    for &ch in b"hello" {
        let pos = text_len(&buf);
        assert!(insert(&mut buf, pos, ch));
    }
    assert_eq!(text_len(&buf), 5);
    assert_eq!(read_text(&buf), b"hello");
    // Sequential same-style inserts should coalesce into 1 piece.
    assert_eq!(header(&buf).piece_count, 1);
}

// ── Insert at beginning ─────────────────────────────────────────────────

#[test]
fn insert_at_beginning() {
    let mut buf = make_with_text(b"ello", 4096);
    assert!(insert(&mut buf, 0, b'h'));
    assert_eq!(text_len(&buf), 5);
    assert_eq!(read_text(&buf), b"hello");
}

// ── Insert in middle ────────────────────────────────────────────────────

#[test]
fn insert_in_middle() {
    let mut buf = make_with_text(b"hello", 4096);
    assert!(insert(&mut buf, 2, b'X'));
    assert_eq!(text_len(&buf), 6);
    assert_eq!(read_text(&buf), b"heXllo");
}

// ── Insert at end ───────────────────────────────────────────────────────

#[test]
fn insert_at_end() {
    let mut buf = make_with_text(b"hello", 4096);
    assert!(insert(&mut buf, 5, b'!'));
    assert_eq!(text_len(&buf), 6);
    assert_eq!(read_text(&buf), b"hello!");
}

// ── Insert bytes (multi-byte) ───────────────────────────────────────────

#[test]
fn insert_bytes_in_middle() {
    let mut buf = make_with_text(b"hello world", 4096);
    assert!(insert_bytes(&mut buf, 5, b" beautiful"));
    assert_eq!(read_text(&buf), b"hello beautiful world");
}

// ── Delete single ───────────────────────────────────────────────────────

#[test]
fn delete_single() {
    let mut buf = make_with_text(b"hello", 4096);
    assert!(delete(&mut buf, 2));
    assert_eq!(text_len(&buf), 4);
    assert_eq!(read_text(&buf), b"helo");
}

// ── Delete range ────────────────────────────────────────────────────────

#[test]
fn delete_range_middle() {
    let mut buf = make_with_text(b"hello world", 4096);
    assert!(delete_range(&mut buf, 5, 6));
    assert_eq!(read_text(&buf), b"helloworld");
}

// ── Delete at boundaries ────────────────────────────────────────────────

#[test]
fn delete_first_char() {
    let mut buf = make_with_text(b"hello", 4096);
    assert!(delete(&mut buf, 0));
    assert_eq!(read_text(&buf), b"ello");
}

#[test]
fn delete_last_char() {
    let mut buf = make_with_text(b"hello", 4096);
    assert!(delete(&mut buf, 4));
    assert_eq!(read_text(&buf), b"hell");
}

// ── Delete empty ────────────────────────────────────────────────────────

#[test]
fn delete_empty() {
    let mut buf = make_empty(4096);
    assert!(!delete(&mut buf, 0));
}

// ── Delete out of range ─────────────────────────────────────────────────

#[test]
fn delete_out_of_range() {
    let mut buf = make_with_text(b"hi", 4096);
    assert!(!delete_range(&mut buf, 0, 5));
}

// ── Style application ───────────────────────────────────────────────────

#[test]
fn apply_style_whole() {
    let mut buf = make_with_text(b"hello", 4096);
    // Add a bold style.
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    apply_style(&mut buf, 0, 5, bold_id);
    for i in 0..5 {
        assert_eq!(style_at(&buf, i), Some(bold_id));
    }
}

#[test]
fn apply_style_range() {
    let mut buf = make_with_text(b"hello", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    // Apply bold to middle: "hELlo" (positions 1..4)
    apply_style(&mut buf, 1, 4, bold_id);
    assert_eq!(style_at(&buf, 0), Some(0)); // body
    assert_eq!(style_at(&buf, 1), Some(bold_id));
    assert_eq!(style_at(&buf, 2), Some(bold_id));
    assert_eq!(style_at(&buf, 3), Some(bold_id));
    assert_eq!(style_at(&buf, 4), Some(0)); // body
    // Text should be unchanged.
    assert_eq!(read_text(&buf), b"hello");
}

// ── Styled runs ─────────────────────────────────────────────────────────

#[test]
fn styled_runs_single_style() {
    let buf = make_with_text(b"hello world", 4096);
    assert_eq!(styled_run_count(&buf), 1);
    let run = styled_run(&buf, 0).unwrap();
    assert_eq!(run.byte_offset, 0);
    assert_eq!(run.byte_len, 11);
    assert_eq!(run.style_id, 0);
}

#[test]
fn styled_runs_multiple() {
    let mut buf = make_with_text(b"hello world!", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    let italic_id = add_style(&mut buf, &italic_style()).unwrap();
    // "hello" = body, " " = bold, "world!" = italic
    apply_style(&mut buf, 5, 6, bold_id);
    apply_style(&mut buf, 6, 12, italic_id);
    assert_eq!(styled_run_count(&buf), 3);

    let r0 = styled_run(&buf, 0).unwrap();
    assert_eq!(r0.byte_offset, 0);
    assert_eq!(r0.byte_len, 5);
    assert_eq!(r0.style_id, 0);

    let r1 = styled_run(&buf, 1).unwrap();
    assert_eq!(r1.byte_offset, 5);
    assert_eq!(r1.byte_len, 1);
    assert_eq!(r1.style_id, bold_id);

    let r2 = styled_run(&buf, 2).unwrap();
    assert_eq!(r2.byte_offset, 6);
    assert_eq!(r2.byte_len, 6);
    assert_eq!(r2.style_id, italic_id);
}

#[test]
fn styled_runs_coalesce() {
    let mut buf = make_empty(4096);
    // Insert two separate pieces with the same style (by inserting, changing
    // style, inserting, then setting style back).
    // Actually, just insert text with same style at different times — the
    // styled_run iteration should coalesce.
    assert!(insert_bytes(&mut buf, 0, b"hello"));
    assert!(insert_bytes(&mut buf, 5, b" "));
    assert!(insert_bytes(&mut buf, 6, b"world"));
    // All same style → should coalesce to 1 run.
    assert_eq!(styled_run_count(&buf), 1);
}

// ── Copy run text ───────────────────────────────────────────────────────

#[test]
fn copy_run_text_simple() {
    let buf = make_with_text(b"hello world", 4096);
    let run = styled_run(&buf, 0).unwrap();
    let mut out = [0u8; 32];
    let n = copy_run_text(&buf, &run, &mut out);
    assert_eq!(&out[..n], b"hello world");
}

#[test]
fn copy_run_text_cross_buffer() {
    // Create text in original buffer, then insert in the middle.
    // The resulting run should span original and add buffers.
    let mut buf = make_with_text(b"hd", 4096);
    assert!(insert_bytes(&mut buf, 1, b"ello worl"));
    // Full text: "hello world" — but the 'h' is from original, "ello worl" from add, 'd' from original.
    // All same style so 1 coalesced run.
    assert_eq!(styled_run_count(&buf), 1);
    let run = styled_run(&buf, 0).unwrap();
    let mut out = [0u8; 32];
    let n = copy_run_text(&buf, &run, &mut out);
    assert_eq!(&out[..n], b"hello world");
}

// ── Round-trip serialization ────────────────────────────────────────────

#[test]
fn round_trip_serialization() {
    let mut buf = make_with_text(b"hello world", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    apply_style(&mut buf, 0, 5, bold_id);

    // The buffer IS the serialized form. Validate it.
    assert!(validate(&buf));

    // Read it back as a fresh reference.
    let h = header(&buf);
    assert_eq!(h.magic, MAGIC);
    assert_eq!(h.version, VERSION);
    assert_eq!(text_len(&buf), 11);
    assert_eq!(read_text(&buf), b"hello world");
    assert_eq!(style_at(&buf, 0), Some(bold_id));
    assert_eq!(style_at(&buf, 5), Some(0));
}

// ── Invalid magic rejected ──────────────────────────────────────────────

#[test]
fn invalid_magic_rejected() {
    let mut buf = make_empty(4096);
    // Corrupt the magic.
    buf[0] = 0xFF;
    assert!(!validate(&buf));
}

// ── Capacity limits ─────────────────────────────────────────────────────

#[test]
fn capacity_limits_add_buffer() {
    let mut buf = make_empty(HEADER_SIZE + MAX_ADD_BUFFER + MAX_PIECES * 16 + 1024);
    // Fill the add buffer.
    let big = vec![b'x'; MAX_ADD_BUFFER];
    assert!(insert_bytes(&mut buf, 0, &big));
    // One more byte should fail.
    assert!(!insert(&mut buf, 0, b'y'));
}

#[test]
fn capacity_limits_pieces() {
    // Create a buffer big enough for many pieces but insert in the middle
    // repeatedly to force splits until we hit MAX_PIECES.
    let mut buf = make_with_text(b"ab", 1024 * 1024);

    // Each insert in the middle of a piece creates 2 new pieces (split + insert),
    // net +2. We start with 1 piece. After N middle inserts: 1 + 2*N pieces.
    // MAX_PIECES = 512, so we need (512-1)/2 = 255 middle inserts to reach 511,
    // then one more should fail or reach exactly 512.
    let mut success_count = 0;
    for _ in 0..300 {
        let tl = text_len(&buf);
        if tl < 2 {
            break;
        }
        if insert(&mut buf, 1, b'x') {
            success_count += 1;
        } else {
            break;
        }
    }
    assert!(success_count > 0);
    assert!(header(&buf).piece_count as usize <= MAX_PIECES);
}

// ── Style palette ───────────────────────────────────────────────────────

#[test]
fn style_palette() {
    let mut buf = make_empty(4096);
    assert!(add_default_styles(&mut buf));
    assert_eq!(style_count(&buf), 7);

    // Verify body style.
    let s = style(&buf, 0).unwrap();
    assert_eq!(s.font_family, FONT_SANS);
    assert_eq!(s.font_size_pt, 14);
    assert_eq!(s.weight, 400);
    assert_eq!(s.role, ROLE_BODY);

    // Verify heading 1.
    let s = style(&buf, 1).unwrap();
    assert_eq!(s.font_size_pt, 24);
    assert_eq!(s.weight, 700);
    assert_eq!(s.role, ROLE_HEADING1);

    // Verify code.
    let s = style(&buf, 6).unwrap();
    assert_eq!(s.font_family, FONT_MONO);
    assert_eq!(s.font_size_pt, 13);
    assert_eq!(s.role, ROLE_CODE);
    assert_eq!(s.color, [0x66, 0x66, 0x66, 255]);
}

#[test]
fn style_palette_full() {
    let mut buf = make_empty(4096);
    for _ in 0..MAX_STYLES {
        assert!(add_style(&mut buf, &default_body_style()).is_some());
    }
    // 33rd should fail.
    assert!(add_style(&mut buf, &default_body_style()).is_none());
}

// ── Current style ───────────────────────────────────────────────────────

#[test]
fn current_style_insert() {
    let mut buf = make_empty(4096);
    add_style(&mut buf, &default_body_style()).unwrap();
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();

    // Insert with body style.
    assert!(insert_bytes(&mut buf, 0, b"hello "));
    // Switch to bold.
    set_current_style(&mut buf, bold_id);
    let pos = text_len(&buf);
    assert!(insert_bytes(&mut buf, pos, b"world"));

    assert_eq!(style_at(&buf, 0), Some(0)); // body
    assert_eq!(style_at(&buf, 5), Some(0)); // body (the space)
    assert_eq!(style_at(&buf, 6), Some(bold_id));
    assert_eq!(style_at(&buf, 10), Some(bold_id));
}

// ── Operation ID tracking ───────────────────────────────────────────────

#[test]
fn operation_id_tracking() {
    let mut buf = make_empty(4096);
    assert_eq!(header(&buf).operation_id, 0);

    let op1 = next_operation(&mut buf);
    assert_eq!(op1, 1);

    let op2 = next_operation(&mut buf);
    assert_eq!(op2, 2);

    assert_eq!(header(&buf).operation_id, 2);
}

// ── UTF-8 multibyte ─────────────────────────────────────────────────────

#[test]
fn utf8_multibyte() {
    let mut buf = make_empty(4096);
    // '€' is 3 bytes in UTF-8: 0xE2 0x82 0xAC
    let euro = "€".as_bytes();
    assert_eq!(euro.len(), 3);
    assert!(insert_bytes(&mut buf, 0, euro));
    assert_eq!(text_len(&buf), 3);
    assert_eq!(byte_at(&buf, 0), Some(0xE2));
    assert_eq!(byte_at(&buf, 1), Some(0x82));
    assert_eq!(byte_at(&buf, 2), Some(0xAC));
}

// ── Cursor position ─────────────────────────────────────────────────────

#[test]
fn cursor_position() {
    let mut buf = make_with_text(b"hello", 4096);
    assert_eq!(cursor_pos(&buf), 0);
    set_cursor_pos(&mut buf, 3);
    assert_eq!(cursor_pos(&buf), 3);
}

// ── Validate rejects truncated buffer ───────────────────────────────────

#[test]
fn validate_truncated() {
    let buf = make_with_text(b"hello world", 4096);
    // Truncate to just the header.
    assert!(!validate(&buf[..HEADER_SIZE]));
}

// ── Multiple inserts and deletes ────────────────────────────────────────

#[test]
fn insert_delete_sequence() {
    let mut buf = make_empty(4096);
    assert!(insert_bytes(&mut buf, 0, b"hello"));
    assert!(insert_bytes(&mut buf, 5, b" world"));
    assert_eq!(read_text(&buf), b"hello world");

    // Delete " world" (positions 5..11)
    assert!(delete_range(&mut buf, 5, 11));
    assert_eq!(read_text(&buf), b"hello");

    // Insert at beginning
    assert!(insert_bytes(&mut buf, 0, b"say "));
    assert_eq!(read_text(&buf), b"say hello");
}

// ── text_slice partial ──────────────────────────────────────────────────

#[test]
fn text_slice_partial() {
    let buf = make_with_text(b"hello world", 4096);
    let mut out = [0u8; 5];
    let n = text_slice(&buf, 6, 11, &mut out);
    assert_eq!(n, 5);
    assert_eq!(&out, b"world");
}

// ── byte_at out of range ────────────────────────────────────────────────

#[test]
fn byte_at_out_of_range() {
    let buf = make_with_text(b"hi", 4096);
    assert_eq!(byte_at(&buf, 2), None);
    assert_eq!(byte_at(&buf, 100), None);
}

// ── style_at out of range ───────────────────────────────────────────────

#[test]
fn style_at_out_of_range() {
    let buf = make_with_text(b"hi", 4096);
    assert_eq!(style_at(&buf, 2), None);
}

// ── styled_run out of range ─────────────────────────────────────────────

#[test]
fn styled_run_out_of_range() {
    let buf = make_with_text(b"hi", 4096);
    assert_eq!(styled_run(&buf, 1), None);
    assert_eq!(styled_run(&buf, 100), None);
}

// ── Empty table has 0 styled runs ───────────────────────────────────────

#[test]
fn empty_styled_runs() {
    let buf = make_empty(4096);
    assert_eq!(styled_run_count(&buf), 0);
    assert_eq!(styled_run(&buf, 0), None);
}

// ── Delete entire content ───────────────────────────────────────────────

#[test]
fn delete_all_content() {
    let mut buf = make_with_text(b"hello", 4096);
    assert!(delete_range(&mut buf, 0, 5));
    assert_eq!(text_len(&buf), 0);
    assert_eq!(read_text(&buf), b"");
    assert!(validate(&buf));
}

// ── Style after insert preserves text ───────────────────────────────────

#[test]
fn style_preserves_text_after_insert() {
    let mut buf = make_empty(4096);
    add_style(&mut buf, &default_body_style()).unwrap();
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();

    assert!(insert_bytes(&mut buf, 0, b"hello world"));
    apply_style(&mut buf, 0, 5, bold_id);

    // Verify text is intact after styling.
    assert_eq!(read_text(&buf), b"hello world");
    assert!(validate(&buf));
}

// ── Large multi-byte text ───────────────────────────────────────────────

#[test]
fn large_utf8_text() {
    let mut buf = make_empty(65536);
    let text = "こんにちは世界"; // 21 bytes of UTF-8
    assert!(insert_bytes(&mut buf, 0, text.as_bytes()));
    assert_eq!(text_len(&buf), 21);
    assert_eq!(read_text(&buf), text.as_bytes());
}
