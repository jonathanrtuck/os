# Content Service Architecture

The rendering and interaction model for the document-centric OS. Derived from
the principle that the OS natively understands and renders content types — there
are no applications, so the OS itself is the renderer for all supported
mimetypes.

**Status:** Draft (revised). Emerged from a design conversation exploring how
web browsing fits into the OS. The architecture generalizes: the same model
handles local documents, web pages, compound documents, and the desktop itself.

**Prerequisite reading:** `architecture.md` (pipeline, responsibilities),
`philosophy.md` (root principles), `foundations.md` (content model, compound
documents), `manifest-model.md` (composition tree).

---

## Core Insight

Every mimetype the OS supports needs logic that turns content bytes into visual
output. An image needs decoding and aspect-fit placement. Text needs shaping and
line breaking. A presentation needs slide layout with positioned content slots.
An HTML page needs DOM parsing, CSS cascade, and box layout.

This per-mimetype logic has to live somewhere. Either it all accumulates in a
single ever-growing component (the presenter), or it is carved into independent
components along the natural boundary — the mimetype. The decomposition is
forced by the problem shape.

The architecture has three layers:

1. **Content handlers** — per-mimetype components behind a uniform trait. Each
   takes content bytes and size constraints, produces a view tree (or
   contributes nodes to one). Internally they vary from trivial (image: one leaf
   node with intrinsic dimensions) to enormous (HTML: DOM, CSS cascade,
   thousands of view nodes). The presenter doesn't know or care about the
   internal complexity.

2. **The view tree** — the universal intermediate representation. A tree of
   nodes carrying layout mode (flow, grid, fixed canvas), style properties
   (margins, fonts), semantic annotations (role, level, name), and content.
   Different front-ends produce view trees from different sources: the manifest
   parser produces one for OS-native documents; a CSS cascade produces one for
   HTML. **The view tree is the fan-out point** — the last representation that
   all rendering pipelines share. Visual, textual, and assistive renderers
   diverge after it.

3. **Rendering pipelines** — parallel consumers of the view tree, each producing
   output for a different modality:
   - **GPU pipeline:** layout engine → text shaping → scene graph → compositor →
     pixels
   - **CLI pipeline:** text-mode reflow → formatted terminal output
   - **Screen reader pipeline:** tree traversal → speech synthesis
   - **Braille pipeline:** tree traversal → tactile cell encoding

   The layout engine is shared infrastructure within the GPU and CLI pipelines.
   Flow, grid, fixed-canvas, and (eventually) flexbox are algorithms in this
   engine, not per-handler reimplementations. Non-visual pipelines may skip
   layout — the screen reader doesn't need to know where a heading is positioned
   to announce "heading level 2."

### The universal rule

**Every content type produces view tree nodes. No exceptions.** The view tree is
mandatory because it is the fan-out point for all rendering pipelines, including
accessibility. Content that bypasses the view tree is invisible to non-visual
renderers.

**The OS always mediates rendering.** Content handlers never touch the GPU
directly. They produce scene descriptions (2D or 3D); the OS's rendering
pipelines turn those into output. This ensures every content type is accessible,
composable, searchable, and renderer-swappable. There are no opaque black boxes
and no second-class content types.

What varies across content types is only whether the layout engine computes
positions or the handler provides them directly as view tree node coordinates.
The layout engine is optional. The view tree is not.

---

## How Content Reaches the View Tree

Every content handler produces view tree nodes. What varies is the front-end
that produces them and whether the layout engine computes positions.

### Layout-intent content (manifest, HTML, SVG)

The handler produces view tree nodes with layout intent (margins, flow mode,
constraints). The layout engine resolves them into positions.

```text
content bytes → front-end → view tree (with layout intent) → layout engine → positions
```

- **OS-native compounds:** manifest parser reads the composition tree, produces
  view tree nodes with spatial/temporal axis annotations. The layout engine
  handles FixedCanvas, Flow, Grid, Sequential, etc.
- **HTML:** CSS cascade resolves computed styles for every DOM node, produces
  view tree nodes. The same layout engine handles flow, grid, flex, absolute
  positioning.
- **SVG:** SVG parser produces view tree nodes with transforms and the SVG
  coordinate model. Partial layout (text positioning, viewBox), mostly absolute.

