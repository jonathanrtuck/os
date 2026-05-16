# Roadmap

Milestone plan for the document-centric OS. Foundation-up: generic
infrastructure first, UX iteration last.

## Completed

| Version        | Theme                                                                   | Completed  |
| -------------- | ----------------------------------------------------------------------- | ---------- |
| v0.1           | Microkernel (memory, scheduling, IPC, syscalls, handle table, virtio)   | 2026-03-10 |
| v0.2           | Kernel audit, display pipeline, rendering architecture (3 backends)     | 2026-03-19 |
| v0.3           | Rendering + UI foundation (animation, composition, text, visual polish) | 2026-03-25 |
| v0.4           | Filesystem, Document Store (COW, snapshots, undo)                       | 2026-03-26 |
| v0.5           | Rich text (piece table, multi-style runs, content-type dispatch)        | 2026-03-30 |
| v0.6 (rewrite) | Kernel rewrite from first principles                                    | 2026-04-19 |
| v0.7           | Userspace rebuild — services, drivers, integration, rendering           | 2026-05-11 |
| v0.8           | Media pipeline, rendering v2, content service architecture              | 2026-05-15 |

### v0.6 — Kernel Rewrite

Complete kernel rewrite guided by
`design/research/kernel-userspace-interface.md`. 35 syscalls, 6 object types
(VMO, Endpoint, Event, Thread, Address Space, Resource). Framekernel discipline
(all `unsafe` in `frame/`). SMP up to 8 cores. Synchronous call/recv/reply IPC
with priority inheritance, direct switch (−13.4% IPC latency). 12-phase
verification campaign: 557 tests, 4 fuzz targets, 33 proptests, mutation
testing, Miri, sanitizers. 26 bugs found and fixed. Benchmark baselines gated.

### v0.7 — Userspace Rebuild

Rebuilt the full userspace stack on the verified kernel. Five phases:

1. **Protocol + service infrastructure** — protocol crate (17 message types),
   service pack tool (SVPK), init, name service with handle transfer
2. **Drivers** — console (PL011), virtio-input, virtio-blk, Metal render,
   virtio-9p, virtio-rng, virtio-snd. Kernel extended with device VMOs, DMA VMOs
   (capability-gated via Resource type)
3. **Core libraries** — 13 libraries (scene, drawing, animation, fs, piecetable,
   layout, icons, fonts, render, store, png, jpeg, wav), 1,100+ tests
4. **Core services** — store (COW filesystem over blk), document (shared VMO
   buffer, undo ring), layout (word-breaking, seqlock results), presenter (scene
   graph builder), text editor (key dispatch, multi-hop RPC), filesystem (VFS
   over 9p), PNG decoder, JPEG decoder, audio mixer
5. **Integration + visual chrome** — full boot (15 services), scene-graph
   compositor, analytical shadows, Content::Path rasterizer, title bar + clock,
   page geometry, hardware cursor, pointer interaction (click/double/triple +
   drag selection), content-type typography (Inter + JetBrains Mono + Source
   Serif 4, font fallback), document switching (Ctrl+Tab spring animation,
   text + image spaces), rich text rendering (proportional layout, italic axis),
   120Hz frame loop, play button with audio playback

### v0.8 — Media + Architecture

Three major efforts built on the v0.7 service infrastructure:

1. **Media pipeline** — video playback (H.264 hardware decode via VideoToolbox,
   MP4/AVI container parsing, PTS-scheduled frame presentation, zero-copy
   rendering via host texture binding), audio playback (AAC decode via
   AudioConverter, WAV, virtio-snd driver, audio mixer service, synchronized A/V
   with play/pause/seek). Document switching generalized from 2-space to N-space
   strip with spring animation.

2. **Rendering pipeline v2** — 8 optimizations driven by video playback
   performance: vsync alignment (host counter), damage tracking (scissored
   partial redraws, TBDR-aware retained blit), async frame submission
   (double-buffered DMA, CPU/GPU overlap), atlas LRU eviction (shelf-based,
   dirty rect upload), vertex buffer caching (skip scene walk on image-only
   changes), GPU compute glyph compositing, single-submission emit (one command
   buffer per frame), integer presenter math (eliminated FP context
   save/restore).

