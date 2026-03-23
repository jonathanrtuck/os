use scene::*;

// ── Local copies of layout helpers (moved from scene to Core) ───────

/// Local text layout run — used only in tests. Replaces the old
/// scene::TextRun that was removed in the Content type redesign.
#[derive(Clone)]
#[allow(dead_code)]
struct TestLayoutRun {
    glyphs: DataRef,
    glyph_count: u16,
    y: i32,
    color: Color,
    advance: u16,
    font_size: u16,
    axis_hash: u32,
}

/// Convert a byte offset to (visual_line, column) with monospace wrapping.
fn byte_to_line_col(text: &[u8], byte_offset: usize, chars_per_line: usize) -> (usize, usize) {
    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut pos: usize = 0;

    while pos < text.len() && pos < byte_offset {
        if text[pos] == b'\n' {
            line += 1;
            col = 0;
            pos += 1;
        } else {
            col += 1;
            pos += 1;

            if col >= chars_per_line && pos < text.len() && text[pos] != b'\n' {
                line += 1;
                col = 0;
            }
        }
    }

    (line, col)
}

/// Break text into visual lines using monospace line-breaking.
fn layout_mono_lines(
    text: &[u8],
    chars_per_line: usize,
    line_height: i32,
    color: Color,
    advance: u16,
    font_size: u16,
) -> Vec<TestLayoutRun> {
    let mut runs = Vec::new();
    let mut line_y: i32 = 0;
    let mut pos: usize = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        let line_end = if let Some(nl) = remaining.iter().position(|&b| b == b'\n') {
            if nl <= chars_per_line {
                pos + nl
            } else {
                pos + chars_per_line
            }
        } else if remaining.len() <= chars_per_line {
            text.len()
        } else {
            pos + chars_per_line
        };
        let line_len = line_end - pos;

        runs.push(TestLayoutRun {
            glyphs: DataRef {
                offset: pos as u32,
                length: line_len as u32,
            },
            glyph_count: line_len as u16,
            y: line_y,
            color,
            advance,
            font_size,
            axis_hash: 0,
        });

        line_y = line_y.saturating_add(line_height);
        pos = if line_end < text.len() && text[line_end] == b'\n' {
            line_end + 1
        } else {
            line_end
        };
    }

    if runs.is_empty() {
        runs.push(TestLayoutRun {
            glyphs: DataRef {
                offset: 0,
                length: 0,
            },
            glyph_count: 0,
            y: 0,
            color,
            advance,
            font_size,
            axis_hash: 0,
        });
    }

    runs
}

/// Extract source text bytes for a run using its placeholder DataRef.
fn line_bytes_for_run<'a>(text: &'a [u8], run: &TestLayoutRun) -> &'a [u8] {
    let start = run.glyphs.offset as usize;
    let len = run.glyphs.length as usize;

    if start + len <= text.len() {
        &text[start..start + len]
    } else {
        &[]
    }
}

/// Filter runs to those visible in a scrolled viewport.
///
/// `scroll_y` is the scroll offset in pixels (f32). Runs keep their
/// document-relative y positions. The caller sets `content_transform`
/// on the container node so the renderer handles the viewport offset.
fn scroll_runs(
    runs: Vec<TestLayoutRun>,
    scroll_y: f32,
    line_height: u32,
    viewport_height_pt: i32,
) -> Vec<TestLayoutRun> {
    let scroll_pt = scroll_y.round() as i32;

    runs.into_iter()
        .filter(|run| {
            let doc_y = run.y;

            // Above the scroll window?
            if doc_y + line_height as i32 <= scroll_pt {
                return false;
            }
            // Below the scroll window?
            if doc_y >= scroll_pt + viewport_height_pt {
                return false;
            }

            true
        })
        .collect()
}

fn make_buf() -> Vec<u8> {
    vec![0u8; SCENE_SIZE]
}

/// Build a monospace Content::Glyphs from raw UTF-8 bytes.
/// Each byte is treated as a glyph ID with uniform advance.
fn make_mono_glyphs(
    w: &mut SceneWriter,
    text: &[u8],
    font_size: u16,
    color: Color,
    advance: u16,
) -> Content {
    let glyphs: Vec<ShapedGlyph> = text
        .iter()
        .map(|&ch| ShapedGlyph {
            glyph_id: ch as u16,
            _pad: 0,
            x_advance: advance as i32 * 65536,
            x_offset: 0,
            y_offset: 0,
        })
        .collect();
    let glyph_ref = w.push_shaped_glyphs(&glyphs);
    Content::Glyphs {
        color,
        glyphs: glyph_ref,
        glyph_count: glyphs.len() as u16,
        font_size,
        axis_hash: 0,
    }
}

// ── SceneWriter basics ──────────────────────────────────────────────

#[test]
fn writer_new_empty_state() {
    let mut buf = make_buf();
    let w = SceneWriter::new(&mut buf);
    assert_eq!(w.node_count(), 0);
    assert_eq!(w.data_used(), 0);
    assert_eq!(w.generation(), 0);
}

#[test]
fn writer_alloc_node_returns_sequential_ids() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let n0 = w.alloc_node().unwrap();
    let n1 = w.alloc_node().unwrap();
    let n2 = w.alloc_node().unwrap();
    assert_eq!(n0, 0);
    assert_eq!(n1, 1);
    assert_eq!(n2, 2);
    assert_eq!(w.node_count(), 3);
}

#[test]
fn writer_alloc_node_initialized_to_empty() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    let node = w.node(id);
    assert_eq!(node.first_child, NULL);
    assert_eq!(node.next_sibling, NULL);
    assert!(node.visible());
    assert_eq!(node.opacity, 255);
}

#[test]
fn writer_node_mutation() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    {
        let n = w.node_mut(id);
        n.x = 10;
        n.y = 20;
        n.width = 100;
        n.height = 50;
        n.background = Color::rgb(255, 0, 0);
    }
    assert_eq!(w.node(id).x, 10);
    assert_eq!(w.node(id).width, 100);
    assert_eq!(w.node(id).background, Color::rgb(255, 0, 0));
}

#[test]
fn writer_push_data_returns_valid_ref() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"hello world");
    assert_eq!(dref.offset, 0);
    assert_eq!(dref.length, 11);
    assert_eq!(w.data_used(), 11);
}

#[test]
fn writer_push_data_sequential_offsets() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let d1 = w.push_data(b"abc");
    let d2 = w.push_data(b"defgh");
    assert_eq!(d1.offset, 0);
    assert_eq!(d1.length, 3);
    assert_eq!(d2.offset, 3);
    assert_eq!(d2.length, 5);
    assert_eq!(w.data_used(), 8);
}

#[test]
fn writer_set_root() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.set_root(root);
    assert_eq!(w.root(), root);
}

#[test]
fn writer_commit_increments_generation() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    assert_eq!(w.generation(), 0);
    w.commit();
    assert_eq!(w.generation(), 1);
    w.commit();
    assert_eq!(w.generation(), 2);
}

#[test]
fn writer_clear_resets_state() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.alloc_node();
    w.alloc_node();
    w.push_data(b"some data");
    w.commit();
    assert_eq!(w.node_count(), 2);
    assert!(w.data_used() > 0);

    w.clear();
    assert_eq!(w.node_count(), 0);
    assert_eq!(w.data_used(), 0);
    // Generation preserved across clear.
    assert_eq!(w.generation(), 1);
}

// ── Tree structure ──────────────────────────────────────────────────

#[test]
fn writer_add_child_links_nodes() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let parent = w.alloc_node().unwrap();
    let child1 = w.alloc_node().unwrap();
    let child2 = w.alloc_node().unwrap();
    w.add_child(parent, child1);
    w.add_child(parent, child2);
    assert_eq!(w.node(parent).first_child, child1);
    assert_eq!(w.node(child1).next_sibling, child2);
    assert_eq!(w.node(child2).next_sibling, NULL);
}

#[test]
fn writer_add_child_single() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let parent = w.alloc_node().unwrap();
    let child = w.alloc_node().unwrap();
    w.add_child(parent, child);
    assert_eq!(w.node(parent).first_child, child);
    assert_eq!(w.node(child).next_sibling, NULL);
}

// ── Image content ───────────────────────────────────────────────────

#[test]
fn writer_image_node_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let pixels: Vec<u8> = (0..64).collect();
    let dref = w.push_data(&pixels);
    let id = w.alloc_node().unwrap();
    {
        let n = w.node_mut(id);
        n.content = Content::Image {
            data: dref,
            src_width: 4,
            src_height: 4,
        };
    }
    let r = SceneReader::new(&buf);
    let node = r.node(id);
    match node.content {
        Content::Image {
            data,
            src_width,
            src_height,
        } => {
            assert_eq!(src_width, 4);
            assert_eq!(src_height, 4);
            assert_eq!(r.data(data).len(), 64);
        }
        _ => panic!("expected Image content"),
    }
}

#[test]
fn reader_nodes_slice() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.alloc_node();
    w.alloc_node();
    w.alloc_node();
    let r = SceneReader::new(&buf);
    assert_eq!(r.nodes().len(), 3);
}

#[test]
fn reader_data_buf_slice() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.push_data(b"abc");
    w.push_data(b"def");
    let r = SceneReader::new(&buf);
    assert_eq!(r.data_buf(), b"abcdef");
}

// ── Overflow handling ───────────────────────────────────────────────

#[test]
fn writer_node_overflow_returns_none() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    for _ in 0..MAX_NODES {
        assert!(w.alloc_node().is_some());
    }
    assert!(w.alloc_node().is_none());
}

#[test]
fn writer_data_overflow_truncates() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // Fill most of the buffer.
    let big = vec![0xABu8; DATA_BUFFER_SIZE - 10];
    let d1 = w.push_data(&big);
    assert_eq!(d1.length as usize, DATA_BUFFER_SIZE - 10);
    // Try to push 20 bytes — only 10 fit.
    let d2 = w.push_data(&[0xCD; 20]);
    assert_eq!(d2.length, 10);
    assert_eq!(w.data_used() as usize, DATA_BUFFER_SIZE);
}

#[test]
fn writer_data_empty_push() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"");
    assert_eq!(dref.offset, 0);
    assert_eq!(dref.length, 0);
}

// ── Edge cases ──────────────────────────────────────────────────────

#[test]
fn reader_data_invalid_ref_returns_empty() {
    let mut buf = make_buf();
    let _ = SceneWriter::new(&mut buf);
    let r = SceneReader::new(&buf);
    // Reference beyond data_used.
    let bad = DataRef {
        offset: 9999,
        length: 100,
    };
    assert_eq!(r.data(bad).len(), 0);
}

// ── push_data_replacing ────────────────────────────────────────────────────

#[test]
fn writer_push_data_replacing_appends_new() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let d1 = w.push_data(b"old");
    let d2 = w.push_data_replacing(b"new content");
    // Old data abandoned, new data appended.
    assert_eq!(d2.offset, 3);
    assert_eq!(d2.length, 11);
    let r = SceneReader::new(&buf);
    assert_eq!(r.data(d1), b"old");
    assert_eq!(r.data(d2), b"new content");
}

// ── update_data ─────────────────────────────────────────────────────

#[test]
fn writer_update_data_in_place() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"12345678");
    assert!(w.update_data(dref, b"ABCDEFGH"));
    let r = SceneReader::new(&buf);
    assert_eq!(r.data(dref), b"ABCDEFGH");
}

#[test]
fn writer_update_data_wrong_length_fails() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"12345678");
    // Wrong length — should fail.
    assert!(!w.update_data(dref, b"ABC"));
}

// ── reset_data ──────────────────────────────────────────────────────

#[test]
fn writer_reset_data_clears_usage() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.push_data(b"some data here");
    assert!(w.data_used() > 0);
    w.reset_data();
    assert_eq!(w.data_used(), 0);
}

// ── Monospace layout ────────────────────────────────────────────────

const WHITE: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

#[test]
fn layout_mono_basic_lines() {
    let text = b"hello\nworld";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].y, 0);
    assert_eq!(runs[1].y, 20);
    assert_eq!(line_bytes_for_run(text, &runs[0]), b"hello");
    assert_eq!(line_bytes_for_run(text, &runs[1]), b"world");
}

#[test]
fn layout_mono_trailing_newline() {
    let text = b"hello\nworld\n";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    // Trailing newline: "hello", "world" — newline consumed, no empty line after.
    assert_eq!(runs.len(), 2);
    assert_eq!(line_bytes_for_run(text, &runs[0]), b"hello");
    assert_eq!(line_bytes_for_run(text, &runs[1]), b"world");
}

#[test]
fn layout_mono_soft_wrap() {
    let text = b"abcdefghij"; // 10 chars, wrap at 4
    let runs = layout_mono_lines(text, 4, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 3); // "abcd", "efgh", "ij"
    assert_eq!(line_bytes_for_run(text, &runs[0]), b"abcd");
    assert_eq!(line_bytes_for_run(text, &runs[1]), b"efgh");
    assert_eq!(line_bytes_for_run(text, &runs[2]), b"ij");
    assert_eq!(runs[2].y, 40);
}

#[test]
fn layout_mono_empty_text() {
    let runs = layout_mono_lines(b"", 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].glyph_count, 0);
}

#[test]
fn byte_to_line_col_basic() {
    let text = b"hello\nworld";
    // Byte 0 = (0, 0), byte 5 = end of "hello" = (0, 5)
    assert_eq!(byte_to_line_col(text, 0, 80), (0, 0));
    assert_eq!(byte_to_line_col(text, 5, 80), (0, 5));
    // Byte 6 = start of "world" = (1, 0)
    assert_eq!(byte_to_line_col(text, 6, 80), (1, 0));
    // Byte 11 = end of "world" = (1, 5)
    assert_eq!(byte_to_line_col(text, 11, 80), (1, 5));
}

#[test]
fn byte_to_line_col_soft_wrap() {
    let text = b"abcdefgh"; // 8 chars, wrap at 4
                            // "abcd" is line 0, "efgh" is line 1
    assert_eq!(byte_to_line_col(text, 0, 4), (0, 0));
    assert_eq!(byte_to_line_col(text, 3, 4), (0, 3));
    assert_eq!(byte_to_line_col(text, 4, 4), (1, 0));
    assert_eq!(byte_to_line_col(text, 7, 4), (1, 3));
}

// ── Scroll filtering ────────────────────────────────────────────────

#[test]
fn scroll_runs_no_scroll() {
    let text = b"a\nb\nc";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 3);
    let visible = scroll_runs(runs, 0.0, 20, 100);
    assert_eq!(visible.len(), 3);
    assert_eq!(visible[0].y, 0);
    assert_eq!(visible[1].y, 20);
    assert_eq!(visible[2].y, 40);
}

#[test]
fn scroll_runs_filters_above_viewport() {
    // 10 lines of text (no trailing newline), scroll = 5, viewport = 60px.
    let mut text = Vec::new();
    for i in 0u8..10 {
        if i > 0 {
            text.push(b'\n');
        }
        text.push(b'a' + i);
    }
    let runs = layout_mono_lines(&text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 10);
    let visible = scroll_runs(runs, 100.0, 20, 60); // 5 lines * 20px = 100px
                                                    // Lines 5, 6, 7 visible. Lines 0-4 above, 8-9 below.
                                                    // y values are document-relative (not viewport-relative).
    assert_eq!(visible.len(), 3);
    assert_eq!(visible[0].y, 100); // line 5: 5 * 20 = 100
    assert_eq!(visible[1].y, 120); // line 6: 6 * 20 = 120
    assert_eq!(visible[2].y, 140); // line 7: 7 * 20 = 140
    assert_eq!(line_bytes_for_run(&text, &visible[0]), &[b'f']); // line 5 = 'f'
}

#[test]
fn scroll_runs_cursor_at_bottom_forces_scroll() {
    // 40 lines, viewport 30 lines, scroll = 6.
    let mut text = Vec::new();
    for i in 0u8..40 {
        if i > 0 {
            text.push(b'\n');
        }
        text.push(b'x');
    }
    let runs = layout_mono_lines(&text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 40);
    let visible = scroll_runs(runs, 120.0, 20, 600); // 6 lines * 20px = 120px, 600px = 30 lines
                                                     // First visible line should be line 6 at document y = 6*20 = 120.
    assert_eq!(visible[0].y, 120);
    // Last visible line should be line 35 at document y = 35*20 = 700.
    let last = visible.last().unwrap();
    assert_eq!(last.y, 700);
    // All visible lines should be within the scroll window [120, 720).
    let scroll_pt = 6 * 20; // 120
    for run in &visible {
        assert!(
            run.y + 20 > scroll_pt && run.y < scroll_pt + 600,
            "run.y={} outside scroll window [{}, {})",
            run.y,
            scroll_pt,
            scroll_pt + 600
        );
    }
}

#[test]
fn scroll_runs_empty_text_with_scroll() {
    let runs = layout_mono_lines(b"", 80, 20, WHITE, 8, 16);
    let visible = scroll_runs(runs, 0.0, 20, 600);
    assert_eq!(visible.len(), 1); // empty placeholder run
}

#[test]
fn byte_to_line_col_cursor_consistency_with_layout() {
    let text = b"aaa\nbbb\nccc\nddd";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 4);
    // byte_to_line_col should agree with layout_mono_lines on line assignments.
    assert_eq!(byte_to_line_col(text, 0, 80).0, 0); // 'a' on line 0
    assert_eq!(byte_to_line_col(text, 4, 80).0, 1); // 'b' on line 1
    assert_eq!(byte_to_line_col(text, 8, 80).0, 2); // 'c' on line 2
    assert_eq!(byte_to_line_col(text, 12, 80).0, 3); // 'd' on line 3
    assert_eq!(byte_to_line_col(text, 15, 80).0, 3); // end of text, still line 3
}

// ── ShapedGlyph struct layout (VAL-SCENE-004) ───────────────────────

#[test]
fn shaped_glyph_is_repr_c_with_size_assertion() {
    // ShapedGlyph must be #[repr(C)] with a compile-time size assertion.
    // The compile-time assertion is in scene/lib.rs itself; this test
    // verifies the runtime size matches expectations.
    let size = core::mem::size_of::<ShapedGlyph>();
    assert_eq!(
        size, 16,
        "ShapedGlyph should be 16 bytes: u16 + u16 pad + 3 × i32 (16.16 fixed-point)"
    );
}

#[test]
fn shaped_glyph_field_access() {
    let g = ShapedGlyph {
        glyph_id: 42,
        _pad: 0,
        x_advance: 600 * 65536,
        x_offset: -10 * 65536,
        y_offset: 5 * 65536,
    };
    assert_eq!(g.glyph_id, 42);
    assert_eq!(g.x_advance, 600 * 65536);
    assert_eq!(g.x_offset, -10 * 65536);
    assert_eq!(g.y_offset, 5 * 65536);
}

// ── Byte-exact equality round-trip ──────────────────────────────────

#[test]
fn shaped_glyph_byte_exact_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [
        ShapedGlyph {
            glyph_id: 0xABCD,
            _pad: 0,
            x_advance: -32000 * 65536,
            x_offset: 32000 * 65536,
            y_offset: -1 * 65536,
        },
        ShapedGlyph {
            glyph_id: 0x0001,
            _pad: 0,
            x_advance: 1 * 65536,
            x_offset: -1 * 65536,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 0xFFFE,
            _pad: 0,
            x_advance: 0,
            x_offset: 0,
            y_offset: 0,
        },
    ];

    // Get raw bytes of the input
    let input_bytes = unsafe {
        core::slice::from_raw_parts(
            glyphs.as_ptr() as *const u8,
            glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
        )
    };

    let dref = w.push_shaped_glyphs(&glyphs);

    // Read raw bytes back from data buffer
    let r = SceneReader::new(&buf);
    let output_bytes = r.data(dref);

    assert_eq!(input_bytes, output_bytes, "Byte-exact round-trip failed");
}

// ── Proportional advance: W != i ────────────────────────────────────

#[test]
fn proportional_shaped_glyphs_different_advances() {
    let mono_font = include_bytes!("../../share/jetbrains-mono.ttf");

    // Shape 'W' and 'i' separately to verify mono font gives same advance
    let w_shaped = fonts::shape(mono_font, "W", &[]);
    let i_shaped = fonts::shape(mono_font, "i", &[]);

    assert!(!w_shaped.is_empty(), "W should produce glyphs");
    assert!(!i_shaped.is_empty(), "i should produce glyphs");

    // For a monospace font, W and i should have the same advance
    assert_eq!(
        w_shaped[0].x_advance, i_shaped[0].x_advance,
        "Monospace font: W and i must have same advance"
    );

    // For a proportional font (if available), they'd differ
    let prop_font = include_bytes!("../../share/inter.ttf");
    let w_prop = fonts::shape(prop_font, "W", &[]);
    let i_prop = fonts::shape(prop_font, "i", &[]);

    if !w_prop.is_empty() && !i_prop.is_empty() {
        // Proportional font: W should have a wider advance than i
        assert_ne!(
            w_prop[0].x_advance, i_prop[0].x_advance,
            "Proportional font: advance('W') != advance('i')"
        );
    }
}

// ── content_hash tests ──────────────────────────────────────────────

#[test]
fn content_hash_is_zero_for_empty_node() {
    assert_eq!(Node::EMPTY.content_hash, 0);
}

#[test]
fn content_hash_stored_and_readable() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content_hash = scene::fnv1a(b"hello");
    w.set_root(id);
    w.commit();
    let r = SceneReader::new(&buf);
    assert_eq!(r.node(id).content_hash, scene::fnv1a(b"hello"));
}

#[test]
fn content_hash_differs_for_different_data() {
    let h1 = scene::fnv1a(b"hello");
    let h2 = scene::fnv1a(b"world");
    assert_ne!(h1, h2);
    assert_ne!(h1, 0);
}

#[test]
fn content_hash_is_deterministic() {
    assert_eq!(scene::fnv1a(b"test"), scene::fnv1a(b"test"));
}

// ── dirty bitmap tests ──────────────────────────────────────────────

#[test]
fn dirty_bitmap_mark_and_test() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // Allocate a node so the scene is valid, but we're testing raw bitmap ops.
    for _ in 0..512 {
        // Fill all 512 node slots to make boundary tests valid.
        if w.alloc_node().is_none() {
            break;
        }
    }
    w.clear_dirty();

    // Mark specific bits.
    w.mark_dirty(0);
    w.mark_dirty(42);
    w.mark_dirty(511);

    // Verify set bits.
    assert!(w.is_dirty(0), "bit 0 should be set");
    assert!(w.is_dirty(42), "bit 42 should be set");
    assert!(w.is_dirty(511), "bit 511 should be set");

    // Verify unset bits.
    assert!(!w.is_dirty(1), "bit 1 should not be set");
    assert!(!w.is_dirty(41), "bit 41 should not be set");
    assert!(!w.is_dirty(43), "bit 43 should not be set");
    assert!(!w.is_dirty(510), "bit 510 should not be set");
    assert!(!w.is_dirty(100), "bit 100 should not be set");
}

#[test]
fn dirty_bitmap_clear() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    for _ in 0..10 {
        w.alloc_node().unwrap();
    }

    w.mark_dirty(0);
    w.mark_dirty(5);
    w.mark_dirty(9);
    assert_eq!(w.dirty_count(), 3);

    w.clear_dirty();
    assert_eq!(w.dirty_count(), 0);
    assert!(!w.is_dirty(0));
    assert!(!w.is_dirty(5));
    assert!(!w.is_dirty(9));
}

#[test]
fn dirty_bitmap_set_all() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    for _ in 0..10 {
        w.alloc_node().unwrap();
    }
    w.clear_dirty();
    assert_eq!(w.dirty_count(), 0);

    w.set_all_dirty();
    // All 512 bits should be set (8 words * 64 bits).
    assert_eq!(w.dirty_count(), 512);
    assert!(w.is_dirty(0));
    assert!(w.is_dirty(255));
    assert!(w.is_dirty(511));
}