The manifest and the DOM+CSS are different ways of _describing_ layout intent.
The layout engine is the shared computation that resolves intent into positions.
The per-format complexity lives in the front-end that produces the view tree
nodes, not in the layout algorithms.

What the layout engine computes:

| Manifest spatial mode | CSS equivalent                | Layout algorithm                |
| --------------------- | ----------------------------- | ------------------------------- |
| Flow                  | `display: block` normal flow  | Inline/block formatting context |
| FixedCanvas           | `position: absolute`          | Direct coordinate placement     |
| Grid                  | `display: grid`               | Grid track sizing               |
| Freeform              | `position: absolute`, no clip | Direct placement, no bounds     |

The CSS-specific complexity — cascade, specificity, inheritance, shorthand
expansion, `calc()`, custom properties, media queries — lives in the CSS
front-end that produces the view tree. The layout engine never sees CSS syntax.
It sees "flow node, 12pt margin, 3 children" regardless of whether that came
from a manifest or from `display: block; margin: 12pt`.

### Pre-positioned content (PDF, PostScript)

The handler interprets a drawing program and produces view tree nodes with
positions already filled in. The layout engine has nothing to do — the format
already specifies exact coordinates. But the view tree nodes still carry
semantic annotations (role, name, reading order) so non-visual renderers can
describe the document.

```text
content bytes → interpreter → view tree (with positions + semantics)
```

- **PDF:** a stack-based content stream interpreter. Pushes/pops graphics state,
  applies transforms, produces view tree nodes with positioned glyphs and paths.
  Each page has a fixed MediaBox. Tagged PDFs provide the semantic structure
  (roles, reading order) directly; untagged PDFs require heuristic extraction.
- **PostScript:** a Turing-complete stack machine. Same model as PDF (PDF
  descends from PostScript).

These handlers skip the layout engine, but they do NOT skip the view tree. Their
output is view tree nodes with positions and semantics — accessible, composable,
searchable, like every other content type.

### Leaf content (image, text, video, audio)

The handler decodes content bytes and produces view tree leaf nodes with
intrinsic dimensions. A parent (the manifest layout engine or an HTML handler)
positions the result within its own layout.

```text
content bytes → decoder → view tree leaf (intrinsic dimensions + semantics)
```

- **image/png, image/jpeg:** decode pixels, report intrinsic dimensions.
- **text/plain:** shape glyphs, break lines within given width.
- **text/markdown:** parse block AST, produce styled text + heading nodes.
- **video/mp4:** decode current frame, report frame dimensions (with playback
  state).
- **audio/\*:** no visual output (or a waveform visualization). Playback state
  and transport controls.

### 3D content (models, scenes, interactive)

The handler produces view tree nodes with 3D scene descriptions: nodes with
transforms, geometry, materials, cameras. The OS's 3D rendering pipeline renders
them into the allocated 2D rectangle.

```text
content bytes → parser/simulation → view tree (with 3D scene + semantics)
```

- **model/gltf:** parses scene hierarchy, produces view tree with 3D nodes
  (transforms, meshes, PBR materials). Static scene.
- **Interactive 3D** (CAD, games): handler runs simulation (physics, game
  logic), produces updated view tree with 3D scene description each frame. The
  simulation is handler-internal; the output is a scene description.

3D content appears as a leaf in 2D layout (positioned like an image) with a 3D
scene subtree that the OS's 3D renderer processes. The 3D renderer is a
rendering pipeline leaf node, swappable at runtime (Metal rasterizer, software
ray tracer, wireframe viewer).

For content that needs custom GPU programs (custom shaders, compute-based
effects), the handler submits program descriptions through the OS's GPU compute
interface. The OS mediates GPU access — the handler never touches the GPU
directly. This is the same model as WebGPU: the content describes what to
compute; the OS does the computing.

### Summary

| Content type   | Produces view tree?  | Layout engine? | OS renders?       |
| -------------- | -------------------- | -------------- | ----------------- |
| text/plain     | Yes                  | Yes (flow)     | Yes               |
| image/jpeg     | Yes                  | Yes (leaf)     | Yes               |
| Presentation   | Yes                  | Yes (fixed)    | Yes               |
| text/html      | Yes                  | Yes (flow)     | Yes               |
| PDF            | Yes (pre-positioned) | No             | Yes               |
| model/gltf     | Yes (3D scene)       | 2D: yes (leaf) | Yes (3D renderer) |
| Interactive 3D | Yes (3D scene/frame) | 2D: yes (leaf) | Yes (3D renderer) |

