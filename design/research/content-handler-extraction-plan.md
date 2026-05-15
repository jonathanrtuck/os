# Content Service Architecture — Implementation Plan

From the current monolithic presenter to the content service architecture
described in `content-service-architecture.md`. The view tree is the SSOT.
Content viewers produce it. Rendering pipelines consume it. The orchestrator
manages it.

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
│
├── kernel/                                 (unchanged — complete, frozen ABI)
│
├── tools/                                  (unchanged)
│
├── test/                                   (unchanged)
│
└── user/
    ├── system/                             (renamed from top-level abi/ipc/heap/virtio)
    │   ├── abi/                            kernel interface, syscall wrappers
    │   ├── ipc/                            IPC primitives, messages, rings
    │   ├── heap/                           bare-metal allocator (GlobalAlloc)
    │   └── virtio/                         virtio MMIO transport, split virtqueue
    │
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
    │
    ├── codecs/                             (new grouping — moved from libraries/)
    │   ├── png/                            PNG decoder (deflate, filters, interlace)
    │   ├── jpeg/                           JPEG decoder (DCT, Huffman)
    │   ├── mp4/                            MP4 container parser (ISO BMFF)
    │   ├── avi/                            AVI container parser (RIFF)
    │   └── wav/                            WAV audio parser (RIFF)
    │
    ├── viewers/                            (new — per-mimetype content viewers)
    │   ├── image/                          (new)
    │   │   ├── Cargo.toml                  deps: view, scene, codecs/png, codecs/jpeg
    │   │   └── src/
    │   │       └── lib.rs                  ImageViewer — image/jpeg, image/png
    │   │                                   Calls codec libraries directly (no
    │   │                                   decoder services). Produces single-
    │   │                                   node ViewSubtree with intrinsic dims.
    │   │
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
    │   │
    │   ├── video/                          (new)
    │   │   ├── Cargo.toml                  deps: view, scene, icons, codecs/mp4,
    │   │   │                               codecs/avi
    │   │   └── src/
    │   │       └── lib.rs                  VideoViewer — video/mp4, video/avi
    │   │                                   Container parsing moved in from
    │   │                                   video-decoder service. Hardware
    │   │                                   decode via codec driver IPC.
    │   │
    │   └── workspace/                      (new)
    │       ├── Cargo.toml                  deps: view, scene, fonts, icons
    │       └── src/
    │           └── lib.rs                  WorkspaceViewer —
    │                                       application/x-os-workspace
    │                                       Title bar, clock, document icon,
    │                                       space strip, slide animation.
    │
    ├── editors/                            (moved from top-level)
    │   └── text/                           text editor (unchanged internally)
    │
    └── shared/                             libraries + services — the common substrate
        ├── view/                           (renamed from view-tree — the SSOT)
        │   ├── Cargo.toml
        │   └── src/
        │       ├── lib.rs                  re-exports
        │       ├── node.rs                 ViewNode — all properties
        │       ├── tree.rs                 ViewTree container, traversal
        │       ├── content.rs              ViewContent enum
        │       ├── layout.rs              layout engine (block, fixed, flow)
        │       ├── viewer.rs               Viewer trait, ViewSubtree,
        │       │                           Constraints, EventResponse,
        │       │                           ContentCommand, InputEvent
        │       └── tests.rs
        │
        ├── scene/                          GPU pipeline wire format (unchanged)
        ├── fonts/                          font parsing, shaping, rasterization
        ├── drawing/                        2D rasterization primitives
        ├── animation/                      spring physics, easing curves
        ├── line-break/                     (renamed from layout — text line-breaking)
        ├── piecetable/                     piece table data structure
        ├── icons/                          vector icon path data
        │
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
        │
        ├── presenter/                      (changed — pure orchestrator)
        │   ├── Cargo.toml                  deps: view, scene, all viewers, ...
        │   └── src/
        │       ├── lib.rs                  protocol constants
        │       ├── main.rs                 boot, serve loop, document open/close,
        │       │                           viewer lifecycle, viewer registry,
        │       │                           event routing
        │       └── renderer.rs             (new — GPU renderer: composed
        │                                   ViewTree → scene graph VMO)
        │
        ├── document/                       document manager (edit authority,
        │                                   single-writer, COW undo, shared VMO)
        ├── layout/                         (renamed from layout-service)
        ├── store-service/                  COW filesystem over block driver
        ├── audio/                          mixer service → snd driver
        ├── console/                        PL011 UART debug output
        ├── name/                           service discovery registry
        ├── init/                           process spawner, DMA allocator
        ├── host-fs/                        (renamed from fs-service — 9p bridge)
        │
        ├── benchmarks/                     (merged from bench/ + bench-smp/)
        └── integration-tests/              multi-service test binaries
