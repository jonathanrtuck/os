# OS Design Foundations

A living document capturing the high-level architecture, beliefs, and decisions
for a personal operating system project.

---

## The Core Idea

Current operating systems are app-based: **OS → App → Document.** The OS manages
apps, and documents live inside apps. To edit a text file, you open a text
editor, then open the file within it.

A document-centric OS flips this: **OS → Document → Tool.** The OS manages files
directly. Documents exist independently of any app. To edit a text file, you
open the file, then attach an editor to it.

This mirrors the physical desktop analogy. A paper on your desk exists
independently of the pen you use to write on it. You can look at (view) a
document without owning an editor. But if you want to change it, you need a
tool.

**A document is a file. A file has a mimetype. That's the whole model.** The OS
can natively view all common mimetypes. Editing requires installing an editor
that supports that mimetype. Editors are tools you bring to documents, not
containers that hold them.

### Mimetype Evolution

Documents aren't locked into a format at creation time. If you start with a
plain text file and use an image tool to add an image, the OS prompts you to
confirm changing the mimetype to a richer format. The document evolves based on
what you do with it, rather than requiring you to decide upfront what kind of
document you're making.

### The Economics of Open Formats

Open formats are essential — the OS must champion them, or app lock-in returns
through proprietary formats. But open formats don't kill the editor market. They
change what editors compete on: quality of editing experience rather than file
format captivity.

Precedents that validate this: PDF is an open standard that every OS can render,
but Adobe still sells Acrobat because the editing tools are powerful. PNG/JPG
are open — people still pay for Photoshop because the editing is worth it.
Source code is plain text — people still pay for JetBrains IDEs based on editor
quality. Smaller, specialized editor developers benefit from this model — a
niche audio editor competes on audio editing quality, not on being bundled into
a larger suite.

---

## Guiding Beliefs

1. **The OS exists to help you work with your data, not to host apps.**
   Documents and content are first-class citizens. Applications are tools you
   attach to content, not containers you put content inside.

2. **Files are real byte streams underneath, but the OS understands what kind of
   content they are** and can present them without needing a dedicated app.

3. **Viewing is the default; editing is a deliberate second step.** Opening a
   file shows it. Editing requires an explicit action — like picking up a tool.

4. **The GUI, CLI, and assistive interfaces are all equally fundamental.**
   Neither is "the real one." All talk to the same services and APIs. The CLI
   may expose more than the GUI, but nothing in the GUI is inaccessible from the
   CLI. Accessibility is not a feature bolted onto a visual interface — it is a
   parallel interface to the same system, with the same status as GUI and CLI.
   Semantic structure (headings, roles, relationships) exists in the data model,
   not as annotations added after the fact. Every content type, interaction
   primitive, and navigation mechanism must be designed with all three
   interfaces in mind from the start.

5. **Internally opinionated, externally compatible.** The OS can structure data
   and workflows however it wants internally, but at its boundaries it speaks
   standard formats (common mimetypes). Nothing is trapped. An audio file
   created here plays on macOS or Windows.

6. **Simple everywhere. Complexity is a design smell.** The system should be
   simple at every layer. Essential complexity is pushed into leaf nodes (a PNG
   decoder, a font shaper) behind simple interfaces — complex inside, simple to
   use, and the complexity doesn't leak. The connective tissue — protocols,
   APIs, relationships between components — must be simple. If a system-wide
   interface is complex, the design isn't finished. When user simplicity and
   developer simplicity conflict, users win, but the conflict itself signals the
   design needs more work. But leaf nodes must earn their complexity — total
   system complexity is conserved, not eliminated by displacement. See "The
   adaptation layer" under External Boundaries.

7. **Built to learn from, not to ship.** This is a personal project — an
   exploration of what's possible and what breaks. Decisions should favor
   clarity and interestingness over market viability.

8. **All persistent content with a media type is a file.** The litmus test: does
   it have a media type and does it persist? If yes, it belongs in the
   filesystem. If no (scene graph, IPC channels, process state), it doesn't.
   This keeps the filesystem meaningful as the persistent content store without
   forcing runtime state through it for philosophical purity. The user doesn't
   interact with files directly — the interface presents domain-appropriate
   abstractions (documents, conversations, meetings), not files and paths.

9. **File paths are metadata, not the organizing principle.** A file's path is
   just another attribute — like its creation date or size. Available when
   useful, but not how users find or organize their work.

---

## Glossary

These terms have specific meanings in this design. They are layered — each
builds on the one before it.

**File** — a byte stream at a location on disk. The storage primitive. What the
filesystem manages. Files have no inherent meaning at this layer — meaning comes
from the mimetype system above. Users do not interact with files directly; the
OS manages files as an implementation detail.