---

## The Content Handler Trait

All content handlers implement a uniform interface. The presenter holds
`Box<dyn ContentHandler>` and doesn't branch on handler type or deployment
model.

```text
trait ContentHandler:
    load(content: &[u8], mimetype: MediaType, constraints: Constraints)
        → ViewSubtree + Dimensions

    resize(constraints: Constraints) → Dimensions

    update(content: &[u8]) → ViewSubtree + Dimensions

    event(event: InputEvent) → EventResult

    teardown()
```

- `load` — initial content load. Handler parses bytes, builds internal model (if
  any), produces a view subtree (layout intent + semantic annotations + visual
  style + content).
- `resize` — viewport changed. Handler re-computes at new constraints, produces
  updated view subtree. Content unchanged.
- `update` — content bytes changed (edit, undo). Handler rebuilds its internal
  model from the new bytes, produces updated view subtree.
- `event` — input event routed to this handler. Returns whether it was handled,
  plus any view tree updates (hover state, cursor shape, scroll).
- `teardown` — handler is being closed. Release resources.

**What `ViewSubtree` is:** a tree of `ViewNode` values — layout properties,
semantic annotations, visual style, and content references. Owned data. The
presenter integrates the subtree into the document's full view tree, which is
then consumed by whichever rendering pipelines are active (GPU, CLI, screen
reader, etc.).

**`update` is the mutation path.** The original document
(`content-service-architecture-v1`) only had a one-shot `navigate` method. Real
content changes constantly — typing, undo, external edits. `update` gives the
handler new content bytes and asks it to recompute. Handlers that maintain
internal state (DOM, block AST) incrementally update where possible; stateless
handlers (image decoder) just re-decode.

**Handlers are reconstructible from bytes.** If a handler crashes or is
restarted, it can be rebuilt by calling `load` with the current content bytes.
No external state beyond the content file is needed. This is a requirement, not
an optimization — it makes undo, crash recovery, and handler restarts all work
the same way: restore the content bytes, re-load the handler.

---

## The View Tree

The view tree is the universal intermediate representation — the fan-out point
where all rendering pipelines diverge. It is not a data structure that exists in
the codebase today. It is the missing layer between source formats (manifests,
DOMs, PDF content streams) and all downstream renderers.

Other names for this concept: **layout tree** (Chromium), **box tree** (CSS
spec, Servo), **render tree** (WebKit), **render object tree** (Flutter). Every
rendering engine has this layer. The name varies; the role is the same.

**What the scene graph is not:** the scene graph is the GPU pipeline's
intermediate format — positioned visual primitives in shared memory, consumed by
the compositor. It is downstream of the view tree, not at the same level. The
CLI renderer and screen reader don't need it.

**What the manifest is:** the manifest composition tree is the OS-native
equivalent of a DOM. It declares structure and layout intent for OS-native
compound documents. It IS the view tree input for `application/x-os-*` types.
But it is one producer of view trees, not the view tree itself.

A view tree node carries three concerns:

```text
ViewNode {
    // Layout — consumed by the layout engine, not passed to renderers
    layout_mode: Flow | Grid | Fixed | Flex | None
    position:    Static | Absolute
    margin:      Edges (top, right, bottom, left)
    padding:     Edges
    border:      Edges (width only — visual border is a style concern)
    sizing:      width, height, min/max (Auto | Points)
    intrinsic:   Container | Fixed(w, h) | Measure(fn)

    // Semantic — consumed by CLI, screen reader, braille
    role:          Heading | Paragraph | Image | List | ...
    level:         u8 (heading depth)
    name:          Option<String> (accessible name / alt text)
    reading_order: u16 (when visual order ≠ logical order)

    // Visual style — passes through layout to the scene graph builder
    background:    Color
    border_style:  BorderStyle
    corner_radius: u16
    opacity:       u8
    font:          FontSpec
    color:         Color

    // Content
    content: text | image ref | empty (container)

    // Tree structure
    first_child:  NodeId
    next_sibling: NodeId
}
```

Different front-ends produce these from different sources:

| Front-end       | Source                              | Complexity |
| --------------- | ----------------------------------- | ---------- |
| Manifest parser | Composition tree + axis annotations | Low        |
| CSS cascade     | DOM + stylesheets                   | High       |
| SVG parser      | SVG elements + CSS/attributes       | Medium     |

