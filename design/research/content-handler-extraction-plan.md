# Content Service Architecture — Implementation Plan (IMPLEMENTED)

**Status:** Phases 1–5 and 7 implemented. Phase 6 (directory restructuring)
deferred to a dedicated session.

From the current monolithic presenter to the content service architecture
described in `content-service-architecture.md`. The view tree is the SSOT.
Content viewers produce it. Rendering pipelines consume it. The orchestrator
manages it.

---

## Viewer Trait Design

The `Viewer` trait unifies **output** (ViewSubtree), not input. Viewers vary in
what they need to produce a view tree — images need decoded pixels, text needs
layout results and a document buffer, video needs IPC handles and frame
mappings. Since the presenter uses enum dispatch (`ViewerKind`), each variant
can expose its own update signature. The trait guarantees a common shape for
what the presenter reads back.

```rust
trait Viewer {
    /// The viewer's current visual output — cached between frames.
    fn subtree(&self) -> &ViewSubtree;

    /// Route an input event. Returns a command for the presenter to execute
    /// (IPC calls, cursor moves, playback toggles) or Unhandled.
    fn event(&mut self, event: &InputEvent) -> EventResponse;

    /// Release resources (IPC handles, VMO mappings).
    fn teardown(&mut self);
}
```

Each concrete viewer has its own rebuild method with viewer-specific parameters:

```rust
impl ImageViewer {
    fn rebuild(&mut self, constraints: &Constraints);
}

impl TextViewer {
    /// `doc_buf`: raw document bytes (may be piecetable format).
    /// `results_buf`: layout service results (line positions, byte offsets).
    /// `constraints`: available dimensions.
    fn rebuild(
        &mut self,
        doc_buf: &[u8],
        results_buf: &[u8],
        constraints: &Constraints,
    );
}

impl VideoViewer {
    fn rebuild(&mut self, constraints: &Constraints);
}
```

The presenter matches on `ViewerKind` to call the right `rebuild` with the right
arguments. Then it reads `viewer.subtree()` uniformly to feed `write_subtree`.
Rebuild is only called when content changes, viewport resizes, or the viewer
requests it — not on every frame.

**Why not `dyn Viewer`:** no_std bare metal, no heap allocation for trait
objects, and enum dispatch is zero-cost. The presenter knows every viewer type
at compile time (4 variants). The trait exists as a shape guarantee and
documentation of the contract, not as a dynamic dispatch mechanism.

**Caching invariant:** viewers own their `ViewSubtree` and cache it between
frames. The presenter calls `rebuild` only on content change, resize, or
explicit invalidation. On animation ticks (cursor blink, clock update), the
presenter updates only the affected scene nodes directly — it does not call
`rebuild`. This is more efficient than the current code, which re-shapes every
visible text line on every `build_scene` call.

---

## ViewSubtree and Layout

`ViewSubtree` includes pre-computed layout positions alongside the view tree:

```rust
struct ViewSubtree {
    tree: ViewTree,
    root: NodeId,
    layout: Vec<LayoutBox>,  // indexed by NodeId, from view_tree::layout()
}
```

Viewers compute layout during `rebuild` and store the results. `write_subtree`
reads positions from `subtree.layout[node_id]` — it never calls the layout
engine itself. This makes `write_subtree` a pure mapper from ViewNode +
LayoutBox → scene Node.

**How viewers compute layout:**

The view-tree layout engine already handles all needed positioning modes:

- **FixedCanvas** (`layout_fixed`): children positioned at their `offset_x`,
  `offset_y`. Used by TextViewer (line nodes at layout-service-provided
  positions), WorkspaceViewer (chrome elements at absolute positions).
- **Block** (`layout_block`): children stacked vertically with margin
  collapsing. `Position::Absolute` children placed at their offsets. Not used by
  current viewers, but available for future compound documents.
- **Fixed intrinsics** (`IntrinsicSize::Fixed`): node resolves to its declared
  dimensions. Used by ImageViewer, VideoViewer, glyph line nodes.

Each viewer calls `view_tree::layout()` during rebuild:

```rust
// ImageViewer — single node, Fixed intrinsics
fn rebuild(&mut self, constraints: &Constraints) {
    let (disp_w, disp_h) = aspect_fit(
        self.pixel_width, self.pixel_height,
        constraints.available_width, constraints.available_height,
    );

    // ... update root node intrinsic to (disp_w, disp_h) ...
    self.subtree.layout = view_tree::layout(
        self.subtree.tree.nodes(), self.subtree.root,
        constraints.available_width, constraints.available_height,
        &NoMeasurer,
    );
}

// TextViewer — FixedCanvas root, children at layout-service positions
fn rebuild(&mut self, doc_buf: &[u8], results_buf: &[u8], constraints: &Constraints) {
    // ... build ViewTree: root = FixedCanvas, children = glyph line nodes
    //     with offset_x/offset_y from layout service results ...
    self.subtree.layout = view_tree::layout(
        self.subtree.tree.nodes(), self.subtree.root,
        constraints.available_width, constraints.available_height,
        &NoMeasurer,
    );
}
```

