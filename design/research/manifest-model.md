# Manifest Data Model

Concrete schema for the compound document manifest. This supersedes the previous
draft after a design session that simplified the model from four sections with
three independent axes to a flat, recursively-composed manifest with two
orthogonal layout properties.

**Status:** Design complete. Not yet implemented in code. The axes × positioning
model, URI content references, one-level-deep rule, and navigation-as-viewer
decisions are reflected in `foundations.md` (compound documents section) and
`decisions.md` (Decisions #14 and #15). This document is the detailed schema
reference.

---

## Design Principles

1. **One level deep.** A manifest declares a layout with a flat list of
   children. Children are content references (URIs). If a child needs internal
   structure, it is a subdocument with its own manifest. Depth comes from
   subdocument nesting, not inline group definitions.

2. **Uniform shape.** Every manifest has the same structure: metadata + children
   list + optional layout. A simple document is a manifest with one child and no
   layout. A compound document has multiple children and a layout. The
   simple/compound distinction is a property of the content, not a type in the
   schema.

3. **Axes × positioning.** Layout is decomposed into two orthogonal properties:
   which display axes the layout operates on, and how children are positioned
   along them. Four display axes (width, height, depth, time) form a closed set
   — content-domain concepts (frequency, pitch, price) are mapped to display
   axes by viewers, not declared in the manifest.

4. **URI content references.** All content is addressed by URI. `store:` scheme
   for local document store files, standard schemes (https, rtsp, file) for
   external resources. One reference type, scheme-routed resolution.

5. **Navigation is a viewer concern.** The manifest describes structure and
   spatial layout. Whether to page through children (slides), scroll them, or
   show all at once is the viewer's decision. The manifest does not declare
   temporal modes.

6. **Edge data on children.** Each child carries placement (where it sits in the
   parent's layout) and an optional viewport (what region of the child's content
   to show). Placement is positioning-mode-dependent. Viewport is universal.

---

## Schema

### Manifest

Every manifest has the same shape: metadata, an optional layout, and a children
list.

```text
Manifest {
    -- Metadata (feeds the query system)
    title:       Option<String>
    tags:        Vec<String>
    provenance:  Option<URI>          -- original source (Decision #14 copy semantics)
    attributes:  Map<String, String>  -- open key-value pairs

    -- Layout (absent for simple documents)
    axes:         Vec<Axis>                -- display axes: width | height | depth | time
    positioning:  Option<Positioning>      -- flow | grid | absolute
    properties:   Option<LayoutProperties> -- per-mode (see below)

    -- Children (exactly one for simple documents, many for compound)
    children:     Vec<Child>
}
```

A simple document (one child, no layout):

```text
{ title: "Meeting Notes", children: [{ ref: "store:12" }] }
```

A compound document (multiple children with layout):

```text
{
    title: "Trip Report",
    tags: ["travel"],
    axes: [width, height],
    positioning: flow,
    children: [
        { ref: "store:12" },
        { ref: "store:14" },
        { ref: "store:15" }
    ]
}
```

Simple vs compound is determined at runtime: `axes.is_empty()` and
`children.len() == 1` means the viewer dispatches directly to the child's
content-type viewer, skipping compound layout. This is an optimization, not a
type distinction.

### Display Axes

Four axes describe the user's perceptual space. Closed set — content-domain
concepts are mapped to display axes by viewers, not declared in the manifest.

| Axis     | Orientation | Unit | Examples                          |
| -------- | ----------- | ---- | --------------------------------- |
| `width`  | horizontal  | Mpt  | text columns, image width         |
| `height` | vertical    | Mpt  | text lines, track lanes           |
| `depth`  | into screen | Mpt  | 3D z-axis, parallax layers        |
| `time`   | temporal    | ms   | playback position, clip start/end |

Common combinations:

```text
[width, height]         -- article, canvas, photo grid
[width]                 -- horizontal list, carousel
[height]                -- vertical list, feed
[time, height]          -- video timeline, DAW
[time]                  -- playlist, audio sequence
[width, height, depth]  -- 3D scene
```

### Positioning Modes

Three modes, distinguished by who determines child positions:

| Mode       | Who determines position | Description                                                      |
| ---------- | ----------------------- | ---------------------------------------------------------------- |
| `flow`     | Layout engine           | Computed from content sizes; children affect each other          |
| `grid`     | Container               | Container divides space into regular regions; children fill them |
| `absolute` | Children                | Each child carries explicit coordinates                          |

### Layout Properties

Properties on the manifest root, dependent on positioning mode. Spatial
properties use Mpt; temporal properties use ms.

**Flow properties:**

```text
wrap:     bool                                     -- wrap when full (2+ axes only, default: true)
align:    start | center | end                     -- cross-axis alignment
justify:  start | center | end | between | around  -- main-axis distribution
gap:      per-axis                                 -- spacing between children
```

The primary flow axis is the first declared axis — no separate direction
property. `axes: [width, height]` flows horizontally, wraps vertically.
`axes: [height, width]` flows vertically, wraps horizontally. `axes: [height]`
stacks vertically with no wrapping. Axis order is direction.

**Grid properties:**

```text
divisions:  per-axis  -- number of divisions per axis (e.g. columns for width)
gap:        per-axis  -- spacing between cells
```

For `axes: [width, height]` with `divisions: { width: 3 }`, children auto-flow
into a 3-column grid. For `axes: [width, height]` with
`divisions: { width: 3, height: 2 }`, a fixed 3×2 grid.

**Absolute properties:**

```text
bounds:     per-axis (optional)  -- axis length (None per axis = unbounded)
viewport:   Option<Viewport>     -- initial view (for unbounded or large spaces)
```

An absolute layout with bounds on all axes is a bounded canvas (presentation
slide). Without bounds, it is an unbounded surface (whiteboard). Viewport sets
the initial view region.

```text
Viewport {
    center:  per-axis     -- center position per declared axis
    zoom:    f32          -- 1.0 = 100%
    fov:     Option<f32>  -- 3D only, degrees
}
```

### Child

A child is a content reference with optional edge data.

```text
Child {
    ref:  URI  -- content reference (store: or external)

    -- Placement (positioning-mode-dependent)
    placement:  Option<Placement>

    -- Viewport into child content (universal, optional)
    viewport:  Option<ChildViewport>
}
```

**Placement** — where this child sits in the parent's layout. Fields are
per-axis, keyed by the parent's declared axes:

```text
Placement {
    -- For absolute positioning:
    position:   per-axis (optional)  -- coordinate per axis (Mpt or ms)
    size:       per-axis (optional)  -- extent per axis (None = intrinsic)

    -- For grid positioning:
    cell:       per-axis (optional)  -- grid cell index per axis (None = auto-place)
    span:       per-axis (optional)  -- span multiple cells (default: 1)

    -- For flow positioning:
    -- (typically none — children flow in order)
}
```

Examples for a `[width, height]` absolute layout:

```text
placement: { position: { width: 100, height: 80 },
             size: { width: 800, height: 120 } }
```

For a `[time, height]` absolute layout (video timeline):

```text
placement: { position: { time: 5400, height: 2 },
             size: { time: 3000 } }
```

For a `[width, height]` grid with 3 columns:

```text
placement: { cell: { width: 0, height: 1 },
             span: { width: 2 } }
```

**ChildViewport** — what region of the child to show:

```text
ChildViewport {
    offset:  per-axis  -- crop/pan offset into child content (Mpt or ms)
    zoom:    f32       -- zoom level (1.0 = natural size)
}
```

This is compositional data: "show this region of the child in this context." The
child's content is unchanged — the viewport describes how to frame it. The same
image can appear in two documents with different crops without duplicating
content (COW handles physical sharing).

---

## Examples

### Plain text file

```text
{ title: "Meeting Notes", children: [{ ref: "store:12" }] }
```

One child, no layout. The text viewer handles internal flow.

### Article with inline image

```text
{
    title: "Trip Report",
    tags: ["travel"],
    axes: [width, height],
    positioning: flow,
    children: [
        { ref: "store:12" },
        { ref: "store:14" },
        { ref: "store:15" }
    ]
}
```

Three children in 2D flow: text, image, text. The layout engine stacks them as
blocks, reflowing if the viewport width changes.

### Slide deck

The deck is a 1D grid of slide subdocuments:

```text
{
    title: "Q3 Review",
    tags: ["work"],
    axes: [height],
    positioning: grid,
    children: [
        { ref: "store:20" },
        { ref: "store:21" },
        { ref: "store:22" }
    ]
}
```

Each slide is its own manifest — a bounded 2D absolute layout:

```text
{
    title: "Title Slide",
    axes: [width, height],
    positioning: absolute,
    bounds: { width: 1920, height: 1080 },
    children: [
        { ref: "store:30",
          placement: { position: { width: 100, height: 80 },
                       size: { width: 800, height: 120 } } },
        { ref: "store:31",
          placement: { position: { width: 200, height: 300 },
                       size: { width: 600, height: 400 } },
          viewport: { width: 500, height: 200, zoom: 1.5 } }
    ]
}
```

The second child has a viewport — it shows a cropped/zoomed region. A
presentation viewer shows slides one at a time. A thumbnail viewer shows all in
a grid. Same manifests, different viewers.

### Photo album

```text
{
    title: "Vacation 2026",
    tags: ["photos", "travel"],
    axes: [width, height],
    positioning: grid,
    divisions: { width: 3 },
    gap: { width: 8, height: 8 },
    children: [
        { ref: "store:40" },
        { ref: "store:41" },
        { ref: "store:42" },
        { ref: "store:43" },
        { ref: "store:44" },
        { ref: "store:45" }
    ]
}
```

Six images in a 3-column 2D grid. Auto-placed.

### Freeform whiteboard

```text
{
    title: "Architecture Sketch",
    tags: ["design"],
    axes: [width, height],
    positioning: absolute,
    viewport: { center: { width: 400, height: 300 }, zoom: 1.0 },
    children: [
        { ref: "store:50",
          placement: { position: { width: 100, height: 200 } } },
        { ref: "store:51",
          placement: { position: { width: 500, height: 150 },
                       size: { width: 300, height: 200 } } },
        { ref: "store:52",
          placement: { position: { width: 900, height: 400 } } }
    ]
}
```

Unbounded 2D surface (no bounds on root). Viewport sets the initial view.

### Video editing timeline

```text
{
    title: "Vacation Edit",
    axes: [time, height],
    positioning: absolute,
    bounds: { time: 120000 },
    children: [
        { ref: "store:90",
          placement: { position: { time: 0, height: 0 },
                       size: { time: 5400 } } },
        { ref: "store:91",
          placement: { position: { time: 5400, height: 0 },
                       size: { time: 6600 } } },
        { ref: "store:92",
          placement: { position: { time: 0, height: 1 },
                       size: { time: 12000 } } }
    ]
}
```

Two video clips on track 0, one audio clip on track 1. Time axis in ms, height
axis for track lanes. The viewer handles playback and scrubbing.

### Nested compound document

An article that embeds a slide deck:

```text
{
    title: "Conference Summary",
    tags: ["work"],
    axes: [width, height],
    positioning: flow,
    children: [
        { ref: "store:60" },
        { ref: "store:61" },
        { ref: "store:62" }
    ]
}
```

`store:61` is a slide deck with its own manifest. The system looks up its media
type, finds the presentation viewer, and embeds it via a portal. Nesting is
natural.

### Music playlist

```text
{
    title: "Road Trip Mix",
    axes: [time],
    positioning: grid,
    children: [
        { ref: "store:70" },
        { ref: "store:71" },
        { ref: "store:72" }
    ]
}
```

1D temporal grid. A music viewer plays tracks in sequence. A list viewer shows
them as a scrollable list. Same manifest, different viewers.

### Desktop (workspace)

```text
{
    title: "Desktop",
    axes: [width],
    positioning: grid,
    children: [
        { ref: "store:12" },
        { ref: "store:14" },
        { ref: "store:80" }
    ]
}
```

The desktop IS a compound document. The workspace viewer shows one child at a
time with slide animation — same mechanism as the slide deck, different viewer.

---

## Axes × Positioning Matrix

The two layout properties span the full design space. Axes determine what
dimensions the layout operates on (and their units). Positioning determines how
children are placed. Every cell has real-world examples:

```text
                  flow              grid              absolute
             (content owns)   (container owns)     (children own)
           ┌────────────────┬──────────────────┬───────────────────┐
  spatial  │ article        │ photo album      │ canvas / slide    │
  (w,h)    │ flex layout    │ spreadsheet      │ freeform / poster │
           ├────────────────┼──────────────────┼───────────────────┤
  temporal │ (uncommon)     │ playlist         │ timeline          │
  (t,h)    │                │ slide deck       │ video editor      │
           ├────────────────┼──────────────────┼───────────────────┤
  3D       │ (uncommon)     │ voxel grid       │ 3D scene          │
  (w,h,d)  │                │                  │ spatial computing │
           └────────────────┴──────────────────┴───────────────────┘
```

New layout types are points in this space, not additions to an enum.

---

## Relationship to Existing Code

### View tree (`user/shared/view/`)

The view tree's `Display` enum maps to the axes × positioning model:

| Display       | Axes × Positioning                   |
| ------------- | ------------------------------------ |
| `Block`       | [width, height] flow (vertical)      |
| `Inline`      | [width, height] flow (horizontal)    |
| `FixedCanvas` | [width, height] absolute (bounded)   |
| `Freeform`    | [width, height] absolute (unbounded) |

The manifest is the persistent form of what the view tree represents at runtime.
A manifest is read once at document open, the viewer builds a ViewSubtree, and
the compositor renders it. The manifest-to-view-tree transformation is one-way
(the manifest is the source of truth).

### Viewer trait (`user/shared/view/`)

Each manifest child gets its own viewer, determined by the store's media type
for that URI. The compound viewer creates child viewers, calls `rebuild()` on
each, and composes their subtrees via `ViewContent::Portal { child_idx }`. This
is exactly what `WorkspaceViewer` already does — the manifest generalizes it
from code to data.

### Store (`user/shared/store/`)

Content references use `store:` URIs, which resolve to `FileId` values in the
document store. The store's `CatalogEntry` provides the media type (viewer
selection) and attributes (queryable metadata). The manifest's metadata (title,
tags, provenance) maps to catalog attributes — the manifest identity IS the
catalog entry.