Different rendering pipelines consume different subsets:

| Pipeline      | Reads from ViewNode                           | Ignores                       |
| ------------- | --------------------------------------------- | ----------------------------- |
| GPU (layout)  | Layout fields, intrinsic sizing               | (produces positioned boxes)   |
| GPU (scene)   | Visual style, content, positioned boxes       | Layout fields (already used)  |
| CLI           | Semantic fields, text content, tree structure | Visual style, layout          |
| Screen reader | Semantic fields, name, reading order          | Visual style, layout, content |
| Braille       | Semantic fields, text content                 | Visual style, layout          |

**Implication for future CSS support:** the OS doesn't need a separate layout
engine for HTML. It needs a CSS front-end that produces view trees, feeding the
same layout engine that manifests feed. The layout algorithms (flow, grid,
fixed) are shared. The CSS-specific work (cascade, specificity, inheritance) is
a front-end concern.

**Implication for accessibility:** non-visual renderers consume the view tree
directly, not the scene graph. The view tree IS the document structure — it
knows that a node is "heading level 2" because the manifest or DOM said so. The
scene graph flattens this into positioned visual elements with semantic
annotations copied from the view tree. Non-visual renderers reading the view
tree get a genuine document structure, not a visual tree scraped for semantic
crumbs. This is how mature OS accessibility works: macOS apps provide an
accessibility tree (NSAccessibility) that VoiceOver reads directly — a parallel
data path, not the visual framebuffer with annotations.

---

## The Presenter's Role

The presenter is the orchestrator. It opens documents, calls content handlers,
composes their output into a scene graph, and delivers that scene graph to the
compositor. It is the **document manager** described in the original document,
but it is also the `application/x-os-workspace` content handler — it manages the
desktop as a compound document.

Responsibilities:

1. **Open documents:** determine mimetype (from the store catalog), look up the
   content handler, call `load` with content bytes and constraints.

2. **Compose scene graph:** collect scene data from all active handlers, write
   it into the scene graph VMO(s) for the compositor. For compound documents,
   this means walking the manifest, calling leaf handlers for each content slot,
   and positioning the results according to axis declarations.

3. **Route events:** hit-test the scene graph, determine which handler owns the
   target node, forward the event via `handler.event()`.

4. **Propagate content changes:** when the document service notifies that
   content bytes changed (edit or undo), call `handler.update()` with the new
   bytes.

5. **Manage handler lifecycle:** create handlers on document open, tear down on
   close, restart on crash (call `load` with current bytes).

The presenter does NOT do content-type-specific work. It doesn't parse HTML,
decode images, or shape text. It calls handlers that do those things. Its own
complexity is structural: document management, event routing, scene graph
composition, animation (spring physics for space switching).

**Relationship to the existing presenter:** the current presenter
(`user/servers/presenter/`) does some content-type work inline (building glyph
nodes for text/plain, positioning image nodes). This is correct for the current
state (few content types, all simple). As content types grow, this work migrates
into content handlers behind the trait. The presenter becomes thinner over time.

---

## Compound Documents

A compound document (presentation, article, album) is an OS-native document with
a manifest. The manifest declares a composition tree with spatial, temporal, and
logical axis annotations. The manifest layout engine — which IS the presenter
walking the manifest — interprets these annotations and positions child content.

**There is no "presentation handler" or "article handler."** The manifest's
composition tree declares `spatial: FixedCanvas, temporal: Sequential` for a
presentation, or `spatial: Flow` for an article. The presenter's generic
manifest-driven layout logic handles both. The per-type complexity lives in the
leaf content handlers (image, text, video), not in compound-type-specific layout
code.

The manifest layout engine is the Path 1 front-end for OS-native documents. It
reads the manifest, produces a view tree (or walks the composition tree directly
if the manifest maps cleanly to layout engine input), and the shared layout
engine resolves positions.

### Portals: cross-VMO scene graph composition

When the presenter encounters a content slot in a compound document, the leaf
handler produces scene data. The presenter can either:

- **Inline:** write the handler's scene nodes directly into the presenter's
  scene VMO, adjusting indices and offsets. Simpler for small handlers.
- **Portal:** allocate a child scene VMO for the handler, write the handler's
  nodes there, and create a `Content::Portal` node in the presenter's scene
  graph that references the child VMO.