**Manifest** — the file that defines a document. Contains: references to one or
more content files, relationship descriptions (how parts relate spatially,
temporally, or logically), and metadata (title, tags, custom attributes). The
manifest IS the document's identity — creating a manifest creates a document.
Manifests are the only files the metadata query system indexes. A manifest can
be **static** (a real file on disk, content written and read back) or
**virtual** (a file in the filesystem namespace whose content is generated by
the OS service on read, like Plan 9's `/proc`). Whether a manifest is static or
virtual is an implementation detail — the document interface is the same either
way.

**Document** — an independent thing from the user's perspective. Creating,
opening, viewing, editing, or deleting a document is a single action because a
document is a single conceptual unit. Every document is backed by a manifest
that references one or more content files. Users interact with documents, never
with files or manifests directly. Whether a document has one content file or
many, and whether its manifest is static or virtual, are internal properties —
not user-facing distinctions.

**Content file** — a file containing actual content (a PNG, a text file, an
audio clip). Content files are referenced by manifests. They are real files in
the filesystem, but users access them through their documents, not directly. A
content file may be referenced by multiple documents (shared) or belong
exclusively to one (owned).

### Simple vs compound

The traditional distinction (simple = one file, compound = many) becomes an
internal property in this design, not a separate concept. A document with one
content reference and a document with twenty content references are both
manifests with references — the model is uniform. What varies is:

- How many content files the manifest references (one vs many)
- Which relationship axes are used (a text file has none; a slide deck has
  spatial + temporal + logical)
- Whether a layout engine is involved (single-content documents are rendered
  directly)

### Coordinate units

The OS uses exactly two spatial units:

**Point (pt)** — 1/72 inch. The resolution-independent coordinate unit used
everywhere above the render boundary: node positions, node sizes, font sizes,
layout constants, shadow offsets. One unit for both spatial layout and
typography. Internally represented as 1/1024 pt ("millipoints"): `Mpt = i32`
(signed positions/offsets), `Umpt = u32` (unsigned dimensions). Precision:
~0.001 pt (sub-pixel at any density). Range: ±2,097,151 pt (±2,489 A4 pages).
Conversion: bit shift `>> 10`.

**Pixel (px)** — one physical display element. Used only by the render backends
and drawing library — the final stage where points are converted to hardware
coordinates.

The scale factor (`physical_dpi / 72`) bridges them. It is derived from display
hardware (EDID), user preference, or a sensible default (96 DPI → scale ≈ 1.33).
The render library applies the scale during the scene tree walk. Core and the
scene graph never know about pixels or DPI.

Spring physics and affine transforms stay in f32 — springs because the math
(sin, exp) is natural in float, transforms because they compose via matrix
multiplication and go straight to the GPU. Conversion happens at API boundaries:
`Spring::value()` returns `Mpt`; render services convert `Mpt → f32` pixels for
GPU submission.

### Open terminology questions

- ~~**Referenced vs owned parts.**~~ **SETTLED: Copy semantics.** Embedding
  content in a compound document creates an independent copy. No reference
  tracking, no broken links, no cascading deletes. COW at the filesystem level
  shares physical blocks until copies diverge. The original document's ID is
  stored as provenance metadata, enabling explicit "update to latest"
  (user-initiated pull). One-directional knowledge: compound knows about
  original, original doesn't know about compound.
- **Mimetype of the whole (partially resolved).** Imported documents retain
  their original external mimetype as metadata (e.g.,
  `application/vnd.openxmlformats-officedocument.presentationml.presentation`
  for .pptx). OS-native documents get a custom OS mimetype (e.g.,
  `application/x-os-presentation`, `application/x-os-project`). The
  document-level mimetype drives editor binding: `application/x-os-presentation`
  → presentation editor, `application/x-os-project` → project editor. On export,
  the user selects a target format; the OS pre-selects the original mimetype
  where available. Original mimetype is an optional metadata field (present for
  imports, absent for OS-native). **Remaining:** systematic mapping of IANA
  mimetypes to OS document types; naming convention for OS-native mimetypes; how
  simple documents' mimetypes relate to their content file's mimetype (is a
  document wrapping a single `image/png` still typed as `image/png`, or does it
  get an OS wrapper type?).
- **Is "compound" intrinsic or contextual?** A PDF has a single mimetype but
  contains text + images + vector graphics. As a document, its manifest
  references one content file (the PDF). If the user decomposes it for
  part-by-part editing, a new manifest with extracted parts could be created.
  The "compoundness" is a property of the manifest's structure, not an intrinsic
  property of the content format. But is this decomposition automatic,
  user-initiated, or editor-driven?

---

## External Boundaries (Adopted Standards)

The OS does not exist in a vacuum. It builds on external standards that it
adopts rather than reinvents. These are the constraints imposed by external
reality.

**Hardware (non-negotiable):**

- arm64 (Apple Silicon). CPU, GPU (Metal), storage (NVMe), display, input (USB
  HID), networking (WiFi, Ethernet, Bluetooth), boot (UEFI).

**Networking (non-negotiable for internet connectivity):**

- TCP/IP, HTTP/HTTPS, DNS, TLS, WebSocket.

**Text (adopted deeply):**

- Unicode for character encoding. OpenType/TrueType for fonts.

**Content identity (adopted deeply — foundational to the OS model):**

- IANA mimetype registry. The OS's content type system is built on this. See the
  Decomposition Spectrum for why this is the chosen abstraction boundary.

**Media and file formats (adopted at interop boundary):**

- Image: PNG, JPEG, WebP, SVG, etc.
- Audio: AAC, MP3, FLAC, WAV, etc.
- Video: H.264/H.265, VP9, AV1, etc.
- Document: PDF, HTML/CSS.
- Each format has a decoder/encoder as a leaf node. New format support = new
  leaf node; the OS doesn't change.

**Other:**

- Cryptography: standard primitives (AES, SHA-256, RSA/EC).
- Time: UTC, IANA timezone database.
- Color: sRGB, Display P3, ICC profiles.

**Explicitly rejected:** POSIX — its filesystem API, process model, and untyped
pipes conflict with the document-centric model. See Decision #3 in the decision
register.

### Adoption heuristic

When the OS is forced to adopt a pattern at one external boundary, the first
instinct should be: use the same pattern internally, if the domains are similar
enough. This avoids doing the same thing two different ways. But "similar
enough" is the key qualifier — mimetypes generalize well (forced at interop,
useful everywhere internally). URIs are useful for web addressing but may not
generalize to all internal addressing needs.

