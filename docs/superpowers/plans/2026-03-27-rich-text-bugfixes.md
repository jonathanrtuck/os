# v0.5 Rich Text Bug Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 11 bugs so rich text editing, rendering, and interaction work correctly with all 7 style types.

**Architecture:** Six tasks in dependency order. Each task is self-contained and leaves the build green. Tasks 1-2 fix critical editing/layout bugs. Task 3 adds italic font support (architectural). Task 4 improves the factory sample. Tasks 5-6 add polish (role-based style lookup, selection rendering).

**Tech Stack:** Rust (no_std, aarch64-unknown-none). Build: `cd system && cargo build --release`. Test: `cd system/test && cargo test -- --test-threads=1`. Visual: `hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture N /tmp/out.png --timeout 30`. Disk rebuild: `cd system && cargo build --release` (run.sh auto-rebuilds disk.img when mkdisk sources change).

**Spec:** `docs/superpowers/specs/2026-03-27-rich-text-bugfixes-design.md`

**IMPORTANT:** Always rebuild disk.img before visual testing by running `cargo build --release` from the `system/` directory. The disk image may contain stale content from previous test sessions.

---

## File Structure

### Modified Files

| File                                           | Changes                                                                |
| ---------------------------------------------- | ---------------------------------------------------------------------- |
| `system/libraries/layout/lib.rs`               | Fix newline line break in `break_measured_lines()`                     |
| `system/libraries/piecetable/lib.rs`           | Add `find_style_by_role()` function                                    |
| `system/libraries/protocol/content.rs`         | Add 3 italic font CONTENT_ID constants                                 |
| `system/services/core/input.rs`                | Rich text delete dispatch, role-based style lookup, heading shortcuts  |
| `system/services/core/documents.rs`            | Verify rich_delete/rich_delete_range work correctly                    |
| `system/services/core/layout/mod.rs`           | Italic font info, opsz axis support                                    |
| `system/services/core/layout/full.rs`          | RichFonts italic data, selection rendering, opsz in style registration |
| `system/services/core/main.rs`                 | Load italic font metrics, pass to layout                               |
| `system/services/init/main.rs`                 | Load 3 italic font files via 9p into Content Region                    |
| `system/services/drivers/metal-render/main.rs` | Add italic fonts to font_data_map                                      |
| `tools/mkdisk/main.rs`                         | Rich sample document with all 7 styles                                 |
| `system/test/tests/text_layout.rs`             | Newline line break tests                                               |
| `system/test/tests/piecetable.rs`              | find_style_by_role tests                                               |

---

## Task 1: Fix rich text editing (delete dispatch + backspace)

**Bugs fixed:** #2 (backspace wrong char), #3 (Cmd+A+backspace doesn't clear)

**Files:**

- Modify: `system/services/core/input.rs` — lines 497-600 (backspace/delete handlers)
- Modify: `system/services/core/documents.rs` — verify rich_delete, rich_delete_range

**Context:** `input.rs` calls `doc_delete_range()` (flat buffer, plain text) for ALL documents, even rich text. Rich text documents need `rich_delete_range()` (piece table). The document format is available as `s.doc_format == super::DocumentFormat::Rich`.

Five call sites in `input.rs` need format-checking:

- Line 504: selection + backspace → `doc_delete_range(lo, hi)`
- Line 523: Opt+backspace word delete → `doc_delete_range(boundary, cursor)`
- Line 552: selection + Delete key → `doc_delete_range(lo, hi)`
- Line 571: Opt+Delete word delete → `doc_delete_range(cursor, boundary)`
- Line 600: selection + character input → `doc_delete_range(lo, hi)`

For single-character backspace (no selection, no Opt): currently forwarded to editor process. Verify that the editor correctly calls `rich_delete(cursor_pos - 1)` not `rich_delete(cursor_pos)`. Read `system/user/rich-editor/main.rs` to trace the backspace path.

- [ ] **Step 1: Read input.rs and rich-editor/main.rs to understand the full backspace/delete path for rich text**

Read `system/services/core/input.rs` lines 495-610. Read `system/user/rich-editor/main.rs` to see how backspace is handled when forwarded. Read `system/services/core/documents.rs` for `rich_delete` and `rich_delete_range`.

- [ ] **Step 2: Add format-checking wrapper function**

