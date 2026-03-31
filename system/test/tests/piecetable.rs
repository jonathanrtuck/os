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

// ── Capacity limits (buffer-derived) ────────────────────────────────────

#[test]
fn capacity_add_buffer_fills_to_buf_len() {
    // With a 4096-byte buffer, after header (64) + 1 piece (16), the rest
    // is available for add buffer data. Fill it until the buffer is full.
    let buf_size = 4096;
    let mut buf = make_empty(buf_size);
    // Available = buf_size - HEADER_SIZE - piece overhead per insert.
    // First insert creates 1 piece (16 bytes), so available for text is
    // buf_size - 64 - 16 = 4016.
    let available = buf_size - HEADER_SIZE - 16; // 1 piece slot
    let big = vec![b'x'; available];
    assert!(insert_bytes(&mut buf, 0, &big));
    assert_eq!(text_len(&buf), available as u32);
    // One more byte should fail — buffer is full.
    assert!(!insert(&mut buf, 0, b'y'));
}

#[test]
fn capacity_bigger_buffer_allows_more_data() {
    // A 1024-byte buffer holds less text than a 4096-byte buffer.
    let small = 1024;
    let large = 4096;
    let mut buf_small = make_empty(small);
    let mut buf_large = make_empty(large);

    let text = vec![b'a'; small - HEADER_SIZE - 16];
    assert!(insert_bytes(&mut buf_small, 0, &text));
    assert!(insert_bytes(&mut buf_large, 0, &text));

    // Small buffer is now full — can't insert more.
    assert!(!insert(&mut buf_small, 0, b'z'));
    // Large buffer has room.
    assert!(insert(&mut buf_large, 0, b'z'));
}

