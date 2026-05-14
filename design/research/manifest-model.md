# Manifest Data Model

First-draft sketch of the compound document manifest, grounded in prior art
research. This is a design exploration document — it proposes a data model for
discussion, identifies open questions, and flags areas that need prototyping.

**Status:** Draft. Not settled. Read alongside `foundations.md` (compound
documents section) and `architecture.md` (OS service responsibilities).

**Prior art surveyed:** IIIF Presentation API 3.0, SMIL 3.0, W3C Web Annotation
selectors, glTF 2.0, METS, EPUB OPF, USD, MPEG-DASH, OPC, OAI-ORE, DIDL.
Historical systems: OpenDoc, OLE/COM, KParts, Bonobo, Xerox Star, Oberon,
Taligent, NeXTSTEP (RTFD), Phantom OS, BeOS/BFS, Cairo/WinFS. Academic: ODA (ISO
8613), HyTime (ISO 10744), W3C CDF, Peritext, Automerge, Cambria. Modern:
Notion, Figma, Jupyter, Block Protocol.

---

## Design Constraints (from foundations.md)

These are settled and non-negotiable. The manifest model must satisfy all of
them.

1. **Uniform model.** A single-content document and a hundred-content document
   are both manifests with references. No separate "simple" vs "compound"
   concept.
2. **Three composable axes.** Spatial, temporal, logical — orthogonal and
   independent. A document uses whichever are relevant.
3. **Copy semantics for embedding.** Embedding creates an independent copy. COW
   shares blocks until divergence. Provenance stored as metadata.
4. **Sole-writer architecture.** The OS service is the only writer. No multi-
   writer coordination needed.
5. **Manifest IS document identity.** Creating a manifest creates a document.
   The metadata query system indexes manifests.
6. **Content files referenced by FileId.** No paths. Content files are real
   files in the filesystem, accessed through documents.
7. **Static and virtual manifests.** Same interface, different backing. Virtual
   manifests are generated on demand.
8. **OS always renders (pure function of file state).** Manifest + content files
   → deterministic visual output.
9. **Layout engine interprets the manifest.** The manifest is a declaration; the
   layout engine resolves it into positioned elements.
10. **Internal format, no external interop.** The manifest format is
    read/written only by the OS service. Debuggability from tooling, not format
    readability.

---

## The Data Model

A manifest has four sections: **identity**, **content catalog**, **composition
tree**, and **structures**. The first three are required. Structures are
optional.

### Identity

Document-level metadata. Feeds the query system.

```text
Identity {
    document_type: MediaType        -- e.g. "application/x-os-presentation"
    attributes:    Map<Key, Value>  -- title, tags, created, provenance, retention, ...
}
```

The `document_type` drives editor binding and viewer selection. It is distinct
from the media types of individual content files — a presentation's document
type is `application/x-os-presentation` even though its parts are `image/png`
and `text/plain`.

`attributes` are the same key-value pairs already in the store catalog
(`CatalogEntry.attributes`). The manifest identity section IS the catalog entry
for this document. No duplication — the catalog indexes manifests, and the
manifest's identity section is what gets indexed.

**Relationship to existing code:** `CatalogEntry { media_type, attributes }`
maps directly to this identity section. `Store::create()` currently takes a
media type string; it would also take (or generate) a manifest.

### Content Catalog

Flat list of content references. Every content file this document uses.

```text
ContentRef {
    slot:       u16         -- index into this array (0, 1, 2, ...)
    file_id:    FileId      -- reference to content file in the store
    media_type: MediaType   -- content file's media type
    role:       Role        -- semantic role within this document (see below)
}
```

**Roles** describe what a content file does in this document, not what it is.
The same `image/png` file might have role `Background` in one document and role
`Figure` in another. Roles are a closed set per document type:

| Document Type  | Roles                                        |
| -------------- | -------------------------------------------- |
| Simple (any)   | Body                                         |
| Presentation   | Background, Title, Body, Figure, Note        |
| Article        | Body, Figure, Caption, Aside, Header, Footer |
| Audio album    | Track, CoverArt                              |
| Video project  | VideoClip, AudioClip, Subtitle, Overlay      |
| Source project | Source, Config, Asset, Readme                |