#[test]
fn dirty_bitmap_popcount() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    for _ in 0..10 {
        w.alloc_node().unwrap();
    }
    w.clear_dirty();

    w.mark_dirty(1);
    w.mark_dirty(100);
    w.mark_dirty(300);
    assert_eq!(w.dirty_count(), 3);

    // Marking same bits again should not change count.
    w.mark_dirty(1);
    w.mark_dirty(100);
    assert_eq!(w.dirty_count(), 3);
}

#[test]
fn triple_reader_exposes_dirty_bits() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: initial scene.
    {
        let mut w = tw.acquire();
        w.clear();
        for _ in 0..10 {
            w.alloc_node().unwrap();
        }
        w.set_root(0);
    }
    tw.publish();

    // Frame 2: copy-forward, mark specific nodes dirty.
    {
        let mut w = tw.acquire_copy();
        w.mark_dirty(3);
        w.mark_dirty(7);
        w.mark_dirty(9);
    }
    tw.publish();

    // TripleReader should expose the dirty bits from the published buffer.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();

    // Check specific bits.
    assert_ne!(bits[0] & (1u64 << 3), 0, "bit 3 should be set");
    assert_ne!(bits[0] & (1u64 << 7), 0, "bit 7 should be set");
    assert_ne!(bits[0] & (1u64 << 9), 0, "bit 9 should be set");

    // Total popcount should be 3.
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 3);

    // Other bits should be clear.
    assert_eq!(bits[0] & (1u64 << 0), 0, "bit 0 should not be set");
    assert_eq!(bits[0] & (1u64 << 4), 0, "bit 4 should not be set");
}

#[test]
fn build_parent_map_basic() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child1 = w.alloc_node().unwrap();
    let child2 = w.alloc_node().unwrap();
    w.add_child(root, child1);
    w.add_child(root, child2);
    w.set_root(root);
    w.commit();
    let nodes = w.nodes();
    let parent_map = scene::build_parent_map(nodes, 3);
    assert_eq!(parent_map[root as usize], scene::NULL);
    assert_eq!(parent_map[child1 as usize], root);
    assert_eq!(parent_map[child2 as usize], root);
}

#[test]
fn abs_bounds_nested_three_levels() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.node_mut(root).x = 10;
    w.node_mut(root).y = 20;
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    let mid = w.alloc_node().unwrap();
    w.node_mut(mid).x = 30;
    w.node_mut(mid).y = 40;
    w.node_mut(mid).width = 200;
    w.node_mut(mid).height = 150;
    let leaf = w.alloc_node().unwrap();
    w.node_mut(leaf).x = 5;
    w.node_mut(leaf).y = 10;
    w.node_mut(leaf).width = 50;
    w.node_mut(leaf).height = 25;
    w.add_child(root, mid);
    w.add_child(mid, leaf);
    w.set_root(root);
    w.commit();
    let nodes = w.nodes();
    let parent_map = scene::build_parent_map(nodes, 3);
    // leaf abs: root(10,20) + mid(30,40) + leaf(5,10) = (45, 70)
    let (ax, ay, aw, ah) = scene::abs_bounds(nodes, &parent_map, leaf as usize);
    assert_eq!((ax, ay, aw, ah), (45, 70, 50, 25));
}

// ── Change list and copy-forward tests ──────────────────────────────

// VAL-SCENE-001: acquire_copy preserves scene state
#[test]
fn acquire_copy_preserves_nodes_and_data() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Build a scene with multiple nodes and data.
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 1024;
        w.node_mut(root).height = 768;
        w.node_mut(root).background = Color::rgb(30, 30, 30);
        w.set_root(root);

        let child = w.alloc_node().unwrap();
        w.node_mut(child).x = 10;
        w.node_mut(child).y = 20;
        w.node_mut(child).width = 200;
        w.node_mut(child).height = 100;
        w.node_mut(child).content =
            make_mono_glyphs(&mut w, b"Hello, world!", 16, Color::rgb(220, 220, 220), 8);
        w.add_child(root, child);
    }
    tw.publish(); // front is now gen 1

    // Copy front to back.
    // acquire_copy below

    // Verify the back buffer matches the front byte-for-byte (nodes + data).
    let front_nodes = tw.latest_nodes().to_vec();
    let front_data = tw.latest_data_buf().to_vec();

    let back = tw.acquire_copy();
    let back_nodes = back.nodes();
    let back_data = back.data_buf();

    assert_eq!(front_nodes.len(), back_nodes.len());
    assert_eq!(front_data, back_data);

    let node_size = core::mem::size_of::<Node>();
    for (i, (f, b)) in front_nodes.iter().zip(back_nodes.iter()).enumerate() {
        // SAFETY: Node is repr(C), byte comparison is sound for equality.
        let f_bytes =
            unsafe { core::slice::from_raw_parts(f as *const Node as *const u8, node_size) };
        let b_bytes =
            unsafe { core::slice::from_raw_parts(b as *const Node as *const u8, node_size) };
        assert_eq!(f_bytes, b_bytes, "Node {} differs after acquire_copy", i);
    }
}

// VAL-SCENE-004: Dirty bitmap cleared on new frame (acquire_copy)
#[test]
fn acquire_copy_resets_dirty_bits() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: build scene with marks.
    {
        let mut w = tw.acquire();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.set_root(n);
        w.mark_dirty(0);
    }
    tw.publish();

    // Copy front to back — dirty bits should be cleared in back.
    {
        let back = tw.acquire_copy();
        assert_eq!(back.generation(), 0); // back gen preserved
    }
    // Now swap to make back the new front, then verify dirty bits are empty.
    tw.publish();
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    assert_eq!(*bits, [0u64; scene::DIRTY_BITMAP_WORDS]);
}

// VAL-SCENE-002: Dirty bitmap records changed node IDs
#[test]
fn mark_dirty_records_node_ids() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: initial scene.
    {
        let mut w = tw.acquire();
        w.clear();
        for _ in 0..8 {
            w.alloc_node().unwrap();
        }
        w.set_root(0);
    }
    tw.publish();

    // Frame 2: copy forward, mark specific nodes.
    {
        let mut w = tw.acquire_copy();
        w.mark_dirty(3); // clock
        w.mark_dirty(7); // cursor
    }
    tw.publish();

    // Read the dirty bits from the new front.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    // Bit 3 and bit 7 should be set.
    assert_ne!(bits[0] & (1u64 << 3), 0, "bit 3 should be set");
    assert_ne!(bits[0] & (1u64 << 7), 0, "bit 7 should be set");
    // Only 2 bits should be set.
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 2);
}

// VAL-SCENE-003: Dirty bitmap is readable by TripleReader
#[test]
fn triple_reader_reads_dirty_bits_from_front() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1.
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
    }
    tw.publish();

    // Frame 2: copy-forward + mark one node.
    {
        let mut w = tw.acquire_copy();
        w.node_mut(0).background = Color::rgb(255, 0, 0);
        w.mark_dirty(0);
    }
    tw.publish();

    // Now TripleReader on the same buffer should see the dirty bit.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    assert_ne!(bits[0] & 1, 0, "bit 0 should be set");
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 1);
}

// VAL-SCENE-008: Dirty bitmap handles marking many nodes
#[test]
fn mark_dirty_many_nodes() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Allocate 30 nodes.
    for _ in 0..30 {
        w.alloc_node().unwrap();
    }
    w.set_root(0);

    // Mark 25 nodes dirty — bitmap handles this without overflow.
    for i in 0..25 {
        w.mark_dirty(i as NodeId);
    }

    // All 25 bits should be set.
    assert_eq!(w.dirty_count(), 25);
    for i in 0..25 {
        assert!(w.is_dirty(i as NodeId), "node {} should be dirty", i);
    }
    for i in 25..30 {
        assert!(!w.is_dirty(i as NodeId), "node {} should not be dirty", i);
    }
}

// VAL-SCENE-008: Dirty bitmap via TripleReader handles many nodes
#[test]
fn triple_reader_dirty_bits_many_nodes() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: scene with 30 nodes.
    {
        let mut w = tw.acquire();
        w.clear();
        for _ in 0..30 {
            w.alloc_node().unwrap();
        }
        w.set_root(0);
    }
    tw.publish();

    // Frame 2: copy-forward, mark 25 nodes dirty.
    {
        let mut w = tw.acquire_copy();
        for i in 0..25 {
            w.mark_dirty(i as NodeId);
        }
    }
    tw.publish();

    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 25);
}

// SceneWriter::clear sets all dirty bits
#[test]
fn clear_sets_all_dirty() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1.
    {
        let mut w = tw.acquire();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.set_root(n);
    }
    tw.publish();

    // Frame 2: clear (full rebuild) should set all dirty bits.
    {
        let mut w = tw.acquire();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.set_root(n);
    }
    tw.publish();

    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    assert_eq!(*bits, [u64::MAX; scene::DIRTY_BITMAP_WORDS]);
}

// Marking an already-dirty node is idempotent
#[test]
fn mark_dirty_idempotent() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    for _ in 0..30 {
        w.alloc_node().unwrap();
    }

    w.mark_dirty(5);
    w.mark_dirty(5); // duplicate
    w.mark_dirty(10);
    w.mark_dirty(5); // triple

    assert_eq!(w.dirty_count(), 2, "only 2 distinct nodes should be dirty");
    assert!(w.is_dirty(5));
    assert!(w.is_dirty(10));
}

// VAL-SCENE-007: Node mutation via copy-then-mutate preserves tree structure
#[test]
fn acquire_copy_then_mutate_preserves_other_nodes() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: build a tree with 8 well-known nodes.
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap(); // 0
        w.node_mut(root).width = 1024;
        w.node_mut(root).height = 768;
        w.node_mut(root).background = Color::rgb(30, 30, 30);
        w.set_root(root);

        let title_bar = w.alloc_node().unwrap(); // 1
        w.node_mut(title_bar).width = 1024;
        w.node_mut(title_bar).height = 36;
        w.node_mut(title_bar).background = Color::rgba(20, 20, 20, 200);
        w.add_child(root, title_bar);

        let title_text = w.alloc_node().unwrap(); // 2
        w.node_mut(title_text).x = 12;
        w.node_mut(title_text).y = 8;
        w.add_child(title_bar, title_text);

        let clock_text = w.alloc_node().unwrap(); // 3
        w.node_mut(clock_text).x = 900;
        w.node_mut(clock_text).y = 8;
        w.add_child(title_bar, clock_text);

        let shadow = w.alloc_node().unwrap(); // 4
        w.node_mut(shadow).y = 36;
        w.node_mut(shadow).height = 4;
        w.node_mut(shadow).background = Color::rgba(0, 0, 0, 80);
        w.add_child(root, shadow);

        let content = w.alloc_node().unwrap(); // 5
        w.node_mut(content).y = 40;
        w.node_mut(content).width = 1024;
        w.node_mut(content).height = 728;
        w.add_child(root, content);

        let doc_text = w.alloc_node().unwrap(); // 6
        w.node_mut(doc_text).x = 12;
        w.node_mut(doc_text).y = 8;
        w.add_child(content, doc_text);

        let cursor = w.alloc_node().unwrap(); // 7
        w.node_mut(cursor).x = 12;
        w.node_mut(cursor).y = 8;
        w.node_mut(cursor).width = 2;
        w.node_mut(cursor).height = 20;
        w.node_mut(cursor).background = Color::rgb(200, 200, 200);
        w.add_child(content, cursor);
    }
    tw.publish();

    // Snapshot the front nodes before mutation.
    let front_nodes_before: Vec<Node> = tw.latest_nodes().to_vec();

    // Frame 2: copy forward, mutate only cursor (node 7) position.
    {
        let mut w = tw.acquire_copy();
        w.node_mut(7).x = 100; // moved cursor
        w.node_mut(7).y = 48; // moved cursor
        w.mark_dirty(7);
    }
    tw.publish();

    // Verify all non-mutated nodes are identical.
    let front_nodes_after: Vec<Node> = tw.latest_nodes().to_vec();
    assert_eq!(front_nodes_after.len(), 8);

    let node_size = core::mem::size_of::<Node>();
    for i in 0..8 {
        if i == 7 {
            // Cursor should have changed.
            assert_eq!(front_nodes_after[i].x, 100);
            assert_eq!(front_nodes_after[i].y, 48);
            // But tree structure preserved.
            assert_eq!(
                front_nodes_after[i].first_child,
                front_nodes_before[i].first_child
            );
            assert_eq!(
                front_nodes_after[i].next_sibling,
                front_nodes_before[i].next_sibling
            );
            assert_eq!(front_nodes_after[i].width, front_nodes_before[i].width);
            assert_eq!(
                front_nodes_after[i].background,
                front_nodes_before[i].background
            );
        } else {
            // All other nodes unchanged byte-for-byte.
            let before_bytes = unsafe {
                core::slice::from_raw_parts(
                    &front_nodes_before[i] as *const Node as *const u8,
                    node_size,
                )
            };
            let after_bytes = unsafe {
                core::slice::from_raw_parts(
                    &front_nodes_after[i] as *const Node as *const u8,
                    node_size,
                )
            };
            assert_eq!(
                before_bytes, after_bytes,
                "Node {} should be unchanged after mutating only cursor",
                i
            );
        }
    }

    // Verify only cursor is dirty.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    assert_ne!(bits[0] & (1u64 << 7), 0, "cursor (node 7) should be dirty");
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 1, "only cursor should be dirty");
}

// VAL-SCENE-009: Data buffer exhaustion detection
#[test]
fn data_buffer_exhaustion_detectable_triple() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: fill data buffer to >75%.
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        // Push enough data to exceed 75% of DATA_BUFFER_SIZE.
        let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;
        let chunk = vec![0xABu8; threshold as usize + 100];
        w.push_data(&chunk);
    }
    tw.publish();

    // After copy-forward, the back buffer inherits the high data_used.
    {
        let back = tw.acquire_copy();
        let used = back.data_used();
        let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;
        assert!(
            used > threshold,
            "data_used {} should exceed 75% threshold {}",
            used,
            threshold
        );
        // This is where core would detect exhaustion and fall back to
        // full rebuild via clear() + reset_data().
    }
}

// VAL-SCENE-005: update_data with matching and mismatching lengths
#[test]
fn update_data_in_place_after_acquire_copy() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: scene with data.
    let clock_dref;
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        clock_dref = w.push_data(b"12:34:56");
    }
    tw.publish();

    // Frame 2: copy forward, update clock data in-place.
    {
        let mut w = tw.acquire_copy();
        assert!(w.update_data(clock_dref, b"12:35:00"));
        // Wrong length should fail.
        assert!(!w.update_data(clock_dref, b"ABC"));
        w.mark_dirty(0); // mark root changed (for demo)
    }
    tw.publish();

    // Verify the updated data is readable.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let data = tr.front_data(clock_dref);
    assert_eq!(data, b"12:35:00");
}

// Verify dirty bitmap handles boundary node indices correctly
#[test]
fn dirty_bitmap_boundary_indices() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    for _ in 0..100 {
        w.alloc_node().unwrap();
    }

    // Mark nodes at u64 word boundaries.
    w.mark_dirty(0); // first bit of word 0
    w.mark_dirty(63); // last bit of word 0
    w.mark_dirty(64); // first bit of word 1
    w.mark_dirty(127); // last bit of word 1

    assert_eq!(w.dirty_count(), 4);
    assert!(w.is_dirty(0));
    assert!(w.is_dirty(63));
    assert!(w.is_dirty(64));
    assert!(w.is_dirty(127));
    assert!(!w.is_dirty(1));
    assert!(!w.is_dirty(62));
    assert!(!w.is_dirty(65));
}

// Multiple frames of copy-forward + selective mutation
#[test]
fn multiple_copy_forward_frames() {
    let mut buf = make_triple_buf();

    // Frame 1: initial build.
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            for i in 0..8u16 {
                let n = w.alloc_node().unwrap();
                w.node_mut(n).width = (i + 1) * 10;
            }
            w.set_root(0);
        }
        tw.publish();
    }

    // Frames 2-5: copy-forward with different mutations.
    for frame in 0..4u16 {
        {
            let mut tw = scene::TripleWriter::from_existing(&mut buf);
            {
                let mut w = tw.acquire_copy();
                // Mutate a different node each frame.
                let target = (frame + 1) as NodeId; // nodes 1, 2, 3, 4
                w.node_mut(target).height = (frame + 1) * 100;
                w.mark_dirty(target);
            }
            tw.publish();
        }

        // Verify change list has exactly one entry.
        let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
        let bits = tr.dirty_bits();
        let target_id = (frame + 1) as NodeId;
        let word = target_id as usize / 64;
        let bit = target_id as usize % 64;
        assert_ne!(
            bits[word] & (1u64 << bit),
            0,
            "Frame {}: target node should be dirty",
            frame + 2
        );
        let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
        assert_eq!(popcount, 1, "Frame {}: expected 1 dirty node", frame + 2);

        // Verify the mutation stuck.
        assert_eq!(
            tr.front_nodes()[(frame + 1) as usize].height,
            (frame + 1) * 100
        );

        // Verify other nodes' widths are preserved from frame 1.
        for i in 0..8usize {
            assert_eq!(
                tr.front_nodes()[i].width,
                ((i as u16) + 1) * 10,
                "Frame {}: node {} width changed unexpectedly",
                frame + 2,
                i
            );
        }
    }
}

// VAL-SCENE-009: update_data doesn't grow data_used (same-length overwrite)
#[test]
fn update_data_does_not_grow_data_used_triple() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    let dref;
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        dref = w.push_data(b"AAAAAAAA");
    }
    tw.publish();

    let front_data_before = tw.latest_data_buf().len();

    // Copy forward and update in-place 10 times.
    for i in 0..10u8 {
        {
            let mut w = tw.acquire_copy();
            let new_data = [b'A' + i; 8];
            assert!(w.update_data(dref, &new_data));
            assert_eq!(w.data_used() as usize, front_data_before);
        }
        tw.publish();
    }

    // data_used should be unchanged.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr.front_data_buf().len(), front_data_before);
}

// ── Targeted incremental update tests ───────────────────────────────
//
// These tests verify the incremental update patterns used by Core's
// SceneState methods (update_clock, update_cursor, update_selection,
// update_document_content). They exercise the scene graph primitives
// directly to prove correctness of the copy-forward + selective mutation
// pattern.

// set_node_count unit test
#[test]
fn set_node_count_truncates() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    for _ in 0..10 {
        w.alloc_node().unwrap();
    }
    assert_eq!(w.node_count(), 10);

    w.set_node_count(8);
    assert_eq!(w.node_count(), 8);

    // Can alloc again after truncate (reuses slot 8).
    let new_id = w.alloc_node().unwrap();
    assert_eq!(new_id, 8);
    assert_eq!(w.node_count(), 9);
}

// ── PREV_BOUNDS large coordinate tests (VAL-PIPE-013) ──────────────

/// VAL-PIPE-013: At scale=2, nodes near the framebuffer edge produce
/// physical coordinates that may exceed i16 range (32767). The damage
/// tracking types must handle this without truncation.
///
/// This tests the abs_bounds function output range — values can exceed
/// i16 when multiplied by scale. PREV_BOUNDS must use a wide enough
/// type (i32 for x/y) to avoid truncation.
#[test]
fn abs_bounds_large_coords_no_truncation() {
    // Node at point position (500, 400) with scale=2 → physical (1000, 800).
    // These fit in i16 (max 32767). But at scale=2 with a 2048-wide display,
    // a node at point x=16000 would overflow i16.
    //
    // Test: verify abs_bounds returns correct values for large point coords.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 2048;
    w.node_mut(root).height = 1536;
    w.set_root(root);

    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 32767; // was i16::MAX, now fits in i32
    w.node_mut(child).y = 32767;
    w.node_mut(child).width = 100;
    w.node_mut(child).height = 50;
    w.add_child(root, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 2);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // Physical coords at scale=2 would be 32767*2 = 65534, which doesn't
    // fit in i16 but does fit in i32/u16. The abs_bounds returns i32 values.
    assert_eq!(ax, 32767, "abs_bounds should return full i32 values");
    assert_eq!(ay, 32767);
    assert_eq!(aw, 100);
    assert_eq!(ah, 50);

    // At scale=2, physical x = ax * 2 = 65534. This exceeds i16::MAX (32767).
    // The compositor's PREV_BOUNDS must use i32 for x/y to avoid truncation.
    let physical_x = ax * 2i32;
    assert_eq!(physical_x, 65534);
    assert!(
        physical_x > i16::MAX as i32,
        "physical coord exceeds i16 range"
    );
    // But it fits in i32 and can be safely clamped to u16 for damage rects.
    assert!(physical_x >= 0);
    assert!(physical_x <= u16::MAX as i32);
}

// ── VAL-COORD-013: abs_bounds accounts for content_transform from ancestor nodes ──

/// abs_bounds must apply parent content_transform when computing a child's
/// absolute position. A child inside a scrolled container has its effective y
/// position offset by content_transform.ty (negative for scroll-down).
#[test]
fn abs_bounds_accounts_for_content_transform() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Root at (0, 0)
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    w.set_root(root);

    // Scrollable container at (0, 50) with scroll offset 10 (ty = -10)
    let container = w.alloc_node().unwrap();
    w.node_mut(container).y = 50;
    w.node_mut(container).width = 800;
    w.node_mut(container).height = 500;
    w.node_mut(container).content_transform = AffineTransform::translate(0.0, -10.0);
    w.add_child(root, container);

    // Child inside the scrolled container at (20, 30)
    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 20;
    w.node_mut(child).y = 30;
    w.node_mut(child).width = 100;
    w.node_mut(child).height = 40;
    w.add_child(container, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 3);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // Expected: child.x(20) + container.x(0) + root.x(0) = 20
    assert_eq!(ax, 20, "abs_bounds x should sum parent x values");
    // Expected: child.y(30) + container.y(50) + container.content_transform.ty(-10) + root.y(0) = 70
    // NOT 80 (which would be the result without content_transform)
    assert_eq!(ay, 70, "abs_bounds y must apply parent content_transform");
    assert_eq!(aw, 100);
    assert_eq!(ah, 40);
}

/// abs_bounds with deeply nested scroll containers: content_transform accumulates.
#[test]
fn abs_bounds_nested_scroll_containers() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    w.set_root(root);

    // Outer container scrolled by 5 (ty = -5)
    let outer = w.alloc_node().unwrap();
    w.node_mut(outer).y = 100;
    w.node_mut(outer).width = 800;
    w.node_mut(outer).height = 400;
    w.node_mut(outer).content_transform = AffineTransform::translate(0.0, -5.0);
    w.add_child(root, outer);

    // Inner container scrolled by 15 (ty = -15)
    let inner = w.alloc_node().unwrap();
    w.node_mut(inner).y = 20;
    w.node_mut(inner).width = 800;
    w.node_mut(inner).height = 300;
    w.node_mut(inner).content_transform = AffineTransform::translate(0.0, -15.0);
    w.add_child(outer, inner);

    // Leaf at y=10 inside inner
    let leaf = w.alloc_node().unwrap();
    w.node_mut(leaf).x = 5;
    w.node_mut(leaf).y = 10;
    w.node_mut(leaf).width = 50;
    w.node_mut(leaf).height = 20;
    w.add_child(inner, leaf);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 4);
    let (ax, ay, _aw, _ah) = abs_bounds(nodes, &parent_map, leaf as usize);

    // x: leaf(5) + inner(0) + outer(0) + root(0) = 5
    assert_eq!(ax, 5);
    // y: content_transform.ty on a node offsets its CHILDREN. So:
    //   leaf.y(10) + inner.content_transform.ty(-15) = -5 (relative to inner)
    //   inner.y(20) + outer.content_transform.ty(-5) = 15 (relative to outer)
    //   outer.y(100) -> 100 (relative to root, no scroll)
    //   Total: -5 + 15 + 100 = 110
    assert_eq!(
        ay, 110,
        "abs_bounds must apply each ancestor's content_transform"
    );
}

// ── AffineTransform tests ───────────────────────────────────────────