**Aspect-fit moves into the viewer.** Currently `build_scene` computes
aspect-fit and centering for images and video. In the new model, the viewer
receives constraints and computes display dimensions during rebuild.

**Centering is the workspace viewer's responsibility.** The workspace viewer
knows the strip dimensions and each child viewer's reported size (from the
child's LayoutBox). It positions each child's portal node centered within its
strip slot:

```rust
// Inside WorkspaceViewer::rebuild — positioning a portal in the strip
let child_subtree = child.viewer.subtree();
let child_root_box = &child_subtree.layout[child_subtree.root as usize];
let center_x = slot_x + (slot_w as i32 - child_root_box.width as i32) / 2;
let center_y = (content_h as i32 - child_root_box.height as i32) / 2;
// Portal node at (center_x, center_y) — no copying
let portal = tree.add(ViewNode {
    offset_x: center_x,
    offset_y: center_y,
    width: Dimension::Points(child_root_box.width),
    height: Dimension::Points(child_root_box.height),
    content: ViewContent::Portal { child_idx: i as u16 },
    ..Default::default()
});
```

**Portal-based composition.** The workspace viewer's ViewTree contains portal
nodes — one per child position in the strip. A portal references a child
viewer's subtree by index. `write_subtree` follows portals: when it encounters
`ViewContent::Portal { child_idx }`, it recurses into `children[child_idx]` at
the portal node's position. Each viewer owns exactly one representation of its
content. No copying, no sync, no dirty tracking between parent and child.

```text
WorkspaceViewer subtree:
  root (FixedCanvas, full display)
    ├── title_bar (chrome)
    ├── clock (chrome)
    ├── icon (chrome)
    ├── strip (FixedCanvas, child_offset_x = -slide_spring)
    │   ├── Portal { child_idx: 0 }  ← centered, points to image viewer
    │   ├── Portal { child_idx: 1 }  ← centered, points to text viewer
    │   └── Portal { child_idx: 2 }  ← centered, points to video viewer
```

```rust
enum ViewContent {
    None,
    Glyphs { ... },
    Image { ... },
    Path { ... },
    Gradient { ... },
    GradientPath { ... },
    Portal { child_idx: u16 },
}
```

**`write_subtree` signature:**

```rust
fn write_subtree(
    subtree: &ViewSubtree,
    children: &[&ViewSubtree],  // portal targets, indexed by child_idx
    scene: &mut SceneWriter,
    parent: scene::NodeId,
    offset_x: scene::Mpt,
    offset_y: scene::Mpt,
)
```

Writes the root node at `(offset_x + layout.x, offset_y + layout.y)` with
LayoutBox dimensions. Walks children depth-first, writing each at its LayoutBox
position relative to the parent's content box (padding inset). Handles
`child_offset_x/y` from ViewNode (scroll offsets, slide transforms) by adding
them to children's scene positions.

When `write_subtree` encounters `ViewContent::Portal { child_idx }`, it recurses
into `children[child_idx]` at the portal node's position and dimensions. The
child subtree is written as if it were an inline subtree at that location. Leaf
viewers (image, text, video) pass `&[]` for children — they have no portals.

**Why portals, not copying:** copying child nodes into the workspace tree
creates two representations of the same content with a sync obligation between
them. When a child rebuilds (text edit, video frame), the workspace must
re-copy. Portals eliminate this: each viewer owns its subtree, and
`write_subtree` follows references. The cost is one match arm in the walk loop —
cheaper than any copy.

---

## TextViewer Dependencies

TextViewer is the largest and most complex viewer. Its dependencies must be
explicit because they determine the rebuild method signature and what state
moves out of the presenter.

**Owned state** (moves into TextViewer at creation, persists across frames):

| Field               | Type                 | Purpose                                   |
| ------------------- | -------------------- | ----------------------------------------- |
| `scroll_y`          | `i32`                | Vertical scroll position                  |
| `sticky_col`        | `Option<u32>`        | Proportional x for vertical navigation    |
| `blink_start`       | `u64`                | Cursor blink epoch                        |
| `cmap_mono`         | `[u16; 128]`         | ASCII → glyph ID for monospace font       |
| `cmap_sans`         | `[u16; 128]`         | ASCII → glyph ID for sans font            |
| `char_width_mpt`    | `Mpt`                | Monospace character advance (millipoints) |
| `glyphs`            | `[ShapedGlyph; MAX]` | Scratch buffer for glyph shaping          |
| `click_count`       | `u8`                 | Double/triple-click state                 |
| `dragging`          | `bool`               | Active drag selection                     |
| `drag_origin_start` | `usize`              | Drag anchor start (byte offset)           |
| `drag_origin_end`   | `usize`              | Drag anchor end (byte offset)             |

**Borrowed on rebuild** (passed by the presenter, not owned):

| Parameter     | Type           | Source                                           |
| ------------- | -------------- | ------------------------------------------------ |
| `doc_buf`     | `&[u8]`        | Slice from `doc_va` (memory-mapped document VMO) |
| `results_buf` | `&[u8]`        | Cached layout results (seqlock snapshot)         |
| `constraints` | `&Constraints` | Available width/height from page geometry        |

