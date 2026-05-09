# v0.6 Parity Plan

Continuation of `design/userspace-rebuild.md`. The base rebuild (Phases 1–5) is
complete: kernel, service infrastructure, drivers, libraries, core services, and
the end-to-end input-to-pixels pipeline all work. This plan closes the remaining
gaps between the rebuild and the v0.6-pre-rewrite prototype.

**Goal:** Full v0.6 functional and visual parity — real glyph rendering, cursor
blink, selection, keyboard navigation, scroll, visual chrome, content-type
typography, PNG decoding, and host filesystem access.

**Working mode:** Autonomous across sessions. Each phase has explicit entry
conditions, implementation steps, and verification criteria. A new session reads
this file, checks STATUS.md and `git log --oneline -20`, identifies the current
phase, and continues.

**Reference tag:** `v0.6-pre-rewrite` — use `git show v0.6-pre-rewrite:<path>`
for any v0.6 source.

---

## Phase 6 — Glyph Atlas + Textured Rendering

**The single highest-leverage change.** Transforms output from colored
rectangles to actual rendered text.

### What exists now

- `libraries/fonts/` — font parsing, metrics, shaping, rasterization (3,403 LOC,
  32 tests). Produces `ShapedGlyph` arrays and can rasterize to grayscale
  bitmaps.
- `libraries/render/` — `CommandWriter` for Metal-over-virtio commands. Already
  has `PIXEL_FORMAT_BGRA8_SRGB`, `PRIM_TRIANGLE`, texture commands.
- Compositor (`userspace/servers/drivers/render/`) — walks scene graph, emits
  solid-color rectangles. Single shader pipeline (vertex + fragment_solid).

### What v0.6 had

- `services/drivers/metal-render/atlas.rs` — 2048×2048 glyph texture atlas with
  open-addressed hash table keyed on `(glyph_id, font_size_px, style_id)`.
  Row-based bin packing.
- `services/drivers/metal-render/shaders.rs` — sRGB-correct shaders: solid,
  textured (glyph atlas sampling), glyph (alpha-only coverage × vertex color),
  blur (horizontal/vertical Gaussian), stencil (path fill).
- `services/drivers/metal-render/scene_walk.rs` — per-node-type vertex emission
  with separate solid and glyph vertex buffers, flushed with different
  pipelines.
- Three Metal pipelines: `PIPE_SOLID`, `PIPE_GLYPH` (alpha blended, samples
  atlas texture), `PIPE_TEXTURED` (full RGBA texture).

### Implementation

#### 6.1 — Metal texture + glyph shader

Add a `fragment_glyph` shader to the compositor's MSL source:

- Samples a texture at `texCoord`, uses the red channel as coverage (alpha-only
  atlas)
- Multiplies coverage by vertex color (sRGB-linearized)
- Returns premultiplied-alpha linear RGBA

Add Metal setup commands to create:

- A 2048×2048 R8Unorm texture (handle `TEX_ATLAS`)
- A nearest-neighbor sampler (handle `SAMPLER_NEAREST`)
- A second render pipeline `PIPE_GLYPH` using `vertex_main` + `fragment_glyph`,
  with alpha blending enabled (srcAlpha + oneMinusSrcAlpha)

Port the sRGB conversion functions from
`v0.6-pre-rewrite:services/drivers/metal-render/shaders.rs`.

**Add to `libraries/render/`:** `CommandWriter` methods for `create_texture`,
`update_texture_region`, `set_fragment_texture`, `set_fragment_sampler`, and the
`create_render_pipeline` variant with alpha blending. Check what already exists
there first — some of these may already be present from the v0.6 port.

**Verification:** Compile, pipeline setup succeeds (no hypervisor crash on
boot). Existing solid-color rendering still works.

#### 6.2 — Glyph atlas

Port `atlas.rs` from v0.6 into `userspace/servers/drivers/render/src/atlas.rs`.
Adapt to the current crate structure:

- Open-addressed hash table, `pack_key(glyph_id, font_size_px, style_id)`
- Row-based bin packing (rows of uniform height, advance x cursor, wrap to next
  row when full)
- `lookup_or_rasterize()` — returns `AtlasEntry` with (u, v, width, height,
  bearing_x, bearing_y). On cache miss: calls `fonts` library to rasterize the
  glyph bitmap, uploads pixels via `update_texture_region` Metal command,
  inserts into hash table.

The atlas needs access to font data. Two approaches:

1. Embed font bytes as `static` data in the compositor binary (simplest, what
   v0.6 did for the initial implementation)
2. Receive font data via a shared VMO from init

**Start with approach 1** — include Inter and JetBrains Mono as `&[u8]` statics
using `include_bytes!()`. This avoids a new IPC path and is sufficient for v0.6
parity.

**Verification:** Host-target unit tests for atlas hash table (insert, lookup,
collision, wrap). Bare-metal: boot doesn't crash, atlas allocates entries.

#### 6.3 — Textured glyph rendering in scene walk

Replace the solid-rectangle glyph rendering in `walk_node()` with textured quad
emission:

1. For each `Content::Glyphs` node, iterate `ShapedGlyph` array
2. For each glyph: `atlas.lookup_or_rasterize(glyph_id, font_size, style_id)`
3. Emit a textured quad: position from glyph advance + bearing offsets, texcoord
   from atlas entry (u, v, width, height normalized to atlas dimensions)
4. Collect glyph vertices in a separate buffer from solid vertices

Frame render sequence:

1. Begin render pass (clear background)
2. Set `PIPE_SOLID`, draw solid vertices (backgrounds, cursor)
3. Flush solid vertices
4. Set `PIPE_GLYPH`, bind atlas texture + sampler, draw glyph vertices
5. End render pass, present

Port `flush_solid_vertices` and `flush_vertices_raw` patterns from
`v0.6-pre-rewrite:services/drivers/metal-render/scene_walk.rs`.

**Verification:**

- `cargo r` boots, types text, sees actual glyph shapes instead of rectangles
- Screenshot test: `test/verify.py` — glyphs are non-uniform (not all same
  color), pixel values vary within a glyph bounding box (anti-aliased edges)
- Compare a "hello" screenshot against known glyph positions

#### 6.4 — Font embedding in presenter scene graph

The presenter builds `Content::Glyphs` nodes with `ShapedGlyph` arrays. Today
these come from the layout service's monospace character-width assumption. For
real rendering, the presenter (or layout service) needs to use the `fonts`
library for actual shaping.

Update the layout service:

- Embed font data (same `include_bytes!` approach)
- Use `fonts::shape()` for actual HarfBuzz shaping instead of fixed-width
  character positioning
- Write real `ShapedGlyph` data (glyph IDs, x_advance, x_offset, y_offset) into
  the layout results VMO

The compositor then uses these real glyph IDs and advances, matching them
against its atlas.

**Verification:**

- Layout test: shaped output for "Hello" produces varying glyph IDs and
  non-uniform advances (proportional font)
- Visual: proportionally-spaced text renders correctly on screen
- Screenshot: character spacing matches v0.6 baseline (within 2px tolerance)

### Phase 6 done when

- Text renders as actual anti-aliased glyphs, not colored rectangles
- JetBrains Mono for code content (monospace still works)
- Screenshot test passes with glyph-level verification
- All existing tests still pass

---

## Phase 7 — Cursor Blink + Selection

### What v0.6 had

- `services/presenter/blink.rs` — 4-phase blink: visible hold (500ms) → fade out
  (150ms, eased) → hidden hold (300ms) → fade in (150ms, eased). Driven by the
  `animation` library timeline. `reset_blink()` on user input.
- Selection: `build_selection_update()` in scene builder. Selection rectangles
  as background-colored nodes behind text. Multi-line selections span from
  sel_start to end-of-line, full middle lines, start-of-line to sel_end.

### Implementation

#### 7.1 — Cursor blink

Port `blink.rs` logic into the presenter:

- Add blink state to the presenter's state struct (phase, phase_start timestamp,
  opacity, animation ID)
- Use the `animation` library's `Timeline` for eased fade transitions
- On key event: call `reset_blink()` (cursor immediately visible, restart cycle)
- On timer tick: advance blink state machine, update cursor node opacity in
  scene graph, trigger re-render if opacity changed

The presenter needs a timer or polling mechanism. Two options:

1. Use `clock_read` to poll time on each IPC recv timeout
2. Use a kernel event with a timer signal

**Use option 1** — the presenter already recv's with a timeout. Check
`abi::ipc::recv_timeout` or add a timeout to the serve loop. On timeout (no
message), advance blink and re-render if needed.

**Verification:** Visual: cursor fades in and out. Typing resets to solid.
Screenshot at t=0 (just typed): cursor visible. Screenshot at t=700ms: cursor
gone or fading.

#### 7.2 — Text selection