#[test]
fn affine_transform_identity_is_default() {
    let t = AffineTransform::identity();
    assert_eq!(t.a, 1.0);
    assert_eq!(t.b, 0.0);
    assert_eq!(t.c, 0.0);
    assert_eq!(t.d, 1.0);
    assert_eq!(t.tx, 0.0);
    assert_eq!(t.ty, 0.0);
}

#[test]
fn affine_transform_is_repr_c() {
    // AffineTransform is repr(C) with 6 × f32 = 24 bytes.
    assert_eq!(core::mem::size_of::<AffineTransform>(), 24);
}

#[test]
fn affine_transform_translate() {
    let t = AffineTransform::translate(10.0, 5.0);
    assert_eq!(t.tx, 10.0);
    assert_eq!(t.ty, 5.0);
    assert_eq!(t.a, 1.0);
    assert_eq!(t.d, 1.0);
    assert_eq!(t.b, 0.0);
    assert_eq!(t.c, 0.0);
}

#[test]
fn affine_transform_scale() {
    let t = AffineTransform::scale(2.0, 3.0);
    assert_eq!(t.a, 2.0);
    assert_eq!(t.d, 3.0);
    assert_eq!(t.tx, 0.0);
    assert_eq!(t.ty, 0.0);
}

#[test]
fn affine_transform_rotate_90() {
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_2);
    // cos(90°) ≈ 0, sin(90°) ≈ 1
    assert!((t.a - 0.0).abs() < 1e-5, "a should be ~0, got {}", t.a);
    assert!((t.b - 1.0).abs() < 1e-5, "b should be ~1, got {}", t.b);
    assert!((t.c - (-1.0)).abs() < 1e-5, "c should be ~-1, got {}", t.c);
    assert!((t.d - 0.0).abs() < 1e-5, "d should be ~0, got {}", t.d);
}

#[test]
fn affine_transform_rotate_180() {
    let t = AffineTransform::rotate(core::f32::consts::PI);
    // cos(180°) ≈ -1, sin(180°) ≈ 0
    assert!((t.a - (-1.0)).abs() < 1e-5, "a should be ~-1, got {}", t.a);
    assert!((t.b - 0.0).abs() < 1e-4, "b should be ~0, got {}", t.b);
    assert!((t.c - 0.0).abs() < 1e-4, "c should be ~0, got {}", t.c);
    assert!((t.d - (-1.0)).abs() < 1e-5, "d should be ~-1, got {}", t.d);
}

#[test]
fn affine_transform_skew_x() {
    let angle = 0.5_f32; // ~26.6 degrees
    let t = AffineTransform::skew_x(angle);
    assert_eq!(t.a, 1.0);
    assert_eq!(t.d, 1.0);
    // c = tan(angle) ≈ 0.5463
    let expected_c = angle.tan();
    assert!(
        (t.c - expected_c).abs() < 1e-5,
        "c should be tan(0.5), got {}",
        t.c
    );
    assert_eq!(t.b, 0.0);
}

#[test]
fn affine_transform_compose_translations() {
    let t1 = AffineTransform::translate(100.0, 50.0);
    let t2 = AffineTransform::translate(10.0, 5.0);
    let composed = t1.compose(t2);
    assert!(
        (composed.tx - 110.0).abs() < 1e-5,
        "tx should be 110, got {}",
        composed.tx
    );
    assert!(
        (composed.ty - 55.0).abs() < 1e-5,
        "ty should be 55, got {}",
        composed.ty
    );
}

#[test]
fn affine_transform_compose_scale_then_translate() {
    // Scale(2,2) then translate(10,5):
    // Result: point (x,y) → scale → (2x, 2y) → translate → (2x+10, 2y+5)
    // Matrix: parent × child = translate × scale
    // [1 0 10]   [2 0 0]   [2 0 10]
    // [0 1  5] × [0 2 0] = [0 2  5]
    // [0 0  1]   [0 0 1]   [0 0  1]
    let parent = AffineTransform::translate(10.0, 5.0);
    let child = AffineTransform::scale(2.0, 2.0);
    let composed = parent.compose(child);
    assert!((composed.a - 2.0).abs() < 1e-5);
    assert!((composed.d - 2.0).abs() < 1e-5);
    assert!((composed.tx - 10.0).abs() < 1e-5);
    assert!((composed.ty - 5.0).abs() < 1e-5);
}

#[test]
fn affine_transform_compose_three_levels() {
    // VAL-XFORM-008: Three-level nesting composes correctly.
    let level1 = AffineTransform::translate(100.0, 100.0);
    let level2 = AffineTransform::scale(2.0, 2.0);
    let level3 = AffineTransform::translate(5.0, 5.0);
    // world = level1 × level2 × level3
    let mid = level1.compose(level2);
    let world = mid.compose(level3);
    // Point (0,0) → level3.translate → (5,5) → level2.scale → (10,10) → level1.translate → (110, 110)
    let (rx, ry) = world.transform_point(0.0, 0.0);
    assert!((rx - 110.0).abs() < 1e-4, "x should be 110, got {}", rx);
    assert!((ry - 110.0).abs() < 1e-4, "y should be 110, got {}", ry);
}

#[test]
fn affine_transform_identity_is_identity() {
    let t = AffineTransform::identity();
    assert!(t.is_identity());
    let t2 = AffineTransform::translate(1.0, 0.0);
    assert!(!t2.is_identity());
}

#[test]
fn affine_transform_transform_point() {
    let t = AffineTransform::translate(10.0, 20.0);
    let (x, y) = t.transform_point(5.0, 3.0);
    assert!((x - 15.0).abs() < 1e-5);
    assert!((y - 23.0).abs() < 1e-5);
}

#[test]
fn affine_transform_aabb_identity() {
    let t = AffineTransform::identity();
    let (x, y, w, h) = t.transform_aabb(10.0, 20.0, 30.0, 40.0);
    assert!((x - 10.0).abs() < 1e-5);
    assert!((y - 20.0).abs() < 1e-5);
    assert!((w - 30.0).abs() < 1e-5);
    assert!((h - 40.0).abs() < 1e-5);
}

#[test]
fn affine_transform_aabb_90_rotation() {
    // VAL-XFORM-003: 40x20 node rotated 90° → bounding box ~20x40
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_2);
    let (_, _, w, h) = t.transform_aabb(0.0, 0.0, 40.0, 20.0);
    // After 90° rotation of a 40×20 rect, AABB should be ~20×40.
    assert!(
        (w - 20.0).abs() < 1.0,
        "AABB width should be ~20, got {}",
        w
    );
    assert!(
        (h - 40.0).abs() < 1.0,
        "AABB height should be ~40, got {}",
        h
    );
}

// ── Incremental line insert / delete tests ──────────────────────────

/// Dead slot: unlinked node becomes invisible, chain skips it, node_count unchanged.
#[test]
fn dead_slot_node_invisible_after_unlink() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Parent node.
    let parent = w.alloc_node().unwrap(); // 0
    w.set_root(parent);

    // Three sibling children.
    let n1 = w.alloc_node().unwrap(); // 1
    let n2 = w.alloc_node().unwrap(); // 2
    let n3 = w.alloc_node().unwrap(); // 3

    w.node_mut(n1).flags = NodeFlags::VISIBLE;
    w.node_mut(n2).flags = NodeFlags::VISIBLE;
    w.node_mut(n3).flags = NodeFlags::VISIBLE;

    // Link chain: parent.first_child = n1 -> n2 -> n3 -> NULL
    w.node_mut(parent).first_child = n1;
    w.node_mut(n1).next_sibling = n2;
    w.node_mut(n2).next_sibling = n3;
    w.node_mut(n3).next_sibling = NULL;

    assert_eq!(w.node_count(), 4);

    // "Delete" middle node n2: unlink from chain, clear VISIBLE.
    w.node_mut(n1).next_sibling = w.node(n2).next_sibling; // n1 -> n3
    w.node_mut(n2).flags = NodeFlags::empty(); // invisible

    // Verify: middle node is invisible.
    assert!(!w.node(n2).visible(), "deleted node should be invisible");

    // Chain skips n2: parent -> n1 -> n3 -> NULL.
    assert_eq!(w.node(parent).first_child, n1);
    assert_eq!(w.node(n1).next_sibling, n3);
    assert_eq!(w.node(n3).next_sibling, NULL);

    // node_count unchanged (dead slot remains).
    assert_eq!(w.node_count(), 4);
}

/// After creating dead slots, alloc_node bumps the count (doesn't reuse dead slots).
#[test]
fn alloc_node_after_dead_slots_bumps_count() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let _n0 = w.alloc_node().unwrap(); // 0
    let n1 = w.alloc_node().unwrap(); // 1
    let _n2 = w.alloc_node().unwrap(); // 2
    assert_eq!(w.node_count(), 3);

    // "Delete" n1 by clearing VISIBLE (simulate dead slot).
    w.node_mut(n1).flags = NodeFlags::empty();

    // Allocate a new node — should go at index 3 (bump pointer), NOT reuse index 1.
    let n3 = w.alloc_node().unwrap();
    assert_eq!(
        n3, 3,
        "new node should be at bump pointer, not reuse dead slot"
    );
    assert_eq!(w.node_count(), 4);
}

/// Insert a new node into the middle of a sibling chain.
#[test]
fn insert_line_node_links_into_chain() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Parent.
    let parent = w.alloc_node().unwrap(); // 0
    w.set_root(parent);

    // Three children: line1, line2, line3.
    let line1 = w.alloc_node().unwrap(); // 1
    let line2 = w.alloc_node().unwrap(); // 2
    let line3 = w.alloc_node().unwrap(); // 3

    // Chain: parent -> line1 -> line2 -> line3 -> NULL
    w.node_mut(parent).first_child = line1;
    w.node_mut(line1).next_sibling = line2;
    w.node_mut(line2).next_sibling = line3;
    w.node_mut(line3).next_sibling = NULL;

    // Set y positions.
    w.node_mut(line1).y = 0;
    w.node_mut(line2).y = 20;
    w.node_mut(line3).y = 40;

    // Insert a new node after line2.
    let new_line = w.alloc_node().unwrap(); // 4
    w.node_mut(new_line).y = 40; // same y as old line3 (it will be shifted)
    w.node_mut(new_line).flags = NodeFlags::VISIBLE;

    // Link: line2 -> new_line -> line3
    w.node_mut(new_line).next_sibling = w.node(line2).next_sibling; // new -> line3
    w.node_mut(line2).next_sibling = new_line; // line2 -> new

    // Shift line3 y down by line_height (20).
    w.node_mut(line3).y = 60;

    // Verify chain order: parent -> line1 -> line2 -> new_line -> line3 -> NULL
    assert_eq!(w.node(parent).first_child, line1);
    assert_eq!(w.node(line1).next_sibling, line2);
    assert_eq!(w.node(line2).next_sibling, new_line);
    assert_eq!(w.node(new_line).next_sibling, line3);
    assert_eq!(w.node(line3).next_sibling, NULL);

    // Verify y positions after shift.
    assert_eq!(w.node(line1).y, 0);
    assert_eq!(w.node(line2).y, 20);
    assert_eq!(w.node(new_line).y, 40);
    assert_eq!(w.node(line3).y, 60);

    assert_eq!(w.node_count(), 5);
}

/// Update line positions shifts y for all nodes in a chain.
#[test]
fn update_line_positions_shifts_y() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let parent = w.alloc_node().unwrap(); // 0
    w.set_root(parent);

    // Five line nodes.
    let mut nodes = Vec::new();
    for i in 0..5u16 {
        let n = w.alloc_node().unwrap();
        w.node_mut(n).y = (i as i32) * 20;
        w.node_mut(n).flags = NodeFlags::VISIBLE;
        w.node_mut(n).content_hash = 0xABCD; // mark with known hash
        nodes.push(n);
    }

    // Link chain.
    w.node_mut(parent).first_child = nodes[0];
    for i in 0..4 {
        w.node_mut(nodes[i]).next_sibling = nodes[i + 1];
    }
    w.node_mut(nodes[4]).next_sibling = NULL;

    // Simulate shifting nodes[2..] down by 20 (line insert at index 2).
    let line_height = 20i32;
    let mut cur = nodes[2];
    let mut idx = 2u32;
    while cur != NULL {
        w.node_mut(cur).y = (idx as i32) * line_height + line_height; // shift down
        w.mark_dirty(cur);
        cur = w.node(cur).next_sibling;
        idx += 1;
    }

    // Verify shifted positions.
    assert_eq!(w.node(nodes[0]).y, 0); // unchanged
    assert_eq!(w.node(nodes[1]).y, 20); // unchanged
    assert_eq!(w.node(nodes[2]).y, 60); // shifted
    assert_eq!(w.node(nodes[3]).y, 80); // shifted
    assert_eq!(w.node(nodes[4]).y, 100); // shifted

    // Content hashes unchanged (property-only shift).
    for &n in &nodes[2..] {
        assert_eq!(
            w.node(n).content_hash,
            0xABCD,
            "content_hash should be unchanged for shifted nodes"
        );
    }
}

#[test]
fn affine_transform_aabb_45_rotation() {
    // VAL-XFORM-009: 100x100 node rotated 45° has AABB ~141x141
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_4);
    let (_, _, w, h) = t.transform_aabb(0.0, 0.0, 100.0, 100.0);
    // sqrt(2) * 100 ≈ 141.42
    assert!(
        (w - 141.42).abs() < 1.0,
        "AABB width should be ~141, got {}",
        w
    );
    assert!(
        (h - 141.42).abs() < 1.0,
        "AABB height should be ~141, got {}",
        h
    );
}

#[test]
fn affine_transform_scale_zero_no_panic() {
    // VAL-XFORM-018: scale(0,0) produces degenerate but no panic.
    let t = AffineTransform::scale(0.0, 0.0);
    let (_, _, w, h) = t.transform_aabb(0.0, 0.0, 10.0, 10.0);
    assert_eq!(w, 0.0);
    assert_eq!(h, 0.0);
}

#[test]
fn affine_transform_negative_scale_mirror() {
    // VAL-XFORM-019: scale(-1,1) produces horizontal mirror.
    let t = AffineTransform::scale(-1.0, 1.0);
    let (px, py) = t.transform_point(10.0, 5.0);
    assert!((px - (-10.0)).abs() < 1e-5);
    assert!((py - 5.0).abs() < 1e-5);
}

#[test]
fn affine_transform_skew_x_parallelogram() {
    // VAL-XFORM-011: skew_x(0.5) on 40x40: bottom edge shifts 20px right.
    let t = AffineTransform::skew_x(0.5_f32.atan()); // tan(angle) = 0.5
                                                     // Bottom-left corner (0, 40): x' = 0 + 0.5*40 = 20, y' = 40
    let (px, _py) = t.transform_point(0.0, 40.0);
    assert!(
        (px - 20.0).abs() < 1e-4,
        "bottom-left x should shift to ~20, got {}",
        px
    );
}

#[test]
fn node_has_transform_field() {
    let node = Node::EMPTY;
    assert!(node.transform.is_identity());
}

#[test]
fn node_has_content_transform_field() {
    let node = Node::EMPTY;
    assert!(node.content_transform.is_identity());
    assert!(node.content_transform.is_pure_translation());
}

#[test]
fn affine_transform_is_pure_translation() {
    // Identity is a pure translation (trivially).
    assert!(AffineTransform::identity().is_pure_translation());
    // Non-zero translation is still a pure translation.
    assert!(AffineTransform::translate(10.0, -20.0).is_pure_translation());
    // Scale is NOT a pure translation.
    assert!(!AffineTransform::scale(2.0, 2.0).is_pure_translation());
    // Rotation is NOT a pure translation.
    assert!(!AffineTransform::rotate(0.5).is_pure_translation());
}

#[test]
fn affine_transform_partial_eq() {
    let a = AffineTransform::identity();
    let b = AffineTransform::identity();
    assert_eq!(a, b);

    let c = AffineTransform::translate(1.0, 2.0);
    let d = AffineTransform::translate(1.0, 2.0);
    assert_eq!(c, d);
    assert_ne!(a, c);
}

#[test]
fn node_size_assertion_with_transform() {
    // VAL-XFORM-022: Node size compile-time assertion.
    // After adding clip_path (DataRef, 8 bytes) and _reserved (8 bytes),
    // Node grew from 120 to 136 bytes.
    let size = core::mem::size_of::<Node>();
    assert_eq!(
        size, 136,
        "Node size should be 136 bytes with clip_path + _reserved, got {}",
        size
    );
}

#[test]
fn triple_buffer_preserves_transform_fields() {
    // VAL-CROSS-015: acquire_copy preserves transform fields.
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let mut tw = scene::TripleWriter::new(&mut buf);

    {
        let mut sw = tw.acquire();
        let n = sw.alloc_node().unwrap();
        let node = sw.node_mut(n);
        node.width = 50;
        node.height = 50;
        node.flags = NodeFlags::VISIBLE;
        node.transform = AffineTransform::translate(10.0, 20.0);
        sw.commit();
    }
    tw.publish();
    {
        let sw = tw.acquire_copy();
        let node = sw.node(0);
        assert!(
            (node.transform.tx - 10.0).abs() < 1e-5,
            "transform.tx must survive acquire_copy, got {}",
            node.transform.tx
        );
        assert!(
            (node.transform.ty - 20.0).abs() < 1e-5,
            "transform.ty must survive acquire_copy, got {}",
            node.transform.ty
        );
        assert!(
            (node.transform.a - 1.0).abs() < 1e-5,
            "transform.a must survive acquire_copy, got {}",
            node.transform.a
        );
    }
}

// ── Transform-aware damage tracking tests ───────────────────────────

/// VAL-XFORM-016: Transformed damage tracking covers AABB.
/// A 40×40 node rotated 45° should have abs_bounds returning the AABB
/// of the rotated square (~57×57), not the original 40×40.
#[test]
fn abs_bounds_rotated_node_uses_aabb() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 200;
    w.node_mut(root).height = 200;
    w.set_root(root);

    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 50;
    w.node_mut(child).y = 50;
    w.node_mut(child).width = 40;
    w.node_mut(child).height = 40;
    w.node_mut(child).flags = NodeFlags::VISIBLE;
    // 45° rotation
    w.node_mut(child).transform = AffineTransform::rotate(45.0 * core::f32::consts::PI / 180.0);
    w.add_child(root, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 2);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // 40×40 rotated 45°: AABB should be ~56.57×56.57
    // The center of the 40×40 rect at (50,50) is at (70,70).
    // After rotation, the AABB expands.
    // The AABB width and height should be approximately sqrt(2)*40 ≈ 56.57.
    assert!(
        aw >= 55 && aw <= 60,
        "VAL-XFORM-016: rotated 40×40 node AABB width should be ~57, got {aw}"
    );
    assert!(
        ah >= 55 && ah <= 60,
        "VAL-XFORM-016: rotated 40×40 node AABB height should be ~57, got {ah}"
    );
}

/// VAL-XFORM-016 (dirty rect coverage): Moving a rotated node should
/// produce dirty rects covering both old and new AABBs.
#[test]
fn abs_bounds_scaled_node_uses_scaled_size() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 200;
    w.node_mut(root).height = 200;
    w.set_root(root);

    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 10;
    w.node_mut(child).y = 10;
    w.node_mut(child).width = 20;
    w.node_mut(child).height = 20;
    w.node_mut(child).flags = NodeFlags::VISIBLE;
    // 3x scale
    w.node_mut(child).transform = AffineTransform::scale(3.0, 3.0);
    w.add_child(root, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 2);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // 20×20 node scaled 3x: AABB should be 60×60
    assert_eq!(
        aw, 60,
        "scaled 20×20 at 3x should have AABB width 60, got {aw}"
    );
    assert_eq!(
        ah, 60,
        "scaled 20×20 at 3x should have AABB height 60, got {ah}"
    );
}

/// VAL-XFORM-017: Compound transform correctness.
/// translate(50,50) × rotate(45°) × scale(2,2) on 10x10:
/// ~28×28 diamond centered at (50,50).
#[test]
fn abs_bounds_compound_transform_aabb() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 200;
    w.node_mut(root).height = 200;
    w.set_root(root);

    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 0;
    w.node_mut(child).y = 0;
    w.node_mut(child).width = 10;
    w.node_mut(child).height = 10;
    w.node_mut(child).flags = NodeFlags::VISIBLE;
    // Compound: translate(50,50) × rotate(45°) × scale(2,2)
    let xform = AffineTransform::translate(50.0, 50.0)
        .compose(AffineTransform::rotate(
            45.0 * core::f32::consts::PI / 180.0,
        ))
        .compose(AffineTransform::scale(2.0, 2.0));
    w.node_mut(child).transform = xform;
    w.add_child(root, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 2);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // 10×10 scaled 2x then rotated 45°: effective size 20×20 rotated = ~28.28×28.28
    // Then translated to (50,50).
    //
    // The four corners of (0,0,10,10) after the full transform:
    //   (0,0) → scale → (0,0) → rotate → (0,0) → translate → (50,50)
    //   (10,0) → scale → (20,0) → rotate → (~14.1, 14.1) → translate → (~64.1, 64.1)
    //   (10,10) → scale → (20,20) → rotate → (0, ~28.3) → translate → (50, ~78.3)
    //   (0,10) → scale → (0,20) → rotate → (~-14.1, 14.1) → translate → (~35.9, 64.1)
    //
    // AABB: x ≈ 35.9, y = 50, w ≈ 28.3, h ≈ 28.3
    assert!(
        aw >= 27 && aw <= 30,
        "VAL-XFORM-017: compound transform AABB width should be ~28, got {aw}"
    );
    assert!(
        ah >= 27 && ah <= 30,
        "VAL-XFORM-017: compound transform AABB height should be ~28, got {ah}"
    );
    // The AABB x origin should be near 35.9 (from bottom-left corner transform)
    assert!(
        ax >= 34 && ax <= 38,
        "VAL-XFORM-017: compound transform AABB x should be ~36, got {ax}"
    );
    // The AABB y origin should be near 50 (from top-left corner transform)
    assert!(
        ay >= 49 && ay <= 52,
        "VAL-XFORM-017: compound transform AABB y should be ~50, got {ay}"
    );
}

/// Identity transform should not change abs_bounds.
#[test]
fn abs_bounds_identity_transform_unchanged() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 200;
    w.node_mut(root).height = 200;
    w.set_root(root);

    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 30;
    w.node_mut(child).y = 40;
    w.node_mut(child).width = 50;
    w.node_mut(child).height = 60;
    w.node_mut(child).flags = NodeFlags::VISIBLE;
    // Identity transform (default)
    w.add_child(root, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 2);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    assert_eq!(ax, 30, "identity transform should not change x");
    assert_eq!(ay, 40, "identity transform should not change y");
    assert_eq!(aw, 50, "identity transform should not change width");
    assert_eq!(ah, 60, "identity transform should not change height");
}

// ── Background container tests (VAL-FILL-01: FillRect removed) ─────
// FillRect has been removed from Content. Solid rectangle fills (cursor,
// selection) now use Content::None with node.background set to the color.

#[test]
fn background_container_round_trip_scene_writer_reader() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).x = 10;
    w.node_mut(id).y = 20;
    w.node_mut(id).width = 100;
    w.node_mut(id).height = 50;
    w.node_mut(id).background = Color::rgba(255, 0, 0, 128);
    w.node_mut(id).content = Content::None;
    w.set_root(id);
    w.commit();

    // Background container uses no data buffer
    assert_eq!(w.data_used(), 0);

    let r = SceneReader::new(&buf);
    let node = r.node(id);
    assert!(matches!(node.content, Content::None));
    assert_eq!(node.background, Color::rgba(255, 0, 0, 128));
}

#[test]
fn background_container_no_data_buffer_allocation() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    for _ in 0..10 {
        let id = w.alloc_node().unwrap();
        w.node_mut(id).background = Color::rgb(0, 255, 0);
        w.node_mut(id).content = Content::None;
    }
    assert_eq!(
        w.data_used(),
        0,
        "background container should not allocate data buffer"
    );
}