```rust
Content::Portal {
    scene_id: u32,  // identifies which child scene VMO
}
```

Portal benefits over inlining:

- **Independent dirty tracking.** Each VMO has its own generation counter. A
  change in a child's scene graph doesn't require the parent to re-copy.
- **No index remapping.** Each handler writes with independent node indices.
- **Isolation.** An out-of-process handler writes to its own VMO without
  touching the presenter's memory.

Portal costs:

- **Compositor complexity.** The compositor must follow portal references across
  VMO boundaries during its rendering walk.
- **Overhead for trivial handlers.** An image handler producing one node doesn't
  benefit from its own VMO.

The portal variant does not exist in the scene graph today. Adding it is
straightforward: a new `Content::Portal` variant (fits in the existing 24-byte
`Content` enum), and compositor walk logic to follow the reference.

### Layout negotiation

For flow layouts where children's sizes affect sibling positions:

1. **Presenter → handler:** width constraint ("you have 600pt")
2. **Handler computes:** intrinsic height at that width ("I need 240pt")
3. **Presenter updates:** positions siblings using reported heights

This is the universal pattern across layout systems: CSS normal flow, TeX's box
model, SwiftUI proposed/reported size, Flutter constraints. Fixed-layout
compounds (presentations, fixed-canvas regions) skip negotiation — the parent
provides both width and height.

When a handler's content changes (e.g., text edit adds a line), the handler
reports new dimensions. The presenter re-positions siblings if needed.

**This is bidirectional communication** between the presenter and handlers,
which nuances `architecture.md`'s strict one-way pipeline. The pipeline is
one-way at the macro level (user intent → scene graph → pixels). Layout
negotiation is internal to the "OS service" stage — it was always happening, but
hidden when the presenter did all layout inline. Extracting handlers makes the
bidirectional negotiation visible without changing the pipeline's external
structure.

### Recursive embedding

A compound document can contain compound documents (a presentation embedded in
an article). The presenter recursively opens child documents that are themselves
compound:

1. **Depth limit.** The presenter enforces a maximum nesting depth (e.g., 8).
   Beyond that, the child renders as a thumbnail or placeholder. This is a
   pragmatic guard, not an architectural limitation.

2. **Cycle detection.** Each document in the open chain is tracked by
   `DocumentId`. If a child references a document already in the chain, the
   cycle is broken with a placeholder. Copy semantics (embedding creates an
   independent copy) make cycles unlikely in practice — they require manual
   construction.

3. **Performance model.** Each level of nesting adds one layout negotiation
   round and one scene graph composition step. At depth 5 with 10 children per
   level, that's ~50 handler calls — well within budget for a 16ms frame.
   Incremental updates (only re-layout changed subtrees) keep this manageable.

---

## Event Routing

Input events flow through the presenter, which owns the scene graph and knows
the handler-to-node mapping:

1. Input driver sends pointer/key event to the presenter.
2. Presenter hit-tests the scene graph (depth-first, follows portals if
   present).
3. Identifies which content handler owns the hit node.
4. Calls `handler.event(input_event)` on the target handler directly.

Events do not cascade through intermediate compound handlers. The presenter
walks the scene graph itself and delivers events directly to the target. This is
O(depth) traversal, not O(depth) forwarding hops.

Visual state updates (cursor shape, hover highlight) are scene graph mutations.
The handler updates its scene data; the presenter writes it to the scene VMO;
the compositor renders the change.

---

## Edit Protocol Integration

`foundations.md` specifies: the OS service is the sole writer to document files.
Editors send `beginOperation`/`endOperation` via IPC. The OS takes COW snapshots
at operation boundaries for undo.

Content handlers are NOT writers. They are readers that produce visual output
from content bytes. The edit flow is:

```text
Editor → document service → content file (sole writer)
                ↓ notification
            Presenter → handler.update(new_bytes) → new view subtree
```

1. Editor sends a write request to the document service.
2. Document service applies the write to the content file.
3. Document service notifies the presenter that content changed.
4. Presenter calls `handler.update()` with the updated content bytes.
5. Handler rebuilds its internal model (or incrementally updates it) and
   produces an updated view subtree.
6. Presenter integrates the subtree into the document's view tree.
7. Active rendering pipelines consume the updated view tree (GPU pipeline
   re-layouts and rebuilds scene graph; CLI pipeline re-renders text).