#[test]
fn capacity_pieces_limited_by_buffer_size() {
    // Insert in the middle repeatedly to force splits until the buffer
    // can't fit any more pieces. The limit is buffer-derived, not a constant.
    let mut buf = make_with_text(b"ab", 1024 * 1024);

    let mut success_count = 0;
    for _ in 0..50000 {
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
    // With a 1 MiB buffer, we should be able to fit far more than 512 pieces.
    // Each mid-piece insert adds 2 pieces (net), so success_count > 255.
    assert!(success_count > 255, "got {} inserts, expected > 255", success_count);
    // Piece count fits in u16.
    assert!(header(&buf).piece_count <= u16::MAX);
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
fn style_palette_limited_by_buffer_size() {
    // Style count is u8 (max 255), but the buffer may fill before that.
    // In a 4096-byte buffer, each style is 12 bytes. With header (64),
    // we can fit (4096 - 64) / 12 = 336 styles — but u8 caps at 255.
    let mut buf = make_empty(4096);
    let mut count = 0u16;
    for _ in 0..256 {
        if add_style(&mut buf, &default_body_style()).is_some() {
            count += 1;
        } else {
            break;
        }
    }
    // With 4096 bytes and no pieces/data, should hit u8 ceiling (255).
    assert_eq!(count, 255);
    // 256th must fail.
    assert!(add_style(&mut buf, &default_body_style()).is_none());
}

#[test]
fn style_palette_limited_by_small_buffer() {
    // In a tiny buffer, the buffer fills before the u8 ceiling.
    // header (64) + styles must fit. 128 bytes → room for (128-64)/12 = 5 styles.
    let mut buf = make_empty(128);
    let mut count = 0u16;
    for _ in 0..256 {
        if add_style(&mut buf, &default_body_style()).is_some() {
            count += 1;
        } else {
            break;
        }
    }
    assert_eq!(count, 5);
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

// ── text_slice: regression for start > 0 (length-vs-end bug) ───────────
//
// Bug: `doc_text_for_range` in presenter passed `needed` (length) instead
// of `end` to `piecetable::text_slice`. For any range not starting at byte 0,
// start >= "end" (which was actually length), so text_slice returned 0 bytes.

#[test]
fn text_slice_from_zero_returns_correct_bytes() {
    // This case worked even with the bug (start=0, end=8, so start < end).
    let buf = make_with_text(b"abcdefghij", 4096);
    let mut out = [0u8; 8];
    let n = text_slice(&buf, 0, 8, &mut out);
    assert_eq!(n, 8);
    assert_eq!(&out[..8], b"abcdefgh");
}

#[test]
fn text_slice_nonzero_start_returns_correct_bytes() {
    // This case returned 0 with the bug: start=8, "end"=2 (length),
    // so start(8) >= end(2) → early return 0.
    let buf = make_with_text(b"abcdefghij", 4096);
    let mut out = [0u8; 2];
    let n = text_slice(&buf, 8, 10, &mut out);
    assert_eq!(n, 2);
    assert_eq!(&out[..2], b"ij");
}

#[test]
fn text_slice_spans_piece_boundary() {
    // Build a buffer with multiple pieces by inserting in the middle.
    // "abcde" (original) → insert "XXXXX" at position 2 → "abXXXXXcde"
    let mut buf = make_with_text(b"abcde", 4096);
    assert!(insert_bytes(&mut buf, 2, b"XXXXX"));
    assert_eq!(read_text(&buf), b"abXXXXXcde");

    // Slice [5, 15) would span the inserted piece and original piece.
    // text is "abXXXXXcde" (10 bytes), so [5, 10) = "Xcde\0"... no, let's be precise.
    // Positions: a(0) b(1) X(2) X(3) X(4) X(5) X(6) c(7) d(8) e(9)
    // Slice [5, 10) = "XXcde" — crosses from the inserted "XXXXX" piece into original "cde".
    let mut out = [0u8; 10];
    let n = text_slice(&buf, 5, 10, &mut out);
    assert_eq!(n, 5);
    assert_eq!(&out[..5], b"XXcde");
}

#[test]
fn text_slice_mid_piece_nonzero_start() {
    // Another case that fails with the length-vs-end bug: start in the middle
    // of the text, not at a piece boundary.
    let buf = make_with_text(b"hello world!", 4096);
    let mut out = [0u8; 5];
    let n = text_slice(&buf, 6, 11, &mut out);
    assert_eq!(n, 5);
    assert_eq!(&out[..5], b"world");
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

// ── find_style_by_role ──────────────────────────────────────────────────

#[test]
fn find_style_by_role_finds_bold() {
    let mut buf = make_with_text(b"hello", 4096);
    // Add bold style (ROLE_STRONG) at index 1.
    let bold = bold_style();
    let id = add_style(&mut buf, &bold);
    assert!(id.is_some());
    let found = find_style_by_role(&buf, ROLE_STRONG);
    assert_eq!(found, id);
    let style = piecetable::style(&buf, found.unwrap()).unwrap();
    assert_eq!(style.weight, 700);
}

#[test]
fn find_style_by_role_returns_none_for_missing() {
    let buf = make_with_text(b"hello", 4096);
    // Default palette only has body (ROLE_BODY=0). ROLE_STRONG should not be found.
    assert!(find_style_by_role(&buf, ROLE_STRONG).is_none());
}

#[test]
fn find_style_by_role_finds_all_defaults() {
    let mut buf = make_with_text(b"hello", 4096);
    // Add all default styles.
    assert!(add_style(&mut buf, &heading1_style()).is_some());
    assert!(add_style(&mut buf, &heading2_style()).is_some());
    assert!(add_style(&mut buf, &bold_style()).is_some());
    assert!(add_style(&mut buf, &italic_style()).is_some());
    assert!(add_style(&mut buf, &bold_italic_style()).is_some());
    assert!(add_style(&mut buf, &code_style()).is_some());

    assert!(find_style_by_role(&buf, ROLE_HEADING1).is_some());
    assert!(find_style_by_role(&buf, ROLE_HEADING2).is_some());
    assert!(find_style_by_role(&buf, ROLE_STRONG).is_some());
    assert!(find_style_by_role(&buf, ROLE_EMPHASIS).is_some());
}

// ── Compaction ──────────────────────────────────────────────────────────

#[test]
fn compact_merges_adjacent_same_style_pieces() {
    // Insert text with alternating styles to create multiple pieces,
    // then restyle everything to the same style. Pieces become mergeable.
    let mut buf = make_empty(4096);
    add_style(&mut buf, &default_body_style()).unwrap();
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();

    // Insert "hello" as body, " " as bold, "world" as body.
    assert!(insert_bytes(&mut buf, 0, b"hello"));
    set_current_style(&mut buf, bold_id);
    assert!(insert_bytes(&mut buf, 5, b" "));
    set_current_style(&mut buf, 0);
    assert!(insert_bytes(&mut buf, 6, b"world"));

    // 3 pieces (at minimum — possibly more from coalescing behavior).
    let pc_before = header(&buf).piece_count;
    assert!(pc_before >= 3);

    // Restyle everything to body.
    apply_style(&mut buf, 0, 11, 0);

    // All pieces now have the same style and are in the add buffer sequentially.
    // Compact should merge them.
    let removed = compact(&mut buf);
    assert!(removed > 0, "compact should merge pieces");
    assert_eq!(header(&buf).piece_count, 1, "all pieces merged into one");
    assert_eq!(read_text(&buf), b"hello world");
    assert!(validate(&buf));
}

#[test]
fn compact_preserves_different_styles() {
    let mut buf = make_with_text(b"hello world", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    apply_style(&mut buf, 0, 5, bold_id);

    let pc_before = header(&buf).piece_count;
    let removed = compact(&mut buf);
    // Different styles can't merge — piece count should not decrease
    // (unless adjacent same-style pieces existed from the split).
    assert_eq!(removed, 0, "no mergeable pieces");
    assert_eq!(header(&buf).piece_count, pc_before);
    assert_eq!(read_text(&buf), b"hello world");
    assert!(validate(&buf));
}

#[test]
fn compact_no_op_on_single_piece() {
    let buf = make_with_text(b"hello", 4096);
    let mut buf = buf;
    let removed = compact(&mut buf);
    assert_eq!(removed, 0);
    assert_eq!(header(&buf).piece_count, 1);
}

#[test]
fn compact_reclaims_space_for_more_inserts() {
    // Create a document, apply styles to fragment it, then restyle back.
    // Compact should merge the pieces, freeing slots for more operations.
    let buf_size = 2048;
    let mut buf = make_empty(buf_size);
    add_style(&mut buf, &default_body_style()).unwrap();
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();

    // Insert a block of text (coalesces into 1 piece).
    let text = b"abcdefghijklmnopqrstuvwxyz";
    assert!(insert_bytes(&mut buf, 0, text));
    assert_eq!(header(&buf).piece_count, 1);

    // Apply bold to every other character — creates many pieces.
    for i in (0..26).step_by(2) {
        apply_style(&mut buf, i, i + 1, bold_id);
    }
    let pc_before = header(&buf).piece_count;
    assert!(pc_before > 10, "should have many pieces after styling");

    // Restyle everything back to body.
    apply_style(&mut buf, 0, 26, 0);

    // Now all pieces have the same style. Adjacent same-source pieces
    // with contiguous offsets will merge.
    let text_before = read_text(&buf);
    let removed = compact(&mut buf);
    assert!(removed > 0, "should have merged pieces");
    assert!(header(&buf).piece_count < pc_before);

    // Text is preserved.
    assert_eq!(read_text(&buf), text_before);
    assert!(validate(&buf));
}

// ── In-place delete: correctness across piece boundaries ────────────────

#[test]
fn delete_spanning_multiple_pieces() {
    // Create text with 3 different styles → 3+ pieces.
    let mut buf = make_empty(4096);
    add_style(&mut buf, &default_body_style()).unwrap();
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    let ital_id = add_style(&mut buf, &italic_style()).unwrap();

    assert!(insert_bytes(&mut buf, 0, b"AAABBBCCC"));
    apply_style(&mut buf, 3, 6, bold_id);
    apply_style(&mut buf, 6, 9, ital_id);

    // Delete "BBBC" (positions 3..7) — spans bold and italic pieces.
    assert!(delete_range(&mut buf, 3, 7));
    assert_eq!(read_text(&buf), b"AAACC");
    assert_eq!(style_at(&buf, 0), Some(0)); // A = body
    assert_eq!(style_at(&buf, 3), Some(ital_id)); // C = italic
    assert!(validate(&buf));
}

#[test]
fn delete_entire_middle_piece() {
    let mut buf = make_with_text(b"hello beautiful world", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    apply_style(&mut buf, 6, 15, bold_id); // "beautiful" bold

    // Delete " beautiful" (5..15).
    assert!(delete_range(&mut buf, 5, 15));
    assert_eq!(read_text(&buf), b"hello world");
    assert!(validate(&buf));
}

#[test]
fn delete_punches_hole_in_single_piece() {
    // Single piece: delete a range in the middle, splitting it.
    let mut buf = make_with_text(b"abcdefgh", 4096);
    assert!(delete_range(&mut buf, 3, 5)); // Remove "de"
    assert_eq!(read_text(&buf), b"abcfgh");
    assert!(validate(&buf));
}

// ── In-place apply_style: correctness ───────────────────────────────────

#[test]
fn apply_style_1_to_3_split() {
    // Single piece, style applied to the middle → 3 pieces.
    let mut buf = make_with_text(b"abcdefgh", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    apply_style(&mut buf, 2, 6, bold_id);

    assert_eq!(style_at(&buf, 0), Some(0));       // 'a' body
    assert_eq!(style_at(&buf, 1), Some(0));       // 'b' body
    assert_eq!(style_at(&buf, 2), Some(bold_id)); // 'c' bold
    assert_eq!(style_at(&buf, 5), Some(bold_id)); // 'f' bold
    assert_eq!(style_at(&buf, 6), Some(0));       // 'g' body
    assert_eq!(style_at(&buf, 7), Some(0));       // 'h' body
    assert_eq!(read_text(&buf), b"abcdefgh");
    assert!(validate(&buf));
}

#[test]
fn apply_style_multiple_splits() {
    // Three pieces with different styles, apply a fourth style across all.
    let mut buf = make_empty(4096);
    add_style(&mut buf, &default_body_style()).unwrap();
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();
    let ital_id = add_style(&mut buf, &italic_style()).unwrap();
    let code_id = add_style(&mut buf, &code_style()).unwrap();

    assert!(insert_bytes(&mut buf, 0, b"AAABBBCCC"));
    apply_style(&mut buf, 0, 3, 0);      // body
    apply_style(&mut buf, 3, 6, bold_id); // bold
    apply_style(&mut buf, 6, 9, ital_id); // italic

    // Apply code style to "ABBBCC" (1..8) — crosses all three pieces.
    apply_style(&mut buf, 1, 8, code_id);

    assert_eq!(style_at(&buf, 0), Some(0));        // 'A' still body
    assert_eq!(style_at(&buf, 1), Some(code_id));  // 'A' code
    assert_eq!(style_at(&buf, 4), Some(code_id));  // 'B' code
    assert_eq!(style_at(&buf, 7), Some(code_id));  // 'C' code
    assert_eq!(style_at(&buf, 8), Some(ital_id));  // 'C' still italic
    assert_eq!(read_text(&buf), b"AAABBBCCC");
    assert!(validate(&buf));
}

#[test]
fn apply_style_then_delete_then_compact() {
    // Exercise the full pipeline: style, delete, compact.
    let mut buf = make_with_text(b"the quick brown fox", 4096);
    let bold_id = add_style(&mut buf, &bold_style()).unwrap();

    // Bold "quick" (4..9).
    apply_style(&mut buf, 4, 9, bold_id);
    assert_eq!(read_text(&buf), b"the quick brown fox");

    // Delete "brown " (10..16).
    assert!(delete_range(&mut buf, 10, 16));
    assert_eq!(read_text(&buf), b"the quick fox");

    // Compact.
    compact(&mut buf);
    assert_eq!(read_text(&buf), b"the quick fox");
    assert!(validate(&buf));

    // Style check survives.
    assert_eq!(style_at(&buf, 4), Some(bold_id));
    assert_eq!(style_at(&buf, 10), Some(0)); // "fox" = body
}

// ── Large piece count (beyond old MAX_PIECES=512) ───────────────────────

#[test]
fn large_piece_count_beyond_old_limit() {
    // With a large buffer, we should be able to hold >512 pieces.
    // Use a 256 KiB buffer. Insert at alternating positions to force splits
    // (not boundary inserts, which only add 1 piece).
    let buf_size = 256 * 1024;
    let mut buf = make_with_text(b"ab", buf_size);

    // First insert at position 1 splits the single piece: net +2 → 3 pieces.
    // Subsequent inserts at position 1 land at a piece boundary: net +1 each.
    // To force splits, insert at varying mid-piece positions.
    let mut success = 0;
    for i in 0..600 {
        // Insert at position 1 — always inside or at the boundary of the
        // first piece, depending on coalescing.
        let tl = text_len(&buf);
        // Alternate insert positions to prevent coalescing: 1, then tl/2.
        let pos = if i % 2 == 0 { 1 } else { tl / 2 };
        if insert(&mut buf, pos, b'x') {
            success += 1;
        } else {
            break;
        }
    }
    assert!(success == 600, "all 600 inserts should succeed with 256K buffer, got {}", success);
    // With alternating positions and splits, piece count grows well past 512.
    let pc = header(&buf).piece_count;
    assert!(pc > 512, "piece count {} should exceed old MAX_PIECES=512", pc);
    assert!(validate(&buf));
}