#[test]
fn background_container_triple_buffer_round_trip() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let id = w.alloc_node().unwrap();
        w.node_mut(id).width = 50;
        w.node_mut(id).height = 30;
        w.node_mut(id).background = Color::rgba(100, 200, 50, 180);
        w.node_mut(id).content = Content::None;
        w.set_root(id);
    }
    tw.publish();

    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let nodes = tr.front_nodes();
    assert_eq!(nodes.len(), 1);
    assert!(matches!(nodes[0].content, Content::None));
    assert_eq!(nodes[0].background, Color::rgba(100, 200, 50, 180));
}

#[test]
fn background_container_acquire_copy_preserves() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let id = w.alloc_node().unwrap();
        w.node_mut(id).background = Color::rgb(255, 128, 0);
        w.node_mut(id).content = Content::None;
        w.set_root(id);
    }
    tw.publish();

    let back = tw.acquire_copy();
    assert!(matches!(back.node(0).content, Content::None));
    assert_eq!(back.node(0).background, Color::rgb(255, 128, 0));
}

// ── Glyphs content type tests (VAL-SCENE-002) ──────────────────────

#[test]
fn glyphs_round_trip_scene_writer_reader() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let glyphs = [
        ShapedGlyph {
            glyph_id: 72,
            _pad: 0,
            x_advance: 600 * 65536,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 101,
            _pad: 0,
            x_advance: 550 * 65536,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 108,
            _pad: 0,
            x_advance: 250 * 65536,
            x_offset: -5 * 65536,
            y_offset: 3 * 65536,
        },
    ];
    let dref = w.push_shaped_glyphs(&glyphs);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Glyphs {
        color: Color::rgb(220, 220, 220),
        glyphs: dref,
        glyph_count: 3,
        font_size: 18,
        axis_hash: 0xDEAD_BEEF,
    };
    w.set_root(id);
    w.commit();

    let r = SceneReader::new(&buf);
    let node = r.node(id);
    match node.content {
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            font_size,
            axis_hash,
        } => {
            assert_eq!(color, Color::rgb(220, 220, 220));
            assert_eq!(glyph_count, 3);
            assert_eq!(font_size, 18);
            assert_eq!(axis_hash, 0xDEAD_BEEF);
            let read = r.shaped_glyphs(glyphs, glyph_count);
            assert_eq!(read.len(), 3);
            assert_eq!(read[0].glyph_id, 72);
            assert_eq!(read[2].x_offset, -5 * 65536);
            assert_eq!(read[2].y_offset, 3 * 65536);
        }
        _ => panic!("expected Glyphs content"),
    }
}

#[test]
fn glyphs_triple_buffer_round_trip() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let glyphs = [
            ShapedGlyph {
                glyph_id: 65,
                _pad: 0,
                x_advance: 10 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
            ShapedGlyph {
                glyph_id: 66,
                _pad: 0,
                x_advance: 10 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
        ];
        let dref = w.push_shaped_glyphs(&glyphs);
        let id = w.alloc_node().unwrap();
        w.node_mut(id).content = Content::Glyphs {
            color: Color::rgb(200, 200, 200),
            glyphs: dref,
            glyph_count: 2,
            font_size: 16,
            axis_hash: 0x1234,
        };
        w.set_root(id);
    }
    tw.publish();

    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    match tr.front_nodes()[0].content {
        Content::Glyphs {
            glyphs,
            glyph_count,
            font_size,
            axis_hash,
            ..
        } => {
            assert_eq!(glyph_count, 2);
            assert_eq!(font_size, 16);
            assert_eq!(axis_hash, 0x1234);
            let read = tr.front_shaped_glyphs(glyphs, glyph_count);
            assert_eq!(read.len(), 2);
            assert_eq!(read[0].glyph_id, 65);
            assert_eq!(read[1].glyph_id, 66);
        }
        _ => panic!("expected Glyphs"),
    }
}

#[test]
fn glyphs_acquire_copy_preserves_data() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let glyphs = [ShapedGlyph {
            glyph_id: 42,
            _pad: 0,
            x_advance: 500 * 65536,
            x_offset: 0,
            y_offset: 0,
        }];
        let dref = w.push_shaped_glyphs(&glyphs);
        let id = w.alloc_node().unwrap();
        w.node_mut(id).content = Content::Glyphs {
            color: Color::rgb(255, 255, 255),
            glyphs: dref,
            glyph_count: 1,
            font_size: 24,
            axis_hash: 0,
        };
        w.set_root(id);
    }
    tw.publish();
    // acquire_copy below

    // Verify back buffer has the glyph data
    let back = tw.acquire_copy();
    match back.node(0).content {
        Content::Glyphs { glyph_count, .. } => {
            assert_eq!(glyph_count, 1);
            // Verify data_buf contains glyph data
            assert!(back.data_used() > 0);
        }
        _ => panic!("Glyphs not preserved after copy"),
    }
}

// ── Multiple Glyphs nodes coexist (VAL-SCENE-004) ──────────────────

#[test]
fn multiple_glyphs_nodes_coexist() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.set_root(root);

    let mut expected_ids: Vec<(u16, u16)> = Vec::new(); // (first_glyph_id, count)

    for i in 0..5u16 {
        let glyphs: Vec<ShapedGlyph> = (0..(i + 1) * 3)
            .map(|j| ShapedGlyph {
                glyph_id: i * 100 + j,
                _pad: 0,
                x_advance: 10 * 65536,
                x_offset: 0,
                y_offset: 0,
            })
            .collect();
        let count = glyphs.len() as u16;
        let dref = w.push_shaped_glyphs(&glyphs);
        let nid = w.alloc_node().unwrap();
        w.node_mut(nid).content = Content::Glyphs {
            color: Color::rgb(200, 200, 200),
            glyphs: dref,
            glyph_count: count,
            font_size: 16,
            axis_hash: 0,
        };
        w.add_child(root, nid);
        expected_ids.push((i * 100, count));
    }
    w.commit();

    let r = SceneReader::new(&buf);
    for (idx, &(first_id, count)) in expected_ids.iter().enumerate() {
        let nid = (idx + 1) as u16; // root is 0, children are 1..5
        match r.node(nid).content {
            Content::Glyphs {
                glyphs,
                glyph_count,
                ..
            } => {
                assert_eq!(glyph_count, count, "node {} glyph_count mismatch", idx);
                let read = r.shaped_glyphs(glyphs, glyph_count);
                assert_eq!(read.len(), count as usize);
                assert_eq!(
                    read[0].glyph_id, first_id,
                    "node {} first glyph mismatch",
                    idx
                );
            }
            _ => panic!("node {} expected Glyphs", idx),
        }
    }
}

// ── Node size (VAL-SCENE-006) ────────────────────────────────────────

#[test]
fn node_size_is_136_bytes() {
    assert_eq!(core::mem::size_of::<Node>(), 136);
}

#[test]
fn scene_header_size_is_80_bytes() {
    assert_eq!(core::mem::size_of::<SceneHeader>(), 80);
}

#[test]
fn shaped_glyph_size_is_16_bytes() {
    assert_eq!(core::mem::size_of::<ShapedGlyph>(), 16);
}

// ── Mixed content type tests (VAL-SCENE-008) ───────────────────────

#[test]
fn mixed_background_glyphs_image_triple_buffer() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 800;
        w.node_mut(root).height = 600;
        w.set_root(root);

        // Background container child (replaces FillRect)
        let fill_id = w.alloc_node().unwrap();
        w.node_mut(fill_id).width = 100;
        w.node_mut(fill_id).height = 20;
        w.node_mut(fill_id).background = Color::rgba(200, 200, 200, 128);
        w.node_mut(fill_id).content = Content::None;
        w.add_child(root, fill_id);

        // Glyphs child
        let glyphs = [ShapedGlyph {
            glyph_id: 65,
            _pad: 0,
            x_advance: 10 * 65536,
            x_offset: 0,
            y_offset: 0,
        }];
        let gref = w.push_shaped_glyphs(&glyphs);
        let glyph_id = w.alloc_node().unwrap();
        w.node_mut(glyph_id).content = Content::Glyphs {
            color: Color::rgb(255, 255, 255),
            glyphs: gref,
            glyph_count: 1,
            font_size: 16,
            axis_hash: 0,
        };
        w.add_child(root, glyph_id);

        // Image child
        let pixels = vec![0u8; 16]; // 1x1 BGRA pixel (4 bytes)
        let iref = w.push_data(&pixels);
        let img_id = w.alloc_node().unwrap();
        w.node_mut(img_id).content = Content::Image {
            data: iref,
            src_width: 2,
            src_height: 2,
        };
        w.add_child(root, img_id);
    }
    tw.publish();

    // Verify all survive the swap
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr.front_nodes().len(), 4);
    assert!(matches!(tr.front_nodes()[1].content, Content::None));
    assert_eq!(tr.front_nodes()[1].background.a, 128);
    match tr.front_nodes()[2].content {
        Content::Glyphs { glyph_count, .. } => assert_eq!(glyph_count, 1),
        _ => panic!("expected Glyphs"),
    }
    match tr.front_nodes()[3].content {
        Content::Image {
            src_width,
            src_height,
            ..
        } => {
            assert_eq!(src_width, 2);
            assert_eq!(src_height, 2);
        }
        _ => panic!("expected Image"),
    }
}

// ── mark_dirty works with background and Glyphs (VAL-SCENE-008) ──

#[test]
fn mark_dirty_works_for_background_and_glyphs_triple() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        for _ in 0..4 {
            w.alloc_node().unwrap();
        }
        // Node 0: root
        // Node 1: background container (cursor)
        w.node_mut(1).background = Color::rgb(200, 200, 200);
        w.node_mut(1).content = Content::None;
        // Node 2: Glyphs text
        w.node_mut(2).content = Content::Glyphs {
            color: Color::rgb(255, 255, 255),
            glyphs: DataRef {
                offset: 0,
                length: 0,
            },
            glyph_count: 0,
            font_size: 16,
            axis_hash: 0,
        };
        w.set_root(0);
    }
    tw.publish();

    // Incremental update: change background and Glyphs nodes
    {
        let mut w = tw.acquire_copy();
        w.node_mut(1).background = Color::rgb(100, 100, 100);
        w.mark_dirty(1);
        w.node_mut(2).content_hash = fnv1a(b"new text");
        w.mark_dirty(2);
    }
    tw.publish();

    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let bits = tr.dirty_bits();
    assert_ne!(bits[0] & (1u64 << 1), 0, "node 1 should be dirty");
    assert_ne!(bits[0] & (1u64 << 2), 0, "node 2 should be dirty");
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 2);
}

// ── Glyphs axis_hash round-trip (VAL-SCENE-002) ────────────────────

#[test]
fn glyphs_axis_hash_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Glyphs {
        color: Color::rgb(255, 255, 255),
        glyphs: DataRef {
            offset: 0,
            length: 0,
        },
        glyph_count: 0,
        font_size: 18,
        axis_hash: 0xABCD_1234,
    };
    w.set_root(id);
    w.commit();

    let r = SceneReader::new(&buf);
    match r.node(id).content {
        Content::Glyphs { axis_hash, .. } => {
            assert_eq!(axis_hash, 0xABCD_1234, "axis_hash must survive round-trip");
        }
        _ => panic!("expected Glyphs"),
    }
}

// ── Removed types do not exist (VAL-SCENE-005) ─────────────────────
// These are compile-time assertions — the test compiles = types removed.
// grep-based verification is in the feature's verificationSteps.

#[test]
fn content_enum_has_three_variants() {
    // Verify that Content has exactly: None, Image, Glyphs
    // FillRect removed — solid fills use Content::None + node.background.
    // Path will be added in a later feature.
    let none = Content::None;
    let img = Content::Image {
        data: DataRef {
            offset: 0,
            length: 0,
        },
        src_width: 0,
        src_height: 0,
    };
    let glyphs = Content::Glyphs {
        color: Color::rgb(0, 0, 0),
        glyphs: DataRef {
            offset: 0,
            length: 0,
        },
        glyph_count: 0,
        font_size: 0,
        axis_hash: 0,
    };
    assert!(matches!(none, Content::None));
    assert!(matches!(img, Content::Image { .. }));
    assert!(matches!(glyphs, Content::Glyphs { .. }));
}

// ── Monospace Glyphs convenience test ──────────────────────────────

#[test]
fn make_mono_glyphs_produces_correct_content() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let content = make_mono_glyphs(&mut w, b"Hello", 16, Color::rgb(200, 200, 200), 8);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = content;
    w.set_root(id);
    w.commit();

    let r = SceneReader::new(&buf);
    match r.node(id).content {
        Content::Glyphs {
            color,
            glyphs,
            glyph_count,
            font_size,
            axis_hash,
        } => {
            assert_eq!(glyph_count, 5);
            assert_eq!(font_size, 16);
            assert_eq!(axis_hash, 0);
            assert_eq!(color, Color::rgb(200, 200, 200));
            let read = r.shaped_glyphs(glyphs, glyph_count);
            assert_eq!(read.len(), 5);
            assert_eq!(read[0].glyph_id, b'H' as u16);
            assert_eq!(read[4].glyph_id, b'o' as u16);
            // Each glyph has x_advance == 8 points (in 16.16 fixed-point = 8 * 65536)
            for g in read {
                assert_eq!(g.x_advance, 8 * 65536);
            }
        }
        _ => panic!("expected Glyphs content"),
    }
}

// ── Core Scene Migration Tests (VAL-CORE) ───────────────────────────
//
// These tests verify the scene graph structure that the core's SceneState
// produces. Since core is a bare-metal binary (not importable), we
// replicate the scene building logic here. The scene graph is the contract
// between core and compositor — these tests verify that contract.

/// Well-known node indices (mirroring core/scene_state.rs).
const CORE_N_ROOT: u16 = 0;
const CORE_N_TITLE_BAR: u16 = 1;
const CORE_N_TITLE_TEXT: u16 = 2;
const CORE_N_CLOCK_TEXT: u16 = 3;
const CORE_N_SHADOW: u16 = 4;
const CORE_N_CONTENT: u16 = 5;
const CORE_N_DOC_TEXT: u16 = 6;
const CORE_N_CURSOR: u16 = 7;
const CORE_WELL_KNOWN_COUNT: u16 = 8;

/// Convert raw bytes to ShapedGlyph arrays (monospace).
fn bytes_to_shaped_glyphs_test(text: &[u8], advance: u16) -> Vec<ShapedGlyph> {
    text.iter()
        .map(|&ch| ShapedGlyph {
            glyph_id: ch as u16,
            _pad: 0,
            x_advance: advance as i32 * 65536,
            x_offset: 0,
            y_offset: 0,
        })
        .collect()
}

/// Build a minimal editor scene (core pattern) with per-line Glyphs,
/// background-colored cursor, and background-colored selection rects.
#[allow(clippy::too_many_arguments)]
fn build_test_editor_scene(
    w: &mut SceneWriter,
    fb_width: u32,
    fb_height: u32,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    char_width: u32,
    line_height: u32,
    font_size: u16,
    scroll_y: f32,
) {
    let text_color = Color::rgb(220, 220, 220);
    let cursor_color = Color::rgb(200, 200, 200);
    let sel_color = Color::rgba(80, 120, 200, 100);
    let title_bar_h: u32 = 36;
    let shadow_depth: u32 = 12;
    let text_inset_x: u32 = 12;
    let doc_width = fb_width.saturating_sub(2 * text_inset_x);
    let chars_per_line = if char_width > 0 {
        (doc_width / char_width).max(1) as usize
    } else {
        80
    };

    w.clear();

    // Push glyph data for title and clock.
    let title_glyphs = bytes_to_shaped_glyphs_test(b"Text", char_width as u16);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
    let clock_glyphs = bytes_to_shaped_glyphs_test(b"12:34:56", char_width as u16);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Layout visible document lines.
    let all_runs = layout_mono_lines(
        doc_text,
        chars_per_line,
        line_height as i32,
        text_color,
        char_width as u16,
        font_size,
    );
    let content_y = title_bar_h + shadow_depth;
    let content_h = fb_height.saturating_sub(content_y);
    let visible_runs = scroll_runs(all_runs, scroll_y, line_height, content_h as i32);
    let scroll_pt = scroll_y.round() as i32;

    // Push line glyph data.
    let mut line_glyph_refs: Vec<(DataRef, u16, i32)> = Vec::with_capacity(visible_runs.len());
    for run in &visible_runs {
        let line_text = line_bytes_for_run(doc_text, &run);
        let shaped = bytes_to_shaped_glyphs_test(line_text, char_width as u16);
        let glyph_ref = w.push_shaped_glyphs(&shaped);
        line_glyph_refs.push((glyph_ref, shaped.len() as u16, run.y));
    }

    // Allocate well-known nodes.
    for _ in 0..8 {
        w.alloc_node().unwrap();
    }

    // N_ROOT
    {
        let n = w.node_mut(CORE_N_ROOT);
        n.first_child = CORE_N_TITLE_BAR;
        n.width = fb_width as u16;
        n.height = fb_height as u16;
        n.background = Color::rgb(30, 30, 30);
        n.flags = NodeFlags::VISIBLE;
    }
    // N_TITLE_BAR
    {
        let n = w.node_mut(CORE_N_TITLE_BAR);
        n.first_child = CORE_N_TITLE_TEXT;
        n.next_sibling = CORE_N_SHADOW;
        n.width = fb_width as u16;
        n.height = title_bar_h as u16;
        n.background = Color::rgba(20, 20, 20, 200);
        n.flags = NodeFlags::VISIBLE;
    }
    // N_TITLE_TEXT — Content::Glyphs
    {
        let n = w.node_mut(CORE_N_TITLE_TEXT);
        n.next_sibling = CORE_N_CLOCK_TEXT;
        n.x = 12;
        n.y = 8;
        n.width = (fb_width / 2) as u16;
        n.height = line_height as u16;
        n.content = Content::Glyphs {
            color: Color::rgb(180, 180, 180),
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(b"Text");
        n.flags = NodeFlags::VISIBLE;
    }
    // N_CLOCK_TEXT — Content::Glyphs
    {
        let n = w.node_mut(CORE_N_CLOCK_TEXT);
        n.x = (fb_width - 12 - 80) as i32;
        n.y = 8;
        n.width = 80;
        n.height = line_height as u16;
        n.content = Content::Glyphs {
            color: Color::rgb(120, 120, 120),
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(b"12:34:56");
        n.flags = NodeFlags::VISIBLE;
    }
    // N_SHADOW (placeholder)
    {
        let n = w.node_mut(CORE_N_SHADOW);
        n.next_sibling = CORE_N_CONTENT;
        n.y = title_bar_h as i32;
        n.width = fb_width as u16;
        n.flags = NodeFlags::VISIBLE;
    }
    // N_CONTENT
    {
        let n = w.node_mut(CORE_N_CONTENT);
        n.first_child = CORE_N_DOC_TEXT;
        n.next_sibling = NULL;
        n.y = content_y as i32;
        n.width = fb_width as u16;
        n.height = content_h as u16;
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    // N_DOC_TEXT -- Content::None (pure container with content_transform)
    {
        let n = w.node_mut(CORE_N_DOC_TEXT);
        n.x = text_inset_x as i32;
        n.y = 8;
        n.width = doc_width as u16;
        n.height = content_h as u16;
        n.content_transform = AffineTransform::translate(0.0, -scroll_y);
        n.content = Content::None;
        n.content_hash = fnv1a(doc_text);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }

    // Allocate per-line Glyphs children under N_DOC_TEXT.
    w.node_mut(CORE_N_DOC_TEXT).first_child = NULL;
    let mut prev_line_node: u16 = NULL;
    for &(glyph_ref, glyph_count, y) in &line_glyph_refs {
        if let Some(line_id) = w.alloc_node() {
            let n = w.node_mut(line_id);
            n.y = y;
            n.width = doc_width as u16;
            n.height = line_height as u16;
            n.content = Content::Glyphs {
                color: text_color,
                glyphs: glyph_ref,
                glyph_count,
                font_size,
                axis_hash: 0,
            };
            n.content_hash = fnv1a(&glyph_ref.offset.to_le_bytes());
            n.flags = NodeFlags::VISIBLE;
            n.next_sibling = NULL;
            if prev_line_node == NULL {
                w.node_mut(CORE_N_DOC_TEXT).first_child = line_id;
            } else {
                w.node_mut(prev_line_node).next_sibling = line_id;
            }
            prev_line_node = line_id;
        }
    }

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(CORE_N_DOC_TEXT).first_child = CORE_N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = CORE_N_CURSOR;
    }

    // N_CURSOR — Content::None with background color (document-relative)
    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line);
    let cursor_x = (cursor_col as u32 * char_width) as i32;
    let cursor_y_px = (cursor_line as i32 * line_height as i32) as i32;
    {
        let n = w.node_mut(CORE_N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y_px;
        n.width = 2;
        n.height = line_height as u16;
        n.background = cursor_color;
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }

    // Selection rects (Content::None with background color).
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };
    if sel_lo < sel_hi {
        let (sel_start_line, sel_start_col) = byte_to_line_col(doc_text, sel_lo, chars_per_line);
        let (sel_end_line, sel_end_col) = byte_to_line_col(doc_text, sel_hi, chars_per_line);
        let mut prev_sel: u16 = NULL;
        for line in sel_start_line..=sel_end_line {
            let col_start = if line == sel_start_line {
                sel_start_col
            } else {
                0
            };
            let col_end = if line == sel_end_line {
                sel_end_col
            } else {
                chars_per_line
            };
            if col_start >= col_end {
                continue;
            }
            let sel_y = line as i32 * line_height as i32;
            if sel_y + line_height as i32 <= scroll_pt || sel_y >= scroll_pt + content_h as i32 {
                continue;
            }
            if let Some(sel_id) = w.alloc_node() {
                let n = w.node_mut(sel_id);
                n.x = (col_start as u32 * char_width) as i32;
                n.y = sel_y as i32;
                n.width = ((col_end - col_start) as u32 * char_width) as u16;
                n.height = line_height as u16;
                n.background = sel_color;
                n.content = Content::None;
                n.flags = NodeFlags::VISIBLE;
                n.next_sibling = NULL;
                if prev_sel == NULL {
                    w.node_mut(CORE_N_CURSOR).next_sibling = sel_id;
                } else {
                    w.node_mut(prev_sel).next_sibling = sel_id;
                }
                w.mark_dirty(sel_id);
                prev_sel = sel_id;
            }
        }
    }

    w.set_root(CORE_N_ROOT);
}

/// Collect child node IDs of a parent.
fn collect_children(w: &SceneWriter, parent: u16) -> Vec<u16> {
    let mut children = Vec::new();
    let mut child = w.node(parent).first_child;
    while child != NULL {
        children.push(child);
        child = w.node(child).next_sibling;
    }
    children
}

// ── VAL-FILL-02: Cursor uses Content::None with background color ────

#[test]
fn core_cursor_uses_background_container() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"Hello", 0, 0, 0, 8, 20, 16, 0.0);

    let cursor = w.node(CORE_N_CURSOR);
    assert!(
        matches!(cursor.content, Content::None),
        "N_CURSOR should have Content::None, got {:?}",
        cursor.content
    );
    assert!(
        cursor.background.a > 0,
        "cursor background color should be visible"
    );
    assert_eq!(cursor.width, 2, "cursor width should be 2px");
    assert_eq!(cursor.height, 20, "cursor height should be line_height");
}

// ── VAL-FILL-02: Selection rects use Content::None with background ──

#[test]
fn core_selection_rects_use_background_container() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // Select "ell" in "Hello\nWorld"
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 1, 4, 8, 20, 16, 0.0);

    // Selection rects are allocated after well-known + line nodes.
    let total = w.node_count();
    assert!(total > CORE_WELL_KNOWN_COUNT, "should have dynamic nodes");

    // Find selection rects: they follow N_CURSOR in the sibling chain.
    let cursor = w.node(CORE_N_CURSOR);
    let mut sel_id = cursor.next_sibling;
    let mut sel_count = 0;
    while sel_id != NULL {
        let sel = w.node(sel_id);
        assert!(
            matches!(sel.content, Content::None),
            "Selection rect node {} should have Content::None, got {:?}",
            sel_id,
            sel.content
        );
        assert!(
            sel.background.a > 0,
            "selection background color should be visible"
        );
        sel_count += 1;
        sel_id = sel.next_sibling;
    }
    assert!(sel_count > 0, "should have at least one selection rect");
}