Add selection state to the presenter: `sel_start`, `sel_end` (byte offsets into
document buffer). Selection is a presenter concern — the document service
doesn't know about it.

Scene graph changes:

- Add selection rectangle nodes between the background and text content
- Selection color: semi-transparent highlight (v0.6 used system accent color)
- Multi-line: calculate selection geometry from layout results (line info array)

Key bindings for selection:

- Shift+Left/Right: extend selection by character
- Shift+Up/Down: extend selection by line (requires Phase 8 arrow keys)
- Shift+Home/End: extend to line start/end
- Cmd+A: select all

Selection-aware editing:

- When selection is active and a character is typed: delete selection, insert
  character (single edit operation)
- Backspace/Delete with selection: delete selection range

Requires adding `DELETE_RANGE` to the editor→document protocol (delete from byte
offset A to byte offset B).

**Verification:**

- Shift+Right extends selection, visible highlight appears
- Type over selection: selection replaced with new character
- Multi-line selection: highlight spans correct line regions
- Screenshot test: selection rectangle color and position

### Phase 7 done when

- Cursor blinks with 4-phase eased animation
- Text selection works with Shift+arrow keys
- Typing over a selection replaces it
- Cmd+A selects all

---

## Phase 8 — Keyboard Navigation

### What v0.6 had

- Arrow keys (Up/Down/Left/Right) with character, word (Opt+arrow), and line
  (Cmd+arrow) granularity
- Home/End (line start/end)
- Page Up/Page Down (viewport-height scroll)
- Word boundary detection via `layout::word_boundary_forward/backward`
- Visual line navigation (cursor column preserved across Up/Down moves using a
  "sticky column" — the column is remembered when moving vertically and applied
  to each new line)
- All navigation with Shift modifier extends selection (Phase 7)

### Implementation

#### 8.1 — Arrow keys (Left/Right)

Add Left/Right key handling to the text editor:

- Left: move cursor back one byte (or to previous UTF-8 char boundary)
- Right: move cursor forward one byte (or to next UTF-8 char boundary)
- Opt+Left: `word_boundary_backward` (already exists in `libraries/layout/`)
- Opt+Right: `word_boundary_forward`
- Cmd+Left: start of current visual line
- Cmd+Right: end of current visual line

The text editor dispatches cursor movement to the document service via
`CURSOR_MOVE` IPC. Add new `CursorMove` variants if needed, or compute the
target position in the editor and send an absolute `SET_CURSOR`.

**Verification:** Type text, arrow left/right, verify cursor position changes
correctly. Word boundaries at spaces and punctuation.

#### 8.2 — Arrow keys (Up/Down) + sticky column

Up/Down require layout knowledge — the editor needs to know line boundaries to
move the cursor to the same column on the adjacent line.

Two options:

1. Editor queries the layout service for line info
2. Presenter handles Up/Down (it already has layout results)

**Use option 2** — the presenter owns the viewport and layout results. When it
receives an Up/Down key event, instead of forwarding to the editor, it computes
the new cursor position from the layout results VMO and sends a `SET_CURSOR` to
the document service directly.

Sticky column: when moving Up/Down, remember the original visual column. Apply
it to each new line. Reset sticky column on Left/Right or character insertion.

**Verification:** Type multi-line text. Up/Down moves between lines. Cursor
stays in the same column (or clamps to shorter line length). Sticky column
persists across multiple Up/Down moves.

#### 8.3 — Home/End, Page Up/Page Down

- Home: move to start of current visual line (from layout results)
- End: move to end of current visual line
- Page Up: scroll up by viewport height, move cursor accordingly
- Page Down: scroll down by viewport height

These all operate on the layout results, so they live in the presenter.

**Verification:** Home/End move to line boundaries. Page Up/Down scroll the
viewport and move the cursor.

### Phase 8 done when

- All arrow key combinations work (plain, Opt, Cmd, Shift)
- Up/Down navigate visual lines with sticky column
- Home/End/PageUp/PageDown work
- All navigation modifiers compose with Shift for selection

---

## Phase 9 — Scroll + Viewport

### What v0.6 had

- Smooth scroll with the scroll wheel / trackpad (via input driver pointer
  events)
- Keyboard-driven scroll (Page Up/Down, arrow keys that move past viewport
  edges)