In `input.rs`, add a helper near the existing `doc_delete_range` import:

```rust
/// Delete a byte range using the correct path for the current document format.
fn delete_range_for_format(start: usize, end: usize) -> bool {
    let s = super::state();
    if s.doc_format == super::DocumentFormat::Rich {
        super::documents::rich_delete_range(start, end)
    } else {
        doc_delete_range(start, end)
    }
}
```

- [ ] **Step 3: Replace all 5 call sites**

Replace `doc_delete_range(lo, hi)` → `delete_range_for_format(lo, hi)` at lines 504, 523, 552, 571, 600.

- [ ] **Step 4: Fix single-backspace path if needed**

After reading the editor code in step 1, fix any off-by-one in the backspace → delete position.

- [ ] **Step 5: Build and test**

```bash
cd system && cargo build --release
cd system/test && cargo test -- --test-threads=1
```

- [ ] **Step 6: Visual test — Cmd+A + backspace clears document**

```bash
# Rebuild disk image (fresh factory content)
cd system && cargo build --release

cat > /tmp/test-delete.events << 'SCRIPT'
wait 30
capture /tmp/before-delete.png
key cmd+a
key backspace
wait 5
capture /tmp/after-delete.png
SCRIPT

hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/test-delete.events --timeout 45
```

Expected: `after-delete.png` shows empty page (all text cleared).

- [ ] **Step 7: Commit**

```text
fix(core): dispatch rich text deletes through piece table

Cmd+A + backspace, Opt+backspace word delete, and selection delete
now use rich_delete_range() for text/rich documents instead of the
plain text doc_delete_range().
```

---

## Task 2: Fix newline line breaks

**Bug fixed:** #1 (newlines don't trigger line breaks)

**Files:**

- Modify: `system/libraries/layout/lib.rs` — `break_measured_lines()` (~line 459)
- Test: `system/test/tests/text_layout.rs`

**Context:** First verify the bug exists on a FRESH disk image. The previous screenshots may have had stale content from accumulated test edits.

The `break_measured_lines()` function at line 459 handles `is_newline` — it pushes a `LineBreak` and breaks the inner loop. The outer loop should then start a new line with reset state. Read the code carefully to determine if the bug is here or in `layout_rich_lines()` in core.

- [ ] **Step 1: Rebuild disk.img and verify newline behavior on clean factory document**

```bash
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/fresh-factory.png --timeout 30
```

Look at the screenshot. The factory text is `"Hello, World!\nWelcome to rich text."`. If "Hello, World!" is on line 1 and "Welcome to rich text." is on line 2, newlines work and this bug was caused by stale disk content.

- [ ] **Step 2: If newlines ARE broken, write a failing test**

Add to `system/test/tests/text_layout.rs`:

```rust
#[test]
fn break_measured_lines_newline_splits() {
    use layout::{break_measured_lines, BreakMode, MeasuredChar};
    let chars = vec![
        MeasuredChar { byte_offset: 0, byte_len: 1, width: 8.0, run_index: 0, is_whitespace: false, is_newline: false }, // 'A'
        MeasuredChar { byte_offset: 1, byte_len: 1, width: 0.0, run_index: 0, is_whitespace: false, is_newline: true },  // '\n'
        MeasuredChar { byte_offset: 2, byte_len: 1, width: 8.0, run_index: 0, is_whitespace: false, is_newline: false }, // 'B'
    ];
    let lines = break_measured_lines(&chars, 1000.0, BreakMode::Word);
    assert_eq!(lines.len(), 2, "newline should produce 2 lines");
    assert_eq!(lines[0].byte_start, 0);
    assert_eq!(lines[0].byte_end, 1); // 'A' only
    assert_eq!(lines[1].byte_start, 2);
    assert_eq!(lines[1].byte_end, 3); // 'B' only
}
```

Run: `cd system/test && cargo test text_layout -- --test-threads=1`

- [ ] **Step 3: Fix break_measured_lines if needed**

Based on the test result, fix the newline handling in `break_measured_lines()`. The likely fix: after the newline break, ensure the outer loop's next iteration starts with the correct `line_start_byte`.

- [ ] **Step 4: Verify visually on fresh disk**

```bash
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/newline-fixed.png --timeout 30
```

Expected: "Hello, World!" on line 1 (heading size), "Welcome to rich text." on line 2 (body size).