**Undo works identically.** The document service restores a COW snapshot. The
content bytes revert. The presenter calls `handler.update()` with the restored
bytes. The handler rebuilds from the restored state. The handler does not need
to know that undo happened — it just sees new bytes.

**Handlers are stateless in the undo sense.** A handler may maintain internal
state for performance (a parsed DOM, a decoded image cache), but that state must
be reconstructible from the content bytes alone. This is enforced by the trait:
`load(bytes)` and `update(bytes)` are the only paths for content to enter the
handler. There is no "restore handler state" operation — there is only "here are
the current bytes."

**Compound document edits** (rearranging slides, repositioning content in a
canvas) modify the manifest, not the content files. The manifest is a file in
the store, subject to the same edit protocol and COW snapshotting. The presenter
detects manifest changes and re-walks the composition tree.

---

## Deployment: In-Process vs. Out-of-Process

Whether a content handler runs as a function call within the presenter or as a
separate process behind IPC is a deployment choice. **The trait is the same. The
system properties differ.**

The trait abstracts over both: an in-process module implements the trait
directly; an out-of-process service gets a proxy struct that implements the
trait but internally does IPC send/receive. The presenter holds
`Box<dyn ContentHandler>` and doesn't branch.

**What differs between the two deployment models:**

| Property         | In-process (module)   | Out-of-process (service)      |
| ---------------- | --------------------- | ----------------------------- |
| Call latency     | Nanoseconds           | Microseconds (IPC)            |
| Crash isolation  | Crash takes presenter | Crash is contained            |
| Error handling   | Panics propagate      | Errors returned via IPC       |
| Resource cleanup | Shared address space  | Independent; explicit cleanup |
| Data transfer    | Direct memory access  | Shared VMO or copy            |

These are real architectural differences, not deployment details. A crash in an
in-process image decoder takes down the presenter. A crash in an out-of-process
HTML renderer is contained and recoverable. The trait is the same; the failure
model is not.

**Criteria for out-of-process:**

- **Untrusted input.** HTML from the internet, PDF from unknown sources. A
  parser bug must not take down the OS service.
- **Crash-prone complexity.** CSS cascade + layout is the most likely code to
  have edge-case panics.
- **Heavy computation.** Video decode benefits from independent scheduling.
  (Already a separate service: `video-decoder`.)

**Criteria for in-process:**

- **Trivial logic.** Image decode → one node. Minimal crash risk.
- **Performance-sensitive.** Flow layout negotiation with many children benefits
  from function-call latency.
- **Trusted input.** OS-native manifests and locally-created content.

**Default:** start in-process, promote to out-of-process when there's a reason.
The trait interface makes promotion a refactor, not a redesign.

---

## Shared Libraries

Content handlers share leaf libraries. They never call each other. The presenter
calls handlers. Handlers call libraries.

```text
Presenter
  ├── calls handler A (text/html)
  │     ├── uses resource resolver (fetch bytes)
  │     ├── uses png decoder library
  │     ├── uses font library
  │     └── uses CSS cascade + layout engine (shared with manifest path)
  ├── calls handler B (image/png)
  │     └── uses png decoder library
  └── calls handler C (text/plain)
        └── uses font library
```

An HTML handler that encounters `<img src="photo.jpg">` does not call a sibling
"image content handler." It uses the PNG/JPEG decoder library directly (the same
library the image handler uses) and requests the image bytes through a resource
resolver abstraction. The resource resolver fetches from the local store or
network depending on the base URI. The handler doesn't know or care which.

| Library     | Used by                                          | Status |
| ----------- | ------------------------------------------------ | ------ |
| `scene`     | All handlers + compositor                        | Exists |
| `fonts`     | Any text-producing handler                       | Exists |
| `layout`    | text/plain, text/markdown (line breaking)        | Exists |
| `png`       | image/png handler, HTML handler (inline images)  | Exists |
| `jpeg`      | image/jpeg handler, HTML handler (inline images) | Exists |
| `drawing`   | Path rasterization, glyph rendering              | Exists |
| `animation` | Presenter (spring physics)                       | Exists |
| `manifest`  | Manifest parser (front-end for view tree)        | New    |
| `css`       | CSS cascade (front-end for view tree)            | New    |
| `view-tree` | Manifest parser, CSS cascade, layout engine      | New    |
| `pdf`       | PDF content stream interpreter                   | New    |

