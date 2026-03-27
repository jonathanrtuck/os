# v0.5 Rich Text Bug Fixes

**Date:** 2026-03-27
**Status:** Approved
**Scope:** 11 bugs across layout, editing, font loading, and UI

## Bug Index

| # | Bug | Severity | Category |
|---|-----|----------|----------|
| 1 | Newlines don't trigger line breaks | CRITICAL | Layout |
| 2 | Backspace deletes wrong character | CRITICAL | Editing |
| 3 | Cmd+A + backspace doesn't clear document | CRITICAL | Editing |
| 4 | Italic doesn't render (no italic font loaded) | HIGH | Font loading |
| 5 | Selection not rendered for rich text | HIGH | Layout |
| 6 | Style IDs hardcoded in input.rs | MEDIUM | Editing |
| 7 | Factory document only shows 2 of 7 styles | MEDIUM | Testing |
| 8 | No heading/code keyboard shortcuts | LOW | Editing |
| 9 | Color not visually verified | LOW | Testing |
| 10 | Optical size axis not used | LOW | Rendering |
| 11 | Italic font files not in Content Region | HIGH | Font loading |

Bugs 4 and 11 are the same root cause (italic font architecture). Bugs 7 and 9 are solved by the same fix (better factory document). Effective fix count: 9.

## Fix 1: Newline line breaks

**Root cause:** `libraries/layout/lib.rs`, `break_measured_lines()`. After emitting a `LineBreak` for a `\n` character, the function doesn't reset `line_start_byte` and `width` for the next line. The outer loop continues with stale state, so subsequent text gets merged into the previous line's byte range.

**Fix:** After pushing the `LineBreak` and incrementing `i` past the newline, reset:
```
line_start_byte = chars[i].byte_offset  // start of next line
width = 0.0                              // reset accumulated width
```

**Test:** Create piece table with "line1\nline2". Run `layout_rich_lines()`. Assert two `RichLine` entries with correct byte ranges and y-offsets.

## Fix 2: Rich text delete dispatch (backspace + delete-range)

**Root cause:** `services/core/input.rs` calls `doc_delete_range()` (flat buffer, plain text) for rich text documents instead of `rich_delete_range()` (piece table). Multiple call sites (~lines 504, 552, 571, 600).

For single backspace, the editor forwards the key to core, which calls `rich_delete(cursor_pos)` — but this deletes the byte AT the cursor instead of BEFORE it. Should be `rich_delete(cursor_pos - 1)`.

**Fix:** At every delete call site in `input.rs`, check `s.doc_format == DocumentFormat::Rich` and dispatch to the piece table function:
```rust
if s.doc_format == super::DocumentFormat::Rich {
    super::documents::rich_delete_range(lo, hi)
} else {
    doc_delete_range(lo, hi)
}
```

For backspace without selection: ensure the position passed is `cursor_pos - 1`, not `cursor_pos`.

**Test:** Boot, type "abc", press backspace. Verify "ab" remains (not "ac" or "bc"). Boot, Cmd+A, backspace. Verify document is empty.

## Fix 3: Italic font loading

**Root cause:** Inter has `wght` and `opsz` axes but NO `ital` axis. Italic is a separate font file (`inter-italic.ttf`). Setting `ital=1.0` as a variable axis does nothing. The italic font files exist in `system/share/` but aren't loaded into Content Region.

**Fix:**
1. **Init** loads 6 font files into Content Region (3 regular + 3 italic):
   - `CONTENT_ID_FONT_MONO` (existing), `CONTENT_ID_FONT_MONO_ITALIC` (new)
   - `CONTENT_ID_FONT_SANS` (existing), `CONTENT_ID_FONT_SANS_ITALIC` (new)
   - `CONTENT_ID_FONT_SERIF` (existing), `CONTENT_ID_FONT_SERIF_ITALIC` (new)

2. **Core's StyleTable** maps italic styles to the italic font's content_id:
   ```
   italic body style → content_id = CONTENT_ID_FONT_SANS_ITALIC, axes = [wght=400]
   bold italic style → content_id = CONTENT_ID_FONT_SANS_ITALIC, axes = [wght=700]
   ```

3. **Axis building** in layout and scene building: when a style has `FLAG_ITALIC`, use the italic font's content_id. Do NOT add `ital=1.0` axis (the font is already italic). Only add `wght` if weight != 400.

4. **Renderer** already resolves font data by content_id from the style registry — no renderer changes needed.

5. **Font metrics** for italic fonts: load via `fonts::rasterize::font_metrics()` at startup, store alongside regular font metrics. Italic fonts have different metrics (often wider, sometimes different ascent).