3. **Content service architecture** — decomposition of the monolithic presenter
   into the viewer model. Viewer trait with enum dispatch (TextViewer,
   ImageViewer, VideoViewer, WorkspaceViewer). View tree as SSOT — ViewSubtree
   with pre-computed layout positions. Portal-based zero-copy composition —
   workspace viewer's tree references child viewer subtrees by index.
   ViewerRegistry maps mimetype patterns to viewers with specificity ranking.
   ContentCommand: viewers return intent, presenter dispatches. Decoder services
   eliminated — viewers call codec libraries directly. Directory restructured:
   system/, drivers/, codecs/, shared/, editors/.

## Current State

19 production services. 589 tests + 4 fuzz targets + 33 property tests + 34
bare-metal integration tests. 4 content types: text/plain (full editing),
image/png + image/jpeg (viewing), video/mp4 + video/avi (playback with audio).

**What's in place for future milestones:**

- **Viewer abstraction** — mimetype → viewer → ViewSubtree → scene graph. New
  content types plug in by implementing the Viewer trait and registering with
  the ViewerRegistry. The compositor and rendering pipeline are
  content-agnostic.
- **Portal composition** — the workspace IS a compound document. Child viewers
  compose via portals (zero-copy subtree references). Extending this to
  document-level compound composition is the natural next step.
- **Rendering pipeline** — native Metal renderer with analytical shadows, CPU
  path rasterizer, glyph atlas, font shaping, damage tracking, GPU compute.
  Decision #11 approach B (native renderer) is the reality.
- **Interaction framework** — ContentCommand + hit testing + cursor shapes +
  click/drag selection. The mechanism for routing input to the right viewer
  exists.
- **Data/control split** — bulk data (scene graph, document buffer, decoded
  pixels) via shared memory VMOs. Control (edit requests, input events) via sync
  IPC. Hot path is invisible to the kernel.

## Planned

| Version    | Theme                    | Character      | Key Deliverables                                                                                                                            |
| ---------- | ------------------------ | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| **v0.9**   | **Compound Documents**   | Foundation     | Manifest format. Spatial composition (flow + canvas). Layout engine for multi-type docs. Sub-document editing. Translator proof-of-concept. |
| **v0.10**  | **CLI / TUI**            | Foundation     | The other native OS interface. Shell model, tools-as-subshells, structured pipes. Document query integration.                               |
| **v0.11**  | **Realtime & Streaming** | Foundation     | Conversations, presence, streaming media as document types. Local prototype with mock transport. Temporal axis of compound docs.            |
| **v0.12**  | **Network**              | Infrastructure | Network stack, TCP/IP, DNS, TLS. Unlocks real transport for v0.11's realtime content types.                                                 |
| **v0.13**  | **Web**                  | Foundation     | HTML as compound document. Web engine as translator (approach B). Depends on network + compound docs + layout.                              |
| **v0.14**  | **Real Hardware**        | Infrastructure | Apple Silicon bare-metal target. Driver work behind existing interfaces.                                                                    |
| **v0.15+** | **UX Iteration**         | Polish         | GUI + CLI together. Document browse/search. Look, feel, interaction, animation. Where the document-centric thesis is tested.                |
| **v1.0**   | **Ship**                 |                | Whatever "done" means.                                                                                                                      |

### v0.9 — Compound Documents

The content service architecture (v0.8) provides the mechanism: viewers produce
subtrees, portals compose them, the registry routes mimetypes to viewers. v0.9
builds the compound document model on top of this infrastructure.

**What it delivers:**

- **Manifest format** — the data structure describing a compound document: URI
  content references, display axes (width/height/depth/time), positioning mode
  (flow/grid/absolute), edge data (placement + viewport). Schema designed in
  `design/research/manifest-model.md`. Internal to the OS, no external interop
  requirement.
- **Layout composition** — at minimum flow and grid layout modes over spatial
  axes. The layout engine computes positions for heterogeneous content parts
  within a compound document.
- **Compound document viewer** — uses portals to compose sub-viewers. A rich
  text document with inline images, or a slide deck with text + image regions,
  both rendered through the same viewer composition model.
- **Sub-document editing** — editing text within a compound document using the
  same text editor. Resolves the nesting tension from Decision #17.
- **Translator proof-of-concept** — at least one external format (e.g., Markdown
  with images, or a simple HTML subset) translated into the manifest format.