- [ ] **Step 5: Commit**

```text
fix(layout): newline line breaks in break_measured_lines
```

---

## Task 3: Italic font loading

**Bugs fixed:** #4 (italic doesn't render), #11 (italic font files not in Content Region)

**Files:**

- Modify: `system/libraries/protocol/content.rs` — add 3 italic content IDs
- Modify: `system/services/init/main.rs` — load 3 italic fonts via 9p (~line 1687)
- Modify: `system/services/core/layout/mod.rs` — StyleTable maps italic to italic content_id
- Modify: `system/services/core/layout/full.rs` — RichFonts with italic font data
- Modify: `system/services/core/main.rs` — load italic font metrics
- Modify: `system/services/drivers/metal-render/main.rs` — add italic fonts to font_data_map

**Context:** Inter, JetBrains Mono, and Source Serif 4 all have separate italic font files. They're variable fonts for weight (wght) but italic is a separate file. The italic files are already in `system/share/` as `inter-italic.ttf`, `jetbrains-mono-italic.ttf`, `source-serif-4-italic.ttf`.

The key insight: when a piece table style has `FLAG_ITALIC`, the StyleTable should register it with the ITALIC font's content_id, not the regular font. The `ital` axis value (1.0) should NOT be included in axes — the font is already italic. Only `wght` applies.

- [ ] **Step 1: Add italic content IDs to protocol**

Add to `system/libraries/protocol/content.rs` after the existing constants:

```rust
pub const CONTENT_ID_FONT_MONO_ITALIC: u32 = 4;
pub const CONTENT_ID_FONT_SANS_ITALIC: u32 = 5;
pub const CONTENT_ID_FONT_SERIF_ITALIC: u32 = 6;
```

- [ ] **Step 2: Load italic fonts in init**

In `system/services/init/main.rs` at line ~1687, extend the `fonts_9p` array from 3 to 6 entries. For the disk-loading path (~line 1178), also load italic fonts. Follow the exact same pattern as regular fonts — read file, write to Content Region, register entry.

```rust
let fonts_9p: [(&[u8], u32, &[u8]); 6] = [
    (b"jetbrains-mono.ttf", CONTENT_ID_FONT_MONO, b"mono"),
    (b"inter.ttf", CONTENT_ID_FONT_SANS, b"sans"),
    (b"source-serif-4.ttf", CONTENT_ID_FONT_SERIF, b"serif"),
    (b"jetbrains-mono-italic.ttf", CONTENT_ID_FONT_MONO_ITALIC, b"mono-italic"),
    (b"inter-italic.ttf", CONTENT_ID_FONT_SANS_ITALIC, b"sans-italic"),
    (b"source-serif-4-italic.ttf", CONTENT_ID_FONT_SERIF_ITALIC, b"serif-italic"),
];
```

Also load italic fonts from the disk-loading path (native filesystem). Check both loading paths in init.

- [ ] **Step 3: Extend core with italic font info**

In `services/core/main.rs`, load italic font metrics at startup (same pattern as regular fonts — find entry in Content Region, call `font_metrics()`).

In `layout/mod.rs`, the `SceneConfig` and `StyleTable` need italic content_ids and metrics. When registering a style with `FLAG_ITALIC`:

```rust
let content_id = if style.flags & piecetable::FLAG_ITALIC != 0 {
    match style.font_family {
        piecetable::FONT_MONO => mono_italic_content_id,
        piecetable::FONT_SERIF => serif_italic_content_id,
        _ => sans_italic_content_id,
    }
} else {
    // regular content_id
};
// Do NOT include ital axis — the font IS italic
let mut axes = vec![];
if style.weight != 400 {
    axes.push(AxisValue { tag: *b"wght", value: style.weight as f32 });
}
```

- [ ] **Step 4: Extend RichFonts with italic data**

In `layout/full.rs`, add italic font data to `RichFonts`:

```rust
pub struct RichFonts<'a> {
    // ... existing regular fonts ...
    pub mono_italic_data: &'a [u8],
    pub mono_italic_upem: u16,
    pub mono_italic_content_id: u32,
    // ... same for sans_italic, serif_italic ...
}
```

Update `resolve()` to return the italic font data when the style has `FLAG_ITALIC`.

- [ ] **Step 5: Add italic fonts to metal-render font_data_map**