Roles are hints to the layout engine — they affect default placement, styling,
and accessibility semantics. A `Figure` in a flow layout gets figure/caption
treatment. A `Background` in a fixed canvas fills the canvas. Roles do not
affect the edit protocol or content file storage.

**Why a flat catalog, not inline references?** Following glTF's pattern: the
composition tree references content by `slot` index, not by embedding content
descriptions at every use site. Benefits:

- Same content used in multiple tree nodes without duplication
- Content catalog can be scanned independently of tree structure (useful for
  indexing, validation, export)
- Tree serialization is more compact (indices instead of full references)

**Relationship to existing code:** Each `ContentRef.file_id` is an existing
`FileId` in the store. The store already tracks media types per file. The
manifest's content catalog is a _document-scoped_ view of files the document
uses.

### Composition Tree

The structural heart of the manifest. A tree of nodes describing how content is
arranged. Serialized as a flat array with parent/child relationships via indices
(following glTF's pattern).

```text
Node {
    slot:     Option<u16>       -- content catalog index (leaf nodes have content)
    children: Vec<u16>          -- indices of child nodes in this array
    spatial:  Option<Spatial>   -- spatial axis annotation
    temporal: Option<Temporal>  -- temporal axis annotation
    label:    Option<String>    -- accessibility / logical label
}
```

**Spatial annotations** — how this node is positioned relative to siblings:

```text
Spatial {
    -- For FixedCanvas layouts:
    rect:     Option<Rect>      -- position and size in points (Mpt)

    -- For Flow layouts:
    flow:     Option<FlowHint>  -- Float(Left|Right), Inline, Block, Break

    -- For Grid layouts:
    cell:     Option<GridCell>  -- row, col, row_span, col_span

    -- For Freeform layouts:
    position: Option<Point>     -- position in points (Mpt), no constraints
}
```

Only one variant is populated, determined by the document's spatial axis
declaration. The layout engine ignores annotations that don't match the active
spatial mode.

**Temporal annotations** — when this node is active relative to siblings. This
is where SMIL's model applies:

```text
Temporal {
    mode:     TemporalMode       -- how children relate in time
    begin:    Option<TimeSpec>   -- when this node activates
    end:      Option<TimeSpec>   -- when this node deactivates
    duration: Option<Duration>   -- explicit duration (overrides intrinsic)
}

TemporalMode {
    Simultaneous    -- all children active at once (SMIL <par>)
    Sequential      -- children active one after another (SMIL <seq>)
    Exclusive       -- only one child active at a time (SMIL <excl>)
    Atom            -- leaf: this node is a single temporal unit
}

TimeSpec {
    Absolute(Duration)              -- offset from document start
    Relative(u16, Anchor, Offset)   -- relative to another node
                                    --   node index, Begin|End, ±offset
    Event(EventRef)                 -- triggered by interaction (future)
}
```

SMIL's `par`/`seq`/`excl` map directly to `Simultaneous`/`Sequential`/
`Exclusive`. The `Relative` time spec enables SMIL-style sync-arcs: "this starts
500ms after node 3 ends."

**Tree structure encodes axis semantics.** The tree is not just containment — it
IS the composition structure:

```text
Presentation (Sequential over slides):
  root [temporal: Sequential]
  ├── slide-1 [temporal: Simultaneous, spatial: FixedCanvas(1920×1080)]
  │   ├── bg    [slot: 0, spatial: rect(0,0,1920,1080)]
  │   ├── title [slot: 1, spatial: rect(100,80,1720,200)]
  │   └── chart [slot: 2, spatial: rect(200,400,1520,600)]
  └── slide-2 [temporal: Simultaneous, spatial: FixedCanvas(1920×1080)]
      ├── bg    [slot: 3, spatial: rect(0,0,1920,1080)]
      └── body  [slot: 4, spatial: rect(100,80,1720,920)]

Article (Flow, no temporal):
  root [spatial: Flow]
  ├── heading  [slot: 0]
  ├── para-1   [slot: 1]
  ├── photo    [slot: 2, spatial: flow(Float Right)]
  ├── para-2   [slot: 3]
  └── para-3   [slot: 4]

Video editing timeline (Timed, multi-track):
  root [temporal: Simultaneous]
  ├── video-track [temporal: Sequential, label: "Video"]
  │   ├── clip-a [slot: 0, temporal: begin(0ms), duration(5400ms)]
  │   └── clip-b [slot: 1, temporal: begin(5400ms), duration(6600ms)]
  ├── audio-track [temporal: Sequential, label: "Audio"]
  │   └── music  [slot: 2, temporal: begin(0ms), duration(12000ms)]
  └── subtitle-track [temporal: Sequential, label: "Subtitles"]
      ├── sub-1  [slot: 3, temporal: begin(1000ms), duration(3000ms)]
      └── sub-2  [slot: 4, temporal: begin(5000ms), duration(4000ms)]
```

**The root node declares the document's active axes** by which annotations it
carries. A root with `temporal: Sequential` and children with
`spatial: FixedCanvas(...)` is a presentation. A root with only `spatial: Flow`
is an article. A root with `temporal: Simultaneous` and children carrying
`temporal: Sequential` is a multi-track timeline. The layout engine reads the
root to determine which axis handlers to activate.

**Simple documents** have a single-node tree: one root with one content slot, no
children, no axis annotations. The uniform model means there's no special case —
the layout engine sees a one-node tree and renders the content directly.

### Structures (Optional)

Independent organizational views over the same content, following IIIF's Range
model and METS's parallel structMaps.

```text
Structure {
    purpose: StructurePurpose   -- TableOfContents, Navigation, Accessibility, ...
    entries: Vec<StructureEntry>
}

StructureEntry {
    label:    String               -- human-readable label
    target:   u16                  -- index into composition tree
    children: Vec<StructureEntry>  -- nested structure
}
```

A presentation might have a composition tree (the slides in order) AND a
structure for "table of contents" (mapping section names to slides) AND a
structure for "speaker notes" (mapping notes to slides). These are independent
of each other and of the composition tree.

Most documents won't have structures. A text file or image has a one-node
composition tree and no structures. Structures are the mechanism for documents
complex enough to need multiple navigational views.

---

## Content Selectors

When a manifest references part of a content file (cropping an image, using a
time range of audio), the content catalog entry includes a selector. This
follows the W3C Web Annotation selector vocabulary:

```text
Selector {
    Fragment(String)            -- media type fragment syntax
                                --   "xywh=100,100,300,200" (image crop)
                                --   "t=30,60" (audio/video time range)
    TextQuote {                 -- text passage by context
        prefix:  Option<String>
        exact:   String
        suffix:  Option<String>
    }
    ByteRange(u64, u64)         -- raw byte range (start, end)
}
```

A `ContentRef` with a selector means "this slot references this part of this
file." The content file itself is unchanged — the selector is a non-destructive
view.

**When to use selectors vs. separate content files:** Selectors are for
references into content that exists independently (cropping an image that's also
used uncropped elsewhere, citing a passage from a text). If the content was
created for this document and only this document, it should be a separate
content file. The copy semantics decision (embedding creates an independent
copy) applies at the file level; selectors apply within a file.

---

## Document Type Profiles

Each document type declares which axes are active and what spatial/temporal
modes are valid. These are compile-time-known profiles, not runtime-discovered:

```text
Profile: Presentation
    spatial:  FixedCanvas
    temporal: Sequential
    logical:  Flat
    roles:    [Background, Title, Body, Figure, Note]

Profile: Article
    spatial:  Flow
    temporal: None
    logical:  Sequential
    roles:    [Body, Figure, Caption, Aside, Header, Footer]

Profile: VideoProject
    spatial:  Canvas2D
    temporal: Timed
    logical:  Grouped
    roles:    [VideoClip, AudioClip, Subtitle, Overlay]

Profile: AudioAlbum
    spatial:  None
    temporal: Sequential
    logical:  Flat
    roles:    [Track, CoverArt]

Profile: SourceProject
    spatial:  None
    temporal: None
    logical:  Hierarchical
    roles:    [Source, Config, Asset, Readme]

Profile: PhotoAlbum
    spatial:  None
    temporal: None
    logical:  Flat | Grouped
    roles:    [Photo, CoverPhoto]

Profile: Simple
    spatial:  None
    temporal: None
    logical:  None
    roles:    [Body]
```

A profile constrains what the layout engine expects and what annotations are
valid. The manifest's `document_type` selects the profile. Translators (import
.pptx → manifest) generate manifests that conform to the appropriate profile.