### The adaptation layer

Between external reality and the OS's internal model sits an adaptation layer:
drivers, decoders, translators, protocol handlers. This is where most "leaf node
complexity" (belief #6) lives. It smooths external messiness into internal
consistency.

**Total system complexity is conserved.** The external world's complexity is
fixed — you can only choose where the adaptation cost lives. Making the core
simpler by pushing everything into adapters doesn't reduce complexity; it
displaces it. The optimal core isn't the smallest possible core — it's the one
where moving anything out would cost more in adaptation complexity than it saves
in core simplicity.

The L4 microkernel illustrates the failure mode: a minimal kernel creates a
beautifully simple core but explodes the adaptation layer with IPC protocols,
serialization, and cross-process error handling. The total system got more
complex, not less. A clean core and messy adaptation layer isn't better than a
slightly less clean core with a thinner adaptation layer.

**The design metric:** minimize total irregularity across both the core and its
adaptation layer, jointly.

### Symmetric adaptation

The adaptation layer wraps the OS core on all sides, not just below:

- **Below (hardware):** Drivers adapt device protocols into kernel trait
  interfaces. External reality: clocks, buses, interrupts, device registers.
- **Sides (formats, network):** Translators adapt external formats into
  manifests + content files. Protocol handlers adapt network standards into
  internal representations. External reality: .docx, .pptx, HTML, HTTP, codec
  bitstreams.
- **Above (users):** Editors adapt user creative intent into the edit protocol.
  The shell adapts user navigational intent into metadata queries + document
  lifecycle operations. External reality: unpredictable human input shaped by
  expectations from other systems.

Drivers and editors are structural mirrors — blue-layer adapters facing opposite
directions. A driver translates device registers into
`create_surface`/`fill_rect`/`present`. An editor translates keypresses into
`beginOperation`/`endOperation`. The OS core sits in the middle, semantically
ignorant in both directions.

### Trust and complexity are orthogonal

The adaptation layer model (red/blue/black) describes where **complexity**
lives. The kernel/OS service/tool distinction describes where **trust** lives.
These correlate — the core is both clean and trusted, adapters are both messy
and untrusted — but for different reasons. The kernel is clean because it's
semantically ignorant. The OS service is clean because it's well-designed.
Drivers are untrusted because hardware is messy. Editors are untrusted because
users are unpredictable. Conflating the two axes creates apparent paradoxes;
separating them reveals the architecture's symmetry.

---

## Content Model

### The Type System (Three Layers)

The OS understands content through three layers that build on each other:

**Layer 1 — Byte streams (storage).** At the bottom, files are byte streams
stored on a conventional filesystem. This preserves Unix-style composability and
compatibility. The filesystem (e.g., ZFS) handles storage, integrity, and
permissions.

**Layer 2 — Mimetype registry (identity).** Above storage, the OS maintains a
registry that maps every file to a mimetype. This is determined by a combination
of declared type (metadata/tags) and content detection (inspecting magic bytes),
with declaration taking priority and detection as fallback for untagged files.
The mimetype tree provides a natural hierarchy: `image/png` is a specific case
of `image/*`.

**Layer 3 — Content categories (interaction).** The OS groups mimetypes into a
small set of content categories that it natively understands how to present:
plain text, rich text, image, audio, video, structured data (tables), and
compound documents (containers combining multiple content types). Each category
has a built-in viewer. This is the layer that makes the OS feel document-centric
rather than app-centric.

The key insight: the byte-stream layer and the structured-understanding layer
don't conflict. One is the storage model, the other is the interaction model.
Unix got the first one right. The OS builds the second on top without
undermining the first.

### The Decomposition Spectrum

Any content type can be decomposed further. A video is frames arranged in time.
An image is a grid of pixels. Text is a sequence of code points. Code points are
bytes. Bytes are bits. Taken to its logical conclusion, everything decomposes to
raw data — and you've reinvented Unix.

This is a real spectrum, not a false dilemma. Unix drew its line at the byte
level, aligned with hardware addressing (the CPU addresses bytes, not bits).
That line is pragmatic, not mathematically fundamental, but it's anchored to
something external and stable — hardware.

This OS draws its line at the **mimetype level**, aligned with the IANA mimetype
registry — decades of industry consensus about where meaningful content
boundaries are. `image/png`, `video/mp4`, `text/plain` exist because the
industry converged on those as useful units of content identity. We didn't
invent these boundaries; we take them seriously as an architectural concept.