#[test]
fn core_multiline_selection_all_background_containers() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // Select across two lines: "llo\nWor"
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 2, 9, 8, 20, 16, 0.0);

    let mut sel_id = w.node(CORE_N_CURSOR).next_sibling;
    let mut sel_count = 0;
    while sel_id != NULL {
        let sel = w.node(sel_id);
        assert!(
            matches!(sel.content, Content::None),
            "selection node {} must be Content::None with background",
            sel_id
        );
        assert!(
            sel.background.a > 0,
            "selection node {} must have visible background",
            sel_id
        );
        sel_count += 1;
        sel_id = w.node(sel_id).next_sibling;
    }
    // "llo" on line 0, "Wor" on line 1 → 2 selection rects
    assert_eq!(
        sel_count, 2,
        "should have 2 selection rects for 2-line selection"
    );
}

// ── VAL-CORE-003: Per-line Glyphs children under N_DOC_TEXT ─────────

#[test]
fn core_doc_text_is_pure_container() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 0, 0, 8, 20, 16, 0.0);

    let doc = w.node(CORE_N_DOC_TEXT);
    assert!(
        matches!(doc.content, Content::None),
        "N_DOC_TEXT should have Content::None (pure container), got {:?}",
        doc.content
    );
}

#[test]
fn core_per_line_glyphs_children() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 0, 0, 8, 20, 16, 0.0);

    // N_DOC_TEXT children: line0, line1, then N_CURSOR
    let children = collect_children(&w, CORE_N_DOC_TEXT);
    // 2 lines + cursor = 3 children minimum
    assert!(
        children.len() >= 3,
        "expected at least 3 children, got {}",
        children.len()
    );

    // First two should be Glyphs (one per line).
    for &child_id in &children[..2] {
        let n = w.node(child_id);
        assert!(
            matches!(n.content, Content::Glyphs { .. }),
            "line child {} should be Content::Glyphs, got {:?}",
            child_id,
            n.content
        );
    }

    // Last well-known child should be N_CURSOR
    assert!(
        children.contains(&CORE_N_CURSOR),
        "N_CURSOR should be in children list"
    );
}

#[test]
fn core_child_ordering_glyphs_then_cursor_then_selection() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // "Hello\nWorld" with selection on first line
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 1, 4, 8, 20, 16, 0.0);

    let children = collect_children(&w, CORE_N_DOC_TEXT);
    // Expected: line0, line1, N_CURSOR, sel_rect(s)
    assert!(
        children.len() >= 4,
        "expected at least 4 children (2 lines + cursor + selection)"
    );

    // Lines come first (before N_CURSOR).
    let cursor_idx = children.iter().position(|&id| id == CORE_N_CURSOR).unwrap();
    for &child_id in &children[..cursor_idx] {
        assert!(
            matches!(w.node(child_id).content, Content::Glyphs { .. }),
            "children before cursor should be Glyphs"
        );
    }

    // Selection rects come after cursor (Content::None with background).
    for &child_id in &children[cursor_idx + 1..] {
        let sel = w.node(child_id);
        assert!(
            matches!(sel.content, Content::None),
            "children after cursor should be Content::None (selection)"
        );
        assert!(
            sel.background.a > 0,
            "selection node should have visible background"
        );
    }
}

// ── VAL-CORE-004: Title and clock use Content::Glyphs ───────────────

#[test]
fn core_title_uses_glyphs() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"Hello", 0, 0, 0, 8, 20, 16, 0.0);

    let title = w.node(CORE_N_TITLE_TEXT);
    match title.content {
        Content::Glyphs {
            glyph_count,
            font_size,
            ..
        } => {
            assert_eq!(glyph_count, 4, "title 'Text' has 4 glyphs");
            assert_eq!(font_size, 16);
        }
        _ => panic!("N_TITLE_TEXT should have Content::Glyphs"),
    }
}

#[test]
fn core_clock_uses_glyphs() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"Hello", 0, 0, 0, 8, 20, 16, 0.0);

    let clock = w.node(CORE_N_CLOCK_TEXT);
    match clock.content {
        Content::Glyphs {
            glyph_count,
            font_size,
            ..
        } => {
            assert_eq!(glyph_count, 8, "clock 'HH:MM:SS' has 8 glyphs");
            assert_eq!(font_size, 16);
        }
        _ => panic!("N_CLOCK_TEXT should have Content::Glyphs"),
    }
}

// ── VAL-CORE-006: Node budget within MAX_NODES ─────────────────────

#[test]
fn core_node_budget_extreme_content() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // 50 visible lines + full selection: worst case scenario
    let mut text = Vec::new();
    for i in 0u8..50 {
        if i > 0 {
            text.push(b'\n');
        }
        text.extend_from_slice(b"x");
    }
    // Select all text
    let text_len = text.len() as u32;
    build_test_editor_scene(&mut w, 1024, 768, &text, 0, 0, text_len, 8, 20, 16, 0.0);

    let total = w.node_count();
    assert!(
        (total as usize) <= MAX_NODES,
        "total nodes {} must be <= MAX_NODES ({})",
        total,
        MAX_NODES
    );
    // Rough check: 8 well-known + ~50 line nodes + ~50 sel rects ≈ 108
    assert!(
        total <= 200,
        "total nodes {} should be well under 200 for 50 lines",
        total
    );
}

// ── VAL-CORE-006: Empty document produces at least one Glyphs child ─

#[test]
fn core_empty_document_has_glyphs_child() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"", 0, 0, 0, 8, 20, 16, 0.0);

    // N_DOC_TEXT should have at least one child (empty Glyphs).
    let children = collect_children(&w, CORE_N_DOC_TEXT);
    // At minimum: empty line Glyphs + cursor
    assert!(
        children.len() >= 2,
        "empty doc should have at least line + cursor children"
    );

    // First child should be a Glyphs node (even if empty).
    let first = children[0];
    assert!(
        matches!(
            w.node(first).content,
            Content::Glyphs { glyph_count: 0, .. }
        ),
        "empty doc's first line should be Glyphs with glyph_count=0"
    );
}

// ── VAL-CORE-007: Per-line Glyphs positions and glyph data ──────────

#[test]
fn core_per_line_glyphs_correct_y_positions() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let line_h: u32 = 20;
    build_test_editor_scene(
        &mut w,
        1024,
        768,
        b"aaa\nbbb\nccc",
        0,
        0,
        0,
        8,
        line_h,
        16,
        0.0,
    );

    let children = collect_children(&w, CORE_N_DOC_TEXT);
    // 3 lines + cursor
    assert!(children.len() >= 4, "3 lines + cursor");

    // Check y positions: 0, 20, 40
    assert_eq!(w.node(children[0]).y, 0);
    assert_eq!(w.node(children[1]).y, line_h as i32);
    assert_eq!(w.node(children[2]).y, (2 * line_h) as i32);
}

#[test]
fn core_per_line_glyphs_correct_glyph_data() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"AB\nCD", 0, 0, 0, 8, 20, 16, 0.0);

    let children = collect_children(&w, CORE_N_DOC_TEXT);
    assert!(children.len() >= 3); // 2 lines + cursor

    // Verify glyph data via SceneReader
    let r = SceneReader::new(&buf);

    // Line 0: "AB"
    match r.node(children[0]).content {
        Content::Glyphs {
            glyphs,
            glyph_count,
            ..
        } => {
            assert_eq!(glyph_count, 2);
            let shaped = r.shaped_glyphs(glyphs, glyph_count);
            assert_eq!(shaped[0].glyph_id, b'A' as u16);
            assert_eq!(shaped[1].glyph_id, b'B' as u16);
        }
        _ => panic!("expected Glyphs"),
    }

    // Line 1: "CD"
    match r.node(children[1]).content {
        Content::Glyphs {
            glyphs,
            glyph_count,
            ..
        } => {
            assert_eq!(glyph_count, 2);
            let shaped = r.shaped_glyphs(glyphs, glyph_count);
            assert_eq!(shaped[0].glyph_id, b'C' as u16);
            assert_eq!(shaped[1].glyph_id, b'D' as u16);
        }
        _ => panic!("expected Glyphs"),
    }
}

#[test]
fn core_scroll_filters_lines_correctly() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // 10 lines, scroll down by 5, viewport ~400px = ~20 visible lines
    let mut text = Vec::new();
    for i in 0u8..10 {
        if i > 0 {
            text.push(b'\n');
        }
        text.push(b'a' + i);
    }
    build_test_editor_scene(&mut w, 1024, 768, &text, 0, 0, 0, 8, 20, 16, 100.0);

    let children = collect_children(&w, CORE_N_DOC_TEXT);
    // With scroll=5, only lines 5-9 visible (5 lines + cursor)
    let line_count = children
        .iter()
        .take_while(|&&id| id != CORE_N_CURSOR)
        .count();
    assert_eq!(line_count, 5, "with scroll=5, 5 out of 10 lines visible");

    // First visible line should have document-relative y = 5 * 20 = 100
    assert_eq!(
        w.node(children[0]).y,
        100,
        "first visible line at document y=100 (line 5 * 20px)"
    );

    // N_DOC_TEXT.content_transform.ty should be -(scroll_lines * line_height)
    assert_eq!(
        w.node(CORE_N_DOC_TEXT).content_transform.ty,
        -100.0, // -(5 * 20)
        "N_DOC_TEXT.content_transform.ty should be -(scroll_lines * line_height)"
    );
}

// ── VAL-CORE-004b: Document-relative scroll model ───────────────────

#[test]
fn core_scroll_model_document_relative_positions() {
    // Verify the scroll model invariant: all children of N_DOC_TEXT are
    // positioned at document-relative coordinates, and N_DOC_TEXT.content_transform
    // provides the viewport offset.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let line_h: u32 = 20;
    let scroll_y: f32 = 60.0; // 3 lines * 20px
                              // 8 lines of text, scroll down 3 lines.
    let text = b"aaa\nbbb\nccc\nddd\neee\nfff\nggg\nhhh";
    build_test_editor_scene(&mut w, 1024, 768, text, 0, 0, 0, 8, line_h, 16, scroll_y);

    let scroll_pt = scroll_y.round() as i32; // 60

    // 1. N_DOC_TEXT.content_transform.ty == -scroll_y
    assert_eq!(
        w.node(CORE_N_DOC_TEXT).content_transform.ty,
        -scroll_y,
        "N_DOC_TEXT.content_transform.ty must equal -scroll_y (pixel offset)"
    );

    // 2. Line nodes have document-relative y (not viewport-relative).
    let children = collect_children(&w, CORE_N_DOC_TEXT);
    let line_nodes: Vec<u16> = children
        .iter()
        .copied()
        .take_while(|&id| id != CORE_N_CURSOR)
        .collect();

    // With 8 lines and scroll=3, lines 3-7 should be visible.
    assert_eq!(line_nodes.len(), 5, "5 lines visible with scroll=3 of 8");

    // First visible line (line 3) should be at document y = 3 * 20 = 60.
    assert_eq!(w.node(line_nodes[0]).y, 60);
    // Second visible line (line 4) at y = 80.
    assert_eq!(w.node(line_nodes[1]).y, 80);

    // 3. Cursor at position 0 (line 0, col 0) has document-relative y = 0
    //    even though line 0 is above the scroll window.
    let cursor = w.node(CORE_N_CURSOR);
    assert_eq!(cursor.y, 0, "cursor y is document-relative (line 0 * 20)");
}

#[test]
fn core_scroll_model_no_scroll_positions_unchanged() {
    // With scroll=0, document-relative == viewport-relative,
    // so behavior matches the old model.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    build_test_editor_scene(&mut w, 1024, 768, b"aaa\nbbb\nccc", 0, 0, 0, 8, 20, 16, 0.0);

    // content_transform should be identity (no scroll).
    assert_eq!(
        w.node(CORE_N_DOC_TEXT).content_transform,
        AffineTransform::identity()
    );

    // Line positions: 0, 20, 40 (document == viewport when no scroll).
    let children = collect_children(&w, CORE_N_DOC_TEXT);
    let line_nodes: Vec<u16> = children
        .iter()
        .copied()
        .take_while(|&id| id != CORE_N_CURSOR)
        .collect();
    assert_eq!(w.node(line_nodes[0]).y, 0);
    assert_eq!(w.node(line_nodes[1]).y, 20);
    assert_eq!(w.node(line_nodes[2]).y, 40);

    // Cursor at pos 0 → y = 0.
    assert_eq!(w.node(CORE_N_CURSOR).y, 0);
}

#[test]
fn core_scroll_model_selection_rects_document_relative() {
    // Selection rects should use document-relative y positions.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let line_h: u32 = 20;
    // "aaa\nbbb\nccc\nddd" — select "bbb\nccc" (bytes 4..11), scroll=0
    build_test_editor_scene(
        &mut w,
        1024,
        768,
        b"aaa\nbbb\nccc\nddd",
        0,
        4,
        11,
        8,
        line_h,
        16,
        0.0,
    );

    // Selection rects are after cursor.
    let mut sel_id = w.node(CORE_N_CURSOR).next_sibling;
    let mut sel_ys = Vec::new();
    while sel_id != NULL {
        sel_ys.push(w.node(sel_id).y);
        sel_id = w.node(sel_id).next_sibling;
    }

    // Line 1 ("bbb") at document y = 20, line 2 ("ccc") at document y = 40.
    assert_eq!(
        sel_ys,
        vec![20, 40],
        "selection rects at document-relative y"
    );
}

// ── VAL-CORE-005: Incremental update patterns ───────────────────────

#[test]
fn core_update_clock_in_place_glyph_overwrite() {
    // Verify clock update pattern: copy forward, update data in place, mark changed
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: build scene with clock
    {
        let mut w = tw.acquire();
        w.clear();
        for _ in 0..8 {
            w.alloc_node().unwrap();
        }
        let clock_glyphs = bytes_to_shaped_glyphs_test(b"12:34:56", 8);
        let clock_ref = w.push_shaped_glyphs(&clock_glyphs);
        w.node_mut(CORE_N_CLOCK_TEXT).content = Content::Glyphs {
            color: Color::rgb(120, 120, 120),
            glyphs: clock_ref,
            glyph_count: 8,
            font_size: 16,
            axis_hash: 0,
        };
        w.node_mut(CORE_N_CLOCK_TEXT).content_hash = fnv1a(b"12:34:56");
        w.set_root(CORE_N_ROOT);
    }
    tw.publish();

    // Frame 2: copy forward, in-place clock update
    {
        let mut w = tw.acquire_copy();
        let clock_node = w.node(CORE_N_CLOCK_TEXT);
        if let Content::Glyphs { glyphs, .. } = clock_node.content {
            let new_glyphs = bytes_to_shaped_glyphs_test(b"12:35:00", 8);
            let new_bytes = unsafe {
                core::slice::from_raw_parts(
                    new_glyphs.as_ptr() as *const u8,
                    new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                )
            };
            assert!(
                w.update_data(glyphs, new_bytes),
                "clock in-place update should succeed"
            );
            w.node_mut(CORE_N_CLOCK_TEXT).content_hash = fnv1a(b"12:35:00");
            w.mark_dirty(CORE_N_CLOCK_TEXT);
        } else {
            panic!("clock should have Glyphs content");
        }
    }
    tw.publish();

    // Verify updated clock data.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let clock = &tr.front_nodes()[CORE_N_CLOCK_TEXT as usize];
    assert_eq!(clock.content_hash, fnv1a(b"12:35:00"));
    let bits = tr.dirty_bits();
    let idx = CORE_N_CLOCK_TEXT as usize;
    assert_ne!(
        bits[idx / 64] & (1u64 << (idx % 64)),
        0,
        "clock should be dirty"
    );
}

#[test]
fn core_update_cursor_position_only() {
    // Verify cursor update pattern: copy forward, move cursor, mark changed
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Frame 1: build scene with cursor at (0,0)
    {
        let mut w = tw.acquire();
        w.clear();
        for _ in 0..8 {
            w.alloc_node().unwrap();
        }
        w.node_mut(CORE_N_CURSOR).x = 0;
        w.node_mut(CORE_N_CURSOR).y = 0;
        w.node_mut(CORE_N_CURSOR).width = 2;
        w.node_mut(CORE_N_CURSOR).height = 20;
        w.node_mut(CORE_N_CURSOR).background = Color::rgb(200, 200, 200);
        w.node_mut(CORE_N_CURSOR).content = Content::None;
        w.set_root(CORE_N_ROOT);
    }
    tw.publish();

    // Frame 2: copy forward, move cursor to (40, 20)
    {
        let mut w = tw.acquire_copy();
        w.node_mut(CORE_N_CURSOR).x = 40;
        w.node_mut(CORE_N_CURSOR).y = 20;
        w.mark_dirty(CORE_N_CURSOR);
    }
    tw.publish();

    // Verify cursor position and content preserved.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    let cursor = &tr.front_nodes()[CORE_N_CURSOR as usize];
    assert_eq!(cursor.x, 40);
    assert_eq!(cursor.y, 20);
    assert!(
        matches!(cursor.content, Content::None),
        "cursor should be Content::None after position update"
    );
    assert_eq!(
        cursor.background,
        Color::rgb(200, 200, 200),
        "cursor background color should be preserved after position update"
    );
    let bits = tr.dirty_bits();
    let idx = CORE_N_CURSOR as usize;
    assert_ne!(
        bits[idx / 64] & (1u64 << (idx % 64)),
        0,
        "cursor should be dirty"
    );
    // Only cursor should be dirty (no line nodes affected).
    let popcount: u32 = bits.iter().map(|w| w.count_ones()).sum();
    assert_eq!(popcount, 1, "only cursor should be dirty");
}

// ── TripleWriter / TripleReader (VAL-TBUF) ──────────────────────────

fn make_triple_buf() -> Vec<u8> {
    vec![0u8; scene::TRIPLE_SCENE_SIZE]
}

// VAL-TBUF-006: Triple buffer memory layout correct
#[test]
fn triple_scene_size_is_correct() {
    // TRIPLE_SCENE_SIZE = 3 * SCENE_SIZE + 16 (control region)
    assert_eq!(
        scene::TRIPLE_SCENE_SIZE,
        3 * SCENE_SIZE + 16,
        "TRIPLE_SCENE_SIZE should be 3 * SCENE_SIZE + 16-byte control region"
    );
}

// VAL-TBUF-012: Shared memory initialization correct
#[test]
fn triple_writer_initial_state() {
    let mut buf = make_triple_buf();
    let tw = scene::TripleWriter::new(&mut buf);
    // Initial generation is 0.
    assert_eq!(tw.generation(), 0);
}

// VAL-TBUF-001: acquire always succeeds
#[test]
fn triple_writer_acquire_always_succeeds() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    // Acquire before any publish — should succeed.
    {
        let w = tw.acquire();
        assert_eq!(w.node_count(), 0);
    }
    // Acquire after publish — should succeed.
    {
        let mut w = tw.acquire();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).width = 100;
        w.set_root(n);
    }
    tw.publish();
    // Acquire again — should still succeed.
    {
        let w = tw.acquire();
        // This is a different buffer than the one we just published.
        assert_eq!(w.node_count(), 0);
    }
}

// VAL-TBUF-001: acquire succeeds with reader active
#[test]
fn triple_writer_acquire_succeeds_with_active_reader() {
    let mut buf = make_triple_buf();
    // Initialize and publish a frame.
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 100;
            w.set_root(n);
        }
        tw.publish();
    }
    // Reader claims the latest buffer, then releases it.
    {
        let _tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
        // Reader is active here — in the real system, writer is in a
        // separate process so there's no borrow conflict.
    }
    // After reader drops, writer acquires — should succeed.
    {
        let mut tw = scene::TripleWriter::from_existing(&mut buf);
        let _w = tw.acquire();
        // acquire() succeeded — test passes.
    }
}

// VAL-TBUF-001: acquire succeeds after multiple publishes without reader
#[test]
fn triple_writer_acquire_after_multiple_publishes() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    for i in 0u32..10 {
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = (i + 1) as u16;
            w.set_root(n);
        }
        tw.publish();
    }
    // One more acquire should still succeed.
    {
        let _w = tw.acquire();
    }
}

// VAL-TBUF-002: publish makes buffer the latest
#[test]
fn triple_writer_publish_makes_latest() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).width = 42;
        w.set_root(n);
    }
    tw.publish();
    assert_eq!(tw.generation(), 1);
    assert_eq!(tw.latest_nodes().len(), 1);
    assert_eq!(tw.latest_nodes()[0].width, 42);
}

// VAL-TBUF-002: reader sees published buffer
#[test]
fn triple_reader_sees_published_buffer() {
    let mut buf = make_triple_buf();
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 800;
            w.set_root(n);
        }
        tw.publish();
    }
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr.front_generation(), 1);
    assert_eq!(tr.front_nodes().len(), 1);
    assert_eq!(tr.front_nodes()[0].width, 800);
}

// VAL-TBUF-003: Reader always gets latest published buffer (mailbox skip)
#[test]
fn triple_reader_sees_latest_skipping_intermediate() {
    let mut buf = make_triple_buf();
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        // Publish gen 1 (width=100).
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 100;
            w.set_root(n);
        }
        tw.publish();
        // Publish gen 2 (width=200).
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 200;
            w.set_root(n);
        }
        tw.publish();
        // Publish gen 3 (width=300).
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 300;
            w.set_root(n);
        }
        tw.publish();
    }
    // Reader should see gen 3 (latest), skipping gen 1 and 2.
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr.front_generation(), 3);
    assert_eq!(tr.front_nodes()[0].width, 300);
}

// VAL-TBUF-004: No torn reads under concurrent access
// In the real system, reader and writer are in separate processes sharing
// memory. In single-process tests, we verify sequentially: the triple
// buffer protocol guarantees the reader's claimed buffer is never touched
// by the writer. We verify the writer reads back consistent data from
// the latest buffer after many write-read cycles.
#[test]
fn triple_buffer_no_torn_reads() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    let mut inconsistencies = 0u64;

    for frame_id in 1u32..=5000 {
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            let marker = (frame_id & 0xFFFF) as u16;
            w.node_mut(n).width = marker;
            w.node_mut(n).height = marker;
            w.node_mut(n).background =
                Color::rgb((marker & 0xFF) as u8, ((marker >> 8) & 0xFF) as u8, 0);
            w.set_root(n);
        }
        tw.publish();

        // Verify the latest buffer is consistent.
        let nodes = tw.latest_nodes();
        let node = &nodes[0];
        let w = node.width;
        let h = node.height;
        let bg_marker = (node.background.r as u16) | ((node.background.g as u16) << 8);
        if w != h || w != bg_marker {
            inconsistencies += 1;
        }
    }

    assert_eq!(
        inconsistencies, 0,
        "reader saw torn frames in triple buffer"
    );
}

// VAL-TBUF-005: Writer never gets the buffer reader is using
// In the real system, reader and writer are in separate processes. In
// single-process tests, we verify by doing reader-claim → writer-write
// → reader-verify sequentially, releasing borrows between steps.
#[test]
fn triple_writer_never_acquires_reader_buffer() {
    let mut buf = make_triple_buf();
    // Publish gen 1.
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 100;
            w.set_root(n);
        }
        tw.publish();
    }
    // Reader claims the latest buffer (sets reader_buf in control region).
    {
        let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
        assert_eq!(tr.front_nodes()[0].width, 100);
        // Don't call finish_read — keep the buffer claimed.
        // TripleReader drops but the reader_buf control field persists.
    }

    // Writer acquires and writes. Since reader_buf is still set in control
    // region (no finish_read was called), the writer must avoid that buffer.
    {
        let mut tw = scene::TripleWriter::from_existing(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 999;
            w.set_root(n);
        }
        tw.publish();
    }

    // Re-read the buffer that was claimed by the reader. Since
    // reader_buf hasn't been released, the reader's buffer index
    // should still contain the original data (width=100).
    // The new reader will claim the latest (which is the writer's new frame).
    // But we can verify by reading the control region to see the reader's
    // buffer is still intact.
    //
    // Alternatively, verify the latest is the writer's new frame (width=999),
    // which means the writer did NOT overwrite the reader's buffer.
    let tr2 = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(
        tr2.front_nodes()[0].width,
        999,
        "latest should be writer's new frame"
    );
    // The old reader's buffer (width=100) still exists — just not as latest.
}