**Files:**
- `protocol/content.rs` — add 3 new CONTENT_ID constants
- `services/init/main.rs` — load 3 italic font files from 9p into Content Region
- `services/core/layout/mod.rs` — StyleTable maps italic styles to italic content_ids
- `services/core/layout/full.rs` — RichFonts extended with italic font data
- `services/core/main.rs` — load italic font metrics, pass to layout
- `services/drivers/metal-render/main.rs` — add 3 italic fonts to font_data_map

## Fix 4: Factory sample document

**Fix:** Update `tools/mkdisk/main.rs` to create a rich text document showcasing all 7 styles:

```
[heading1] Rich Text Demo\n
[heading2] Typography\n
[body] Body text in Inter at 14pt. [bold]Bold text.[body] Normal again. [italic]Italic text.[body] And [bold-italic]bold italic.[body] Finally [code]inline code[body] in mono.\n
[heading2] Paragraphs\n
[body] This is a second paragraph to verify newline handling works correctly across style boundaries.\n
```

This provides visual verification of: heading1 (24pt bold), heading2 (18pt semibold), body (14pt regular), bold (14pt w700), italic (14pt italic font), bold-italic (14pt w700 italic font), code (13pt mono, #666666 color).

## Fix 5: Selection rendering for rich text

**Root cause:** `build_rich_document_content()` in `layout/full.rs` explicitly skips selection with `let _ = (sel_start, sel_end)`.

**Fix:** Implement rich text selection rectangles. For each `RichLine` that overlaps the selection range `[sel_start, sel_end)`:
1. Find the x-start within the line by summing glyph advances from line start to `max(sel_start, line_byte_start)`
2. Find the x-end by summing advances to `min(sel_end, line_byte_end)`
3. Emit a selection rectangle node with the line's y and line_height

Reuse the existing selection color and node structure from the mono path (`allocate_selection_rects`). The key difference: mono uses fixed `char_width`, rich uses per-segment proportional advances.

**Implementation:** Add `allocate_rich_selection_rects()` function in `layout/full.rs`. Walk the `rich_lines`, measure x positions for selection boundaries using the same shaping used for cursor positioning (`rich_cursor_position` pattern). Emit nodes as children of `N_DOC_TEXT` before the line nodes.

## Fix 6: Style ID by role lookup

**Root cause:** `input.rs` uses hardcoded `3` for bold, `4` for italic. Breaks if palette order changes.

**Fix:** Add to `piecetable` library:
```rust
pub fn find_style_by_role(buf: &[u8], role: u8) -> Option<u8>
```
Scans the style palette and returns the first style_id with the matching `role` field.

In `input.rs`, Cmd+B calls `find_style_by_role(buf, ROLE_STRONG)`, Cmd+I calls `find_style_by_role(buf, ROLE_EMPHASIS)`. Falls back to style 0 (body) when toggling off.

## Fix 7: Heading shortcuts

**Fix:** Add to `input.rs`:
- Cmd+1 → toggle heading1 (lookup `ROLE_HEADING1`)
- Cmd+2 → toggle heading2 (lookup `ROLE_HEADING2`)

Same toggle pattern as Cmd+B: if current style is already heading → revert to body, else apply heading.

## Fix 8: Optical size axis

**Fix:** When building axes for a style in the StyleTable, add `opsz` axis with `font_size_pt` as the value, but only for fonts that have an `opsz` axis (Inter and Source Serif 4). JetBrains Mono doesn't have opsz.

Check at startup: `fonts::rasterize::font_axes(font_data)` returns the list of axes. If `opsz` is in the list, include it in style registrations.

Result: headings at 24pt get `opsz=24` (wider, more display-like shapes), body at 14pt gets `opsz=14` (optimized for text). This is the typographically correct behavior.

## Execution Order

1. **Newline fix** (layout library) — unblocks visual verification of everything
2. **Delete dispatch** (input.rs) — makes editing work
3. **Italic font loading** (init, protocol, core, renderer) — architectural, enables italic
4. **Factory document** (mkdisk) + optical size — visual verification of all styles
5. **Style ID by role** (piecetable, input.rs) + heading shortcuts
6. **Selection rendering** (layout/full.rs) — most complex, last

## Testing Strategy

**Visual TDD for each fix:** Boot, capture, measure with imgdiff.py.

After all fixes, a single screenshot of the factory document should show:
- Large bold heading ("Rich Text Demo") on its own line
- Medium semibold subheading ("Typography") on its own line
- Body text with inline bold (heavier), italic (slanted), bold-italic, and code (different font, gray color)
- Second subheading and paragraph on separate lines
- Each style visually distinct