**The principle:** the OS's content understanding stops decomposing where
further decomposition stops being useful to the user. The OS understanding "this
is an image" lets it show you the image without a dedicated app. The OS
understanding "this is a grid of pixels" doesn't help anyone. Both are valid
decompositions; only one earns its keep.

**Compound documents compose at the mimetype level.** Their parts are things
that have mimetypes — a slideshow references `image/png` and `text/plain` files,
not pixel rows or code point sequences. An atomic content type (a video file, an
image, a text file) has internal structure, but that structure is the content
type's own concern, not the OS's. A video file's temporal compression, frame
dependencies, and codec metadata are properties of `video/mp4`, not a layout
model the OS decomposes.

**The `application/octet-stream` escape hatch.** The mimetype system has a
bottom: `application/octet-stream` means "unknown bytes." This is the escape
hatch back to Unix-level agnosticism. But it's self-penalizing — labeling
something `application/octet-stream` opts out of everything the OS provides: no
viewer, no editor binding, no content extraction, no meaningful composition in
compound documents. You _can_ bypass the type system, but you only hurt
yourself. The escape hatch exists for genuinely unknown data (e.g., importing a
file the OS has never seen), not as a useful alternative path.

**The parallel:** Unix aligned its abstraction boundary with hardware (bytes).
This OS aligns its abstraction boundary with an established content identity
standard (mimetypes). Both lines are pragmatic but anchored to something
external and stable. Both allow going deeper, but doing so stops serving the
system's purposes.

### File Organization

The filesystem has rich, queryable metadata. Users find and organize files by
querying attributes, not by navigating directory trees. Metadata comes from
three sources: automatic (dates, size, mimetype), content extraction (EXIF, ID3,
embedded document metadata), and user-applied (arbitrary key-value attributes
and tags).

All metadata is queryable through a simple system API (equality, comparison,
AND/OR on attributes), backed by an embedded database engine as a leaf node.
Power users can access raw SQL as an escape hatch.

Paths exist because the underlying storage requires them, but they are just
another attribute — like creation date or file size. Available when useful, not
the organizing principle.

### Mimetypes as the External API

Standard mimetypes serve as the interoperability boundary between this OS and
the outside world. Internally, the OS can represent and organize data however it
wants. When importing or exporting, it translates to/from common formats. This
gives freedom without isolation.

---

## Persistence Architecture

### Two-Library Design

The persistence layer is two separate libraries with a clean boundary:

```text
Core ──[IPC]──→ Document Service ──→ store library ──→ fs library ──→ disk
                 (services/document/)   (libraries/store/)  (libraries/fs/)
```

The **fs library** is generic infrastructure — a COW filesystem useful to anyone
building anything. `BlockDevice` trait, inodes, free-extent allocator,
superblock ring, snapshots, two-flush crash consistency. No document or media
type concepts. Reusable outside this OS.

The **store library** adds document-centric semantics specific to this OS: a
catalog (media types, queryable attributes), the `Query` API, and snapshot-aware
versioning. It wraps `Box<dyn Files>` (trait object, not generic) so it never
sees `BlockDevice` or `Filesystem<D>` — the generic parameter is fully contained
in the fs library and the document service.

Each can be swapped independently. The `Files` trait and `Store` API are the
stability boundaries.

### Catalog

A single catalog file stored in the filesystem holds metadata for all documents.
The catalog is "just a file" — the fs library doesn't know it's special.
Discovery: the store calls `set_root(catalog_id)` during init; on reopen,
`root()` returns the catalog in O(1).

The catalog stores only what the filesystem doesn't already know: media type
(mandatory at creation) and user/system attributes (key-value pairs). Size,
created, modified come from the fs library's `FileMetadata` — no duplication.

### Target Scale (Permanent Design)

This is a personal document-centric OS. It does not have apps, package managers,
`.git` directories, or file trees. It has documents — things with media types
that humans create or consume.

| Scale      | Inode table       | Catalog memory | Expected use                               |
| ---------- | ----------------- | -------------- | ------------------------------------------ |
| 1K files   | 12 KB, 1 block    | ~128 KB        | Early use                                  |
| 10K files  | 120 KB, 8 blocks  | ~1.3 MB        | Moderate (documents, some photos, music)   |
| 100K files | 1.2 MB, 75 blocks | ~12.8 MB       | Heavy (prolific photographer + everything) |

100K is the realistic ceiling for a personal OS without an app ecosystem. The
in-memory BTreeMap catalog and linked-block inode table are the permanent
architecture for this scale, not interim solutions. The `Files` trait is the
stability boundary — if someone adapted this for a million-file use case, they
would replace internals behind that interface.

### Atomic Multi-File Writes

`commit()` is the transaction boundary. All writes between commits land
atomically or not at all (two-flush protocol: crash before second flush → old
superblock wins → old state). Compound document creation — writing multiple
content files plus updating the catalog — is atomic without any additional
machinery. This property is inherited from the COW filesystem design.

### Services as Translation Layers

The document service is a thin IPC wrapper — it translates IPC messages into
store API calls. All document logic lives in the store library, which is shared
with the factory image builder (a host tool that creates pre-populated disk
images using the same code paths). Core never touches the filesystem, the block
device, or the catalog. The IPC boundary is the translation boundary.