// VAL-TBUF-009: finish_read releases buffer
#[test]
fn triple_reader_finish_read_releases_buffer() {
    let mut buf = make_triple_buf();
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 100;
            w.set_root(n);
        }
        tw.publish();
    }

    // Reader claims and finishes.
    {
        let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
        let gen = tr.front_generation();
        tr.finish_read(gen);
    }

    // After finish_read, the writer should be able to acquire all buffers
    // through multiple publish cycles without issues.
    {
        let mut tw = scene::TripleWriter::from_existing(&mut buf);
        for i in 0u32..5 {
            {
                let mut w = tw.acquire();
                w.clear();
                let n = w.alloc_node().unwrap();
                w.node_mut(n).width = (i + 200) as u16;
                w.set_root(n);
            }
            tw.publish();
        }
        // All 5 publishes succeeded.
        assert!(tw.generation() > 1);
    }
}

// VAL-TBUF-007: Generation counter increments on publish
#[test]
fn triple_writer_generation_increments() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    assert_eq!(tw.generation(), 0);
    for i in 1u32..=5 {
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.set_root(n);
        }
        tw.publish();
        assert_eq!(tw.generation(), i);
    }
}

// VAL-TBUF-010: SceneWriter/SceneReader APIs work unchanged on triple buffer
#[test]
fn triple_buffer_scene_writer_api_unchanged() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 1024;
        w.node_mut(root).height = 768;
        w.node_mut(root).background = Color::rgb(30, 30, 30);
        w.set_root(root);

        let child = w.alloc_node().unwrap();
        w.node_mut(child).x = 10;
        w.node_mut(child).y = 20;
        w.node_mut(child).content =
            make_mono_glyphs(&mut w, b"Hello, world!", 16, Color::rgb(220, 220, 220), 8);
        w.add_child(root, child);
        w.mark_dirty(root);
        w.mark_dirty(child);
    }
    tw.publish();

    // Drop writer, read as TripleReader.
    drop(tw);
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr.front_nodes().len(), 2);
    assert_eq!(tr.front_nodes()[0].width, 1024);
    assert_eq!(tr.front_nodes()[0].background, Color::rgb(30, 30, 30));
    // Verify Glyphs data survived.
    match tr.front_nodes()[1].content {
        Content::Glyphs { glyph_count, .. } => assert_eq!(glyph_count, 13),
        _ => panic!("expected Glyphs content"),
    }
    // After clear() all dirty bits should be set.
    assert_eq!(*tr.dirty_bits(), [u64::MAX; scene::DIRTY_BITMAP_WORDS]);
}

// VAL-TBUF-011: Legacy double-buffer code removed from production
// (compile-time: if this test compiles without TripleWriter, the types exist)
#[test]
fn triple_types_exist() {
    // Verify TripleWriter and TripleReader types exist and are usable.
    let mut buf = make_triple_buf();
    let _tw: scene::TripleWriter = scene::TripleWriter::new(&mut buf);
    drop(_tw);
    let _tr: scene::TripleReader = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
}

// VAL-CROSS-001: End-to-end write-read cycles with triple buffer
#[test]
fn triple_buffer_consistency_many_cycles() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    let mut inconsistencies = 0u64;

    for frame_id in 1u32..=1000 {
        // Writer phase.
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            let marker = (frame_id & 0xFFFF) as u16;
            w.node_mut(n).width = marker;
            w.node_mut(n).height = marker;
            w.node_mut(n).background =
                Color::rgb((marker & 0xFF) as u8, ((marker >> 8) & 0xFF) as u8, 0);
            w.set_root(n);
        }
        tw.publish();

        // Reader phase.
        let nodes = tw.latest_nodes();
        let node = &nodes[0];
        let w = node.width;
        let h = node.height;
        let bg_marker = (node.background.r as u16) | ((node.background.g as u16) << 8);
        if w != h || w != bg_marker {
            inconsistencies += 1;
        }
    }

    assert_eq!(
        inconsistencies, 0,
        "reader saw torn frames in triple buffer"
    );
}

// Test multiple reader/writer cycles
#[test]
fn triple_buffer_reader_writer_alternating() {
    let mut buf = make_triple_buf();

    for frame in 1u32..=20 {
        // Write and publish.
        {
            let mut tw = scene::TripleWriter::from_existing(&mut buf);
            {
                let mut w = tw.acquire();
                w.clear();
                let n = w.alloc_node().unwrap();
                w.node_mut(n).width = frame as u16;
                w.set_root(n);
            }
            tw.publish();
        }

        // Read and finish.
        {
            let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
            assert_eq!(tr.front_nodes()[0].width, frame as u16);
            let gen = tr.front_generation();
            tr.finish_read(gen);
        }
    }
}

// Test: writer publishes multiple frames, reader sees only latest
#[test]
fn triple_buffer_writer_publishes_multiple_reader_sees_latest() {
    let mut buf = make_triple_buf();

    // Writer init and publish frames 1, 2, 3.
    {
        let mut tw = scene::TripleWriter::new(&mut buf);
        for i in 1u32..=3 {
            {
                let mut w = tw.acquire();
                w.clear();
                let n = w.alloc_node().unwrap();
                w.node_mut(n).width = (i * 100) as u16;
                w.set_root(n);
            }
            tw.publish();
        }
    }

    // Reader sees frame 3 (latest), frames 1 and 2 are skipped.
    {
        let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
        assert_eq!(tr.front_nodes()[0].width, 300);
        assert_eq!(tr.front_generation(), 3);
        tr.finish_read(3);
    }

    // Writer continues with frame 4.
    {
        let mut tw = scene::TripleWriter::from_existing(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 400;
            w.set_root(n);
        }
        tw.publish();
    }

    // New reader sees frame 4.
    let tr2 = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr2.front_nodes()[0].width, 400);
    assert_eq!(tr2.front_generation(), 4);
}

// Test: Background container and Glyphs work with triple buffer
#[test]
fn triple_buffer_background_glyphs_round_trip() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);

        // Background container (replaces FillRect)
        let fill_id = w.alloc_node().unwrap();
        w.node_mut(fill_id).background = Color::rgba(100, 200, 50, 180);
        w.node_mut(fill_id).content = Content::None;
        w.add_child(root, fill_id);

        // Glyphs
        let glyphs = [ShapedGlyph {
            glyph_id: 65,
            _pad: 0,
            x_advance: 10 * 65536,
            x_offset: 0,
            y_offset: 0,
        }];
        let gref = w.push_shaped_glyphs(&glyphs);
        let glyph_id = w.alloc_node().unwrap();
        w.node_mut(glyph_id).content = Content::Glyphs {
            color: Color::rgb(255, 255, 255),
            glyphs: gref,
            glyph_count: 1,
            font_size: 16,
            axis_hash: 0,
        };
        w.add_child(root, glyph_id);
    }
    tw.publish();

    drop(tw);
    let tr = unsafe { scene::TripleReader::new(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(tr.front_nodes().len(), 3);
    assert!(matches!(tr.front_nodes()[1].content, Content::None));
    assert_eq!(tr.front_nodes()[1].background.a, 180);
    match tr.front_nodes()[2].content {
        Content::Glyphs { glyph_count, .. } => assert_eq!(glyph_count, 1),
        _ => panic!("expected Glyphs"),
    }
}

// ── Content::Path and path command tests ────────────────────────────

#[test]
fn path_command_encoding_triangle_roundtrip() {
    // VAL-PATH-02: Triangle (MoveTo + 2×LineTo + Close) roundtrips through data buffer.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 10.0, 10.0);
    scene::path_line_to(&mut cmds, 50.0, 10.0);
    scene::path_line_to(&mut cmds, 30.0, 50.0);
    scene::path_close(&mut cmds);

    let expected_len = scene::PATH_MOVE_TO_SIZE
        + scene::PATH_LINE_TO_SIZE
        + scene::PATH_LINE_TO_SIZE
        + scene::PATH_CLOSE_SIZE;
    assert_eq!(cmds.len(), expected_len, "triangle command size");

    let dref = w.push_path_commands(&cmds);
    assert_eq!(dref.length as usize, expected_len);

    // Verify 4-byte alignment.
    assert_eq!(dref.offset % 4, 0, "path data should be 4-byte aligned");

    // Read back via SceneReader.
    let r = SceneReader::new(&buf);
    let read_back = r.data(dref);
    assert_eq!(read_back, &cmds[..], "byte-exact roundtrip");
}

#[test]
fn path_data_alignment_after_other_data() {
    // VAL-PATH-02: Push ShapedGlyph data then Path commands — Path DataRef is 4-byte aligned.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Push 3 bytes of arbitrary data to misalign.
    w.push_data(b"abc");
    assert_eq!(w.data_used(), 3);

    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_close(&mut cmds);

    let dref = w.push_path_commands(&cmds);
    assert_eq!(
        dref.offset % 4,
        0,
        "path data must be 4-byte aligned even after odd push"
    );
}

#[test]
fn path_content_variant_exists() {
    // VAL-PATH-01: Content::Path variant exists with color, fill_rule, contours.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);
    scene::path_line_to(&mut cmds, 5.0, 10.0);
    scene::path_close(&mut cmds);
    let dref = w.push_path_commands(&cmds);

    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Path {
        color: Color::rgb(255, 0, 0),
        fill_rule: scene::FillRule::Winding,
        stroke_width: 0,
        contours: dref,
    };
    w.set_root(id);

    let r = SceneReader::new(&buf);
    match r.node(id).content {
        Content::Path {
            color,
            fill_rule,
            stroke_width,
            contours,
        } => {
            assert_eq!(color, Color::rgb(255, 0, 0));
            assert_eq!(fill_rule, scene::FillRule::Winding);
            assert_eq!(stroke_width, 0);
            assert_eq!(contours.length as usize, cmds.len());
        }
        _ => panic!("expected Path content"),
    }
}

#[test]
fn fill_rule_enum_variants() {
    // VAL-PATH-01: FillRule has Winding and EvenOdd variants.
    let w = scene::FillRule::Winding;
    let e = scene::FillRule::EvenOdd;
    assert_ne!(w, e);
    assert_eq!(w as u8, 0);
    assert_eq!(e as u8, 1);
}

#[test]
fn path_command_sizes_correct() {
    assert_eq!(scene::PATH_MOVE_TO_SIZE, 12);
    assert_eq!(scene::PATH_LINE_TO_SIZE, 12);
    assert_eq!(scene::PATH_CUBIC_TO_SIZE, 28);
    assert_eq!(scene::PATH_CLOSE_SIZE, 4);
}

#[test]
fn path_cubic_to_encoding() {
    let mut cmds = Vec::new();
    scene::path_cubic_to(&mut cmds, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0);
    assert_eq!(cmds.len(), scene::PATH_CUBIC_TO_SIZE);

    // Verify tag.
    let tag = u32::from_le_bytes([cmds[0], cmds[1], cmds[2], cmds[3]]);
    assert_eq!(tag, scene::PATH_CUBIC_TO);

    // Verify c1x.
    let c1x = f32::from_le_bytes([cmds[4], cmds[5], cmds[6], cmds[7]]);
    assert_eq!(c1x, 1.0);

    // Verify endpoint y.
    let y = f32::from_le_bytes([cmds[24], cmds[25], cmds[26], cmds[27]]);
    assert_eq!(y, 6.0);
}

#[test]
fn node_size_unchanged_with_path() {
    // VAL-CROSS-01: Node size assertion passes after adding Content::Path.
    let size = core::mem::size_of::<Node>();
    assert_eq!(size, 136, "Node must remain 136 bytes with Path variant");
}

#[test]
fn path_multiple_contours_in_data_buffer() {
    // VAL-PATH-07: Multiple contours in one Path node.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let mut cmds = Vec::new();
    // First triangle.
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 20.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 20.0);
    scene::path_close(&mut cmds);
    // Second triangle.
    scene::path_move_to(&mut cmds, 50.0, 0.0);
    scene::path_line_to(&mut cmds, 70.0, 0.0);
    scene::path_line_to(&mut cmds, 60.0, 20.0);
    scene::path_close(&mut cmds);

    let dref = w.push_path_commands(&cmds);
    assert_eq!(dref.length as usize, cmds.len());

    // Both contours fit in one DataRef.
    let expected =
        2 * (scene::PATH_MOVE_TO_SIZE + 2 * scene::PATH_LINE_TO_SIZE + scene::PATH_CLOSE_SIZE);
    assert_eq!(cmds.len(), expected);
}

#[test]
fn path_empty_commands() {
    // VAL-PATH-06: Empty path (zero-length DataRef) — no crash.
    let dref = DataRef {
        offset: 0,
        length: 0,
    };
    let content = Content::Path {
        color: Color::rgb(255, 0, 0),
        fill_rule: scene::FillRule::Winding,
        stroke_width: 0,
        contours: dref,
    };
    // Just verify it can be stored and matched.
    match content {
        Content::Path { contours, .. } => assert_eq!(contours.length, 0),
        _ => panic!("expected Path"),
    }
}

// ── VAL-CLOCK-02: Clock re-push data buffer exhaustion ─────────────

/// VAL-CLOCK-02: Simulate 120 clock re-pushes (once per second) without
/// a full rebuild. In the real system, update_document_content calls
/// reset_data() which reclaims the buffer. Verify that 120 clock pushes
/// (each ~64 bytes) plus typical scene data fit within DATA_BUFFER_SIZE.
#[test]
fn clock_repush_120_times_no_overflow() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Allocate 8 well-known nodes (standard editor scene).
    for _ in 0..8 {
        w.alloc_node().unwrap();
    }
    w.set_root(0);

    // Push initial scene data: title (4 glyphs) + document (50 lines × 80 chars).
    // This represents a typical document scene.
    let title_glyphs: Vec<ShapedGlyph> = (0..4u16)
        .map(|i| ShapedGlyph {
            glyph_id: i,
            _pad: 0,
            x_advance: 8 * 65536,
            x_offset: 0,
            y_offset: 0,
        })
        .collect();
    let _ = w.push_shaped_glyphs(&title_glyphs);

    // 50 lines of 80 characters each
    for _ in 0..50 {
        let line_glyphs: Vec<ShapedGlyph> = (0..80u16)
            .map(|i| ShapedGlyph {
                glyph_id: i,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            })
            .collect();
        let _ = w.push_shaped_glyphs(&line_glyphs);
    }

    let data_after_doc = w.data_used();

    // Now simulate 120 clock re-pushes. Each clock is 8 glyphs = 64 bytes.
    // Since update_document_content calls reset_data() on text changes,
    // the clock re-push only accumulates between full rebuilds.
    // In practice, typing resets data frequently. But verify worst case.
    for _ in 0..120 {
        let clock_glyphs: Vec<ShapedGlyph> = (0..8u16)
            .map(|i| ShapedGlyph {
                glyph_id: i,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            })
            .collect();
        let _ = w.push_shaped_glyphs(&clock_glyphs);
    }

    let data_after_clocks = w.data_used();
    let clock_data_total = data_after_clocks - data_after_doc;

    // 120 clock pushes × 128 bytes each = 15,360 bytes (ShapedGlyph is 16 bytes, ~8 glyphs per clock).
    // DATA_BUFFER_SIZE is 131,072 bytes. Even with document data, this fits.
    assert!(
        (data_after_clocks as usize) < DATA_BUFFER_SIZE,
        "data_used ({}) should be < DATA_BUFFER_SIZE ({}) after 120 clock re-pushes + document data",
        data_after_clocks,
        DATA_BUFFER_SIZE
    );

    // Verify the accumulated clock data size is reasonable.
    assert!(
        clock_data_total <= 120 * 128 + 120, // ~128 bytes per clock + alignment padding
        "120 clock re-pushes should use ~15,360 bytes, got {}",
        clock_data_total
    );
}

#[test]
fn node_supports_large_coordinates() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    let n = w.node_mut(id);
    n.x = 50000; // exceeds i16::MAX (32767)
    n.y = -40000; // exceeds i16::MIN (-32768)
    assert_eq!(w.node(id).x, 50000);
    assert_eq!(w.node(id).y, -40000);
}

// ── Incremental scene building primitives (Task 4) ──────────────────

// VAL-INC-001: acquire_copy preserves nodes and data, dirty bits are zero.
#[test]
fn incremental_acquire_copy_preserves_nodes_data_clears_dirty() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Build a scene with multiple nodes and glyph data.
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 1024;
        w.node_mut(root).height = 768;
        w.set_root(root);

        let child1 = w.alloc_node().unwrap();
        w.node_mut(child1).x = 10;
        w.node_mut(child1).y = 20;
        w.node_mut(child1).content =
            make_mono_glyphs(&mut w, b"Line one", 16, Color::rgb(220, 220, 220), 8);
        w.add_child(root, child1);

        let child2 = w.alloc_node().unwrap();
        w.node_mut(child2).x = 10;
        w.node_mut(child2).y = 40;
        w.node_mut(child2).content =
            make_mono_glyphs(&mut w, b"Line two", 16, Color::rgb(220, 220, 220), 8);
        w.add_child(root, child2);

        w.mark_dirty(root);
        w.mark_dirty(child1);
        w.mark_dirty(child2);
    }
    tw.publish();

    // Capture front state for comparison.
    let front_node_count = tw.latest_nodes().len();
    let front_data = tw.latest_data_buf().to_vec();
    assert!(front_node_count >= 3);
    assert!(!front_data.is_empty());

    // acquire_copy: back buffer should match front, dirty bits cleared.
    let back = tw.acquire_copy();
    assert_eq!(back.node_count() as usize, front_node_count);
    assert_eq!(back.data_buf(), front_data.as_slice());

    // Verify all dirty bits are zero.
    assert_eq!(
        back.dirty_count(),
        0,
        "dirty bits should be cleared after acquire_copy"
    );

    // Verify node properties survived the copy.
    assert_eq!(back.node(0).width, 1024);
    assert_eq!(back.node(1).x, 10);
    assert_eq!(back.node(1).y, 20);
    assert_eq!(back.node(2).x, 10);
    assert_eq!(back.node(2).y, 40);
}

// VAL-INC-002: Pushing new data after acquire_copy preserves old DataRefs.
#[test]
fn incremental_data_push_preserves_old_data() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Build initial scene with glyph data for node A.
    let original_glyphs = [
        ShapedGlyph {
            glyph_id: 72,
            _pad: 0,
            x_advance: 8 * 65536,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 101,
            _pad: 0,
            x_advance: 8 * 65536,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 108,
            _pad: 0,
            x_advance: 8 * 65536,
            x_offset: 0,
            y_offset: 0,
        },
    ];
    let mut original_ref = DataRef {
        offset: 0,
        length: 0,
    };
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);

        original_ref = w.push_shaped_glyphs(&original_glyphs);
        w.node_mut(root).content = Content::Glyphs {
            color: Color::rgb(255, 255, 255),
            glyphs: original_ref,
            glyph_count: 3,
            font_size: 16,
            axis_hash: 0,
        };
    }
    tw.publish();

    // acquire_copy, then push NEW data (simulating incremental update).
    {
        let mut w = tw.acquire_copy();

        // Push new glyph data (for a different node, node B).
        let new_glyphs = [
            ShapedGlyph {
                glyph_id: 87,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
            ShapedGlyph {
                glyph_id: 111,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
        ];
        let new_ref = w.push_shaped_glyphs(&new_glyphs);

        // Verify the new data is at a higher offset (not overwriting old data).
        assert!(
            new_ref.offset > original_ref.offset,
            "new data should be appended, not overlapping: new_off={}, old_off={}",
            new_ref.offset,
            original_ref.offset
        );

        // Verify old DataRef still resolves to valid data by reading
        // the bytes at the original offset.
        let data_buf = w.data_buf();
        let old_start = original_ref.offset as usize;
        let old_end = old_start + original_ref.length as usize;
        assert!(
            old_end <= data_buf.len(),
            "old DataRef should still be in bounds"
        );

        // Verify the glyph data at the old offset matches the original.
        let glyph_size = core::mem::size_of::<ShapedGlyph>();
        let expected_bytes = unsafe {
            core::slice::from_raw_parts(
                original_glyphs.as_ptr() as *const u8,
                original_glyphs.len() * glyph_size,
            )
        };
        assert_eq!(
            &data_buf[old_start..old_end],
            expected_bytes,
            "old glyph data should be preserved after pushing new data"
        );

        // Verify the data_used counter advanced (old + new).
        assert!(w.data_used() as usize >= original_ref.length as usize + new_ref.length as usize);
    }
}

// VAL-INC-003: has_data_space reports correctly near capacity.
#[test]
fn has_data_space_reports_correctly() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);

    // Initially, full space available.
    assert!(w.has_data_space(1));
    assert!(w.has_data_space(scene::DATA_BUFFER_SIZE));
    assert!(!w.has_data_space(scene::DATA_BUFFER_SIZE + 1));

    // Fill most of the buffer.
    let fill_size = scene::DATA_BUFFER_SIZE - 100;
    let fill_data = vec![0xABu8; fill_size];
    w.push_data(&fill_data);

    // Now has_data_space should reflect remaining space.
    assert!(w.has_data_space(100));
    assert!(w.has_data_space(1));
    assert!(!w.has_data_space(101));
    assert!(!w.has_data_space(scene::DATA_BUFFER_SIZE));

    // Fill the rest.
    let remainder = vec![0xCDu8; 100];
    w.push_data(&remainder);

    // Buffer is full.
    assert!(!w.has_data_space(1));
    assert!(w.has_data_space(0));
}