**Borrowed on event** (passed by the presenter for IPC-dependent operations):

| Parameter     | Type    | Source                         |
| ------------- | ------- | ------------------------------ |
| `doc_buf`     | `&[u8]` | Slice from `doc_va`            |
| `results_buf` | `&[u8]` | Layout results for hit-testing |
| `content_len` | `usize` | From document header           |
| `cursor_pos`  | `usize` | From document header           |
| `sel_anchor`  | `usize` | From document header           |

**Returned to presenter** (via EventResponse / ContentCommand):

| Command                  | Presenter action                          |
| ------------------------ | ----------------------------------------- |
| `MoveCursor(pos)`        | IPC: `doc_cursor_move(pos)`               |
| `Select(anchor, cursor)` | IPC: `doc_select(anchor, cursor)`         |
| `ForwardKey(dispatch)`   | IPC: forward to `editor_ep`               |
| `TogglePlayback`         | (video only)                              |
| `Seek(position)`         | (video only)                              |
| `ScrollTo(y)`            | Presenter updates viewport state register |

The presenter reads `doc_va` and `results_buf` once per frame, then passes
slices to whichever viewer needs them. The TextViewer never touches IPC handles
or the layout endpoint — it computes navigation targets and returns commands;
the presenter executes them.

---

## Space Elimination and Workspace Viewer Ownership

The desktop is a compound document (`application/x-os-workspace`). Its manifest
defines what documents are open and how they're arranged. The `WorkspaceViewer`
is the content handler for this compound document — it owns child viewers, one
per open document.

There is no separate `Document` struct on the presenter. The presenter holds one
`WorkspaceViewer`. The workspace viewer holds child viewers. This matches the
compound document model from `content-service-architecture.md`: compound
handlers manage child handlers; the presenter is the orchestrator.

The `Space` enum is eliminated over Phases 1–3:

**Phase 1d** — Add `viewer: Option<ViewerKind>` to each Space variant. Viewers
are created during document loading. Space data remains as the authoritative
state. This is a transitional scaffold — the viewer reads from Space fields.

**Phase 2** — Viewers take over all rendering. `build_scene` calls
`viewer.subtree()` instead of building scene nodes from Space fields. Space data
is still read during viewer rebuild (image dimensions, video handle).

**Phase 3** — WorkspaceViewer takes ownership of child viewers. State that
currently lives on the presenter moves into the workspace viewer:

| Field                | Currently on | Moves to                                          |
| -------------------- | ------------ | ------------------------------------------------- |
| `spaces: Vec<Space>` | Presenter    | WorkspaceViewer (as `children: Vec<ChildViewer>`) |
| `active_space`       | Presenter    | WorkspaceViewer                                   |
| `slide_spring`       | Presenter    | WorkspaceViewer                                   |
| `slide_animating`    | Presenter    | WorkspaceViewer                                   |
| `last_anim_tick`     | Presenter    | WorkspaceViewer                                   |

Space-variant fields (`content_id`, `decoder_ep`, `frame_vmo`, etc.) move into
the corresponding viewer structs. The `Space` enum and its `Drop` impl are
deleted. Resource cleanup moves to `Viewer::teardown()`.

```rust
struct ChildViewer {
    viewer: ViewerKind,
    mimetype: &'static [u8],
    viewer_override: Option<ViewerKindTag>,  // user-switchable (Phase 5)
}

struct WorkspaceViewer {
    children: Vec<ChildViewer>,
    active: usize,
    slide_spring: SpringI32,
    slide_animating: bool,
    last_anim_tick: u64,
    // Chrome state: clock, title, icon
    subtree: ViewSubtree,  // cached: chrome + child subtrees composed
}
```

**Subtree composition:** the workspace viewer's `ViewSubtree` contains chrome
nodes and portal nodes. It does NOT copy child ViewTree nodes into its own tree.
See "ViewSubtree and Layout" section for the portal-based composition model.
Each child viewer owns its own subtree; the workspace viewer references them via
`ViewContent::Portal { child_idx }`. `write_subtree` follows portals during the
scene-writing walk.

**Phase 4** — Events route through the workspace viewer. The presenter calls
`workspace.event(...)`. The workspace viewer hit-tests its composed subtree to
determine which child viewer owns the target, routes the event to that child,
and returns the resulting `ContentCommand` to the presenter. Workspace-level
events (Ctrl+Tab, Ctrl+W) are handled by the workspace viewer directly.

---

## Target Codebase

Every directory. Every file. Unchanged files omitted — only structural changes
and new files shown. Files marked (new) don't exist yet. Files marked (changed)
exist and need modification. Everything else is unchanged.