**Design decisions narrowed by v0.8:**

- **#10 View State:** The viewer trait's `rebuild()` and ViewSubtree define a
  concrete interface for view output. The remaining question is persistence —
  how view state survives across sessions (opaque blobs per the initial leaning,
  vs. structured).
- **#11 Rendering:** Settled in practice. Approach B (native renderer, web
  translated inward) is the implementation. The web engine becomes a translator
  in v0.13, not a rendering substrate.
- **#15 Layout Engine:** Text layout exists (the layout service). ViewSubtree
  includes pre-computed positions. The remaining scope is spatial composition
  for compound documents — how heterogeneous parts are positioned relative to
  each other.
- **#17 Interaction Model:** ContentCommand + viewer dispatch + hit testing
  provide the input routing framework. The remaining questions are navigation
  between documents and compound editing (how editor nesting works).

### v0.10 — CLI / TUI

The other native OS interface (GUI and CLI are equally fundamental — Guiding
Belief #4). Placed after compound documents so the CLI operates on rich content,
not just text files. Shell model, tools-as-subshells, structured pipes. Document
query integration — the CLI's equivalent of "open a file" is a query, not a
path.

### v0.11 — Realtime & Streaming

Conversations, presence, streaming media as document types. Local prototype with
mock transport. Exercises the temporal axis of compound documents (Decision
#14). Designed to work without a network stack — when the network arrives in
v0.12, it plugs in as transport beneath content types that already work locally.

### v0.13 — Web

HTML as a compound document, translated inward via a web engine in the blue
layer (Decision #11 approach B). The web engine parses HTML/CSS/JS and produces
the OS's compound document format — manifests with spatial layout + referenced
content. The native renderer displays the result through the same pipeline as
every other document type. Depends on network (v0.12) + compound docs (v0.9) +
layout engine (v0.9).

## Sequencing Rationale

**Foundation-up, UX-last.** v0.1–v0.14 build generic infrastructure behind clean
interfaces. UX iteration comes at the end (v0.15+) when all the pieces are on
the table.

**Compound documents next (v0.9).** The viewer/portal/registry infrastructure
from v0.8 is the mechanism; compound documents are the thesis of the entire
project (Decision #2 + #14). Building them now, while the viewer abstraction is
fresh, avoids the model calcifying around single-document assumptions.

**Design decisions merged into implementation (v0.9, not a separate
milestone).** The original plan (old v0.9) separated "settle interfaces" from
"implement compound docs" (old v0.10). But v0.8's content service architecture
already created concrete interfaces for #10/#15/#17. What remains is
implementing compound document layout and composition behind those interfaces —
which was the old v0.10's job. Merging them avoids a two-step that the
architecture already shortcut.

**Realtime before network (v0.11 before v0.12).** Forces the realtime content
model to be designed without assuming a specific transport. Conversations and
streams become document types with temporal semantics, not "network features."
When the network stack arrives, it plugs in as transport beneath content types
that already work locally.

**CLI as its own milestone (v0.10).** The CLI is a fundamental OS interface, not
an afterthought. Placed after compound documents (rich enough to be interesting)
but before network/web.

**Web last among content milestones (v0.13).** HTML is a compound document
format. The translator pattern requires both the compound document model to
translate INTO and the network stack to fetch content FROM.

**Flexibility points:** v0.10 (CLI) and v0.11 (realtime) are swappable — neither
depends on the other. Everything else chains naturally.

## Descoped

- Multi-display (single display only — interfaces clean enough to add later)
- Self-hosting (development stays on macOS)

## Decision Dependencies

Unsettled decisions and when they get resolved:

| Decision              | Status                                                                                  | Resolved in |
| --------------------- | --------------------------------------------------------------------------------------- | ----------- |
| #10 View State        | Interface exists (viewer trait); persistence story unsettled                            | v0.9        |
| #11 Rendering         | Settled by implementation: approach B (native renderer, web as translator)              | v0.8        |
| #15 Layout Engine     | Text layout done; compound spatial layout unsettled                                     | v0.9        |
| #17 Interaction Model | Framework exists (ContentCommand, hit testing); navigation + compound editing unsettled | v0.9–v0.10  |

Settled decisions: #1–9, #12–14, #16, #18. See `decisions.md`.