- Viewport tracking: document taller than screen scrolls, cursor stays visible
- Scroll clamping (can't scroll past document end)
- `Content` clipping in the scene graph (`clips_children` flag on viewport node)

### Implementation

#### 9.1 — Viewport state management

The presenter already writes viewport state to a seqlock VMO for the layout
service (`ViewportState: scroll_y, viewport_width, viewport_height`). Extend
this:

- Track `scroll_y` (in millipoints, matches scene graph coordinate system)
- On cursor movement: auto-scroll to keep cursor visible ("scroll into view")
- On Page Up/Down: adjust scroll_y by viewport_height
- Clamp: `0 <= scroll_y <= max(0, total_doc_height - viewport_height)`

#### 9.2 — Scroll in scene graph

The scene graph already has a viewport node with `clips_children`. The presenter
applies scroll_y as a negative y-offset on the content container node inside the
viewport. Children are positioned relative to the container, so scrolling is
just translating the container.

#### 9.3 — Input driver scroll events

The input driver already handles `EV_ABS` events. Add scroll wheel / trackpad
scroll support:

- `REL_WHEEL` (vertical scroll) and `REL_HWHEEL` (horizontal)
- Forward to presenter as a new `SCROLL_EVENT` IPC message
- Presenter updates scroll_y, triggers layout recompute + scene rebuild

**Verification:**

- Type enough text to exceed viewport height
- Arrow down past bottom: viewport scrolls to keep cursor visible
- Page Down: viewport jumps by screen height
- Scroll event from input: smooth viewport movement
- Can't scroll past document boundaries

### Phase 9 done when

- Documents taller than the viewport scroll correctly
- Cursor always stays visible (auto-scroll on navigation)
- Page Up/Down scrolls by viewport height
- Scroll clamped to document bounds

---

## Phase 10 — Visual Chrome

### What exists now

- Scene library (`libraries/scene/`) — `Node` struct has generic shadow fields:
  `shadow_color` (RGBA8), `shadow_offset_x/y` (i16), `shadow_blur_radius` (u8),
  `shadow_spread` (i8). `has_shadow()` returns true when any shadow parameter is
  non-default. `Content::Path` variant stores fill/stroke colors, fill rule,
  stroke width, and path command data. `CursorShape` enum
  (Inherit/Pointer/Text). Path command parsing and stroke expansion in
  `stroke.rs`.
- Render library (`libraries/render/`) — `emit_shadow_quad` (6-vertex quad with
  3σ padding), `pack_shadow_params` (48-byte uniform), `render_shadow` and
  `shadow_overflow` in CPU scene walker. No GPU pipeline support yet.
- Icons library (`libraries/icons/`) — Tabler icon path data, 1,080 LOC, 37
  tests. Compile-time SVG path extraction.
- Compositor (`userspace/servers/drivers/render/`) — walks scene graph, emits
  solid-color backgrounds and glyph atlas quads. Two pipelines: `PIPE_SOLID`,
  `PIPE_GLYPH`. No shadow, stencil, or path rendering.

### What v0.6 had

- `PIPE_SHADOW` — analytical Gaussian fragment shader using separable erf()
  integrals for rectangles, SDF-based erfc for rounded rects. Single-pass, no
  offscreen render targets.
- Stencil-cover path rendering — stencil buffer on render target, stencil write
  pass rasterizes path geometry, cover pass draws bounding quad with stencil
  test. Fill and stroke support.
- Hardware cursor — vector path cursor rendered to dedicated GPU textures
  (MSAA + stencil + resolve + blur + sRGB). Separate hypervisor layer via
  `CMD_SET_CURSOR_FROM_TEXTURE`. Independent position updates via
  `CMD_SET_CURSOR_POSITION`.
- Title bar — full-width bar with document name, clock, mimetype icon.
- Document page — A4-proportioned white surface with box shadow (blur=64,
  spread=36), centered in viewport.

### Implementation

#### 10.1 — Analytical shadow pipeline

Add `PIPE_SHADOW` to the compositor's Metal shader and pipeline setup. The
shadow system is generic — any scene node with `has_shadow()` gets a shadow. The
document shadow is one consumer; cursor shadows and future UI elements reuse the
same pipeline.

Fragment shader (`fragment_shadow`):

- Receives shadow parameters as vertex attributes or uniform buffer: rect bounds
  (min/max in pixels), color (linear RGBA), sigma (pixels), corner radius
  (pixels)
- Sharp rectangles: separable 1D Gaussian integral —
  `alpha = shadow_1d(p.x, min_x, max_x, inv_s2) * shadow_1d(p.y, min_y, max_y, inv_s2)`
  where
  `shadow_1d(p, lo, hi, inv_s2) = 0.5 * (erf((hi-p)*inv_s2) - erf((lo-p)*inv_s2))`
- Rounded rectangles: SDF from `sd_rounded_rect()`, then
  `alpha = 0.5 * (1.0 - erf(dist * inv_s2))`
- `erf_approx`: Abramowitz & Stegun 7.1.26 (max error 1.5×10⁻⁷)
- Sigma conversion: `sigma_pt = blur_radius / 2.0` (W3C convention),
  `sigma_px = sigma_pt * scale`

Port `erf_approx` and `fragment_shadow` from
`v0.6-pre-rewrite:services/drivers/metal-render/shaders.rs`.

Compositor architecture change — **streaming vertex submission:**

The current compositor accumulates all vertices into unbounded `Vec<u8>`
buffers, then batch-submits after the scene walk. This wastes memory
proportional to visible content (3MB+ at Retina resolution with dense text).
Phase 10 requires mid-walk pipeline switches (solid → shadow → solid → glyph →
...), which is incompatible with the accumulate-then-submit model anyway.

Refactor to v0.6's streaming model: flush vertices to the GPU during the walk
whenever the buffer exceeds a threshold or the pipeline changes. The vertex
buffer becomes fixed-size and recycled. This bounds memory usage regardless of
content density and naturally supports multi-pipeline rendering.

This is prerequisite work for shadow and path rendering, not optional cleanup.

Scene walk changes:

- Before rendering a node's background, check `has_shadow()`
- If true: flush current solid vertices, switch to `PIPE_SHADOW`, emit shadow
  quad with 3σ padding via `emit_shadow_quad`, flush, switch back to
  `PIPE_SOLID`
- Shadow opacity: `shadow_color.a * node_opacity`

Metal setup: create `PIPE_SHADOW` render pipeline with `vertex_main` +
`fragment_shadow`, alpha blending enabled (premultiplied).

**Verification:**

- Set shadow fields on any scene node, verify soft shadow renders
- Compare shadow appearance against v0.6 baseline screenshot
- Test: blur_radius=0 produces hard shadow (solid offset quad)
- Test: spread > 0 expands shadow rect correctly
- Test: corner_radius > 0 produces rounded shadow

#### 10.2 — Stencil-cover path rendering

Add stencil buffer and path rendering pipeline to the compositor.

Metal setup:

- Create a stencil texture (same dimensions as render target, pixel format
  Stencil8)
- Attach as `stencilAttachment` on the render pass descriptor
- Create `PIPE_STENCIL` pipeline: vertex shader only (no fragment), stencil
  write enabled (increment on front-face, decrement on back-face)
- Create `PIPE_COVER` pipeline: vertex + fragment_solid, stencil test (pass when
  stencil ≠ 0), stencil op = zero (reset for next path)

Rendering a `Content::Path` node:

1. Parse path commands from scene data buffer
2. Flatten cubic Béziers to triangle fans (stencil fill) — use the scene
   library's `stroke::flatten_path` for stroke expansion
3. Stencil write pass: draw triangle fan with `PIPE_STENCIL`
4. Cover pass: draw bounding quad with `PIPE_COVER` (fragment reads vertex color
   = fill_color or stroke_color)
5. Clear stencil via stencil op (no separate clear)

Port the stencil-cover rendering from
`v0.6-pre-rewrite:services/drivers/metal-render/scene_walk.rs`
(`draw_path_stencil_cover`).

For stroke: use the scene library's `stroke::expand_stroke()` to convert stroke
to filled geometry, then render as fill.

**Verification:**

- Render a Tabler icon as a `Content::Path` node — visible vector icon
- Test both fill and stroke rendering
- Test multiple icons in the same frame (stencil resets correctly)
- Screenshot: icon appearance matches v0.6 (clean edges, correct shape)

#### 10.3 — Mouse pointer

The mouse pointer renders to dedicated GPU textures and is sent to the
hypervisor as a separate hardware cursor layer — no compositing with the main
scene.

**Protocol additions** (`protocol::metal`):

- `CMD_SET_CURSOR_FROM_TEXTURE` (0x0F13) — texture handle, width, height,
  hotspot_x, hotspot_y
- `CMD_SET_CURSOR_POSITION` (0x0F11) — x, y (framebuffer pixels)
- `CMD_SET_CURSOR_VISIBLE` (0x0F12) — visible flag

**CursorState shared memory** (40 bytes + path data):

- `shape_generation: u32` — atomic, bumped when cursor shape changes
- `opacity: u32` — 0=hidden, 255=visible
- `viewbox: f32` — icon viewbox size (24.0 for Tabler)
- `stroke_width: f32` — stroke width in viewbox units
- `hotspot_x: f32`, `hotspot_y: f32` — in viewbox units
- `fill_color: u32`, `stroke_color: u32` — packed RGBA
- `data_len: u32` — path command byte count
- `flags: u32` — bit 0: FLAG_STROKE_ONLY
- Path command bytes follow at offset 40

**Compositor cursor pipeline:**

1. Allocate 6 cursor textures: MSAA render target (4× RGBA16Float, 96×96), MSAA
   stencil (4×), resolve (1×, RGBA16Float), sRGB output (1×, BGRA8_sRGB), two
   blur ping-pong (1×, RGBA16Float)
2. On `shape_generation` change (check each frame): a. Render cursor shadow:
   draw path offset by (1.5, 1.5) viewbox units in shadow color (black @25%) via
   stencil-cover to MSAA target, resolve, copy to blur buffer b. Blur shadow:
   3-pass box blur via compute shaders (`blur_h`/`blur_v`), sigma=4.0px,
   ping-pong between blur buffers c. Composite: new render pass — draw blurred
   shadow as textured quad, then draw crisp cursor paths on top (fill + stroke
   colors) d. Resolve MSAA → resolve texture e. Dither to sRGB:
   `fragment_dither` with 4×4 Bayer matrix → sRGB texture f. Send
   `CMD_SET_CURSOR_FROM_TEXTURE` with sRGB texture handle and hotspot
3. On pointer position change: send `CMD_SET_CURSOR_POSITION`

**Default cursor:** Arrow shape (Tabler `arrow-pointer` or custom path data).

**Input driver changes:**

- Forward `EV_ABS` pointer position to presenter (already partially wired)
- Presenter writes position to `CursorState`, sends position update to
  compositor

Port cursor rendering from
`v0.6-pre-rewrite:services/drivers/metal-render/main.rs` (cursor texture setup,
rasterization pipeline, `CMD_SET_CURSOR_FROM_TEXTURE`).

**Verification:**

- Mouse pointer visible on screen, follows trackpad/mouse movement
- Cursor has soft drop shadow
- Cursor appears on a separate layer (verify via hypervisor: cursor doesn't
  composite into the main framebuffer capture)
- Test: hide cursor (opacity=0), verify `CMD_SET_CURSOR_VISIBLE(false)` sent
- Screenshot of main framebuffer should NOT contain the cursor

#### 10.4 — Title bar + clock + title icon

Add chrome nodes to the presenter's scene graph:

- `N_TITLE_BAR`: full-width rectangle, height = `title_bar_h`, background =
  `chrome_bg`, 1px bottom border = `chrome_border`
- `N_TITLE_ICON`: mimetype-aware Tabler icon to the left of title text.
  `Content::Path` with stroke rendering (from 10.2). Size = `line_height + 2`
  points, vertically centered. Icon selected by document content type
  (`icon_lib::get("document", mimetype)`)
- `N_TITLE_TEXT`: document name or "untitled". Inter font (sans-serif), shaped
  via `fonts::shape()`. Position: right of icon with 8pt gap. Color =
  `chrome_title_color`
- `N_CLOCK_TEXT`: current time from `clock_read`. Right-aligned at
  `fb_width - 12 - 80`. Inter font. Updated periodically (on each input event or
  timer tick — reuse the blink timer from Phase 7)

The presenter already uses the `fonts` library for document text shaping. Chrome
text (title, clock) uses the same shaping path with Inter as the font.

**Verification:**

- Title bar visible at top of screen with distinct background
- Document name displayed next to mimetype icon
- Clock shows current time, updates on keypress
- Screenshot: title bar layout matches v0.6 (icon + text + clock positions)

#### 10.5 — Page geometry + document shadow

The text content sits on a white page surface with a soft shadow, centered in
the viewport. This uses the generic shadow system from 10.1.

- `N_PAGE`: white background rectangle, A4 proportions, centered horizontally
  and vertically within the content area. Shadow fields:
  `shadow_blur_radius = 64`, `shadow_spread = 36`, `shadow_offset_x = 0`,
  `shadow_offset_y = 0`, `shadow_color = rgba(0, 0, 0, 255)`
- Content area: begins at `y = title_bar_h`, clips children
  (`NodeFlags::CLIPS_CHILDREN`). Width = framebuffer width, height =
  `fb_height - title_bar_h`
- Text inset: `text_inset_x` padding on left/right within the page
- Page dimensions: derive from framebuffer size with A4 aspect ratio, or use
  v0.6's page sizing logic

Port page geometry from `v0.6-pre-rewrite:services/presenter/scene/document.rs`
(N_PAGE setup, margin calculations, page centering).

**Verification:**

- White page visible with soft shadow behind it
- Text renders within page margins, not edge-to-edge
- Page centered in viewport
- Shadow matches v0.6: soft Gaussian blur, visible on all sides
- Screenshot comparison: page + shadow appearance matches v0.6 baseline

### Phase 10 done when

- Analytical Gaussian shadow renders on any node with shadow fields set
- Vector icons render via stencil-cover path pipeline
- Mouse pointer visible on separate hardware cursor layer with drop shadow
- Title bar with document icon, name, and clock
- White document page with soft shadow, centered, with proper margins
- All existing tests still pass
- Screenshot comparison matches v0.6 chrome appearance

---

## Phase 11 — Content-Type Typography

### What v0.6 had

- `services/presenter/typography.rs` — `TypographyConfig` per content type: font
  family (Mono/Sans/Serif), OpenType features, weight, tracking
- `services/presenter/fallback.rs` — `FallbackChain`: ordered list of fonts,
  tries each until a valid glyph is found (handles missing glyphs in primary
  font)
- Content types: Code (JetBrains Mono), Prose (Source Serif 4), UI (Inter)
- Three font files embedded: Inter, JetBrains Mono, Source Serif 4 (regular +
  italic for each = 6 files)

### Implementation

#### 11.1 — Font embedding

Embed all 6 font files via `include_bytes!`:

- `inter.ttf`, `inter-italic.ttf`
- `jetbrains-mono.ttf`, `jetbrains-mono-italic.ttf`
- `source-serif-4.ttf`, `source-serif-4-italic.ttf`

These are embedded in both the layout service (for shaping/metrics) and the
compositor (for rasterization). In v0.6 they were in `assets/`.

Check if the font files still exist in the repo or need to be restored from the
v0.6 tag.

#### 11.2 — Typography config

Port `TypographyConfig` from `v0.6-pre-rewrite:services/presenter/typography.rs`
into the presenter. Maps content type → font selection + OpenType features.

The content type comes from the document's mimetype (stored in the document
service or derived from file extension). For v0.6 parity, support `text/plain`
(monospace) as the default.