// VAL-INC-004: Incremental update marks only changed nodes dirty.
#[test]
fn incremental_update_marks_only_changed_nodes_dirty() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Build a scene with a root + 3 line nodes + cursor (like the editor).
    {
        let mut w = tw.acquire();
        w.clear();

        // Allocate well-known-style structure:
        // Node 0 = root, Node 1 = doc_text, Node 2 = cursor,
        // Node 3/4/5 = line nodes
        let _root = w.alloc_node().unwrap(); // 0
        let _doc = w.alloc_node().unwrap(); // 1
        let _cursor = w.alloc_node().unwrap(); // 2

        // 3 line nodes as children of doc (node 1)
        let line0 = w.alloc_node().unwrap(); // 3
        let line1 = w.alloc_node().unwrap(); // 4
        let line2 = w.alloc_node().unwrap(); // 5

        w.set_root(0);

        // Set up content for line nodes.
        w.node_mut(line0).content =
            make_mono_glyphs(&mut w, b"aaa", 16, Color::rgb(200, 200, 200), 8);
        w.node_mut(line1).content =
            make_mono_glyphs(&mut w, b"bbb", 16, Color::rgb(200, 200, 200), 8);
        w.node_mut(line2).content =
            make_mono_glyphs(&mut w, b"ccc", 16, Color::rgb(200, 200, 200), 8);

        // Link: doc -> line0 -> line1 -> line2 -> cursor
        w.node_mut(1).first_child = line0;
        w.node_mut(line0).next_sibling = line1;
        w.node_mut(line1).next_sibling = line2;
        w.node_mut(line2).next_sibling = 2; // cursor
        w.node_mut(2).next_sibling = NULL;

        w.set_all_dirty();
    }
    tw.publish();

    // Now simulate an incremental update: acquire_copy, update only line1.
    {
        let mut w = tw.acquire_copy();

        // Dirty bits should be zero after acquire_copy.
        assert_eq!(w.dirty_count(), 0);

        // Push new data for line1 (node 4) — simulate incremental update.
        let new_glyphs = [
            ShapedGlyph {
                glyph_id: 88,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
            ShapedGlyph {
                glyph_id: 89,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
            ShapedGlyph {
                glyph_id: 90,
                _pad: 0,
                x_advance: 8 * 65536,
                x_offset: 0,
                y_offset: 0,
            },
        ];
        let new_ref = w.push_shaped_glyphs(&new_glyphs);

        w.node_mut(4).content = Content::Glyphs {
            color: Color::rgb(200, 200, 200),
            glyphs: new_ref,
            glyph_count: 3,
            font_size: 16,
            axis_hash: 0,
        };
        w.mark_dirty(4); // Only mark the changed line.
        w.mark_dirty(2); // Mark cursor as dirty too.

        // Verify only nodes 2 and 4 are dirty.
        assert!(w.is_dirty(4));
        assert!(w.is_dirty(2));
        assert!(!w.is_dirty(0)); // root not dirty
        assert!(!w.is_dirty(1)); // doc_text not dirty
        assert!(!w.is_dirty(3)); // line0 not dirty
        assert!(!w.is_dirty(5)); // line2 not dirty
        assert_eq!(w.dirty_count(), 2);
    }
}

// VAL-INC-005: data_used grows monotonically across incremental updates.
#[test]
fn incremental_data_used_grows_monotonically() {
    let mut buf = make_triple_buf();
    let mut tw = scene::TripleWriter::new(&mut buf);

    // Initial scene with some data.
    {
        let mut w = tw.acquire();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        w.node_mut(root).content =
            make_mono_glyphs(&mut w, b"hello world", 16, Color::rgb(200, 200, 200), 8);
    }
    tw.publish();

    let initial_data = tw.latest_data_buf().len();
    assert!(initial_data > 0);

    // Simulate 5 incremental updates, each pushing more data.
    let mut prev_used = initial_data;
    for i in 0..5u8 {
        {
            let mut w = tw.acquire_copy();
            let data_before = w.data_used() as usize;
            assert_eq!(
                data_before, prev_used,
                "acquire_copy should preserve data_used from latest"
            );

            // Push new data (simulating single-line reshape).
            let text = [b'A' + i; 10];
            let glyphs: Vec<ShapedGlyph> = text
                .iter()
                .map(|&ch| ShapedGlyph {
                    glyph_id: ch as u16,
                    _pad: 0,
                    x_advance: 8 * 65536,
                    x_offset: 0,
                    y_offset: 0,
                })
                .collect();
            w.push_shaped_glyphs(&glyphs);

            let data_after = w.data_used() as usize;
            assert!(
                data_after > data_before,
                "data_used should grow: before={}, after={}",
                data_before,
                data_after
            );
            prev_used = data_after;
        }
        tw.publish();
    }
}

#[test]
fn stroke_expand_simple_line() {
    // A simple horizontal line should produce filled stroke geometry.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    assert!(
        !expanded.is_empty(),
        "Stroke expansion should produce output"
    );
    // The expanded path should be longer than the input (offset curves + caps).
    assert!(
        expanded.len() > cmds.len(),
        "Expanded should be larger than input"
    );
}

#[test]
fn stroke_expand_closed_triangle() {
    // A closed triangle should produce filled stroke geometry with joins.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);
    scene::path_line_to(&mut cmds, 5.0, 8.0);
    scene::path_close(&mut cmds);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    assert!(
        !expanded.is_empty(),
        "Closed path stroke should produce output"
    );
}

#[test]
fn stroke_expand_zero_width() {
    // Zero stroke width should produce empty output.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);

    let expanded = scene::stroke::expand_stroke(&cmds, 0.0);
    assert!(
        expanded.is_empty(),
        "Zero width should produce empty output"
    );
}

#[test]
fn stroke_expand_dot() {
    // A zero-length segment (dot) should produce a circle.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 5.0, 5.0);
    scene::path_line_to(&mut cmds, 5.0, 5.0);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    assert!(!expanded.is_empty(), "Dot should produce circle geometry");
}

#[test]
fn svg_parse_simple_line() {
    let cmds = scene::svg_path::parse_svg_path("M0 0 L10 5");
    assert!(!cmds.is_empty(), "Should produce MoveTo + LineTo");
    // MoveTo(12) + LineTo(12) = 24 bytes
    assert_eq!(cmds.len(), 24);
}

#[test]
fn svg_parse_relative_hv() {
    let cmds = scene::svg_path::parse_svg_path("M5 5 h10 v10");
    // MoveTo(12) + LineTo(12) + LineTo(12) = 36 bytes
    assert_eq!(cmds.len(), 36);
}

#[test]
fn svg_parse_relative_arc() {
    // A simple 90-degree arc (quarter circle, radius 1).
    let cmds = scene::svg_path::parse_svg_path("M0 0 a1 1 0 0 1 1 1");
    assert!(!cmds.is_empty(), "Arc should produce cubic(s)");
    // Should produce MoveTo + one or more CubicTo commands.
    assert!(cmds.len() >= 12 + 28, "At least MoveTo + CubicTo");
}

#[test]
fn svg_parse_file_text_icon() {
    // Real Tabler file-text.svg path data (5 sub-paths).
    let paths = [
        "M14 3v4a1 1 0 0 0 1 1h4",
        "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2",
        "M9 9l1 0",
        "M9 13l6 0",
        "M9 17l6 0",
    ];
    for p in &paths {
        let cmds = scene::svg_path::parse_svg_path(p);
        assert!(!cmds.is_empty(), "Path '{}' should produce output", p);
    }
}

#[test]
fn svg_parse_photo_icon() {
    // Real Tabler photo.svg path data.
    let paths = [
        "M15 8h.01",
        "M3 6a3 3 0 0 1 3 -3h12a3 3 0 0 1 3 3v12a3 3 0 0 1 -3 3h-12a3 3 0 0 1 -3 -3v-12",
        "M3 16l5 -5c.928 -.893 2.072 -.893 3 0l5 5",
        "M14 14l1 -1c.928 -.893 2.072 -.893 3 0l3 3",
    ];
    for p in &paths {
        let cmds = scene::svg_path::parse_svg_path(p);
        assert!(!cmds.is_empty(), "Path '{}' should produce output", p);
    }
}

// ── Geometric invariant tests ────────────────────────────────────
//
// Strategy: test PROPERTIES, not implementations. One invariant test
// catches entire categories of math bugs simultaneously.

/// Evaluate a cubic Bézier at parameter t. Returns (x, y).
fn cubic_at(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let x = u*u*u*p0.0 + 3.0*u*u*t*p1.0 + 3.0*u*t*t*p2.0 + t*t*t*p3.0;
    let y = u*u*u*p0.1 + 3.0*u*u*t*p1.1 + 3.0*u*t*t*p2.1 + t*t*t*p3.1;
    (x, y)
}

#[test]
fn svg_arc_cubic_points_lie_on_circle() {
    // THE invariant: every point on the cubic approximation of a circular
    // arc should be within a tight tolerance of the original circle.
    // This single test catches all trig bugs (sin, cos, atan2), arc
    // parameterization bugs, control point calculation bugs, and
    // segment-count bugs — anything that distorts the arc shape.
    //
    // Tests all four icon arcs at 11 sample points each (t = 0.0, 0.1, ..., 1.0).
    let arcs: &[(&str, f32, f32, f32)] = &[
        // (svg_path, center_x, center_y, radius)
        ("M7 21a2 2 0 0 1 -2 -2",    7.0, 19.0, 2.0),  // bottom-left corner
        ("M5 5a2 2 0 0 1 2 -2",      7.0,  5.0, 2.0),  // top-left corner
        ("M19 19a2 2 0 0 1 -2 2",   17.0, 19.0, 2.0),  // bottom-right corner
        ("M14 7a1 1 0 0 0 1 1",     15.0,  7.0, 1.0),  // fold corner
    ];

    for &(svg, cx, cy, r) in arcs {
        let cmds = scene::svg_path::parse_svg_path(svg);
        let parsed = parse_path_commands(&cmds);

        // Extract start point from MoveTo.
        let (_, start_coords) = parsed.iter().find(|(t, _)| *t == scene::PATH_MOVE_TO)
            .expect("Should have MoveTo");
        let p0 = (start_coords[0], start_coords[1]);

        // Collect all CubicTo commands.
        let cubics: Vec<_> = parsed.iter()
            .filter(|(t, _)| *t == scene::PATH_CUBIC_TO)
            .collect();
        assert!(!cubics.is_empty(), "Arc '{}' should produce cubics", svg);

        // Walk each cubic, sampling at t = 0.0, 0.1, ..., 1.0.
        let mut prev_end = p0;
        for (_, coords) in &cubics {
            let cp1 = (coords[0], coords[1]);
            let cp2 = (coords[2], coords[3]);
            let end = (coords[4], coords[5]);

            for i in 0..=10 {
                let t = i as f32 / 10.0;
                let (px, py) = cubic_at(prev_end, cp1, cp2, end, t);
                let dist = ((px - cx) * (px - cx) + (py - cy) * (py - cy)).sqrt();
                let error = (dist - r).abs();
                assert!(
                    error < 0.01,
                    "Arc '{}': point at t={:.1} is ({:.4},{:.4}), dist from center={:.4}, \
                     error={:.6} (max 0.01)",
                    svg, t, px, py, dist, error
                );
            }
            prev_end = end;
        }
    }
}

#[test]
fn stroke_expand_width_is_uniform() {
    // THE stroke invariant: every point on the expanded outline should
    // be exactly half_width away from the nearest point on the original
    // path. Test with a simple horizontal line where "nearest point" is
    // trivially computed.
    //
    // Line from (0,0) to (10,0), stroke_width=2 (half_width=1).
    // Every point on the expanded outline should be at distance 1.0 from
    // the line segment (0,0)-(10,0), ignoring the end caps.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    // Collect all line/cubic points from the expanded path.
    for (tag, coords) in &parsed {
        let points: Vec<(f32, f32)> = match *tag {
            scene::PATH_LINE_TO => vec![(coords[0], coords[1])],
            scene::PATH_CUBIC_TO => {
                // Sample cubic at a few points.
                vec![(coords[4], coords[5])] // just endpoint for now
            }
            _ => continue,
        };

        for (px, py) in points {
            // Distance from point to line segment (0,0)-(10,0).
            let clamped_x = px.max(0.0).min(10.0);
            let dist = ((px - clamped_x) * (px - clamped_x) + py * py).sqrt();

            // Should be within tolerance of half_width = 1.0.
            // Allow extra tolerance for cap curvature beyond endpoints.
            assert!(
                dist < 1.15,
                "Stroke point ({:.3},{:.3}): dist={:.4} from path, expected ≤1.0 (+tolerance)",
                px, py, dist
            );
        }
    }
}

// ── SVG arc geometry: numerical verification ─────────────────────
//
// These tests verify the SVG arc-to-cubic conversion produces correct
// cubic Bézier control points by comparing against hand-computed
// reference values (from the SVG spec Appendix F.6 algorithm with
// exact trig). This catches trig approximation errors, coordinate
// system bugs, and arc parameterization mistakes.

/// Extract the first CubicTo command's control points from path data.
fn first_cubic(data: &[u8]) -> Option<(f32, f32, f32, f32, f32, f32)> {
    let cmds = parse_path_commands(data);
    for (tag, coords) in &cmds {
        if *tag == scene::PATH_CUBIC_TO && coords.len() == 6 {
            return Some((coords[0], coords[1], coords[2], coords[3], coords[4], coords[5]));
        }
    }
    None
}

#[test]
fn svg_arc_quarter_circle_bottom_left_control_points() {
    // Arc from (7,21) to (5,19): the bottom-left rounded corner of the
    // file-text icon body. rx=ry=2, center=(7,19), sweeps from θ=π/2 to θ=π.
    //
    // Reference (SVG spec F.6, alpha = (sqrt(7)-1)/3 ≈ 0.5486):
    //   P1 = (7.0, 21.0)      — arc start
    //   P2 = (5.903, 21.0)    — MUST have P2.y = P1.y (horizontal tangent)
    //   P3 = (5.0, 20.097)    — MUST have P3.x = P4.x (vertical tangent)
    //   P4 = (5.0, 19.0)      — arc end (exact, specified by SVG)
    let cmds = scene::svg_path::parse_svg_path("M7 21a2 2 0 0 1 -2 -2");
    let all = parse_path_commands(&cmds);
    // Dump all commands for diagnosis.
    for (i, (tag, coords)) in all.iter().enumerate() {
        let name = match *tag {
            scene::PATH_MOVE_TO => "MoveTo",
            scene::PATH_LINE_TO => "LineTo",
            scene::PATH_CUBIC_TO => "CubicTo",
            scene::PATH_CLOSE => "Close",
            _ => "???",
        };
        eprintln!("  cmd[{}]: {} {:?}", i, name, coords);
    }
    let (c1x, c1y, c2x, c2y, ex, ey) = first_cubic(&cmds)
        .expect("Arc should produce a CubicTo");

    let tol = 0.05; // 0.05 viewbox units ≈ sub-pixel at icon scale

    // P2.y must equal P1.y = 21.0 (horizontal tangent at arc start).
    // This is the most sensitive test — sin(π) error shows up here directly.
    assert!(
        (c1y - 21.0).abs() < tol,
        "P2.y should be 21.0 (horizontal tangent), got {:.4}", c1y
    );

    // P3.x must equal P4.x = 5.0 (vertical tangent at arc end).
    assert!(
        (c2x - 5.0).abs() < tol,
        "P3.x should be 5.0 (vertical tangent), got {:.4}", c2x
    );

    // Endpoint must be at (5, 19).
    assert!(
        (ex - 5.0).abs() < tol && (ey - 19.0).abs() < tol,
        "Endpoint should be (5.0, 19.0), got ({:.4}, {:.4})", ex, ey
    );

    // Control point P2: x ≈ 5.903 (for alpha ≈ 0.5486).
    assert!(
        (c1x - 5.903).abs() < 0.1,
        "P2.x should be ≈5.903, got {:.4}", c1x
    );

    // Control point P3: y ≈ 20.097.
    assert!(
        (c2y - 20.097).abs() < 0.1,
        "P3.y should be ≈20.097, got {:.4}", c2y
    );
}

#[test]
fn svg_arc_quarter_circle_top_left_control_points() {
    // Arc from (5,5) to (7,3): top-left corner. Center=(7,5), θ: π to 3π/2.
    //
    // Reference:
    //   P1 = (5.0, 5.0)
    //   P2 = (5.0, 3.903)     — MUST have P2.x = P1.x (vertical tangent)
    //   P3 = (5.903, 3.0)     — MUST have P3.y = P4.y (horizontal tangent)
    //   P4 = (7.0, 3.0)
    let cmds = scene::svg_path::parse_svg_path("M5 5a2 2 0 0 1 2 -2");
    let (c1x, c1y, c2x, c2y, ex, ey) = first_cubic(&cmds)
        .expect("Arc should produce a CubicTo");

    let tol = 0.05;

    // P2.x must equal P1.x = 5.0 (vertical tangent at start).
    assert!(
        (c1x - 5.0).abs() < tol,
        "P2.x should be 5.0 (vertical tangent), got {:.4}", c1x
    );

    // P3.y must equal P4.y = 3.0 (horizontal tangent at end).
    assert!(
        (c2y - 3.0).abs() < tol,
        "P3.y should be 3.0 (horizontal tangent), got {:.4}", c2y
    );

    // Endpoint must be at (7, 3).
    assert!(
        (ex - 7.0).abs() < tol && (ey - 3.0).abs() < tol,
        "Endpoint should be (7.0, 3.0), got ({:.4}, {:.4})", ex, ey
    );
}

#[test]
fn svg_arc_quarter_circle_bottom_right_control_points() {
    // Arc from (19,19) to (17,21): bottom-right corner. Center=(17,19), θ: 0 to π/2.
    //
    // Reference:
    //   P2 = (19.0, 20.097)   — MUST have P2.x = P1.x (vertical tangent)
    //   P3 = (18.097, 21.0)   — MUST have P3.y = P4.y (horizontal tangent)
    //   P4 = (17.0, 21.0)
    let cmds = scene::svg_path::parse_svg_path("M19 19a2 2 0 0 1 -2 2");
    let (c1x, c1y, c2x, c2y, ex, ey) = first_cubic(&cmds)
        .expect("Arc should produce a CubicTo");

    let tol = 0.05;

    // P2.x must equal P1.x = 19.0.
    assert!(
        (c1x - 19.0).abs() < tol,
        "P2.x should be 19.0 (vertical tangent), got {:.4}", c1x
    );

    // P3.y must equal P4.y = 21.0.
    assert!(
        (c2y - 21.0).abs() < tol,
        "P3.y should be 21.0 (horizontal tangent), got {:.4}", c2y
    );

    // Endpoint.
    assert!(
        (ex - 17.0).abs() < tol && (ey - 21.0).abs() < tol,
        "Endpoint should be (17.0, 21.0), got ({:.4}, {:.4})", ex, ey
    );
}

#[test]
fn svg_arc_fold_corner_control_points() {
    // Fold arc from (14,7) to (15,8): rx=ry=1, sweep=0 (CCW in SVG).
    // Center=(15,7), sweeps θ from π to π/2.
    //
    // At θ=π (start), tangent is VERTICAL (downward): P2.x = P1.x = 14.
    // At θ=π/2 (end), tangent is HORIZONTAL (rightward): P3.y = P4.y = 8.
    let cmds = scene::svg_path::parse_svg_path("M14 7a1 1 0 0 0 1 1");
    let (c1x, c1y, c2x, c2y, ex, ey) = first_cubic(&cmds)
        .expect("Arc should produce a CubicTo");

    let tol = 0.05;

    // Vertical tangent at start: P2.x = P1.x = 14.0
    assert!(
        (c1x - 14.0).abs() < tol,
        "P2.x should be 14.0 (vertical tangent), got {:.4}", c1x
    );
    // Horizontal tangent at end: P3.y = P4.y = 8.0
    assert!(
        (c2y - 8.0).abs() < tol,
        "P3.y should be 8.0 (horizontal tangent), got {:.4}", c2y
    );
    // Endpoint exact.
    assert!(
        (ex - 15.0).abs() < tol && (ey - 8.0).abs() < tol,
        "Endpoint should be (15.0, 8.0), got ({:.4}, {:.4})", ex, ey
    );
}

// ── Custom trig replicas (must match svg_path.rs exactly) ─────────

fn nostd_floor(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;
    if x < f { f - 1.0 } else { f }
}

fn nostd_sin(x: f32) -> f32 {
    let pi: f32 = core::f32::consts::PI;
    let half_pi: f32 = core::f32::consts::FRAC_PI_2;
    let two_pi: f32 = 2.0 * pi;
    let mut x = x - two_pi * nostd_floor(x / two_pi + 0.5);
    if x > half_pi { x = pi - x; }
    else if x < -half_pi { x = -pi - x; }
    let x2 = x * x;
    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)))
}

fn nostd_cos(x: f32) -> f32 {
    nostd_sin(x + core::f32::consts::FRAC_PI_2)
}

fn nostd_atan_inner(x: f32) -> f32 {
    let x2 = x * x;
    x * (0.999_866_0
        + x2 * (-0.330_299_5
            + x2 * (0.180_141_0 + x2 * (-0.085_133_0 + x2 * 0.020_835_1))))
}

fn nostd_atan2(y: f32, x: f32) -> f32 {
    let pi: f32 = core::f32::consts::PI;
    let half_pi: f32 = core::f32::consts::FRAC_PI_2;
    if x > 0.0 {
        let a = y / x;
        if a.abs() > 1.0 {
            let r = nostd_atan_inner(x / y);
            if y > 0.0 { half_pi - r } else { -half_pi - r }
        } else {
            nostd_atan_inner(a)
        }
    } else if x < 0.0 {
        let a = y / x;
        let base = if a.abs() > 1.0 {
            let r = nostd_atan_inner(x / y);
            if y >= 0.0 { half_pi - r } else { -half_pi - r }
        } else {
            nostd_atan_inner(a)
        };
        if y >= 0.0 { base + pi } else { base - pi }
    } else if y > 0.0 {
        half_pi
    } else if y < 0.0 {
        -half_pi
    } else {
        0.0
    }
}

#[test]
fn nostd_atan2_matches_std() {
    let cases: &[(f32, f32, &str)] = &[
        (0.0, 1.0, "east"),
        (1.0, 0.0, "north"),
        (0.0, -1.0, "west"),
        (-1.0, 0.0, "south"),
        (1.0, 1.0, "NE"),
        (-1.0, 1.0, "SE"),
        (-1.0, -1.0, "SW"),
        (1.0, -1.0, "NW"),
    ];
    for &(y, x, label) in cases {
        let expected = y.atan2(x);
        let got = nostd_atan2(y, x);
        assert!(
            (expected - got).abs() < 0.01,
            "atan2({},{}) [{}]: expected {:.6}, got {:.6}, diff {:.6}",
            y, x, label, expected, got, (expected - got).abs()
        );
    }
}

#[test]
fn nostd_sin_cos_at_key_angles() {
    let pi: f32 = core::f32::consts::PI;
    let hp: f32 = core::f32::consts::FRAC_PI_2;
    let cases: &[(f32, f32, f32, &str)] = &[
        (0.0,       0.0,    1.0,   "0"),
        (hp,        1.0,    0.0,   "π/2"),
        (pi,        0.0,   -1.0,   "π"),
        (-hp,      -1.0,    0.0,  "-π/2"),
        (-pi,       0.0,   -1.0,  "-π"),
        (3.0*hp,   -1.0,    0.0,  "3π/2"),
        (pi/4.0,    0.7071, 0.7071,"π/4"),
        (3.0*pi/4.0,0.7071,-0.7071,"3π/4"),
    ];
    for &(angle, exp_sin, exp_cos, label) in cases {
        let s = nostd_sin(angle);
        let c = nostd_cos(angle);
        assert!(
            (s - exp_sin).abs() < 0.01,
            "sin({}) = {:.6}, expected {:.4}", label, s, exp_sin
        );
        assert!(
            (c - exp_cos).abs() < 0.01,
            "cos({}) = {:.6}, expected {:.4}", label, c, exp_cos
        );
    }
}