```text
os/
├── Cargo.toml                              (changed — workspace members)
├── CLAUDE.md                               (changed — update architecture refs)
├── STATUS.md                               (changed — update current state)
├── design/
│   ├── architecture.md                     (changed — update pipeline desc)
│   ├── glossary.md                         (changed — view tree, viewer terms)
│   └── research/
│       ├── content-service-architecture.md (changed — reflect final naming)
│       └── ...
├── kernel/                                 (unchanged — complete, frozen ABI)
├── tools/                                  (unchanged)
├── test/                                   (unchanged)
└── user/
    ├── system/                             (renamed from top-level abi/ipc/heap/virtio)
    │   ├── abi/                            kernel interface, syscall wrappers
    │   ├── ipc/                            IPC primitives, messages, rings
    │   ├── heap/                           bare-metal allocator (GlobalAlloc)
    │   └── virtio/                         virtio MMIO transport, split virtqueue
    ├── drivers/                            (moved from servers/drivers/)
    │   ├── blk/                            virtio-blk block device
    │   ├── input/                          virtio-input keyboard + tablet
    │   ├── render/                         (changed — render lib merged in)
    │   │   ├── Cargo.toml                  compositor + rendering algorithms
    │   │   └── src/
    │   │       ├── main.rs                 Metal compositor, scene walk
    │   │       ├── lib.rs                  protocol (comp::SETUP, etc.)
    │   │       ├── atlas.rs                glyph atlas
    │   │       ├── path.rs                 CPU path rasterizer
    │   │       ├── cache.rs                (from render lib)
    │   │       ├── damage.rs               (from render lib)
    │   │       ├── walk.rs                 (from render lib)
    │   │       └── ...
    │   ├── snd/                            virtio-snd audio output
    │   ├── video/                          virtio-video-decode (codec driver)
    │   ├── 9p/                             virtio-9p host filesystem
    │   └── rng/                            virtio-rng entropy
    ├── codecs/                             (new grouping — moved from libraries/)
    │   ├── png/                            PNG decoder (deflate, filters, interlace)
    │   ├── jpeg/                           JPEG decoder (DCT, Huffman)
    │   ├── mp4/                            MP4 container parser (ISO BMFF)
    │   ├── avi/                            AVI container parser (RIFF)
    │   └── wav/                            WAV audio parser (RIFF)
    ├── viewers/                            (new — per-mimetype content viewers)
    │   ├── image/                          (new)
    │   │   ├── Cargo.toml                  deps: view, scene, codecs/png, codecs/jpeg
    │   │   └── src/
    │   │       └── lib.rs                  ImageViewer — image/jpeg, image/png
    │   │                                   Calls codec libraries directly (no
    │   │                                   decoder services). Produces single-
    │   │                                   node ViewSubtree with intrinsic dims.
    │   ├── text/                           (new)
    │   │   ├── Cargo.toml                  deps: view, scene, fonts, piecetable,
    │   │   │                               line-break
    │   │   └── src/
    │   │       ├── lib.rs                  TextViewer — text/plain, text/rich
    │   │       ├── shaping.rs              glyph shaping, cmap, font fallback
    │   │       ├── selection.rs            selection geometry (rich + plain)
    │   │       ├── cursor.rs               cursor positioning, blink
    │   │       └── navigation.rs           word/line nav, click-to-place,
    │   │                                   drag selection, multi-click
    │   ├── video/                          (new)
    │   │   ├── Cargo.toml                  deps: view, scene, icons, codecs/mp4,
    │   │   │                               codecs/avi
    │   │   └── src/
    │   │       └── lib.rs                  VideoViewer — video/mp4, video/avi
    │   │                                   Container parsing moved in from
    │   │                                   video-decoder service. Hardware
    │   │                                   decode via codec driver IPC.
    │   └── workspace/                      (new)
    │       ├── Cargo.toml                  deps: view, scene, fonts, icons
    │       └── src/
    │           └── lib.rs                  WorkspaceViewer —
    │                                       application/x-os-workspace
    │                                       Title bar, clock, document icon,
    │                                       space strip, slide animation.
    ├── editors/                            (moved from top-level)
    │   └── text/                           text editor (unchanged internally)
    └── shared/                             libraries + services — the common substrate
        ├── view/                           (renamed from view-tree — the SSOT)
        │   ├── Cargo.toml
        │   └── src/
        │       ├── lib.rs                  re-exports
        │       ├── node.rs                 ViewNode — all properties
        │       ├── tree.rs                 ViewTree container, traversal
        │       ├── content.rs              ViewContent enum
        │       ├── layout.rs               layout engine (block, fixed, flow)
        │       ├── viewer.rs               Viewer trait, ViewSubtree,
        │       │                           Constraints, EventResponse,
        │       │                           ContentCommand, InputEvent
        │       └── tests.rs
        ├── scene/                          GPU pipeline wire format (unchanged)
        ├── fonts/                          font parsing, shaping, rasterization
        ├── drawing/                        2D rasterization primitives
        ├── animation/                      spring physics, easing curves
        ├── line-break/                     (renamed from layout — text line-breaking)
        ├── piecetable/                     piece table data structure
        ├── icons/                          vector icon path data
        ├── store/                          (changed — fs library folded in)
        │   ├── Cargo.toml
        │   └── src/
        │       ├── lib.rs                  document store API
        │       ├── serialize.rs            catalog format, metadata
        │       ├── filesystem.rs           (from fs lib — COW filesystem)
        │       ├── block.rs                (from fs lib — block allocation)
        │       ├── inode.rs                (from fs lib — inode management)
        │       ├── snapshot.rs             (from fs lib — COW snapshots)
        │       └── ...
        ├── presenter/                      (changed — pure orchestrator)
        │   ├── Cargo.toml                  deps: view, scene, all viewers, ...
        │   └── src/
        │       ├── lib.rs                  protocol constants
        │       ├── main.rs                 boot, serve loop, document open/close,
        │       │                           viewer lifecycle, viewer registry,
        │       │                           event routing
        │       └── renderer.rs             (new — GPU renderer: composed
        │                                   ViewTree → scene graph VMO)
        ├── document/                       document manager (edit authority,
        │                                   single-writer, COW undo, shared VMO)
        ├── layout/                         (renamed from layout-service)
        ├── store-service/                  COW filesystem over block driver
        ├── audio/                          mixer service → snd driver
        ├── console/                        PL011 UART debug output
        ├── name/                           service discovery registry
        ├── init/                           process spawner, DMA allocator
        ├── host-fs/                        (renamed from fs-service — 9p bridge)
        ├── benchmarks/                     (merged from bench/ + bench-smp/)
        └── integration-tests/              multi-service test binaries
```