---

## GUI, CLI, and Assistive Interfaces

From `foundations.md`: "The GUI, CLI, and assistive interfaces are all equally
fundamental."

The view tree makes this concrete. Content handlers produce view trees carrying
both visual and semantic data. Different rendering pipelines consume the view
tree, each extracting what it needs:

```text
content handler → view tree (universal representation)
                    ├── GPU pipeline:     layout → shaping → scene graph → compositor → pixels
                    │                     (3D subtrees → 3D renderer → composited into 2D)
                    ├── CLI pipeline:     text-mode reflow → formatted text → terminal
                    ├── Screen reader:    tree walk → speech synthesis → audio
                    └── Braille display:  tree walk → braille encoding → device
```

These are parallel pipelines with a shared input, not one pipeline with
swappable leaf renderers. The GPU pipeline has multiple stages (2D layout, text
shaping, 3D rendering for 3D subtrees) that non-visual pipelines don't need. The
screen reader skips layout entirely — it doesn't care where a heading is
positioned, only that it exists.

The scene graph is the GPU pipeline's intermediate format — the cross-process
boundary between the presenter (which does layout and text shaping) and the
compositor (which rasterizes 2D content and composites). 3D subtrees are
rendered by the OS's 3D rendering pipeline into their allocated 2D rectangles,
then composited with the rest. Non-visual renderers bypass both the scene graph
and the 3D renderer.

Accessibility is not bolted onto a visual format — it is a parallel rendering
pipeline consuming the same view tree that the GPU pipeline consumes. This is
the designed approach, not the expedient one.

---

## Relationship to Existing Architecture

This model refines `architecture.md`'s pipeline. The one-way macro flow becomes:

```text
User Input → Content Handler → View Tree → [rendering pipeline] → Output
```

The pipeline has a fan-out at the view tree. `architecture.md` described a
single pipeline ending at the scene graph. The refined model recognizes that the
pipeline branches into multiple modality-specific pipelines, with the view tree
as the branching point:

```text
                                 ┌─ 2D layout → scene graph → compositor → pixels (GPU)
                                 │  (3D subtrees → 3D renderer → composited into 2D)
Content Handler → View Tree ─────┼─ text reflow → terminal output (CLI)
                                 ├─ tree walk → speech (screen reader)
                                 └─ tree walk → braille cells (braille)
```

What changes:

- **The "OS Service" stage** is decomposed. The monolithic presenter splits into
  the presenter (orchestrator + workspace handler) and per-mimetype content
  handlers. The pipeline stages are the same; the "OS Service" stage has
  internal structure now.

- **The view tree** is the new universal intermediate representation.
  `architecture.md` said "the scene graph is the universal interface." The scene
  graph is now the GPU pipeline's internal format. The view tree is the actual
  universal interface — it's the last representation all rendering modalities
  share.

- **The presenter** evolves incrementally. Currently it does some content-type
  work inline (text/plain glyph building, image node placement). As new content
  types are added, this work migrates into handlers. The presenter becomes
  thinner over time, converging on pure orchestration.

- **The compositor** gains portal traversal if/when portals are adopted —
  following cross-VMO references during the scene graph walk.

- **The scene graph** gains `Content::Portal` — a variant that references
  another scene VMO. (The existing `Content` enum has room; portal fits within
  the 24-byte constraint.) The scene graph no longer needs semantic fields
  (role, level, name) once non-visual renderers consume the view tree directly.
  In the interim, semantic fields remain for backwards compatibility.

- **The layout engine** becomes shared infrastructure, not presenter-internal
  logic. Manifest-driven layout and CSS layout feed the same algorithms. The
  `layout` library evolves from line-breaking into a general layout engine.

The adaptation layer (drivers below, editors above, translators at the sides) is
unchanged. Editors still send write requests via the edit protocol. Drivers
still translate hardware. Content handlers affect only the center of the system
— the layer that understands content.

### Relationship to existing code

