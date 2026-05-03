# Glossary

Terms with specific meaning in this system. Grouped by layer, alphabetical
within each group. Cross-references point to where each concept is explored in
depth.

For the data model glossary (file, manifest, document, content file, coordinate
units), see [foundations.md § Glossary](foundations.md#glossary).

---

## Architecture

**Adaptation layer** — the ring of components that surround the OS core on all
sides, translating between external reality and the core's internal model.
Drivers adapt hardware below; translators adapt formats at the sides; editors
and the shell adapt users above. Total system complexity is conserved — the
adaptation layer absorbs external messiness so the core stays clean. Described
by the red/blue/black color model. See
[foundations.md § The adaptation layer](foundations.md#the-adaptation-layer) and
[architecture.md § The Adaptation Layer](architecture.md#the-adaptation-layer).

**Document pipeline** — three cooperating processes that form the center of the
system, replacing the former monolithic "core" service (decomposed v0.5):
`document` (sole writer to document state — applies edits, manages piece table
or flat buffer), `layout` (text layout with styled runs, word/character
breaking, font metrics), and `presenter` (scene graph builder, input router,
style shortcuts). Together they understand content at the mimetype level (text
has lines, images have dimensions, compound documents have parts) but are
semantically ignorant of codec internals, user intent, and hardware. See
[architecture.md § The OS Service](architecture.md#the-os-service) and
[design/userspace.md](userspace.md).

**Decoder** — a sandboxed service that reads encoded bytes from the File Store
(shared memory, read-only) and writes decoded content (pixels, samples) into the
Content Region (shared memory, read-write). Each decoder handles one format
family. Decoders are leaf nodes: complex inside, simple IPC interface
(`DecodeRequest` → `DecodeResponse`). A generic harness
(`services/decoders/harness.rs`) handles all IPC plumbing; format-specific code
is just `header()` + `decode()` functions. See
[design/userspace.md](userspace.md) and the journal entry "Image Decoding as a
Service Interface."

**Document service** (`services/document/`) — the process that owns document
state. Sole writer to the document buffer (piece table for `text/rich`, flat
UTF-8 for `text/plain`). Applies write requests from editors, manages the add
buffer, and sends commit messages to the store service at operation boundaries.
One of the three document pipeline processes (with layout and presenter). See
[design/userspace.md](userspace.md).

**Driver** — a userspace process that translates between device hardware and OS
primitives. Drivers are untrusted, restartable leaf nodes. Structurally
symmetric with editors: a driver translates device registers into OS primitives,
an editor translates user gestures into write requests. Both face unpredictable
external reality. See also: render service. See
[architecture.md § The Adaptation Layer](architecture.md#the-adaptation-layer).

**Init** — the root task. The only process the kernel spawns directly. Init
spawns all other processes, orchestrates shared memory allocation, creates IPC
channels, and runs the startup handshake. Microkernel pattern (cf. Fuchsia
`component_manager`, seL4 root task). Currently scaffolding; the pattern is
foundational. See [design/userspace.md § 2.1](userspace.md).

**Kernel** — manages hardware resources (memory, CPU time, interrupts, process
isolation). Semantically ignorant — does not know what a document, mimetype, or
pixel is. Provides handles to typed objects (channels, VMOs, threads, events,
scheduling contexts) with rights attenuation. Does not look inside the data that
flows through them. 46 syscalls, 4 SMP cores, per-core EEVDF scheduler with work
stealing, GICv3. See `kernel/DESIGN.md`.

**Leaf node** — a component at the outermost edge of a pipeline, connecting to
nothing downstream. Leaf nodes are where essential complexity lives: a PNG
decoder, a font shaper, a device driver, a format translator. Complex inside,
simple interface. The complexity is contained and cannot leak inward. "Leaf
node" is relative to the system boundary you're looking at — the render service
is a leaf of our OS, but from a wider view it's another translator. See
[philosophy.md § Push complexity outward to the leaves](philosophy.md#push-complexity-outward-to-the-leaves).

**Layout engine** (`services/layout/`) — the process that computes text layout:
styled runs, word/character breaking, font metrics, line positions. Receives
layout requests from the presenter and returns positioned glyph data. One of the
three document pipeline processes (with document and presenter). Not to be
confused with the `layout` library, which provides the underlying layout
algorithms. See [design/userspace.md](userspace.md).

**Library** — a statically linked `no_std` Rust crate with no syscalls and no
side effects. Libraries are pure computation: callers provide all memory, I/O,
and context. Libraries are host-testable — the same code runs in the OS and in
`cargo test` on macOS. Examples: `sys`, `virtio`, `drawing`, `fonts`, `scene`,
`ipc`, `protocol`, `render`, `animation`, `layout`, `fs`, `store`. See
[design/userspace.md § 1](userspace.md).

**Presenter** (`services/presenter/`) — the process that builds the scene graph,
routes input events, and handles style shortcuts (Cmd+B, Cmd+I, Cmd+1/2). The
presenter is the bridge between user interaction and visual output: it receives
input from the input driver, dispatches to editors or handles system gestures,
requests layout from the layout engine, and compiles the result into scene graph
nodes. One of the three document pipeline processes (with document and layout).
See [design/userspace.md](userspace.md).

**Red / Blue / Black** — a color model for classifying components by where
complexity lives. **Black** (core): clean, trusted, semantically aware. **Blue**
(adaptation layer): messy, untrusted, absorbs external complexity. **Red**
(kernel): clean, trusted, semantically ignorant. Trust and complexity are
orthogonal axes that happen to correlate — the core is both clean and trusted,
adapters are both messy and untrusted, but for different reasons. See
[foundations.md § Trust and complexity are orthogonal](foundations.md#trust-and-complexity-are-orthogonal).

**Render service** — a thick driver that reads the scene graph from shared
memory and produces pixels. "Thick" means the entire rendering pipeline (tree
walk, rasterization, compositing, GPU presentation) runs in a single process —
no cross-process IPC for frame submission. Sole implementation: `metal-render`
(native Metal GPU via hypervisor passthrough with 4x MSAA). Previous
implementations (`cpu-render`, `virgil-render`) were removed after render
consolidation (v0.5). Render services are leaf nodes: they consume a
content-agnostic scene graph and emit pixels. See
[design/userspace.md](userspace.md).

**Service** — a long-running userspace process with a specific role in the
system. Services communicate via IPC (event rings, state registers, or shared
memory). Each service is a translator: it transforms data from one shape to
another across a process boundary. Examples: init, presenter, document, layout,
store, metal-render, decoder services, input drivers. See
[design/userspace.md § 2](userspace.md).

**Store service** (`services/store/`) — the process that handles document
persistence and undo/redo. Wraps the `store` and `fs` libraries. Receives commit
messages from the document service, writes document state to the COW filesystem
via virtio-blk, and manages snapshots for undo (64-entry ring).
`MSG_DOC_SNAPSHOT` / `MSG_DOC_RESTORE` for undo/redo. See
[design/userspace.md](userspace.md).

**Translator** — a component that converts between the OS's internal
representation and an external format. Import translators (`.docx` → manifest +
content files) and export translators (manifest + content files → `.pptx`) are
leaf nodes — complex inside, simple interface. New format support = new
translator; the OS doesn't change. Translation is inherently lossy. See
[foundations.md § Interop](foundations.md#interop).

---

## Kernel

**ASLR (Address Space Layout Randomization)** — per-process randomization of
heap, DMA, device, and stack region base addresses (~14 bits of entropy each).
Defense-in-depth alongside the capability model. **KASLR** randomizes the kernel
image load address at boot (8-bit entropy, 32 MiB slide) via PIE + PIC +
post-link relocation fixup. See `kernel/aslr.rs`, `kernel/relocate.rs`.

**Badge** — a u64 value attached to a handle, preserved through transfer and
attenuation. Enables userspace servers to identify callers without a global PID
namespace — each client gets a unique badge when the server mints their handle.
See `kernel/handle.rs`.

**Capability (handle)** — the kernel's access control primitive. A process can
only interact with a resource (channel, VMO, thread, event, scheduling context)
by holding a handle to it. Handles carry rights and a badge. No ambient
authority — nothing is accessible without a handle. See `kernel/handle.rs`.

**CWC (Concurrent Work Conservation)** — a property of the SMP scheduler: no
idle core coexists with an overloaded core after a scheduling round.
Property-tested via model tests. Linux CFS was shown to violate CWC (Ipanema,
EuroSys 2020). See `kernel/scheduler.rs`.

**EEVDF (Earliest Eligible Virtual Deadline First)** — the scheduling algorithm,
independently chosen by both this kernel and Linux (6.6+). Per-core ready queues
with virtual lag (vlag) tracking for fairness. Threads are placed via
cache-affine wake (prefer `last_core`). See `kernel/scheduler.rs`.

**Event** — a kernel object with a 64-bit signal bitmask. Threads can wait on
events and set/clear bits atomically. Used for lightweight synchronization
between processes. See `kernel/event.rs`.

**PAC (Pointer Authentication Code)** / **BTI (Branch Target Identification)** —
ARM64 hardware control-flow integrity features. PAC signs return addresses with
per-process keys (5 × 128-bit, loaded on context switch). BTI enforces that
indirect branches land on valid targets. Strictly superior to stack canaries.
See `kernel/arch/aarch64/context.rs`.

**Pager** — a userspace process that supplies pages to a VMO on demand, via a
channel. When a thread faults on an uncommitted VMO page, the kernel sends a
fault message to the pager channel; the pager responds with physical memory.
Fault deduplication ensures only one request per page. See `kernel/vmo.rs`.

**Rights** — a bitmask of 8 named permissions on a handle: READ, WRITE, SIGNAL,
WAIT, MAP, TRANSFER, CREATE, KILL. Rights are monotonically attenuated — you can
only remove rights on transfer, never add them. Per-syscall enforcement ensures
a handle without WRITE cannot be used to write. See `kernel/handle.rs`.

**Scheduling context** — a handle-based object that groups threads into a
budget. Threads sharing a scheduling context share CPU time allocation. Used by
userspace to express workload structure. Work stealing prefers to migrate entire
scheduling context groups (workload-granularity migration). See
`kernel/scheduler.rs`.

**VMO (Virtual Memory Object)** — the kernel's memory primitive. A named
collection of pages with five novel features: versioned (COW snapshots with
bounded ring), sealed (immutable freeze with PTE invalidation), content-typed
(u64 tag for IPC type safety), lazy-backed (demand-paged), and pager-backed
(userspace fault handling). Cross-process `vmo_map` replaces the older
`memory_share` syscall. 10 syscalls (30–39). See `kernel/vmo.rs`.

**Work stealing** — idle SMP cores steal runnable threads from the busiest
remote core's ready queue. EEVDF virtual lag is preserved across migration so
fairness position is maintained. Budget-aware: only steals threads with
scheduling context budget remaining. See `kernel/scheduler.rs`.

---

## Content Pipeline

**Content Region** — a 4 MiB shared memory region holding persistent decoded
content: font TTF data and decoded image pixels. Init allocates it; core manages
the registry and allocator (`ContentAllocator` with first-fit, coalescing,
generation-based deferred GC); render services read it (read-only). Write-once
entry semantics for lock-free concurrent reads. Distinct from the File Store and
the Scene Graph. See [design/userspace.md § 0](userspace.md) and
`protocol/content.rs`.

**File Store** — a 1 MiB shared memory region holding raw encoded file bytes.
Shared between core and decoder services. Core writes encoded data; decoders
read it. The compositor never sees encoded files. Distinct from the Content
Region. See [design/userspace.md § 0](userspace.md).

**Scene graph** — the data boundary between core and the render services. A tree
of positioned, decorated, content-agnostic visual nodes in shared memory. Core
writes it; render services read it. They never communicate any other way. The
scene graph is a compiled output of the document model — it is not the document
model itself. The document has semantic content; the scene graph has geometry.
Content types: `None`, `Image`, `InlineImage`, `Path`, `Glyphs`. Triple-buffered
with generation-based swap. See
[architecture.md § The Scene Graph](architecture.md#the-scene-graph) and
`libraries/scene/`.

---

## Data Model

See [foundations.md § Glossary](foundations.md#glossary) for the core terms:
**file**, **manifest**, **document**, **content file**, **point (pt)**,
**millipoint (Mpt/Umpt)**, **pixel (px)**.

Additional terms:

**Attribute** — a key-value metadata pair attached to a document's catalog
entry. Three sources: automatic (dates, size, mimetype), content-extracted
(EXIF, ID3), and user-applied (arbitrary tags and values). Attributes are
queryable via the store library's `Query` API. See
[foundations.md § File Organization](foundations.md#file-organization).

**Catalog** — a single file in the filesystem holding metadata for all
documents: media type (mandatory) and attributes (key-value pairs). Size,
created, and modified come from the fs library's `FileMetadata` — no
duplication. The catalog is "just a file" that the fs library doesn't know is
special. See [foundations.md § Catalog](foundations.md#catalog).

**Content category** — one of a small set of groups the OS uses to determine how
to present content: plain text, rich text, image, audio, video, structured data
(tables), compound document. Each category has a built-in viewer. This is Layer
3 of the type system. See
[foundations.md § The Type System](foundations.md#the-type-system-three-layers).

**Compound document** — a document whose manifest references multiple content
files with relationships between them. Not a separate concept — the manifest
model is uniform. What makes a document "compound" is having multiple content
references with relationship axes (spatial, temporal, logical). See
[foundations.md § Compound Documents](foundations.md#compound-documents).

**FileId** — a newtype wrapping a filesystem inode identifier. The stable
identity of a document across renames, moves, and metadata changes. Used
throughout the store and document service APIs.

**Manifest** — see [foundations.md § Glossary](foundations.md#glossary). The
file that defines a document. Can be **static** (real file on disk) or
**virtual** (generated on demand, like Plan 9's `/proc`).

**Media type (mimetype)** — the OS's primary content identity mechanism, drawn
from the IANA registry. Foundational to the data model: mimetypes drive viewer
selection, editor binding, content category classification, and interop.
Declared at creation, content detection as fallback. See
[foundations.md § Content Model](foundations.md#content-model).

**Workspace** — the root compound document. The desktop is modeled as a compound
document with a custom media type (e.g., `application/x-os-workspace`). The
document strip is its spatial layout axis; system chrome (clock, controls) are
sub-documents or metadata; GUI/TUI/a11y are different layout projections of the
same structure. See [decisions.md § #17](decisions.md) and
[journal.md](journal.md) "System-as-Compound-Document."

---

## Editing

**Edit protocol** — the thin IPC protocol through which editors modify
documents. Editors send write requests; the OS service (sole writer) applies
them. The OS takes snapshots at operation boundaries. The protocol tracks
ordering and attribution; the OS is semantically ignorant of what the operations
mean. See
[foundations.md § The Edit Protocol](foundations.md#the-edit-protocol).

**Editor** — a userspace process that understands one content type and
translates user creative intent into write requests via the edit protocol.
Editors are modal (one active per document), untrusted, restartable, and
structurally symmetric with drivers. An editor has read-only access to the
document (zero-copy memory mapping); all writes go through the OS service.
Editors may draw temporary visual chrome (crop bounds, selection highlights) but
these never affect the file. See
[architecture.md § Editors](architecture.md#editors).

**Operation** — a unit of editing work, bounded by
`beginOperation`/`endOperation` hints from the editor (or inferred by idle-gap
detection). Each operation boundary creates a COW snapshot for undo. A lazy
editor that never sends operation hints still gets correct (character-level)
undo. A diligent editor that groups writes into named operations gets better
undo granularity. See
[foundations.md § The Edit Protocol](foundations.md#the-edit-protocol).

**Piece table** — the data structure for rich text content (`text/rich` media
type). A sequence of pieces referencing either the original text or an
append-only add buffer, with style IDs per piece. Enables operation-aware
editing (selective undo, future collaboration) via the same rebase machinery the
architecture calls for. Leaf node library at `libraries/piecetable/`. See
[journal.md](journal.md) "v0.5 Rich Text Design."

**Shell** — the navigation interface (GUI, CLI, or TUI). A blue-layer tool: an
untrusted userspace process that translates navigational intent (find documents,
open something, switch contexts) into OS service operations (metadata queries,
document lifecycle). Pluggable — a different shell provides a different
interaction model. Ambient (must respond to system gestures even while an editor
is active), unlike editors which are modal. See
[foundations.md § The Tool Model](foundations.md#the-tool-model-under-exploration).

**Viewer** — the OS's built-in rendering of a document, always present. Opening
any file presents it via the viewer. Editors augment the view; they do not
replace it. The viewer is a pure function of state:
`file bytes + mimetype + view state → visual output`. See
[foundations.md § Viewer-First Design](foundations.md#viewer-first-design).

---

## IPC

**Channel** — a bidirectional IPC connection between two processes. Two shared
memory pages (one ring per direction), created by the kernel. The `ipc` library
provides lock-free SPSC ring buffer mechanics; the `protocol` library defines
message types. Notification via `channel_signal` syscall. See
[design/userspace.md § 1.5](userspace.md) and `libraries/ipc/`.

**Event ring** — a SPSC ring buffer of 64-byte messages over a shared memory
page. Used for discrete events where order and count matter: key presses, button
clicks, configuration messages. One of the two IPC mechanisms (the other is
state registers). See [design/userspace.md § 0](userspace.md).

**State register** — an atomic value in shared memory, overwritten by the
producer, read once per frame by the consumer. Used for continuous data where
only the latest value matters: pointer position. Zero queue, zero overflow. One
of the two IPC mechanisms (the other is event rings). See
[design/userspace.md § 0](userspace.md).

---

## Displaced Terms

Terms this system deliberately avoids, and why.

**App / Application** — bundles too many responsibilities: rendering, editing,
file format ownership, window management, data storage. This system decomposes
it: the OS renders (→ viewer), tools edit (→ editor), formats are OS-managed (→
media type), windows are OS chrome (→ compositor / workspace), storage is the
filesystem (→ document). No single component holds all those roles.

**Folder / Directory** — an organizational container in a path hierarchy. This
system organizes by queryable metadata (→ attribute, catalog). Paths exist as a
filesystem internal but are not the organizing principle and are not
user-facing.

**Open / Close** — implies an application claims exclusive ownership of a file.
Documents are always viewable; editors bind temporarily (→ operation, edit
protocol). The document exists independently of any tool.

**Save** — implies a pending-changes model where edits accumulate in memory
until explicitly flushed. Edits write immediately via COW (→ operation). There
is no unsaved state. There is no "save before closing?" dialog. Every edit is
durable the moment it happens.

**Window** — a rectangular region owned by an application, managed by a window
manager. This system has no window manager in the traditional sense. The
workspace (→ workspace) is a compound document; the arrangement of content is a
layout concern, not a windowing concern. Leaning toward one-document-at-a-time,
non-windowed.