### What's gone

- **presenter/src/build.rs** — rendering logic moved to viewers. Scene writing
  moved to `renderer.rs`. Orchestration moved to `main.rs`.
- **presenter/src/input.rs** — text navigation moved to `viewers/text/`.
  Workspace-level dispatch (Ctrl+Tab, Ctrl+W) moved to WorkspaceViewer.
- **presenter/src/pointer.rs** — text click/selection/drag moved to
  `viewers/text/`. Video toggle moved to `viewers/video/`.
- **presenter/src/handlers.rs** — temporary file from initial extraction.
  Replaced by `viewers/` crates.
- **Space enum** — child viewers owned by WorkspaceViewer at end of Phase 3.
  Space-variant fields move into viewer structs.
- **Space::Showcase** — deleted (Phase 3).
- **libraries/fs/** — folded into `shared/store/` as internal modules.
- **libraries/render/** — merged into `drivers/render/`.
- **servers/jpeg-decoder/** — deleted. Image viewer calls codecs directly.
- **servers/png-decoder/** — deleted. Image viewer calls codecs directly.
- **servers/video-decoder/** — deleted. Video viewer does container parsing and
  talks to codec driver directly.
- **servers/hello/** — deleted (historical test artifact).
- **user/bench/, user/bench-smp/** — merged into `shared/benchmarks/`.

### What the presenter looks like

```rust
// main.rs — ~300 lines (down from ~1500)

// Enum dispatch — zero-cost, no vtable, no heap.
// Each variant owns its state and implements Viewer.
enum ViewerKind {
    Image(ImageViewer),
    Text(TextViewer),
    Video(VideoViewer),
}

struct Presenter {
    // The desktop compound document — owns all child viewers.
    workspace: WorkspaceViewer,

    // Shared infrastructure (IPC endpoints)
    doc_ep: Handle,
    layout_ep: Handle,
    render_ep: Handle,
    editor_ep: Handle,
    audio_ep: Handle,
    console_ep: Handle,

    // Shared read state (presenter reads once, passes to workspace)
    doc_va: usize,
    results_reader: ipc::register::Reader,
    results_buf: [u8; layout_service::RESULTS_VALUE_SIZE],

    // Display
    display_width: u32,
    display_height: u32,
    scene_bufs: [&'static mut [u8]; 2],
    swap_va: usize,
    swap_gen: u32,

    // Pointer (shared, not content-specific)
    pointer_x: i32,
    pointer_y: i32,
    cursor_shape: u8,

    // Viewer registry
    registry: ViewerRegistry,
}
```

The presenter's `build_scene` becomes:

```rust
fn build_scene(&mut self) {
    // Read shared state once
    self.results_reader.read(&mut self.results_buf);

    let doc_buf = /* slice from doc_va */;
    // Rebuild workspace (which rebuilds dirty children)
    let ctx = RebuildContext {
        doc_buf, results_buf: &self.results_buf,
        display_width: self.display_width,
        display_height: self.display_height,
        now_ns: abi::system::clock_read().unwrap_or(0),
    };

    self.workspace.rebuild(&ctx);

    // Write to scene — workspace subtree has portals to child subtrees
    let subtree = self.workspace.subtree();
    let child_subtrees: Vec<&ViewSubtree> = self.workspace.child_subtrees();

    write_subtree(subtree, &child_subtrees, &mut scene, root, pt(0), pt(0));

    self.swap_scene();
}
```

---

## Naming Conventions

**Plural directories** are containers holding multiple crates: `drivers/`,
`codecs/`, `viewers/`, `editors/`

**Crate directories** use the natural name for the crate: `fonts/`, `icons/`
(plural nouns), `animation/`, `drawing/` (singular/gerund), `audio/`, `store/`,
`document/` (singular)

**Trait:** `Viewer` (not ContentHandler). Implementations: `ImageViewer`,
`TextViewer`, `VideoViewer`, `WorkspaceViewer`.

**Viewer ≠ media type.** Viewers group by rendering model. `image/jpeg` and
`image/png` share a viewer (both decode to pixels). `image/svg+xml` gets its own
viewer (vector paths). `text/html` gets its own viewer (DOM + CSS). The mimetype
is the dispatch key; the viewer boundary is where rendering logic diverges.

**Viewer registry:** maps mimetype → prioritized list of viewers. Multiple
viewers can register for the same mimetype. Specificity rules (`text/markdown` >
`text/*` > `*/*`), user preference breaks ties.

---

## Phases

### Phase 1: Infrastructure

**1a.** Rename `ContentHandler` → `Viewer`. Redesign trait to unify output (not
input) — `subtree()`, `event()`, `teardown()`. Remove `load`/`resize`/`update`
from trait; each viewer type gets its own `rebuild` method with appropriate
parameters. Update view-tree crate. Update `ViewSubtree` to include
`layout: Vec<LayoutBox>`. Add `ViewContent::Portal { child_idx: u16 }` variant
for compound document composition.

Update `ImageHandler` in handlers.rs to implement the new trait shape:
`rebuild(&mut self, constraints)` replaces `load`/`resize`/`update`, and
`subtree()` returns the cached `ViewSubtree`. The one callsite in build.rs (line
~1569) temporarily calls `rebuild` + `subtree()` instead of creating a throwaway
handler. Text and video rendering remain as inline build.rs code until Phase 2.

**1b.** `write_subtree` — recursive ViewTree → scene graph writer (see
"ViewSubtree and Layout" section for full specification). Lives in `handlers.rs`
(moves to `renderer.rs` in Phase 6). Calls `write_view_node` per node, reads
positions from pre-computed `LayoutBox` results, handles `child_offset_x/y`.
Follows `ViewContent::Portal` nodes by recursing into the referenced child
subtree.

**1c.** `Constraints.now_ns` — timestamp for animated content.

**1d.** Add `viewer: Option<ViewerKind>` to Space variants. Viewers are created
during document loading and persist across frames. Space data remains
authoritative — the viewer reads from it. This is a transitional scaffold, not a
parallel implementation.

### Phase 2: Viewer Extraction — Rendering

Extract content-type-specific rendering from build.rs into viewer structs.
Viewers live in `presenter/src/handlers.rs` initially (moved to `viewers/`
crates in Phase 6). After Phase 2, all rendering flows through
`viewer.subtree()` + `write_subtree`.

**2a.** ImageViewer — already exists as a stub. Make persistent (created once at
document load, cached ViewSubtree). Rebuild on resize only. Declares intrinsic
dimensions + shadow + content_id.

**2b.** VideoViewer — owns `content_id`, pixel dimensions, `playing` state,
`decoder_ep`, `frame_vmo`, `frame_va`. Produces video frame + play/pause button
subtree. Takes ownership of IPC handles from Space::Video (cleanup via
`teardown()`).

**2c.** TextViewer — largest and highest-risk extraction. Moves owned state out
of Presenter (see "TextViewer Dependencies" section above). Source locations:

| Source                              | Lines | What moves                                                                                                        |
| ----------------------------------- | ----- | ----------------------------------------------------------------------------------------------------------------- |
| build.rs text arm (lines 1250–1561) | ~310  | Plain text shaping, rich text nodes, selection geometry, cursor                                                   |
| build.rs rich-text helpers          | ~250  | `build_rich_text_nodes`, `build_rich_selection_nodes`, `compute_rich_cursor`, `byte_to_x_rich`, `xy_to_byte_rich` |
| build.rs shared helpers             | ~80   | `build_font_axes`, `aspect_fit`, `SelectionSpan`, `build_selection_nodes`                                         |
| main.rs text helpers                | ~150  | `page_rect`, `text_origin`, `text_area_dims`, `ensure_cursor_visible`, `clamp_scroll`                             |
| (Phase 4 — not yet)                 |       | input.rs and pointer.rs move in Phase 4                                                                           |

Total rendering extraction: **~790 lines** (rendering only, Phase 2).

**Rewrite, not just move.** The text rendering helpers (`build_rich_text_nodes`,
`build_rich_selection_nodes`, plain text line loop, etc.) currently write
directly to a `SceneWriter` — they call `scene.alloc_node()`,
`scene.push_shaped_glyphs()`, `scene.add_child()`. In the new model, they
produce `ViewNode` values in a `ViewTree` — they call
`tree.add(ViewNode { ... })`. The glyph shaping logic (font lookup, cmap, run
splitting, width calculation) moves unchanged. Only the output target changes:

| Old pattern (scene-writing)                 | New pattern (view-tree-writing)                        |
| ------------------------------------------- | ------------------------------------------------------ |
| `scene.alloc_node()` → `scene.node_mut(id)` | `tree.add(ViewNode { ... })`                           |
| `scene.push_shaped_glyphs(&glyphs)`         | `ViewContent::Glyphs { glyphs: glyphs.to_vec(), ... }` |
| `scene.add_child(parent, child)`            | set `parent.first_child` / `sibling.next_sibling`      |
| `Content::Glyphs { color, glyph_ref, ... }` | `ViewContent::Glyphs { color, glyphs, ... }`           |
| Explicit `n.x = pt(...)` / `n.y = pt(...)`  | `offset_x` / `offset_y` on ViewNode                    |

`ViewContent::Glyphs` already stores `Vec<ShapedGlyph>` (owned data).
`write_subtree` converts these to scene DataRefs via `scene.push_shaped_glyphs`
during the scene-writing walk — the same conversion that `write_view_node`
already does.

The text viewer's `rebuild` method:

1. Reads layout service results (line positions, byte offsets, run metadata)
2. For each visible line: shapes glyphs (unchanged logic), creates a ViewNode
   with `ViewContent::Glyphs`, positions via `offset_x`/`offset_y`
3. Creates selection highlight nodes (if selection active)
4. Creates cursor node (with blink animation)
5. Calls `view_tree::layout()` on the FixedCanvas root → stores LayoutBoxes

After Phase 2c, the text Space arm of `build_scene` calls through the viewer
instead of building scene nodes inline. This is transitional — after Phase 3,
the workspace viewer composes children internally and the per-Space arms are
gone.

### Phase 3: Workspace Viewer + Space Elimination

**3a.** Extract desktop chrome (title bar, clock, icon, strip, slide animation)
into WorkspaceViewer. Delete Space::Showcase. The workspace viewer produces a
single composed ViewSubtree: chrome nodes at the top, child viewer subtrees
positioned in the strip. `build_scene()` becomes ~30 lines: read shared state,
call `workspace.rebuild(ctx)`, call `write_subtree(workspace.subtree(), ...)`.

**3b.** WorkspaceViewer takes ownership of child viewers (see "Space Elimination
and Workspace Viewer Ownership" section). Move Space-variant fields into their
viewer structs:

- `Space::Image { content_id, width, height }` → already in `ImageViewer`
- `Space::Video { decoder_ep, frame_vmo, ... }` → already in `VideoViewer`
- `Space::Text` (stateless variant) → `TextViewer` holds all text state

Move presenter state into WorkspaceViewer: `active_space`, `slide_spring`,
`slide_animating`, `last_anim_tick`. Delete `Space` enum, its `Drop` impl, and
the `mimetype()` method. Resource cleanup moves to `Viewer::teardown()`.
`spaces: Vec<Space>` becomes `workspace.children: Vec<ChildViewer>`.

### Phase 4: Event Routing

**4a.** `ContentCommand` enum — viewers return commands instead of performing
IPC. Commands: `MoveCursor(pos)`, `Select(anchor, cursor)`,
`ForwardKey(KeyDispatch)`, `TogglePlayback`, `Seek(pos)`, `ScrollTo(y)`,
`SetCursorShape(u8)`, `Unhandled`. Presenter executes the IPC.

**4b.** Text event migration — move input.rs (381 lines) and text-specific
pointer.rs code (~160 lines) into TextViewer. TextViewer's `event()` receives
borrowed `doc_buf`, `results_buf`, cursor state, and returns `ContentCommand`
values. The presenter reads document header, passes state in, executes returned
commands.

Total event extraction: **~540 lines**.

Combined Phase 2c + 4b text extraction: **~1330 lines** total.

**4c.** Event routing through workspace viewer. The presenter calls
`workspace.event(input_event, &event_ctx)`. The workspace viewer:

1. Handles workspace-level events directly (Ctrl+Tab → switch active child,
   Ctrl+W → close child and teardown its viewer)
2. Hit-tests its composed view tree to determine which child viewer owns the
   pointer target
3. Routes the event to the target child viewer
4. Returns the child's `ContentCommand` up to the presenter

The presenter executes the returned command (IPC calls, cursor updates). The
presenter never directly addresses child viewers — all interaction goes through
the workspace viewer. This is the compound document event routing model from
`content-service-architecture.md`: events do not cascade through intermediate
handlers; the workspace viewer walks its own tree and delivers directly.

### Phase 5: Viewer Registry

The registry is the mechanism that makes viewers interchangeable and
user-switchable — the architectural promise from
`content-service-architecture.md`.

**Data structure:**

```rust
struct ViewerEntry {
    pattern: MimetypePattern,
    kind: ViewerKindTag,
    priority: u8,           // lower = higher priority within same specificity
}