| Existing component              | Evolution                                                                                                      |
| ------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `user/servers/presenter/`       | Becomes the orchestrator; sheds inline content-type work into handlers                                         |
| `user/libraries/scene/`         | Gains `Content::Portal` variant                                                                                |
| `user/libraries/layout/`        | Grows from line-breaking into general layout engine (flow, grid, fixed)                                        |
| `user/libraries/render/`        | Incremental renderer gains portal-aware dirty tracking                                                         |
| `user/servers/drivers/render/`  | Compositor gains portal walk logic                                                                             |
| `user/servers/png-decoder/`     | Already an out-of-process content handler (decoder service)                                                    |
| `user/servers/jpeg-decoder/`    | Same — already out-of-process                                                                                  |
| `user/servers/video-decoder/`   | Same — already out-of-process with its own demux + decode logic                                                |
| `Content::Image { content_id }` | Existing Content Region mechanism handles texture upload; open question #2 from v1 is already partially solved |

---

## Open Questions

### 1. View tree design

The view tree is the critical new abstraction. Its design determines whether
manifest-driven layout and CSS-driven layout can genuinely share an engine.
Questions:

- What are the node types? (block, inline, grid container, grid item, flex
  container, flex item, absolute, fixed?)
- How are style properties represented? (CSS-like computed values? A simpler
  subset?)
- Does the manifest parser produce view tree nodes directly, or does it produce
  an intermediate form that a separate step normalizes?

**Leaning:** start with the primitives the OS needs now (flow, fixed canvas),
design the view tree node to accommodate CSS properties without implementing
them all. The interface is the bet; the completeness grows over time.

### 2. Handler and editor dispatch

Handlers declare which mimetypes they support. Multiple handlers can support the
same mimetype. The system maintains a prioritized registry, and the user can
override the default.

This is the same dispatch model as editors (from `foundations.md`): mimetype
specificity determines priority (`text/markdown` > `text/*` > `*/*`), with user
preference breaking ties. One mechanism, two registries:

- **Handlers (viewing):** mimetype → prioritized list of content handlers.
  Example: `image/svg+xml` → SVG renderer (default) | text/plain handler ("view
  source"). The text/plain handler supports `text/*`, so it is always available
  as a fallback for any text-based format — "view source" falls out naturally
  from the specificity rules.
- **Editors (editing):** mimetype → prioritized list of editors. Example:
  `text/plain` → text editor (default) | hex editor.

**Discovery mechanism:** static registry for in-process handlers (linked into
the presenter binary), name service for out-of-process handlers (register on
boot). The presenter merges both into one prioritized list. Out-of-process
handlers can override in-process defaults (e.g., a sandboxed HTML handler
replacing a built-in one). The user can switch handlers per-document at runtime.

### 3. Scene graph node limit per VMO

The current scene graph has `MAX_NODES = 512`. A complex web page might need
thousands of nodes. Options: configurable MAX_NODES per VMO, hierarchical
portals within a single handler to split large trees, or a default increase.

### 4. Handler multiplexing

If five text/plain documents are open, are there five handler instances or one
multiplexing handler? Separate instances provide isolation. A single instance is
more efficient (shared font data). The handler trait works either way.

**Leaning:** one instance per document for handlers with state. The presenter
can pool stateless handlers (images).

### 5. Compound document editing

How do content-type editors bind to parts within a compound document? See
`manifest-model.md` open question #1 for full analysis. Current leaning: option
C (edit-in-context with explicit activation gesture), aligned with "viewing is
the default; editing is a deliberate second step."

### 6. Content handler for the desktop

The desktop (`application/x-os-workspace`) is a compound document. Its content
handler is currently the presenter itself — it handles space switching, title
bar chrome, and document positioning. Should the desktop handler be extracted
from the presenter into a regular content handler, making the presenter purely
an orchestrator?

**Leaning:** extract it. The desktop handler produces view tree nodes
(horizontal strip, title bar, document slots as portals). It doesn't manage
other handlers or route events — that's the presenter's job. The desktop handler
is a compound document handler like any other.

What the desktop handler needs is the same as any compound handler:

- **The manifest** — the workspace manifest lists open documents (content
  catalog) and their arrangement (composition tree with spatial: horizontal
  strip). The list of open documents IS the manifest's content references.
  Display dimensions are the constraints passed to the handler, same as any
  content type.
- **Navigation state** — the active space index is the document's view state
  (scroll position, current slide, active tab — every document type has
  equivalent state). The spring animation target is derived from it.

The desktop handler receives its manifest + view state, produces view tree nodes
(strip container with portals per document, title bar with clock), and the
presenter fills the portals with each document's content handler output. No
special capabilities needed.