In `metal-render/main.rs`, find where `font_data_map` is built (3 entries for regular fonts). Extend to 6 entries by looking up the italic content IDs in the Content Region.

- [ ] **Step 6: Build and test**

```bash
cd system && cargo build --release
cd system/test && cargo test -- --test-threads=1
```

- [ ] **Step 7: Commit**

```text
feat: load italic font files for all three font families

Inter, JetBrains Mono, and Source Serif 4 italic files loaded into
Content Region. StyleTable maps italic styles to italic content_ids.
Renderer resolves italic font data via style registry.
```

---

## Task 4: Factory sample document + optical size

**Bugs fixed:** #7 (minimal factory doc), #9 (color not tested), #10 (opsz not used)

**Files:**

- Modify: `tools/mkdisk/main.rs` — richer sample document
- Modify: `system/services/core/layout/mod.rs` — add opsz axis to style registration
- Modify: `system/services/core/layout/full.rs` — pass opsz when building axes

**Context:** The factory document should showcase all 7 styles. Also add `opsz` (optical size) axis to style registrations — Inter and Source Serif 4 have opsz axes.

- [ ] **Step 1: Update mkdisk sample document**

Replace the sample text in `tools/mkdisk/main.rs` with a richer document:

```rust
let sample_text = b"Rich Text Demo\nTypography\nBody text. Bold text. Italic text. Bold italic. Inline code.\nParagraphs\nSecond paragraph to verify newline handling.\n";
```

Apply styles to ranges:

- "Rich Text Demo\n" → heading1 (style 1)
- "Typography\n" → heading2 (style 2)
- "Body text. " → body (style 0)
- "Bold text. " → bold (style 3)
- "Italic text. " → italic (style 4)
- "Bold italic. " → bold-italic (style 5)
- "Inline code." → code (style 6)
- "\nParagraphs\n" → heading2 (style 2)
- "Second paragraph..." → body (style 0)

- [ ] **Step 2: Add opsz axis to style registration**

In core's layout, when building axes for the StyleTable, check if the font has an `opsz` axis. If so, include `opsz = font_size_pt` as an axis value. The `fonts::rasterize::font_axes()` function returns the available axes — check at startup and cache which fonts support opsz.

- [ ] **Step 3: Build, rebuild disk, visual test**

```bash
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/sample-doc.png --timeout 30
```

Expected: Multiple styles visible — large heading, medium subheading, body with inline bold (heavier), italic (slanted), code (mono, gray), all on separate lines.

- [ ] **Step 4: Commit**

```text
feat: rich factory document with all 7 styles + optical size axis

Sample document showcases heading1, heading2, body, bold, italic,
bold-italic, and code styles. Optical size axis (opsz) enabled for
Inter and Source Serif 4.
```

---

## Task 5: Style ID by role + heading shortcuts

**Bugs fixed:** #6 (hardcoded style IDs), #8 (no heading shortcuts)

**Files:**

- Modify: `system/libraries/piecetable/lib.rs` — add `find_style_by_role()`
- Modify: `system/services/core/input.rs` — use role lookup for Cmd+B/I, add Cmd+1/2
- Test: `system/test/tests/piecetable.rs`

- [ ] **Step 1: Write failing test for find_style_by_role**

Add to `system/test/tests/piecetable.rs`:

```rust
#[test]
fn find_style_by_role_finds_bold() {
    let buf = make_with_text(b"hello", 4096);
    // Default palette has bold at role ROLE_STRONG
    let id = piecetable::find_style_by_role(&buf, piecetable::ROLE_STRONG);
    assert!(id.is_some());
    let style = piecetable::style(&buf, id.unwrap()).unwrap();
    assert_eq!(style.weight, 700);
}

#[test]
fn find_style_by_role_returns_none_for_missing() {
    let buf = make_empty(4096);
    // Empty table has no styles
    assert!(piecetable::find_style_by_role(&buf, piecetable::ROLE_STRONG).is_none());
}
```

- [ ] **Step 2: Implement find_style_by_role**

Add to `system/libraries/piecetable/lib.rs`:

```rust
/// Find the first style in the palette with the given semantic role.
pub fn find_style_by_role(buf: &[u8], role: u8) -> Option<u8> {
    let h = header(buf);
    for i in 0..h.style_count {
        if let Some(s) = style(buf, i) {
            if s.role == role {
                return Some(i);
            }
        }
    }
    None
}
```