enum MimetypePattern {
    Exact(&'static [u8]),   // b"image/jpeg" — matches one type
    Subtype(&'static [u8]), // b"text" — matches text/*
    Universal,              // matches */*
}

enum ViewerKindTag {
    Image,
    Text,
    Video,
    Workspace,
}

struct ViewerRegistry {
    entries: Vec<ViewerEntry>,  // sorted: exact > subtype > universal, then priority
}
```

Linear scan on lookup. With <32 entries, this outperforms any tree or hash
structure (cache-friendly, no pointer chasing, no hashing overhead). The entries
are sorted by specificity at construction time, so the first match is always the
highest-priority result.

**5a.** `ViewerRegistry` struct and `MimetypePattern` matching.
`lookup(mimetype)` returns the highest-priority `ViewerKindTag`.
`alternatives(mimetype)` returns all matching tags in priority order (for user
switching).

**5b.** Registration — each viewer declares supported types via a constant:

```rust
impl ImageViewer {
    const SUPPORTED_TYPES: &[MimetypePattern] = &[
        MimetypePattern::Exact(b"image/jpeg"),
        MimetypePattern::Exact(b"image/png"),
    ];
}

impl TextViewer {
    const SUPPORTED_TYPES: &[MimetypePattern] = &[
        MimetypePattern::Exact(b"text/rich"),
        MimetypePattern::Subtype(b"text"),  // fallback for any text/*
    ];
}
```

The registry is built at presenter boot by collecting all viewer declarations,
assigning priorities (exact-type entries at priority 0, subtype at priority 1,
universal at priority 2), and sorting.

**5c.** Document open by registry lookup — when the workspace viewer opens a new
child document, it queries the registry with the mimetype, gets the
`ViewerKindTag`, and constructs the appropriate viewer. Replaces the current
hardcoded match on catalog content type.

**5d.** User-switchable viewers — `ChildViewer` has an optional
`viewer_override: Option<ViewerKindTag>`. When set, the workspace viewer uses
this tag instead of the registry's default. Switching tears down the old viewer
and creates a new one from the registry's alternative list.

User override is runtime-only (not persisted across document close). Persisting
viewer preferences requires document metadata in the store — future work beyond
this plan.

### Phase 6: Structural Reorganization

The big directory move. Do as one atomic commit.

- `abi/`, `ipc/`, `heap/`, `virtio/` → `system/`
- `servers/drivers/` → `drivers/`
- `libraries/render/` → merge into `drivers/render/`
- `libraries/png`, `jpeg`, `mp4`, `avi`, `wav` → `codecs/`
- `presenter/src/handlers.rs` viewer code → `viewers/` crates
- `libraries/view-tree/` → `shared/view/` (split into modules)
- `libraries/layout/` → `shared/line-break/`
- `libraries/fs/` → fold into `shared/store/`
- `servers/fs/` → `shared/host-fs/`
- `servers/layout/` → `shared/layout/`
- `servers/jpeg-decoder/`, `png-decoder/` → delete (viewers call codecs)
- `servers/video-decoder/` → delete (video viewer + codec driver)
- `bench/` + `bench-smp/` → `shared/benchmarks/`
- `servers/hello/` → delete
- Remaining `libraries/` → `shared/`
- Remaining `servers/` → `shared/`
- Update all Cargo.toml paths, workspace members, CLAUDE.md files

### Phase 7: Design Doc Updates

- `architecture.md` — update pipeline to show view tree as fan-out point
- `glossary.md` — add viewer, view tree, content command terms
- `content-service-architecture.md` — update to reflect final naming/structure
  and the Viewer trait redesign (output-unified, input-specific)
- `STATUS.md` — update current state

---

## Execution

Phases 1–3 are one session (viewer extraction + Space elimination). Phases 4–5
are a second session (event routing + registry). Phase 6 is a focused structural
move (can be its own session or paired). Phase 7 is doc updates (pair with Phase
6).

| Phase | What                                 | Risk   | Est. lines moved |
| ----- | ------------------------------------ | ------ | ---------------- |
| 1     | Infrastructure                       | Low    | ~150             |
| 2     | Viewer extraction                    | High   | ~790             |
| 3     | Workspace viewer + Space elimination | Medium | ~350             |
| 4     | Event routing                        | High   | ~540             |
| 5     | Viewer registry                      | Medium | ~200             |
| 6     | Structural reorg                     | Low    | ~0 (moves)       |
| 7     | Design doc updates                   | Low    | ~200             |

Phase 2c (TextViewer rendering) and Phase 4b (TextViewer events) are the
highest-risk steps. Combined they move ~1330 lines of text-specific code. They
are separated into two phases because rendering can be verified independently of
event routing — each phase has a clean verification boundary.

## Verification

After each phase:

1. `cargo check` — presenter compiles for bare-metal
2. `cargo t` — all workspace tests pass
3. `cargo test -p view-tree --target aarch64-apple-darwin` — view-tree tests
   pass

After Phase 3 (Space elimination):

4. Visual regression: boot, Ctrl+Tab through content types, verify image
   display, video play/pause, text rendering, clock updates
5. Verify resource cleanup: close documents, confirm no handle leaks

After Phase 4 (event routing):

6. Text editing: type, delete, undo, selection (shift+arrow, double-click,
   triple-click, drag), scroll, Cmd+A, word navigation (Alt+arrow)
7. Video: click play/pause button
8. Workspace: Ctrl+Tab space switch, Ctrl+W close

After Phase 6 (structural reorg):

9. Every `Cargo.toml` path resolves
10. `cargo build --release` produces a bootable OS
11. Full visual regression: boot, exercise all content types, all interactions