**Profiles are OS-level knowledge, not manifest data.** The manifest doesn't
carry the profile definition — it carries a `document_type` that the OS service
maps to a profile. This keeps manifests small and avoids embedding redundant
schema information in every document.

---

## Serialization

The manifest is an internal, sole-writer format. No external interop. The design
priorities are:

1. **Fast reads.** The OS service reads manifests on every document open and
   potentially on every render cycle (for virtual manifests). Read performance
   dominates.
2. **Compact.** Manifests are files in the COW filesystem. Smaller manifests
   mean fewer blocks, faster snapshots, less storage.
3. **Crash-safe.** The sole-writer model + COW filesystem provide crash safety
   at the filesystem level. The manifest format itself doesn't need internal
   crash recovery.
4. **Evolvable.** The format will change as the design evolves. A version
   field + backwards-compatible extensions.

**Proposed approach: custom binary, inspired by the existing catalog format.**

The existing catalog serialization (`serialize.rs`) uses a straightforward
binary encoding: magic + count + entries with length-prefixed strings. The
manifest extends this pattern:

```text
Header:
    [magic: u32]  [version: u16]  [flags: u16]

Identity:
    [document_type_len: u16]  [document_type bytes]
    [attr_count: u16]  per attr: [key_len: u16] [key] [val_len: u16] [val]

Content catalog:
    [content_count: u16]
    per content:
        [file_id: u64]  [media_type_len: u16]  [media_type bytes]
        [role: u8]  [selector_type: u8]  [selector data if present]

Composition tree (flat array):
    [node_count: u16]
    per node:
        [slot: i16]  (-1 = no content)
        [child_count: u16]  [child indices: u16 × child_count]
        [spatial_type: u8]  [spatial data if present]
        [temporal_type: u8]  [temporal data if present]
        [label_len: u16]  [label bytes]  (0 = no label)

Structures (optional, present if flags bit 0 set):
    [structure_count: u16]
    per structure:
        [purpose: u8]  [entry_count: u16]
        per entry: [label_len: u16] [label] [target: u16]
                   [child_count: u16] [children recursively]
```