- [ ] **Step 3: Update input.rs to use role lookup**

Replace hardcoded `3` and `4` in Cmd+B and Cmd+I handlers with:

```rust
let bold_id = piecetable::find_style_by_role(buf, piecetable::ROLE_STRONG).unwrap_or(0);
let italic_id = piecetable::find_style_by_role(buf, piecetable::ROLE_EMPHASIS).unwrap_or(0);
```

- [ ] **Step 4: Add Cmd+1 and Cmd+2 shortcuts**

In input.rs, add handlers for KEY_1 + cmd and KEY_2 + cmd. Same toggle pattern as Cmd+B — look up `ROLE_HEADING1` / `ROLE_HEADING2`, toggle between that style and body (style 0).

- [ ] **Step 5: Build and test**

```bash
cd system && cargo build --release
cd system/test && cargo test -- --test-threads=1
```

- [ ] **Step 6: Commit**

```text
feat(core): role-based style lookup and heading shortcuts

Cmd+B/I now look up styles by ROLE_STRONG/ROLE_EMPHASIS instead of
hardcoded indices. Added Cmd+1 (heading1) and Cmd+2 (heading2).
```

---

## Task 6: Selection rendering for rich text

**Bug fixed:** #5 (selection not rendered)

**Files:**

- Modify: `system/services/core/layout/full.rs` — implement `allocate_rich_selection_rects()`

**Context:** The mono path has `allocate_selection_rects()` that creates colored background rectangles. Rich text needs the same but with proportional x-positioning using glyph advances.

- [ ] **Step 1: Read the mono selection implementation**

Read `allocate_selection_rects()` in `layout/full.rs` (or `layout/mod.rs`). Understand: how selection rects are positioned (x from column × char_width, y from line × line_height), how they're linked as children of `N_DOC_TEXT`, and what color/opacity they use.

- [ ] **Step 2: Implement allocate_rich_selection_rects()**

Create a new function in `layout/full.rs`:

```rust
fn allocate_rich_selection_rects(
    w: &mut scene::SceneWriter<'_>,
    rich_lines: &[RichLine],
    scratch: &[u8],
    pt_buf: &[u8],
    fonts: &RichFonts<'_>,
    sel_start: u32,
    sel_end: u32,
    scroll_y: scene::Mpt,
    viewport_height: i32,
) -> Option<u16> { ... }
```

For each `RichLine` that overlaps [sel_start, sel_end):

1. Compute x_start: walk shaped glyphs from line start to `max(sel_start, line_start)`, summing advances
2. Compute x_end: continue walking to `min(sel_end, line_end)`
3. Emit a selection rect node at (x_start, line.y) with width (x_end - x_start) and height line.line_height
4. Use the same selection color as the mono path

- [ ] **Step 3: Wire into build_rich_document_content()**

Replace `let _ = (sel_start, sel_end);` with a call to `allocate_rich_selection_rects()`. Link the selection nodes before the line text nodes (so selection appears behind text).

- [ ] **Step 4: Build and visual test**

```bash
cd system && cargo build --release

cat > /tmp/test-selection.events << 'SCRIPT'
wait 30
key cmd+a
wait 5
capture /tmp/rich-selection.png
SCRIPT

hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/test-selection.events --timeout 45
```

Expected: Blue/purple selection highlight visible behind all text.

- [ ] **Step 5: Commit**

```text
feat(core): selection rendering for rich text documents

Proportional selection rectangles using glyph advance measurement.
Same visual style as mono text selection.
```

---

## Summary

| Task                       | Bugs Fixed  | Complexity  | Dependencies                |
| -------------------------- | ----------- | ----------- | --------------------------- |
| 1. Delete dispatch         | #2, #3      | Low         | None                        |
| 2. Newline breaks          | #1          | Low-Med     | None (verify first)         |
| 3. Italic fonts            | #4, #11     | High        | None                        |
| 4. Factory doc + opsz      | #7, #9, #10 | Medium      | Task 3 (italic in sample)   |
| 5. Role lookup + shortcuts | #6, #8      | Low         | None                        |
| 6. Selection rendering     | #5          | Medium-High | Task 2 (line breaks needed) |

Tasks 1, 2, 3, and 5 are independent and can be parallelized.