---

## Viewing and Editing

### Viewer-First Design

Opening any file presents it using the OS's built-in viewer for that content
category. The viewer is always the OS's own — it provides a consistent rendering
experience regardless of what editors are installed.

### Editor Augmentation Model

When the user chooses to edit, the **viewer stays**. The editor augments the
view by adding tools and intercepting modification input, rather than replacing
the viewer with its own UI.

The OS renderer is a **pure function of state**: file bytes + mimetype + view
state → visual output. No side effects, no accumulated state. When an operation
is committed, the file changes, and the renderer produces a new output from the
new state.

**The desktop analogy:** A document is on your desk, open to a page. You pick up
a pen — you can write where you're looking. Put it down, pick up a highlighter —
you can highlight where you're looking. Put down all tools — you can still look
and flip pages. Where you are in the document is independent of which tool you
hold.

**Input routing:**

- Navigation (scroll, page, cursor movement) → always the OS, with or without an
  editor attached
- Modification (keystrokes, brush strokes) → the active editor, which issues
  operations through the edit protocol
- No editor attached → modification input ignored or handled as OS shortcuts

**OS-provided interaction primitives** shared across all editors of a content
type: cursor positioning and text selection (text), selection regions (images),
playhead (audio/video). These are part of the OS's content-type understanding,
not editor-specific.

**Editors get read-only access; all writes go through the OS service.** Editors
receive a read-only memory mapping of the document for fast zero-copy reads.
Modifications go through the OS service via IPC write requests. The OS service
is the sole writer to document files — it applies writes immediately and
controls when snapshots are taken. This ensures undo is automatic and
non-circumventable: "never make the wrong path the happy path." A lazy editor
that ignores operation hints still gets correct undo. Documents are shared
resources (the OS renders, versions, and indexes them), so mediated write access
follows the same principle as the kernel mediating access to shared hardware.

**No pending changes — edits are immediately durable.** There is no separate
"working state" and "persisted state." When the OS service applies an editor's
write request, the file on disk is updated immediately. The file on disk is
always current. There is no "save" action — every edit is durable the moment it
happens. The COW filesystem makes this cheap (only changed blocks are written)
and reversible (previous versions are retained as snapshots). This eliminates
"unsaved changes," "save before closing?" dialogs, and the entire class of
data-loss bugs from crashes before saving.

**Editor overlays:** Editors can draw temporary visual chrome — crop bounds,
selection highlights, tool cursors — but these are tool UI, not document
content. They never affect the file.

### The Tool Model (under exploration)

The shell (GUI/CLI navigation interface) is a blue-layer tool — an untrusted
process (EL0) that translates navigational intent (find documents, open
something, switch contexts) into OS service operations (metadata queries,
document open/close). The shell is pluggable: a different shell can provide a
different interaction model. The OS will be tuned toward its primary shell's
needs, but the interface is available to any replacement. The interaction model
is a shell design question, not an OS service design question.

Unlike editors, the shell is ambient — it must respond to system gestures
(switch document, invoke search) even while an editor is active. Current
thinking splits this into: system gestures baked into OS service input routing
(always work, not pluggable) and navigation UI provided by the shell (pluggable,
restartable). The exact boundary between OS service and shell input handling is
an open question.

See Decision #17 in the decision register for full exploration status, including
unresolved tension around compound document editing (editors bind to content
types vs. one editor per document).

### Editor-to-Content Binding

Editors declare which mimetypes (or mimetype patterns) they operate on. Each
editor brings its own set of tools for that content type. Dispatch follows
mimetype specificity:

- A `text/xml` editor takes priority over a general `text/*` editor for XML
  files.
- An `image/*` editor handles any image type.
- If multiple editors match at the same specificity, the user can choose.

Editors can be narrow (an XML-specific editor for `text/xml`) or broad (a media
editor that handles both `image/*` and `video/*` for shared operations like
cropping or color adjustment).

### The Edit Protocol

Editors don't own files directly. They issue **operations** through a protocol.
The OS mediates between editors and data.

**Tools are modal.** Only one editor is active on a document at a time — the
"pen on the desk" metaphor. You put one tool down before picking up another.
This eliminates concurrent-editor composition as a protocol concern and makes
the operation log a simple sequential list.

**The protocol is a transaction model:**

- Editor sends `beginOperation` to open a transaction. The OS takes a COW
  snapshot.
- Editor sends write requests. The OS applies each write to the document
  immediately (it is the sole writer).
- Editor sends `endOperation` to commit. The snapshot becomes an undo point. The
  operation log records: which editor, when, which document, human-readable
  description.
- Editor sends `cancelOperation` to roll back. The OS restores the snapshot
  taken at `beginOperation`. No undo entry is created. As if the operation never
  happened.

**Two transaction modes** control whether intermediate writes are visible:

- **Streaming** (default): each pending write is rendered immediately. The user
  sees continuous feedback. For typing, slider drags, brush strokes — any
  operation where real-time preview matters.
- **Batched**: writes accumulate in the document but are not rendered until
  `endOperation`. For multi-part structural edits (find-replace-all, paste
  compound content, format conversion) where intermediate states are meaningless
  or visually broken.