This is compact, zero-allocation-parseable (can read in place from a memory
mapping), and follows the same patterns as the existing catalog serializer.

**Alternative considered: CBOR (RFC 8949).** Standardized binary JSON. More
self-describing, slightly larger, widely implemented. The advantage is that
debugging tools exist (cbor.me, cbor-diag). The disadvantage is that it requires
a CBOR library (or writing one — we're `no_std`). The custom format is simpler
for `no_std` and follows established patterns in the codebase.

**Decision: defer.** The data model matters more than the serialization. Start
with the custom binary format (consistent with existing code), add a
pretty-printer tool for debugging. If CBOR or another format proves better
during prototyping, the serialization is a leaf node behind the Store API —
swappable without changing anything above.

---

## Relationship to Existing Code

### Store library (`user/libraries/store/`)

The store currently models files as flat entries:
`FileId → CatalogEntry { media_type, attributes }`. Manifests extend this:

- **CatalogEntry becomes the manifest identity section.** Same fields, same
  serialization, same query behavior. The catalog remains the index over all
  documents.
- **Manifest content is stored as file data.** A manifest is a file whose
  content (read via `Store::read()`) is the serialized composition tree +
  content catalog + structures. The identity section lives in the catalog, not
  in the file content. This avoids duplicating metadata.
- **Content files are regular files.** A content file referenced by a manifest
  is just another file in the store, with its own `CatalogEntry`. The manifest
  references it by `FileId`.
- **`Store::create()` gains a variant** that creates a document (manifest file +
  catalog entry) and optionally creates/associates content files.

### Document service (`user/servers/document/`)

The document service wraps the store with IPC. It would add:

- `OPEN_DOCUMENT`: read manifest, return composition tree to OS service
- `CREATE_DOCUMENT`: create manifest + content files atomically
- `MODIFY_LAYOUT`: update composition tree (spatial/temporal annotations)
- `ADD_CONTENT`: add a content file to a document's catalog
- `REMOVE_CONTENT`: remove a content file (and any tree nodes referencing it)

The edit protocol (`beginOperation`/`endOperation`) applies to content files,
not manifests. Editing text within a presentation modifies the text content
file; the manifest (which slide the text is on, its position) is modified by
layout operations, not content editing operations.

### OS service (layout engine)

The OS service reads the manifest and drives layout:

1. Read manifest → get composition tree + content catalog
2. For each node, load content file data (via store read + memory mapping)
3. Walk the tree, applying spatial/temporal layout per the active profile
4. Compile positioned elements into scene graph nodes
5. Write scene graph to shared memory for compositor

The composition tree is the layout engine's input. The scene graph is its
output. The manifest is the bridge between document semantics and visual
presentation.

---

## Open Questions

### 1. Compound document editing (the hard problem)

How do content-type editors bind to parts within a compound document? Three
approaches identified in prior art research:

**(A) Star model:** The OS provides editing modes per content type. Clicking on
text in a presentation activates text editing; clicking on an image activates
image editing. No separate "editors" — it's all OS-provided. _Pro:_ Simple,
consistent. _Con:_ Limits third-party editor innovation.

**(B) Nested editors (OpenDoc model):** Content-type editors activate within the
compound document context. The presentation is the active document; clicking on
a text region activates a text editor for that region. _Pro:_ Reuses content-
type editors everywhere. _Con:_ The select-vs-activate UX problem (Apple's
OpenDoc user studies found this devastating).

**(C) Edit-in-context with explicit activation:** Extends B with the "view is
default, edit is deliberate" principle. Clicking always selects (view mode). A
deliberate gesture (double-click, Enter, or explicit "edit" action) activates
the content-type editor for that part. _Pro:_ Eliminates select-vs-activate
ambiguity. _Con:_ Extra gesture to start editing.

**Current leaning:** Option C. It maps to the KParts ReadOnly/ReadWrite split
and aligns with foundations.md: "viewing is the default; editing is a deliberate
second step." The manifest model supports this — each node has a content slot
with a media type, which determines which editor can activate for it.

**What needs prototyping:** The UX of editing a text block inside a
presentation. Does double-click feel natural? How does the editor's chrome
(toolbar, cursor) integrate with the compound document's chrome? How does
focus/escape work to exit the sub-editor?

### 2. Manifest format for simple documents

A plain text file becomes a manifest with a one-node composition tree pointing
at one content file. This is correct but potentially wasteful — every simple
document gets a manifest file in addition to its content file.

**Options:**

**(A) Every document has a manifest file.** Uniform. Simple. Costs one extra
file per simple document. At 100K documents, that's 100K extra small files. The
COW filesystem handles small files efficiently (inline in inode for files under
one block).

**(B) Implicit manifests for simple documents.** A file in the store with no
explicit manifest is treated as a single-content document. The manifest is
virtual — generated on demand from the catalog entry. Only compound documents
get real manifest files.

**(C) Manifest embedded in catalog entry.** For simple documents, the catalog
entry IS the manifest (identity + implicit single-content tree). For compound
documents, the catalog entry points to a manifest file.

**Current leaning:** Option A. Uniformity is worth the cost. The existing store
already inlines small files in inode blocks, so a manifest for a simple document
costs near-zero extra storage. Option B introduces a special case that
complicates every code path that reads manifests.

**What needs measuring:** Actual overhead of option A at 10K and 100K documents.
If manifest files average 64 bytes (identity + one content ref + one tree node),
that's 6.4 MB at 100K — well within budget.

### 3. Content file lifecycle

When a content file is removed from a manifest, what happens to the file itself?

**(A) Reference counting.** Content files track how many manifests reference
them. Unreferenced files are deleted (or marked for retention-based cleanup).

**(B) Sole ownership.** Each content file belongs to exactly one manifest. Copy
semantics means embedding creates a new file. Removing a content ref deletes the
file.

**(C) Garbage collection.** Periodically scan all manifests, identify
unreferenced content files, clean them up.

**Current leaning:** Option B. Copy semantics already mean each embedding is
independent. A content file belongs to the manifest that created it. Deletion is
immediate and predictable. The one exception: a content file shared between a
manifest and a virtual manifest (e.g., an inbox message is both a standalone
document and part of the inbox). This needs the reference-counting mechanism
from option A, but only for this case.

**What needs design:** The interaction between sole ownership and virtual
manifests. Virtual manifests reference content by query, not by explicit file
list. The content is "owned" by the standalone document, not the virtual
manifest. This might just work — virtual manifests don't own content, they view
it.

### 4. Axis interactions

The composition tree handles axes independently per node. But some documents
need cross-axis interactions:

- **Animation:** spatial position varies over the temporal axis. A text element
  fades in = its opacity changes over a time range.
- **Conditional visibility:** logical structure drives spatial presence. A
  collapsible section hides its children when collapsed.
- **Responsive layout:** spatial arrangement changes based on available space. A
  two-column article becomes single-column on narrow displays.

The current model can represent these with additional annotations:

```text
-- Animation: spatial annotation with temporal keyframes
SpatialAnimated {
    keyframes: Vec<(Duration, Spatial)>
    interpolation: Linear | EaseInOut | Step
}

-- Conditional visibility: node-level flag
visibility: Always | WhenExpanded | WhenState(StateRef)
```

**What needs design:** Whether these are first-class manifest primitives or
extensions handled by content-type-specific editors. Animations in a
presentation might be manifest-level (the OS layout engine handles them).
Animations in a web page might be content-level (the HTML content file contains
CSS animations, handled by the web translator).

**Current leaning:** Keep the manifest model static (no animation primitives).
Animation is a rendering concern, not a document structure concern. The
composition tree describes the document's structure; the layout engine can
interpolate between states over time without the manifest encoding keyframes.
This keeps the manifest simple and pushes temporal rendering complexity into the
layout engine (where it belongs per architecture.md's principle: the OS service
does all the thinking).

### 5. Lenses vs. translators

The current design uses translators (one-way import/export). Lenses
(bidirectional views into foreign formats) would eliminate the import step for
supported formats.

**What a lens looks like in the manifest model:** A lens-backed document has a
manifest whose content catalog references the original file (e.g., the .pptx)
with a lens identifier instead of individual extracted content files. The OS
service invokes the lens to project the foreign format into a composition tree
on demand.

```text
ContentRef {
    slot:       0
    file_id:    FileId(original-pptx)
    media_type: "application/vnd...presentationml.presentation"
    role:       LensSource
    lens:       "pptx-v1"
}
```

The lens provides: (a) a read path that produces a composition tree from the
foreign file, and (b) a write path that translates layout/content modifications
back into the foreign format.

**What needs prototyping:** Whether bidirectional translation is feasible for
real-world formats. PPTX → manifest is straightforward (decompose the ZIP,
extract content, build tree). Manifest → PPTX with no information loss is much
harder — PPTX has hundreds of features (transitions, animations, SmartArt,
embedded macros) that the manifest model can't represent. A lens that loses
information on the write path is worse than an explicit import/export workflow
where the user understands the conversion boundary.

**Current leaning:** Start with translators. Add lenses for specific formats
where round-trip fidelity is achievable (simpler formats like Markdown, CSV,
plain image formats). Complex formats (PPTX, DOCX) are better served by explicit
import/export with a clear "this is now an OS-native document" boundary.

### 6. Manifest evolution and versioning

The manifest format will change. How do old manifests stay readable?

**Proposed approach:** Version field in the header. The OS service supports
reading all historical versions and always writes the current version. Opening
an old-version manifest reads it with the old parser, then re-saves in the
current format. This is automatic, transparent, and one-directional (no need to
write old formats).

COW snapshots preserve the original bytes, so reverting to a historical snapshot
restores the old-format manifest — the old-version reader handles it.

**What needs design:** The boundary between "version bump" (backwards-compatible
extension: new optional fields) and "format change" (incompatible: restructured
data). A flags field in the header enables optional extensions without version
bumps. Major restructuring requires a version bump with a migration path.

### 7. Accessibility

The manifest tree carries structural information that accessibility interfaces
need: logical hierarchy, labels, roles, reading order. The composition tree's
node labels and the structures section (table of contents, navigation) feed the
accessibility layer directly.

**What needs design:** Whether the manifest carries enough semantic information
for screen reader navigation, or whether additional accessibility annotations
are needed. The `label` field on tree nodes and the `role` on content refs are a
start. Full accessibility may need:

- Language declarations per node (for multilingual documents)
- Alt text for non-text content (stored as a node attribute, not content)
- Reading order that differs from visual order (a structure with purpose =
  ReadingOrder)
- Heading levels (implied by structure depth, or explicit annotation?)

This deserves its own design document once the manifest model stabilizes.

---

## What to Try First

If prototyping the manifest model, the suggested order:

1. **Extend the store with manifest file support.** Add a `create_document`
   method that creates a manifest file + content file(s). Read/write the binary
   format. Verify round-trip serialization. This is pure library work, no OS
   service changes needed.

2. **Build a simple document (one content file).** Verify that the uniform model
   works: a text file is a manifest with one tree node pointing at one content
   file. Open it, read the manifest, render the content. This proves the model
   works for the base case without adding complexity.

3. **Build a presentation (multiple content files, fixed canvas + sequential).**
   Import a few images + text blocks. Create a manifest with slides. Verify the
   layout engine can walk the composition tree and produce a scene graph with
   positioned content per slide. This proves the multi-axis model works.

4. **Build a flow article (text + images, flow layout).** Verify text reflow
   around images. This exercises the spatial axis in a different mode and tests
   the flow hint annotations.

5. **Prototype compound editing.** Open a presentation, double-click a text
   block, verify the text editor activates for that content region. This is the
   hardest UX problem and should be tested early.

---

## Key Insights from Prior Art

These informed specific design choices above and are recorded here for
reference.

**IIIF's Canvas model (spatial axis).** A Canvas is a virtual coordinate space
that content is placed onto, not a container that holds content. This separation
means the same content can appear on multiple canvases at different positions.
Informed the decision to separate content catalog from composition tree —
content refs are in the catalog, positioning is in the tree.

**SMIL's par/seq/excl (temporal axis).** Temporal composition is tree-
structured, not flat. Nesting `par` inside `seq` inside `par` enables complex
temporal arrangements (a presentation slide with build animations is
`seq(par(simultaneous content), par(simultaneous content))`). Informed the
`TemporalMode` enum directly.

**Web Annotation selectors (content targeting).** A standardized vocabulary for
"this part of this resource." Fragment selectors (`xywh=`, `t=`), text quote
selectors, byte range selectors. Informed the `Selector` type for partial
content references.

**glTF's flat-array-with-indices (serialization).** Instead of deeply nested
trees, use flat arrays of objects that reference each other by index. More
compact, faster to parse, allows sharing. Informed the composition tree's
serialization as a flat node array with child indices.

**METS/IIIF parallel structures (multiple views).** The same content can have
multiple independent organizational hierarchies (physical order, logical
chapters, navigation landmarks). Informed the optional Structures section.

**OpenDoc's select-vs-activate failure (compound editing UX).** Apple's user
studies found that users could not form a mental model of when clicking selected
vs. activated a part. "Even the most adept users had a menu of strategies to
randomly try." Informed the leaning toward option C (explicit activation
gesture) for compound editing.

**KParts' ReadOnly/ReadWrite split.** The most successful compound component
framework survived by making view the default and edit the exception. The
simplest part to write was a read-only viewer. This mirrors "view is default,
edit is deliberate" and suggests that the manifest model should make read-only
rendering trivially easy — every node can be viewed; editing is an optional
capability per content type.

**NeXTSTEP's RTFD (filesystem as container).** A compound document is a
directory containing an RTF stream + attachments. The filesystem itself is the
container format. In this OS, the store is the container — manifests reference
content files by FileId, and the store handles atomic multi-file operations via
COW commit. Same architectural insight, different mechanism.

**BFS attributes (lightweight metadata).** BeOS proved that typed key-value
attributes on files, indexed with B+ trees and queryable, achieve 80% of a
relational database's value at 1% of the complexity. The existing store catalog
follows this pattern exactly.

**The OS-as-mediator insight.** Every compound document system that let
components negotiate directly with each other (OLE, OpenDoc, Bonobo) collapsed
under N² complexity. Systems where a central authority mediated (KParts via
XMLGUI, the OS service in this design) survived. The manifest model reinforces
this: the composition tree is data that the OS service interprets. Components
(editors) never see or modify the tree directly.