#### 11.3 — Font fallback chain

Port `FallbackChain` from `v0.6-pre-rewrite:services/presenter/fallback.rs`:

- Try primary font first
- On `.notdef` glyph (glyph_id 0), try next font in chain
- Return `FallbackGlyph` with font_index for atlas keying

Integrate into layout service shaping and compositor atlas lookup.

#### 11.4 — Style registry

Port the style registry from v0.6 — a shared data structure that maps style IDs
to font metrics (ascent, descent, weight, caret skew). The layout service writes
it; the presenter and compositor read it for per-run metrics.

**Verification:**

- Default content type renders with JetBrains Mono (monospace)
- Characters missing from JetBrains Mono fall back to Inter
- Font metrics (ascent, descent, line height) match v0.6 values
- Screenshot: text appearance matches v0.6 monospace rendering

### Phase 11 done when

- Three font families embedded and selectable by content type
- Font fallback chain handles missing glyphs
- Typography defaults match v0.6 for text/plain content

---

## Phase 12 — PNG Decoder

### What v0.6 had

- `services/decoders/png/` — 1,350 lines of pure Rust PNG decoder: chunk
  parsing, IHDR/PLTE/tRNS/IDAT, inflate (zlib decompression), filter
  reconstruction (None/Sub/Up/Average/Paeth), interlacing (Adam7), bit depth
  expansion, palette lookup, alpha handling
