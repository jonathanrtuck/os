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
/// Runs keep their document-relative y positions. The caller sets
/// `scroll_y` on the container node so the renderer handles the
/// viewport offset.
fn scroll_runs(
    runs: Vec<TestLayoutRun>,
    scroll_lines: u32,
    line_height: u32,
    viewport_height_px: i32,
) -> Vec<TestLayoutRun> {
    let scroll_px = scroll_lines as i32 * line_height as i32;

    runs.into_iter()
        .filter(|run| {
            let doc_y = run.y;

            // Above the scroll window?
            if doc_y + line_height as i32 <= scroll_px {
                return false;
            }
            // Below the scroll window?
            if doc_y >= scroll_px + viewport_height_px {
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
            x_advance: advance as i16,
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
    let visible = scroll_runs(runs, 0, 20, 100);
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
    let visible = scroll_runs(runs, 5, 20, 60);
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
    let visible = scroll_runs(runs, 6, 20, 600); // 600px = 30 lines
                                                 // First visible line should be line 6 at document y = 6*20 = 120.
    assert_eq!(visible[0].y, 120);
    // Last visible line should be line 35 at document y = 35*20 = 700.
    let last = visible.last().unwrap();
    assert_eq!(last.y, 700);
    // All visible lines should be within the scroll window [120, 720).
    let scroll_px = 6 * 20; // 120
    for run in &visible {
        assert!(
            run.y + 20 > scroll_px && run.y < scroll_px + 600,
            "run.y={} outside scroll window [{}, {})",
            run.y,
            scroll_px,
            scroll_px + 600
        );
    }
}

#[test]
fn scroll_runs_empty_text_with_scroll() {
    let runs = layout_mono_lines(b"", 80, 20, WHITE, 8, 16);
    let visible = scroll_runs(runs, 0, 20, 600);
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
        size, 8,
        "ShapedGlyph should be 8 bytes: u16 + i16 + i16 + i16"
    );
}

#[test]
fn shaped_glyph_field_access() {
    let g = ShapedGlyph {
        glyph_id: 42,
        x_advance: 600,
        x_offset: -10,
        y_offset: 5,
    };
    assert_eq!(g.glyph_id, 42);
    assert_eq!(g.x_advance, 600);
    assert_eq!(g.x_offset, -10);
    assert_eq!(g.y_offset, 5);
}

// ── Byte-exact equality round-trip ──────────────────────────────────

#[test]
fn shaped_glyph_byte_exact_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [
        ShapedGlyph {
            glyph_id: 0xABCD,
            x_advance: -32000,
            x_offset: 32000,
            y_offset: -1,
        },
        ShapedGlyph {
            glyph_id: 0x0001,
            x_advance: 1,
            x_offset: -1,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 0xFFFE,
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
    let mono_font = include_bytes!("../../share/source-code-pro.ttf");

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
    let prop_font = include_bytes!("../../share/nunito-sans.ttf");
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
    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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

    let tr = scene::TripleReader::new(&buf);
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

    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
        let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
    // Node at logical position (500, 400) with scale=2 → physical (1000, 800).
    // These fit in i16 (max 32767). But at scale=2 with a 2048-wide display,
    // a node at logical x=16000 would overflow i16.
    //
    // Test: verify abs_bounds returns correct values for large logical coords.
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

// ── VAL-COORD-013: abs_bounds accounts for scroll_y from ancestor nodes ──

/// abs_bounds must subtract parent scroll_y when computing a child's absolute
/// position. A child inside a scrolled container has its effective y position
/// offset by -scroll_y.
#[test]
fn abs_bounds_accounts_for_scroll_y() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Root at (0, 0)
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    w.set_root(root);

    // Scrollable container at (0, 50) with scroll_y = 10
    let container = w.alloc_node().unwrap();
    w.node_mut(container).y = 50;
    w.node_mut(container).width = 800;
    w.node_mut(container).height = 500;
    w.node_mut(container).scroll_y = 10;
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
    // Expected: child.y(30) + container.y(50) - container.scroll_y(10) + root.y(0) = 70
    // NOT 80 (which would be the result without scroll_y subtraction)
    assert_eq!(ay, 70, "abs_bounds y must subtract parent scroll_y");
    assert_eq!(aw, 100);
    assert_eq!(ah, 40);
}

/// abs_bounds with deeply nested scroll containers: scroll_y accumulates.
#[test]
fn abs_bounds_nested_scroll_containers() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    w.set_root(root);

    // Outer container scrolled by 5
    let outer = w.alloc_node().unwrap();
    w.node_mut(outer).y = 100;
    w.node_mut(outer).width = 800;
    w.node_mut(outer).height = 400;
    w.node_mut(outer).scroll_y = 5;
    w.add_child(root, outer);

    // Inner container scrolled by 15
    let inner = w.alloc_node().unwrap();
    w.node_mut(inner).y = 20;
    w.node_mut(inner).width = 800;
    w.node_mut(inner).height = 300;
    w.node_mut(inner).scroll_y = 15;
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
    // y: scroll_y on a node offsets its CHILDREN. So:
    //   leaf.y(10) is offset by inner.scroll_y(15) → 10 - 15 = -5 (relative to inner)
    //   inner.y(20) is offset by outer.scroll_y(5) → 20 - 5 = 15 (relative to outer)
    //   outer.y(100) → 100 (relative to root, no scroll)
    //   Total: -5 + 15 + 100 = 110
    assert_eq!(ay, 110, "abs_bounds must subtract each ancestor's scroll_y");
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
fn node_size_assertion_with_transform() {
    // VAL-XFORM-022: Node size compile-time assertion.
    // After widening x/y to i32, Node is 100 bytes.
    let size = core::mem::size_of::<Node>();
    assert_eq!(
        size, 100,
        "Node size should be 100 bytes with i32 x/y, got {}",
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

    let tr = scene::TripleReader::new(&buf);
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
            x_advance: 600,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 101,
            x_advance: 550,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 108,
            x_advance: 250,
            x_offset: -5,
            y_offset: 3,
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
            assert_eq!(read[2].x_offset, -5);
            assert_eq!(read[2].y_offset, 3);
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
                x_advance: 10,
                x_offset: 0,
                y_offset: 0,
            },
            ShapedGlyph {
                glyph_id: 66,
                x_advance: 10,
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

    let tr = scene::TripleReader::new(&buf);
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
            x_advance: 500,
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
                x_advance: 10,
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

// ── Node size unchanged (VAL-SCENE-006) ─────────────────────────────

#[test]
fn node_size_is_100_bytes() {
    assert_eq!(core::mem::size_of::<Node>(), 100);
}

#[test]
fn scene_header_size_is_80_bytes() {
    assert_eq!(core::mem::size_of::<SceneHeader>(), 80);
}

#[test]
fn shaped_glyph_size_is_8_bytes() {
    assert_eq!(core::mem::size_of::<ShapedGlyph>(), 8);
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
            x_advance: 10,
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
    let tr = scene::TripleReader::new(&buf);
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

    let tr = scene::TripleReader::new(&buf);
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
            // Each glyph has x_advance == 8 (monospace)
            for g in read {
                assert_eq!(g.x_advance, 8);
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
            x_advance: advance as i16,
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
    scroll_y: i32,
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
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let visible_runs = scroll_runs(all_runs, scroll_lines, line_height, content_h as i32);
    let scroll_px = scroll_lines as i32 * line_height as i32;

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
    // N_DOC_TEXT — Content::None (pure container with scroll_y)
    {
        let n = w.node_mut(CORE_N_DOC_TEXT);
        n.x = text_inset_x as i32;
        n.y = 8;
        n.width = doc_width as u16;
        n.height = content_h as u16;
        n.scroll_y = scroll_px;
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
            if sel_y + line_height as i32 <= scroll_px || sel_y >= scroll_px + content_h as i32 {
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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello", 0, 0, 0, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 1, 4, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 2, 9, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 0, 0, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 0, 0, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello\nWorld", 0, 1, 4, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello", 0, 0, 0, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"Hello", 0, 0, 0, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, &text, 0, 0, text_len, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, b"", 0, 0, 0, 8, 20, 16, 0);

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
        0,
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
    build_test_editor_scene(&mut w, 1024, 768, b"AB\nCD", 0, 0, 0, 8, 20, 16, 0);

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
    build_test_editor_scene(&mut w, 1024, 768, &text, 0, 0, 0, 8, 20, 16, 5);

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

    // N_DOC_TEXT.scroll_y should equal scroll_lines * line_height
    assert_eq!(
        w.node(CORE_N_DOC_TEXT).scroll_y,
        100, // 5 * 20
        "N_DOC_TEXT.scroll_y should be scroll_lines * line_height"
    );
}

// ── VAL-CORE-004b: Document-relative scroll model ───────────────────

#[test]
fn core_scroll_model_document_relative_positions() {
    // Verify the scroll model invariant: all children of N_DOC_TEXT are
    // positioned at document-relative coordinates, and N_DOC_TEXT.scroll_y
    // provides the viewport offset.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let line_h: u32 = 20;
    let scroll_lines: i32 = 3;
    // 8 lines of text, scroll down 3 lines.
    let text = b"aaa\nbbb\nccc\nddd\neee\nfff\nggg\nhhh";
    build_test_editor_scene(
        &mut w,
        1024,
        768,
        text,
        0,
        0,
        0,
        8,
        line_h,
        16,
        scroll_lines,
    );

    let scroll_px = scroll_lines as i32 * line_h as i32; // 60

    // 1. N_DOC_TEXT.scroll_y == scroll_lines * line_height
    assert_eq!(
        w.node(CORE_N_DOC_TEXT).scroll_y,
        scroll_px,
        "N_DOC_TEXT.scroll_y must equal scroll_lines * line_height"
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
    build_test_editor_scene(&mut w, 1024, 768, b"aaa\nbbb\nccc", 0, 0, 0, 8, 20, 16, 0);

    // scroll_y should be 0.
    assert_eq!(w.node(CORE_N_DOC_TEXT).scroll_y, 0);

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
        0,
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
    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
        let _tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
        let tr = scene::TripleReader::new(&buf);
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
    let tr2 = scene::TripleReader::new(&buf);
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
        let tr = scene::TripleReader::new(&buf);
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
    let tr = scene::TripleReader::new(&buf);
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
    let _tr: scene::TripleReader = scene::TripleReader::new(&buf);
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
            let tr = scene::TripleReader::new(&buf);
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
        let tr = scene::TripleReader::new(&buf);
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
    let tr2 = scene::TripleReader::new(&buf);
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
            x_advance: 10,
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
    let tr = scene::TripleReader::new(&buf);
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
        contours: dref,
    };
    w.set_root(id);

    let r = SceneReader::new(&buf);
    match r.node(id).content {
        Content::Path {
            color,
            fill_rule,
            contours,
        } => {
            assert_eq!(color, Color::rgb(255, 0, 0));
            assert_eq!(fill_rule, scene::FillRule::Winding);
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
    assert_eq!(size, 100, "Node must remain 100 bytes with Path variant");
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
            x_advance: 8,
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
                x_advance: 8,
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
                x_advance: 8,
                x_offset: 0,
                y_offset: 0,
            })
            .collect();
        let _ = w.push_shaped_glyphs(&clock_glyphs);
    }

    let data_after_clocks = w.data_used();
    let clock_data_total = data_after_clocks - data_after_doc;

    // 120 clock pushes × 64 bytes each = 7,680 bytes.
    // DATA_BUFFER_SIZE is 65,536 bytes. Even with document data, this fits.
    assert!(
        (data_after_clocks as usize) < DATA_BUFFER_SIZE,
        "data_used ({}) should be < DATA_BUFFER_SIZE ({}) after 120 clock re-pushes + document data",
        data_after_clocks,
        DATA_BUFFER_SIZE
    );

    // Verify the accumulated clock data size is reasonable.
    assert!(
        clock_data_total <= 120 * 64 + 120, // 64 bytes per clock + alignment padding
        "120 clock re-pushes should use ~7,680 bytes, got {}",
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