```

### What's gone

- **presenter/src/build.rs** — rendering logic moved to viewers. Scene writing
  moved to `renderer.rs`. Orchestration moved to `main.rs`.
- **presenter/src/input.rs** — text navigation moved to `viewers/text/`.
  Workspace-level dispatch (Ctrl+Tab, Ctrl+W) inlined in `main.rs`.
- **presenter/src/pointer.rs** — text click/selection/drag moved to
  `viewers/text/`. Video toggle moved to `viewers/video/`.
- **presenter/src/handlers.rs** — temporary file from initial extraction.
  Replaced by `viewers/` crates.
- **Space enum** — replaced by `Vec<Document>` where each Document holds a
  viewer instance.
- **Space::Showcase** — deleted.
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
// main.rs — ~400 lines (down from ~1500)

struct Document {
    viewer: ViewerKind,         // enum dispatch (no vtable in no_std)
    mimetype: &'static [u8],
}

struct Presenter {
    documents: Vec<Document>,
    workspace: WorkspaceViewer,
    active_doc: usize,

    // Shared infrastructure
    doc_ep: Handle,
    layout_ep: Handle,
    render_ep: Handle,
    editor_ep: Handle,
    audio_ep: Handle,
    console_ep: Handle,

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

    // Animation
    slide_spring: SpringI32,
    slide_animating: bool,
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

**1a.** Rename `ContentHandler` → `Viewer`, `ViewSubtree` stays. Update
view-tree crate (soon `view/`).

**1b.** `write_subtree` — recursive ViewTree → scene graph writer. Walks
depth-first, calls `write_view_node` per node, handles `child_offset_x/y`.

**1c.** `Constraints.now_ns` — timestamp for animated content.

**1d.** Persistent viewers in Space — Space variants hold viewer instances,
created during document loading.

### Phase 2: Viewer Extraction

Extract content-type-specific rendering from build.rs into viewer crates
(initially as modules in `presenter/src/handlers.rs`, moved to `viewers/` crates
in Phase 6).

**2a.** ImageViewer — make persistent, complete integration. Declares intrinsic
dimensions + shadow + content_id.

**2b.** VideoViewer — owns content_id, pixel dimensions, playing state. Produces
video frame + play/pause button subtree.

**2c.** TextViewer — largest extraction. Owns scroll_y, char_width_mpt, cmap
tables, glyph buffer, blink_start. Moves ~800 lines of text building code.
Rendering only (event routing in Phase 4).

### Phase 3: Workspace Viewer

Extract desktop chrome (title bar, clock, icon, strip) into WorkspaceViewer.
Delete Space::Showcase. `build_scene()` becomes ~40 lines of orchestration.

### Phase 4: Event Routing

**4a.** Richer EventResponse — viewers return `ContentCommand` enums
(MoveCursor, Select, TogglePlayback, Seek). Presenter executes IPC.

**4b.** Text event migration — key navigation, click/selection/drag move into
TextViewer.

**4c.** Hit testing on view tree — replace scene graph hit testing.

### Phase 5: Viewer Registry

Mimetype → viewer constructor dispatch. Document open by mimetype lookup.
User-switchable viewers per document. Delete hardcoded content-type knowledge
from presenter.

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
- `STATUS.md` — update current state

---

## Execution

Phases 1–3 are one session (viewer extraction). Phases 4–5 are a second session
(event routing + registry). Phase 6 is a focused structural move (can be its own
session or paired). Phase 7 is doc updates (pair with Phase 6).

| Phase | What               | Risk   | Est. lines moved |
| ----- | ------------------ | ------ | ---------------- |
| 1     | Infrastructure     | Low    | ~100             |
| 2     | Viewer extraction  | Medium | ~1000            |
| 3     | Workspace viewer   | Medium | ~200             |
| 4     | Event routing      | High   | ~600             |
| 5     | Viewer registry    | Low    | ~100             |
| 6     | Structural reorg   | Low    | ~0 (moves)       |
| 7     | Design doc updates | Low    | ~200             |

Phase 2c (TextViewer) is the highest-risk single step.

## Verification

After each phase:

1. `cargo check` — presenter compiles for bare-metal
2. `cargo t` — all workspace tests pass
3. `cargo test -p view-tree --target aarch64-apple-darwin` — view-tree tests
   pass

After Phase 6 (structural reorg): 4. Every `Cargo.toml` path resolves 5.
`cargo build --release` produces a bootable OS 6. Visual regression: boot,
Ctrl+Tab through content types, text editing, image display, video play/pause,
clock updates