The editor decides both the boundaries (what constitutes one undoable operation)
and the mode (whether the user sees intermediate states). This is database
transaction semantics — BEGIN, COMMIT, ROLLBACK — applied to document editing.

**Fallback for lazy editors:** If an editor sends writes without explicit
`beginOperation`/`endOperation`, the OS detects operation boundaries
automatically via idle-gap detection (e.g., a pause in write activity). A lazy
editor gets correct (if coarser) undo. A diligent editor gets precise undo. The
wrong path is never the easy path.

**Editors are read-only consumers.** Editors receive a read-only memory mapping
of the document for fast zero-copy reads. All modifications go through the OS
service via IPC. This makes undo automatic and non-circumventable.

The OS is logistics — it doesn't understand what operations mean, it just tracks
boundaries, ordering, and attribution. This keeps the protocol as simple
connective tissue.

**Undo is global, not per-editor.** The OS walks backward through the operation
log regardless of which editor produced each operation. This matches the user's
mental model: "undo the last thing I did." The COW filesystem restores the
previous version. The originating editor does not need to be active for undo to
work.

**Content-type handlers (optional, for advanced features).** Sequential undo
works for all content types with zero additional machinery. For selective undo
(undo operation A while keeping later operations B and C) and future
collaboration, content-type handlers provide rebase logic — the ability to
adjust operations when earlier operations are removed or concurrent operations
arrive. These handlers are leaf nodes (complex inside, simple interface),
analogous to how git understands text merging. Text rebase is a solved problem
(OT/CRDTs). Audio and video (1D time-axis content) are structurally similar to
text. Image operations (2D regions) are less battle-tested but tractable.
Content types without a rebase handler gracefully degrade to sequential-only
undo.

**Cross-content-type interactions are layout's job, not the edit protocol's.**
When resizing an image causes text to reflow in a compound document, the layout
engine handles that — not the image editor or the text content-type handler. The
edit protocol only needs to handle same-type, same-region operation conflicts.

---

## Undo, History, and (Future) Collaboration

### Sequential Undo (Base Case) — Implemented

Every `endOperation` creates a COW snapshot as an undo point. The Document Model
(A) maintains an undo ring (64 entries) of snapshot IDs. Undo (Cmd+Z) restores
the previous snapshot; redo (Cmd+Shift+Z) restores the next. Editing after undo
truncates the redo history.

Undo granularity is controlled by the editor's transaction boundaries. A text
editor that calls `beginOperation` when typing starts and `endOperation` on word
boundary or pause gets word-level undo. An image editor that wraps each tool
gesture in a transaction gets gesture-level undo. `cancelOperation` rolls back
to the `beginOperation` snapshot without creating an undo entry — this handles
Escape during a drag, errors mid-operation, etc.

Undo is global — the OS undoes the most recent operation regardless of which
editor produced it. The originating editor does not need to be active.

### Selective Undo (Upgrade Path)

Selectively undoing an earlier operation while keeping later ones requires
**rebasing** — adjusting later operations to account for the removal. This is
only possible when a content-type handler provides rebase logic:

- **Text:** Solved problem. OT and CRDTs handle positional rebasing (git merge
  is a simpler version of this).
- **Audio/Video:** Structurally similar to text — 1D sequence along a time axis.
  The same rebase principles apply with different domain primitives.
- **Images:** 2D region-based operations. Less battle-tested but tractable — two
  operations on non-overlapping regions are independent; overlapping regions
  conflict.
- **No handler:** Graceful degradation to sequential-only undo.

### Cross-Session History

File history is provided by the COW filesystem's snapshot retention. "Show me
this document as it was last Tuesday" is a filesystem query, not an application
feature. The operation log handles fine-grained in-session undo; the filesystem
handles long-term history.

### Collaboration-Ready Architecture

The architecture supports future multi-user collaboration because:

1. The operation log captures every change with attribution and ordering.
2. Content-type handlers with rebase logic can resolve concurrent edits (the
   same machinery needed for selective undo).
3. Cross-type conflicts are mediated by the layout engine, not the edit
   protocol.

The networking and conflict-resolution layers are deferred. Collaboration and
selective undo require the same investment (content-type rebase handlers), so
building one unlocks both. The system is built for one user first, with the
structural capacity to grow.

---

## Compound Documents

Documents are manifests with references + relationship descriptions. A document
with one content reference (a single text file) is structurally the same as a
document with many (a slide deck) — the manifest model is uniform. What makes a
document "compound" is not a separate concept but simply having multiple content
references with relationships between them.

### Relationship Axes (Three Composable Axes)

The relationships between parts in a document are described along three
orthogonal, composable axes. Each axis is independent — a document uses
whichever axes are relevant, and most use only one or two.

**Spatial** — where parts are positioned relative to each other:

- **Flow** — content reflows when things change (documents, articles, emails,
  web pages)
- **Fixed canvas** — objects at specific positions on fixed-size pages
  (presentations, posters)
- **Grid** — rows and columns (spreadsheets, dashboards)
- **Freeform canvas** — arbitrary positioning on an unbounded surface
  (whiteboards, design tools, mind maps)
- **None** — parts have no spatial relationship (playlists, source code
  projects)