External URIs (https, rtsp, file) resolve through the appropriate subsystem. The
viewer handles availability, caching, and latency differences.

---

## Open Questions

### 1. Compound document editing

How do content-type editors bind to parts within a compound document?

**Current leaning: edit-in-context with explicit activation.** Clicking always
selects (view mode). A deliberate gesture (double-click, Enter) activates the
content-type editor for that part. This maps to Decision #6 (view is default,
edit is deliberate) and aligns with the KParts ReadOnly/ReadWrite split.

Needs prototyping: the UX of editing a text block inside a presentation.

### 2. Simple document manifests

~~Is a simple document manifest a real file or a virtual construct?~~

**Resolved: every document has a manifest file.** The manifest format is uniform
(same shape for simple and compound documents). A simple document manifest is
~20 bytes (no axes, one child). The COW filesystem inlines small files in inode
blocks, so the cost is near-zero. The leaf/branch type distinction was
considered and rejected in favor of uniform shape — see below.

### 3. Content file lifecycle

When a content file is removed from a manifest, is the file deleted?

**Current leaning: sole ownership.** Copy semantics mean each embedding is
independent. A content file belongs to the manifest that created it. Deletion is
immediate. Virtual manifests reference content they don't own — they view it.

### 4. Text splitting

Adding an image to a text document creates three children: text-before, image,
text-after. The text content is split into two files. This is structurally clean
(HTML works this way) but raises editing questions:

- Typing at the boundary between text parts — which file gets the keystroke?
- Deleting the image — do the two text parts merge back into one file?
- Undo — does undoing image insertion restore the original single text file?

The compound viewer handles these as layout operations on the manifest, not
content edits on individual files. The sole-writer architecture (the OS service
is the only writer) makes this safe — split, merge, and reorder are sequential
writes followed by a COW snapshot.

### 5. Manifest serialization format

Internal to the OS. No external interop requirement.

**Current leaning: custom binary, consistent with existing code.** The store's
catalog already uses a binary format (magic + count + length-prefixed entries).
The manifest extends this pattern. A debug pretty-printer provides human
readability. If a standard format (CBOR, FlatBuffers) proves better during
prototyping, serialization is a leaf node behind the store API — swappable
without changing anything above.

### 6. The store: URI scheme

The `store:` URI scheme needs defining:

- `store:12` — reference by FileId (current approach)
- Resolution: URI → FileId → store read

Simple, fast (integer parse), consistent with existing code. Other scheme
details (authority, path components) can be added if needed later without
changing existing references.

---

## Considered and Rejected

### Leaf/branch type distinction

The original schema had two manifest forms: a **leaf** (`ref: URI` — single
content reference, no layout) and a **branch** (`axes + positioning + children`
— layout with children). This was rejected in favor of a uniform shape where
every manifest has a `children` list and optional layout fields.

**Why rejected:** The design already says "simple vs compound is an internal
property, not a user-facing concept." A type-level split contradicts this by
making simple/compound a structural distinction in the data model. The uniform
shape means one parsing path, one validation path, and one code path in the
viewer dispatch. The performance argument (skipping compound viewer for simple
documents) is a trivial runtime check (`children.len() == 1 && axes.is_empty()`)
on the uniform type, not a reason to bifurcate the format.