- `services/decoders/harness.rs` — decoder harness for IPC integration
- Decode request via sync IPC: receive compressed PNG bytes, return decoded RGBA
  pixel buffer in a VMO

### Implementation

#### 12.1 — PNG library

Port the PNG decoder from `v0.6-pre-rewrite:services/decoders/png/png.rs` into a
library crate `libraries/png/`. The decoder is pure computation — no kernel
dependencies. Adapt to the current crate style (module doc, co-located impl,
tests).

**Verification:** Host-target tests with the PNG test suite fixtures
(`host/fixtures/pngsuite/`). Decode reference PNGs, verify pixel-exact output
against known values (or at minimum: correct dimensions, non-zero pixel data, no
panics).

#### 12.2 — PNG decoder service

Create `userspace/servers/png-decoder/`:

- Registers as "png-decoder" with name service
- IPC protocol: `DECODE` request with PNG data VMO handle → reply with decoded
  RGBA VMO handle (width, height in reply payload)
- Uses the `libraries/png/` crate for actual decoding

Add to the service pack and boot sequence.

**Verification:** Integration test: create a VMO with a small PNG (solid color),
send decode request, verify reply dimensions and pixel values.

### Phase 12 done when

- PNG decoder service runs as a separate process
- Can decode standard PNG files (8-bit RGB, RGBA, palette, grayscale)
- Host-target test suite passes with PNGSuite fixtures