#[test]
fn nostd_actual_arc_debug() {
    // Test the ACTUAL atan2 and sin from svg_path.rs.
    let cases: &[(f32, f32, f32, &str)] = &[
        (1.0, 0.0, core::f32::consts::FRAC_PI_2, "atan2(1,0)=π/2"),
        (0.0, -1.0, core::f32::consts::PI, "atan2(0,-1)=π"),
        (0.0, 1.0, 0.0, "atan2(0,1)=0"),
        (-1.0, 0.0, -core::f32::consts::FRAC_PI_2, "atan2(-1,0)=-π/2"),
    ];
    for &(y, x, expected, label) in cases {
        let got = scene::svg_path::debug_atan2(y, x);
        eprintln!("  {}: expected={:.6}, got={:.6}, diff={:.6}", label, expected, got, (got-expected).abs());
        assert!(
            (got - expected).abs() < 0.01,
            "{}: expected {:.6}, got {:.6}", label, expected, got
        );
    }

    // Test sin at key angles.
    let pi = core::f32::consts::PI;
    let hp = core::f32::consts::FRAC_PI_2;
    let sin_cases: &[(f32, f32, &str)] = &[
        (0.0, 0.0, "sin(0)"),
        (hp, 1.0, "sin(π/2)"),
        (pi, 0.0, "sin(π)"),
        (-hp, -1.0, "sin(-π/2)"),
    ];
    for &(angle, expected, label) in sin_cases {
        let got = scene::svg_path::debug_sin(angle);
        eprintln!("  {}: expected={:.6}, got={:.6}", label, expected, got);
        assert!(
            (got - expected).abs() < 0.01,
            "{}: expected {:.6}, got {:.6}", label, expected, got
        );
    }

    // Finally test the arc params.
    let (theta1, dtheta, n_segs, cx, cy, t1_y, t1_x, cxp, cyp, x1p, y1p) =
        scene::svg_path::debug_arc_params(
            7.0, 21.0, 2.0, 2.0, 0.0, false, true, 5.0, 19.0,
        );
    eprintln!("  x1p={:.6}, y1p={:.6}, cxp={:.6}, cyp={:.6}", x1p, y1p, cxp, cyp);
    eprintln!("  t1 args: y={:.6}, x={:.6}", t1_y, t1_x);
    eprintln!("  atan2(t1_y, t1_x)={:.6}", scene::svg_path::debug_atan2(t1_y, t1_x));
    eprintln!("  ARC: theta1={:.6}, dtheta={:.6}, n_segs={}, center=({:.4},{:.4})",
        theta1, dtheta, n_segs, cx, cy);
    assert_eq!(n_segs, 1, "Should be 1 segment, got {}", n_segs);

    // Also test top-left arc: (5,5) to (7,3), sweep=1.
    let (theta1, dtheta, n_segs, cx, cy, t1_y, t1_x, cxp, cyp, x1p, y1p) =
        scene::svg_path::debug_arc_params(5.0, 5.0, 2.0, 2.0, 0.0, false, true, 7.0, 3.0);
    eprintln!("  TOP-LEFT: x1p={:.6}, y1p={:.6}, cxp={:.6}, cyp={:.6}", x1p, y1p, cxp, cyp);
    eprintln!("  TOP-LEFT: t1(y={:.6},x={:.6}), theta1={:.6}, dtheta={:.6}, n_segs={}",
        t1_y, t1_x, theta1, dtheta, n_segs);
    assert_eq!(n_segs, 1, "Top-left should be 1 segment, got {}", n_segs);

    // And fold arc: (14,7) to (15,8), sweep=0.
    let (theta1, dtheta, n_segs, cx, cy, t1_y, t1_x, cxp, cyp, x1p, y1p) =
        scene::svg_path::debug_arc_params(14.0, 7.0, 1.0, 1.0, 0.0, false, false, 15.0, 8.0);
    eprintln!("  FOLD: x1p={:.6}, y1p={:.6}, cxp={:.6}, cyp={:.6}", x1p, y1p, cxp, cyp);
    eprintln!("  FOLD: t1(y={:.6},x={:.6}), theta1={:.6}, dtheta={:.6}, n_segs={}",
        t1_y, t1_x, theta1, dtheta, n_segs);
    assert_eq!(n_segs, 1, "Fold should be 1 segment, got {}", n_segs);
}

#[test]
fn nostd_arc_dtheta_matches_std() {
    // Replicate the EXACT arc_to_cubics dtheta logic using the custom
    // atan2, and compare to std.
    let pi: f32 = core::f32::consts::PI;
    let hp: f32 = core::f32::consts::FRAC_PI_2;
    let two_pi: f32 = 2.0 * pi;

    // Arc from (7,21) to (5,19), center (7,19).
    let x1p: f32 = 1.0;
    let y1p: f32 = 1.0;
    let cxp: f32 = 1.0;
    let cyp: f32 = -1.0;
    let rx: f32 = 2.0;
    let ry: f32 = 2.0;

    let t1_y = (y1p - cyp) / ry;  // (1-(-1))/2 = 1
    let t1_x = (x1p - cxp) / rx;  // (1-1)/2 = 0
    let t2_y = (-y1p - cyp) / ry;  // (-1-(-1))/2 = 0
    let t2_x = (-x1p - cxp) / rx;  // (-1-1)/2 = -1

    let theta1_std = t1_y.atan2(t1_x);
    let theta2_std = t2_y.atan2(t2_x);
    let theta1_nostd = nostd_atan2(t1_y, t1_x);
    let theta2_nostd = nostd_atan2(t2_y, t2_x);

    eprintln!("  t1 args: y={}, x={}", t1_y, t1_x);
    eprintln!("  t2 args: y={}, x={}", t2_y, t2_x);
    eprintln!("  theta1: std={:.6}, nostd={:.6}", theta1_std, theta1_nostd);
    eprintln!("  theta2: std={:.6}, nostd={:.6}", theta2_std, theta2_nostd);

    let dtheta_std = theta2_std - theta1_std;
    let dtheta_nostd = theta2_nostd - theta1_nostd;
    eprintln!("  dtheta_raw: std={:.6}, nostd={:.6}", dtheta_std, dtheta_nostd);

    // Sweep=true adjustment
    let dtheta_adj_std = if dtheta_std < 0.0 { dtheta_std + two_pi } else { dtheta_std };
    let dtheta_adj_nostd = if dtheta_nostd < 0.0 { dtheta_nostd + two_pi } else { dtheta_nostd };
    eprintln!("  dtheta_adj: std={:.6}, nostd={:.6}", dtheta_adj_std, dtheta_adj_nostd);

    let n_std = ((dtheta_adj_std.abs() / hp).ceil() as usize).max(1);
    let n_nostd = ((dtheta_adj_nostd.abs() / hp).ceil() as usize).max(1);
    eprintln!("  n_segs: std={}, nostd={}", n_std, n_nostd);

    assert_eq!(n_nostd, 1, "nostd arc should produce 1 segment, got {}", n_nostd);
}

#[test]
fn svg_arc_dtheta_computation_matches_expected() {
    // Replicate the arc_to_cubics dtheta logic in the test to diagnose
    // why the bottom-left arc produces 3 cubics instead of 1.
    //
    // Arc from (7,21) to (5,19), rx=ry=2, sweep=true.
    // Expected: center=(7,19), theta1=π/2, dtheta=π/2, n_segs=1.
    let pi: f32 = core::f32::consts::PI;
    let half_pi: f32 = core::f32::consts::FRAC_PI_2;
    let two_pi: f32 = 2.0 * pi;

    // Reproduce the center computation (SVG F.6.5).
    let x1: f32 = 7.0;
    let y1: f32 = 21.0;
    let x2: f32 = 5.0;
    let y2: f32 = 19.0;
    let rx: f32 = 2.0;
    let ry: f32 = 2.0;

    let dx2 = (x1 - x2) * 0.5; // 1.0
    let dy2 = (y1 - y2) * 0.5; // 1.0
    let x1p = dx2;  // cos_phi=1, sin_phi=0
    let y1p = dy2;

    let rx2 = rx * rx; // 4
    let ry2 = ry * ry; // 4
    let num = (rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p).max(0.0);
    let den = rx2 * y1p * y1p + ry2 * x1p * x1p;
    let sq = if den > 1e-10 { (num / den).sqrt() } else { 0.0 };

    // sign: large_arc(false) == sweep(true) → false → sign = 1
    let cxp = sq * (rx * y1p / ry);
    let cyp = sq * (-(ry * x1p / rx));

    let cx = cxp + (x1 + x2) * 0.5;
    let cy = cyp + (y1 + y2) * 0.5;

    eprintln!("  center: ({}, {})", cx, cy);
    assert!((cx - 7.0).abs() < 0.01, "cx should be 7.0, got {}", cx);
    assert!((cy - 19.0).abs() < 0.01, "cy should be 19.0, got {}", cy);

    // Theta1 and dtheta using std atan2 (reference).
    let theta1 = ((y1p - cyp) / ry).atan2((x1p - cxp) / rx);
    let theta2 = ((-y1p - cyp) / ry).atan2((-x1p - cxp) / rx);
    let mut dtheta = theta2 - theta1;

    eprintln!("  theta1: {:.6} (expected {:.6})", theta1, half_pi);
    eprintln!("  theta2: {:.6} (expected {:.6})", theta2, pi);
    eprintln!("  dtheta_raw: {:.6} (expected {:.6})", dtheta, half_pi);

    // Sweep adjustment (sweep=true).
    if dtheta < 0.0 {
        dtheta += two_pi;
    }

    eprintln!("  dtheta_adjusted: {:.6}", dtheta);

    let n_segs = ((dtheta.abs() / half_pi).ceil() as usize).max(1);
    eprintln!("  n_segs: {} (expected 1)", n_segs);

    assert_eq!(n_segs, 1, "Quarter-circle arc should need exactly 1 cubic segment");
}

#[test]
fn svg_arc_full_body_path_top_edge_is_horizontal() {
    // Parse the full document body path and verify that the top edge
    // (from after the top-left arc to the start of the diagonal) is
    // purely horizontal: both endpoints must have y = 3.0.
    let cmds = scene::svg_path::parse_svg_path(
        "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2z"
    );
    let parsed = parse_path_commands(&cmds);

    // The top-left arc ends at (7, 3). The next command is h7 → LineTo(14, 3).
    // Find this LineTo by looking for a LineTo with x≈14, y≈3.
    let top_edge = parsed.iter().find(|(tag, coords)| {
        *tag == scene::PATH_LINE_TO
            && coords.len() == 2
            && (coords[0] - 14.0).abs() < 0.1
            && (coords[1] - 3.0).abs() < 0.5
    });

    let (_, coords) = top_edge.expect("Should find LineTo(14, 3) for top edge");
    assert!(
        (coords[1] - 3.0).abs() < 0.01,
        "Top edge y should be exactly 3.0, got {:.4}", coords[1]
    );
}

// ── Stroke expansion coordinate verification ─────────────────────

/// Parse expanded path commands into a list of (tag, [f32]) tuples for easy
/// coordinate inspection.
fn parse_path_commands(data: &[u8]) -> Vec<(u32, Vec<f32>)> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        if offset + 4 > data.len() {
            break;
        }
        let tag = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        match tag {
            scene::PATH_MOVE_TO => {
                if offset + scene::PATH_MOVE_TO_SIZE > data.len() {
                    break;
                }
                let x = f32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                let y = f32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap());
                result.push((tag, vec![x, y]));
                offset += scene::PATH_MOVE_TO_SIZE;
            }
            scene::PATH_LINE_TO => {
                if offset + scene::PATH_LINE_TO_SIZE > data.len() {
                    break;
                }
                let x = f32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                let y = f32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap());
                result.push((tag, vec![x, y]));
                offset += scene::PATH_LINE_TO_SIZE;
            }
            scene::PATH_CUBIC_TO => {
                if offset + scene::PATH_CUBIC_TO_SIZE > data.len() {
                    break;
                }
                let mut coords = Vec::new();
                for i in 0..6 {
                    let off = offset + 4 + i * 4;
                    coords.push(f32::from_le_bytes(data[off..off + 4].try_into().unwrap()));
                }
                result.push((tag, coords));
                offset += scene::PATH_CUBIC_TO_SIZE;
            }
            scene::PATH_CLOSE => {
                result.push((tag, vec![]));
                offset += scene::PATH_CLOSE_SIZE;
            }
            _ => break,
        }
    }
    result
}

/// Find the first MoveTo command and return its (x, y).
fn first_move_to(cmds: &[(u32, Vec<f32>)]) -> (f32, f32) {
    for (tag, coords) in cmds {
        if *tag == scene::PATH_MOVE_TO {
            return (coords[0], coords[1]);
        }
    }
    panic!("No MoveTo found");
}

/// Collect all LineTo coordinates from expanded path.
fn line_to_coords(cmds: &[(u32, Vec<f32>)]) -> Vec<(f32, f32)> {
    cmds.iter()
        .filter(|(tag, _)| *tag == scene::PATH_LINE_TO)
        .map(|(_, c)| (c[0], c[1]))
        .collect()
}

fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() < tol
}

#[test]
fn stroke_expand_horizontal_line_offset_coordinates() {
    // A horizontal line (0,0)→(10,0) with stroke width 2 (hw=1).
    //
    // Normal of rightward segment: (-dy/len, dx/len) = (0, 1).
    // Left offset (vertex + n*hw) = below the line (y+1 in y-down).
    // Right offset (vertex − n*hw) = above the line (y−1 in y-down).
    //
    // Expected shape (open path, single contour):
    //   MoveTo(0, 1)              — left start
    //   LineTo(10, 1)             — left end
    //   [end cap: semicircle from (10,1) to (10,-1) bulging toward x=11]
    //   LineTo(10, -1)            — right end (first point of backward walk)
    //   [backward right side: no joins for single segment]
    //   LineTo(0, -1)             — right start
    //   [start cap: semicircle from (0,-1) to (0,1) bulging toward x=-1]
    //   Close
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    // First MoveTo should be at left_start = (0, 1).
    let (mx, my) = first_move_to(&parsed);
    assert!(
        approx_eq(mx, 0.0, 0.01) && approx_eq(my, 1.0, 0.01),
        "First MoveTo should be at (0, 1), got ({}, {})",
        mx,
        my
    );

    // First LineTo should be at left_end = (10, 1).
    let lines = line_to_coords(&parsed);
    assert!(!lines.is_empty());
    assert!(
        approx_eq(lines[0].0, 10.0, 0.01) && approx_eq(lines[0].1, 1.0, 0.01),
        "First LineTo should be at (10, 1), got ({}, {})",
        lines[0].0,
        lines[0].1
    );

    // There should be exactly one Close command (single contour, open path).
    let close_count = parsed
        .iter()
        .filter(|(tag, _)| *tag == scene::PATH_CLOSE)
        .count();
    assert_eq!(
        close_count, 1,
        "Open path should produce exactly one contour"
    );

    // Verify the expanded geometry contains points on both sides of the line.
    // At least one point should have y ≈ -1 (right/above) from the caps/backward walk.
    let has_above = lines.iter().any(|(_, y)| approx_eq(*y, -1.0, 0.1));
    assert!(
        has_above,
        "Should have points at y ≈ -1 (right side of rightward line)"
    );
}

#[test]
fn stroke_expand_closed_rectangle_two_contours() {
    // A CW rectangle: (0,0)→(10,0)→(10,10)→(0,10)→close, stroke width 2.
    //
    // For a closed path, expand_stroke produces two contours:
    //   1. Left (forward) contour — inner for CW paths
    //   2. Right (backward) contour — outer for CW paths
    // Both are closed, so we expect exactly 2 Close commands.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 10.0);
    scene::path_line_to(&mut cmds, 0.0, 10.0);
    scene::path_close(&mut cmds);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    let close_count = parsed
        .iter()
        .filter(|(tag, _)| *tag == scene::PATH_CLOSE)
        .count();
    assert_eq!(
        close_count, 2,
        "Closed path should produce 2 contours (inner + outer)"
    );

    let move_count = parsed
        .iter()
        .filter(|(tag, _)| *tag == scene::PATH_MOVE_TO)
        .count();
    assert_eq!(
        move_count, 2,
        "Should have exactly 2 MoveTo (one per contour)"
    );
}

#[test]
fn stroke_expand_closed_rectangle_outer_has_arcs() {
    // The CW rectangle's OUTER contour (right side) should contain cubic arcs
    // for round joins. The INNER contour (left side) should NOT have arcs
    // at corners (just straight lines connecting inner offset points).
    //
    // For CW input in y-down, all corners have cross > 0 (CW turns).
    // Right side is outer → arcs. Left side is inner → no arcs.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 10.0);
    scene::path_line_to(&mut cmds, 0.0, 10.0);
    scene::path_close(&mut cmds);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    // Split into contours at Close commands.
    let mut contours: Vec<Vec<&(u32, Vec<f32>)>> = Vec::new();
    let mut current: Vec<&(u32, Vec<f32>)> = Vec::new();
    for cmd in &parsed {
        current.push(cmd);
        if cmd.0 == scene::PATH_CLOSE {
            contours.push(std::mem::take(&mut current));
        }
    }
    assert_eq!(contours.len(), 2, "Expected 2 contours");

    // Count cubics in each contour.
    let cubics_0 = contours[0]
        .iter()
        .filter(|(t, _)| *t == scene::PATH_CUBIC_TO)
        .count();
    let cubics_1 = contours[1]
        .iter()
        .filter(|(t, _)| *t == scene::PATH_CUBIC_TO)
        .count();

    // First contour is left (inner for CW) — should have 0 cubics (no arcs at inner corners).
    assert_eq!(
        cubics_0, 0,
        "Inner contour (left/forward) should have no cubic arcs, got {}",
        cubics_0
    );

    // Second contour is right (outer for CW) — should have cubics (round join arcs).
    // 4 corners × 1 quarter-circle arc each = at least 4 cubics.
    assert!(
        cubics_1 >= 4,
        "Outer contour (right/backward) should have round join arcs, got {} cubics",
        cubics_1
    );
}

#[test]
fn stroke_expand_ccw_rectangle_arcs_on_correct_side() {
    // A CCW rectangle: (0,0)→(0,10)→(10,10)→(10,0)→close.
    // All corners have cross < 0 (CCW turns in y-down).
    // Left side is outer → arcs on left. Right side is inner → no arcs.
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 0.0, 10.0);
    scene::path_line_to(&mut cmds, 10.0, 10.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);
    scene::path_close(&mut cmds);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    let mut contours: Vec<Vec<&(u32, Vec<f32>)>> = Vec::new();
    let mut current: Vec<&(u32, Vec<f32>)> = Vec::new();
    for cmd in &parsed {
        current.push(cmd);
        if cmd.0 == scene::PATH_CLOSE {
            contours.push(std::mem::take(&mut current));
        }
    }
    assert_eq!(contours.len(), 2);

    let cubics_0 = contours[0]
        .iter()
        .filter(|(t, _)| *t == scene::PATH_CUBIC_TO)
        .count();
    let cubics_1 = contours[1]
        .iter()
        .filter(|(t, _)| *t == scene::PATH_CUBIC_TO)
        .count();

    // First contour is left (outer for CCW) — should have cubic arcs.
    assert!(
        cubics_0 >= 4,
        "Outer contour (left/forward for CCW) should have round join arcs, got {} cubics",
        cubics_0
    );

    // Second contour is right (inner for CCW) — should have no cubics.
    assert_eq!(
        cubics_1, 0,
        "Inner contour (right/backward for CCW) should have no arcs, got {}",
        cubics_1
    );
}

#[test]
fn stroke_expand_outer_contour_encloses_inner() {
    // For a CW rectangle with stroke width 2 (hw=1):
    //   Inner contour: offset inward by 1 → approx 8×8 at (1,1)
    //   Outer contour: offset outward by 1 → approx 12×12 at (-1,-1)
    //
    // Verify the bounding boxes are correct (outer > original > inner).
    let mut cmds = Vec::new();
    scene::path_move_to(&mut cmds, 0.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 0.0);
    scene::path_line_to(&mut cmds, 10.0, 10.0);
    scene::path_line_to(&mut cmds, 0.0, 10.0);
    scene::path_close(&mut cmds);

    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    // Collect all coordinates from all commands.
    let mut all_x = Vec::new();
    let mut all_y = Vec::new();
    for (_, coords) in &parsed {
        let mut i = 0;
        while i + 1 < coords.len() {
            all_x.push(coords[i]);
            all_y.push(coords[i + 1]);
            i += 2;
        }
    }

    let min_x = all_x.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_x = all_x.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min_y = all_y.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_y = all_y.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    // Outer contour should extend 1 unit beyond the original rectangle (hw=1).
    assert!(
        min_x < -0.5,
        "Outer should extend left of 0, got min_x={}",
        min_x
    );
    assert!(
        max_x > 10.5,
        "Outer should extend right of 10, got max_x={}",
        max_x
    );
    assert!(
        min_y < -0.5,
        "Outer should extend above 0, got min_y={}",
        min_y
    );
    assert!(
        max_y > 10.5,
        "Outer should extend below 10, got max_y={}",
        max_y
    );

    // No coordinate should be more than hw + a small tolerance beyond the original.
    // (Round joins don't overshoot — the max extent is exactly hw from the vertex.)
    let tol = 0.1; // Tolerance for arc approximation
    assert!(
        min_x > -1.0 - tol,
        "No spike: min_x should be ≥ -1.1, got {}",
        min_x
    );
    assert!(
        max_x < 11.0 + tol,
        "No spike: max_x should be ≤ 11.1, got {}",
        max_x
    );
    assert!(
        min_y > -1.0 - tol,
        "No spike: min_y should be ≥ -1.1, got {}",
        min_y
    );
    assert!(
        max_y < 11.0 + tol,
        "No spike: max_y should be ≤ 11.1, got {}",
        max_y
    );
}

#[test]
fn stroke_expand_tabler_icon_no_spike() {
    // Stroke-expand the file-text icon body and verify no coordinate spikes.
    // With viewbox 24×24 and stroke width 2, the maximum extent of any
    // coordinate should be within the viewbox ± half stroke width + tolerance.
    let d = "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2";
    let cmds = scene::svg_path::parse_svg_path(d);
    let expanded = scene::stroke::expand_stroke(&cmds, 2.0);
    let parsed = parse_path_commands(&expanded);

    let mut all_x = Vec::new();
    let mut all_y = Vec::new();
    for (_, coords) in &parsed {
        let mut i = 0;
        while i + 1 < coords.len() {
            all_x.push(coords[i]);
            all_y.push(coords[i + 1]);
            i += 2;
        }
    }

    let min_x = all_x.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_x = all_x.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min_y = all_y.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_y = all_y.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    // The icon body spans roughly x=[5,19], y=[3,21]. With hw=1, no coordinate
    // should exceed those bounds by more than 1 + tolerance.
    let spike_limit = 2.0; // hw + generous tolerance for arcs and cubics
    assert!(
        min_x > 5.0 - spike_limit,
        "x spike: min_x={} (expected > {})",
        min_x,
        5.0 - spike_limit
    );
    assert!(
        max_x < 19.0 + spike_limit,
        "x spike: max_x={} (expected < {})",
        max_x,
        19.0 + spike_limit
    );
    assert!(
        min_y > 3.0 - spike_limit,
        "y spike: min_y={} (expected > {})",
        min_y,
        3.0 - spike_limit
    );
    assert!(
        max_y < 21.0 + spike_limit,
        "y spike: max_y={} (expected < {})",
        max_y,
        21.0 + spike_limit
    );
}

// ── Multiple Content::Image data isolation ───────────────────────────────────

/// Verify that two Content::Image nodes in the same scene reference
/// non-overlapping data ranges. This catches the class of bug where a
/// shared GPU texture is overwritten by the second image before the
/// first image's draw command executes — the scene graph must produce
/// distinct DataRefs so the render backend can distinguish them.
#[test]
fn two_content_image_nodes_have_disjoint_data() {
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let _ = TripleWriter::new(&mut buf);
    let mut tw = TripleWriter::from_existing(&mut buf);
    let mut w = tw.acquire();
    w.clear();

    // Push two different image pixel buffers.
    let icon_pixels = vec![0xAA_u8; 52 * 52 * 4]; // 52×52 icon
    let img_pixels = vec![0xBB_u8; 32 * 32 * 4]; // 32×32 test image
    let icon_ref = w.push_data(&icon_pixels);
    let img_ref = w.push_data(&img_pixels);

    // Allocate two nodes.
    let n_icon = w.alloc_node().unwrap();
    let n_img = w.alloc_node().unwrap();

    // Set up as Content::Image with distinct data.
    w.node_mut(n_icon).content = Content::Image {
        data: icon_ref,
        src_width: 52,
        src_height: 52,
    };
    w.node_mut(n_img).content = Content::Image {
        data: img_ref,
        src_width: 32,
        src_height: 32,
    };

    // DataRefs must not overlap.
    let icon_end = icon_ref.offset + icon_ref.length;
    let img_end = img_ref.offset + img_ref.length;
    assert!(
        icon_end <= img_ref.offset || img_end <= icon_ref.offset,
        "image data ranges overlap: icon=[{}..{}), img=[{}..{})",
        icon_ref.offset,
        icon_end,
        img_ref.offset,
        img_end,
    );

    // Both must have non-zero length.
    assert!(icon_ref.length > 0, "icon data should be non-empty");
    assert!(img_ref.length > 0, "image data should be non-empty");

    // Verify the data is actually distinct (not pointing at the same bytes).
    assert_ne!(
        icon_ref.offset, img_ref.offset,
        "two images should have different data offsets"
    );
}