**Temporal** — when parts are active relative to each other:

- **Simultaneous** — all parts present at once (a page with text and images)
- **Sequential** — parts experienced one at a time in order (slideshow, music
  album)
- **Timed** — parts positioned at specific points along a time axis with
  synchronization (video editing, audio production, animation)
- **None** — no temporal relationship (most static documents)

**Logical** — how parts are grouped and structured:

- **Flat** — unordered set (photo collection)
- **Sequential** — ordered list (playlist, chapter sequence)
- **Hierarchical** — tree structure (source code project, document outline)
- **Graph** — network of relationships (mind map, knowledge graph)
- **None** — no logical structure beyond containment

Every document is a point in this three-dimensional space. Examples:

| Document              | Spatial                  | Temporal              | Logical                       |
| --------------------- | ------------------------ | --------------------- | ----------------------------- |
| Slide deck            | fixed canvas (per slide) | sequential            | flat list of slides           |
| Source code project   | none                     | none                  | hierarchical (directory tree) |
| Music album           | none                     | sequential (playback) | flat list of tracks           |
| Mixed-media article   | flow                     | none                  | sequential sections           |
| Video editing project | 2D frame                 | timed (synchronized)  | grouped by track              |
| Photo album           | none                     | none                  | flat or grouped               |

**Axes can interact.** Spatial arrangement can vary over time (animation =
spatial varying over temporal). Logical structure can affect spatial layout
(collapsible sections). The layout model declares which axes are present;
content-type-specific editors and renderers handle the coupling between axes.
The OS declares structure, not semantics — consistent with the OS being
semantically ignorant about content (Decision #9).

**Version history is orthogonal.** COW snapshots (Decision #12) provide version
history for all documents regardless of their layout axes. This is an OS-level
mechanism, not a layout axis. An audio file has content temporality (the
waveform) AND version history (the edits) — these are fundamentally different.
Content temporality is part of what the document _is_. Version history is how
the document has _changed_. Undo operates on the version axis, outside the
layout model entirely.

If the rendering technology is a web engine, CSS natively supports flow, grid,
and fixed positioning — covering most spatial sub-types. Temporal and logical
axes require custom handling.

### Structure

A document is a **manifest** file that references:

- Content files (one or more real files — a PNG, a text file, an audio clip)
- Relationship descriptions along one or more of the three axes
- Metadata (title, tags, custom attributes — co-located with document identity)

The manifest is the source of truth for document identity and metadata. The
metadata query system indexes manifests for fast querying. Content files are the
source of truth for content; a separate content index enables full-text search.

"Resize image in a slideshow" is a spatial layout operation (changing the
manifest's arrangement rules), not an image operation — the image bytes don't
change.

### Static and Virtual Manifests

A manifest can be backed by disk (static) or by computation (virtual):

- **Static manifest** — a real file on disk. Content written, read back,
  COW-snapshotted. Examples: a text file, a slide deck, a source code project, a
  viewed webpage (persisted by the web translator).
- **Virtual manifest** — a file in the filesystem namespace whose content is
  generated on demand. Content derived from internal system state (inbox = query
  over messages, search results, system dashboard) or external sources (a
  streaming video = manifest with remote reference, content fetched on demand).
  Examples span from Plan 9-style `/proc` (internal state) to streaming media
  (external source).

Both are files. Both are documents. Both participate in the metadata query
system. The user doesn't know or care which kind backs a given document. This
follows the Plan 9 principle: `/proc` is a file, it has a path, you can read it,
but nothing is stored on disk.

Virtual documents don't need their own COW history. Their "state at time T" is
recoverable by re-evaluating the query against the snapshot of the world at time
T — the underlying static documents have COW history, and virtual documents
inherit time-travel for free. This is the same reason database views don't need
their own transaction log.

**Design constraint:** Rewind performance must be uniform across static and
virtual documents. If virtual document rewind is noticeably slower (requiring
index reconstruction instead of snapshot lookup), the static/virtual distinction
leaks through the abstraction. This requires the metadata DB to live on the COW
filesystem so that its historical state is preserved in snapshots — querying
"inbox last Tuesday" reads from the metadata DB at Tuesday's snapshot, same cost
as a current query. See Decision #16 (filesystem COW design).

### Retention Policies

All documents are persistent by default. There is no "transient" document
concept — persistence is not a document type, it's a retention policy.

Every document has a retention policy that determines how long it and its COW
history are kept. Some documents are permanent (user-created content, explicitly
saved files). Others have shorter retention (viewed webpages might be kept for
30 days, background artifacts for 7 days). The COW pruning system handles
cleanup — the same mechanism needed for edit history management also manages
document lifecycle.

This eliminates a potential abstraction leak: if "transient" documents existed
alongside "persistent" ones, the user would need to know which type a document
is to predict behavior (can I find this page tomorrow? can I rewind this?). With
retention policies, all documents behave the same way — they're all persistent,
searchable, and rewindable. Some just get pruned sooner. Retention policies are
user-configurable per document type.

### Layout as Cross-Type Mediator

The OS's layout engine handles cross-content-type interactions along whichever
axes are active:

- Resize an image → text reflows around it (spatial: flow responds)
- Remove a time range in a video → corresponding audio is trimmed (temporal:
  timed enforces track synchronization)
- Reorder slides → sequence updates (temporal: sequential adjusts)
- Expand a section → child elements appear (logical: hierarchical drives spatial
  visibility)

Cross-type operations are never the edit protocol's problem. The layout engine
mediates. Only same-type, same-region conflicts need content-type rebase
handlers.

### Interop

At boundaries, **translators** convert between the OS's internal representation
and external formats:

- Import .pptx → extract content as individual files, generate manifest with
  spatial (fixed canvas) + temporal (sequential) layout
- Export to .pptx → read manifest + referenced files, pack into pptx structure
- Import .docx → extract content, generate manifest with spatial (flow) layout
- Import .html (view a webpage) → extract content as individual files, generate
  manifest with appropriate layout. Same translator pattern as any other import.
  The document is persisted like any other — subject to retention policy (e.g.,
  viewed webpages kept for 30 days). This gives rewindable browsing (COW history
  of page views), offline access (previously viewed pages are on disk), and
  full-text search across browsed content — all for free, with no special
  browser features. See Decision #11.
- Import external files (airdrop, download) → create manifest wrapping the file,
  extract metadata, index
- Export → user chooses target format. Pre-selected to the document's original
  mimetype where available (e.g., re-exporting an imported .pptx defaults to
  .pptx). For OS-native documents with no original format, the user selects from
  available translators (like choosing png vs jpg vs webp for an image).

Each translator is a leaf node — complex inside (parsing docx is genuinely
hard), simple interface. New format support = new translator; the OS doesn't
change. Translation is inherently lossy (some features won't map between
formats), which is true of all format conversion.

---

## Open Questions (To Be Resolved)

### Interaction Model

The GUI's look, feel, and navigation are not yet defined:

- Windowed, fullscreen-per-workspace, tiling, or something else? (Leaning:
  one-document-at-a-time, non-windowed)
- How does the user navigate between open documents?
- What does "launching" something look like in a system with no app launcher?
- How do tags and queries surface in the GUI?
- How does compound document editing work when editors bind to content types but
  only one editor is active per document?

The shell is a blue-layer tool (untrusted EL0 process, pluggable). System
gestures live in the OS service (not pluggable); navigation UI lives in the
shell (pluggable). The exact boundary between them is an open question. See
Decision #17 in the decision register.

---

## Audience, Goals, and Scope

**Audience:** Personal design project.

**Primary artifact:** A coherent, complete OS design. Implementation is
selective — build to validate uncertain assumptions, stub or use off-the-shelf
for the rest.

**Success criteria (in priority order):**

1. **Coherent design** — the design documents are thorough and defensible; when
   you pull on one thread, the whole thing holds together
2. **Working prototype** — the most uncertain or interesting parts of the design
   are proven out with real code
3. **Deep learning** — the designer understands OS design deeply through the
   process

**Non-goal:** A daily driver. This OS does not need to replace the designer's
actual computing environment.

**Target use cases:** Personal workstation. Everything a single person does at a
desktop computer for creative and knowledge work:

- Text documents (reading, writing, editing)
- Images (viewing, editing)
- Audio (listening, editing)
- Video (watching, editing)
- Email
- Calendar
- Messaging (chat, Slack-like)
- Videoconferencing (edge case — live sessions, not documents)
- Web browsing
- Coding and development

All content is modeled internally as files (local or mounted from remote
services), following the Plan 9 philosophy. Not targeting mobile devices or
servers initially.

**Comparable prior art:** BeOS — designed for creative professionals doing media
work on a personal machine.

**Prototype scope:** The prototype needs depth, not breadth. If the system can
view and simply edit text, view and rotate an image, and demonstrate that the
concept works and scales to the full use case list, that is success. The design
documents carry the breadth; the prototype proves the architecture.

**Development model:** The OS is developed on a host OS (macOS). There is no
self-hosting goal. The prototype does not need to reimagine dev tools — it needs
to prove out the novel parts of the design (file addressing, editor protocol,
viewer framework, type-aware shell).

---

## Influences and References

- **Mercury OS** (Jason Yuan) — speculative vision of an OS with no apps or
  folders, assembling content and actions fluidly based on user intention.
- **Ideal OS** (Josh Marinacci) — argument that desktop OSes are bloated with
  legacy cruft and should be rebuilt from scratch, learning from past lessons.
- **OpenDoc** (Apple/IBM, mid-1990s) — component-based document editing where
  editors were embedded parts, not standalone apps. Closest historical attempt
  at this document-centric model. Failed due to technical limitations of the era
  and economic headwinds.
- **Xerox Star** (1981) — genuinely document-centric desktop. Users started with
  documents, not apps.
- **Plan 9** (Bell Labs) — everything-is-a-file philosophy taken to its logical
  conclusion. Radical simplicity at the systems level.
- **BeOS / BFS** — rich queryable metadata built into the filesystem, enabling
  attribute-based file discovery rather than path-based navigation.

Previous document-centric attempts (OpenDoc, Xerox Star) likely failed not
because the idea was wrong, but because: they tried to be universal, forcing
every computing task into the document metaphor; 1990s component architectures
weren't up to the technical challenge; and economic incentives of major OS
vendors were aligned with app-centric models.