---

## Phase 13 — Filesystem + virtio-9p

### What v0.6 had

- `services/drivers/virtio-9p/` (598 lines) — 9P2000 protocol over virtio.
  Mounts a host-shared directory. Provides file read/write/stat operations.
- `services/filesystem/` (469 lines) — VFS layer over store + 9p. Routes file
  operations to the appropriate backend. Provides a unified file interface to
  the document service.

### Implementation

#### 13.1 — virtio-9p driver

Port the 9P driver from `v0.6-pre-rewrite:services/drivers/virtio-9p/main.rs`.
Adapt to the current driver patterns:

- Probe MMIO for virtio 9P device (device ID 9)
- Negotiate features
- Setup virtqueue + DMA buffers
- Implement 9P2000.L message set: version, attach, walk, open, read, write,
  clunk, stat
- Register as "9p" with name service
- IPC serve loop: translate IPC requests to 9P messages over virtqueue

The hypervisor needs a `--share <dir>` flag to expose a host directory as a
virtio-9p device. Check if this already exists.

**Verification:**

- Driver probes and discovers 9P device
- Read a known file from shared host directory
- Contents match host-side file

#### 13.2 — Filesystem service

Port `v0.6-pre-rewrite:services/filesystem/main.rs`. This is a VFS multiplexer:

- Routes to store service for the document database (COW filesystem on block
  device)
- Routes to 9p driver for host-shared files
- Provides a unified `OPEN`, `READ`, `WRITE`, `STAT`, `LIST` interface
- Register as "fs" with name service

**Verification:**

- Open a file from the COW store → returns content
- Open a file from the 9p share → returns host content
- Document service can load a file from the filesystem on boot

#### 13.3 — Document loading on boot

Wire the document service to the filesystem service:

- On boot: optionally load a file (path specified via bootstrap config or
  command-line argument to hypervisor)
- If no file specified: start with empty document (current behavior)
- File content loaded into the piece table, rendered via the full pipeline

**Verification:**

- Boot with `--file <path>`: document content appears on screen
- Edit the loaded content: edits work normally
- No file specified: empty document as before

### Phase 13 done when

- Host files accessible via virtio-9p
- Document service can load files on boot
- Read/write cycle works through the VFS layer

---

## Completion Criteria

All phases complete when:

1. `cargo r` boots to a text editor that visually matches v0.6-pre-rewrite
2. Actual glyph rendering (anti-aliased, proportional spacing)
3. Cursor blinks with eased animation
4. Text selection with Shift+arrows, visual highlight
5. Full keyboard navigation (arrows, word/line, Home/End, Page Up/Down)
6. Scrolling for long documents
7. Title bar with clock, document margins
8. PNG decoding works
9. Host files loadable via 9p
10. All existing tests pass (557 kernel + 1,045 library)
11. Screenshot tests verify visual output

## Session Resume Protocol

At the start of each session:

1. Read this file for the plan
2. Read `STATUS.md` for current state
3. Run `git log --oneline -20` to see recent work
4. Identify which phase is current (check verification criteria of preceding
   phases — if they pass, that phase is done)
5. Continue the current phase from where it left off
6. When a phase is complete: update `STATUS.md`, commit, start next phase
7. When all phases are complete: update `STATUS.md` with "v0.6 parity achieved",
   notify the user

## Cross-Session State

After completing each phase, append a status line to `STATUS.md` under a new
"### v0.6 Parity Progress" section:

```md
### v0.6 Parity Progress

- Phase 6: [NOT STARTED | IN PROGRESS step N | COMPLETE]
- Phase 7: ...
- ...
- Phase 13: ...
```

This is the primary state signal for session resumption.
