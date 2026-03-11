# Project: Document-Centric OS

## What This Is

A personal project exploring an alternative operating system design where documents (files) are first-class citizens and applications are interchangeable tools that attach to content. This is a learning/exploration project, not a product.

## Project Phase

**Design phase with research spikes.** Primarily working through architecture and design decisions. Code is written selectively — either to validate uncertain assumptions (research spikes) or to flesh out components backed by settled decisions. The designer alternates between design exploration and coding based on interest, not a linear plan.

## Working Mode

This is a long-running exploration project with no deadline. Sessions may be days or months apart. The designer wants a **thinking partner**, not a project manager:

- **Explore, don't push.** Help think through ideas, poke holes, surface tradeoffs. Don't rush toward decisions or implementation.
- **Hold context across sessions.** Use MEMORY.md, the exploration journal, and "Where We Left Off" to resume seamlessly.
- **Connect the dots.** Flag similarities, inconsistencies, or connections to previous discussions. Remind when something was already explored or rejected.
- **Guide gently.** Suggest topics that would address gaps in the emerging design. Ask for clarity when needed. Flag dead ends or common traps.
- **Research partner.** Help investigate historical OSes, prior art, and existing approaches. Bring relevant examples into design discussions.
- **Respect the pace.** The designer may want to deep-dive a topic, switch to coding, or just chat loosely. Follow their energy.

## Key Design Documents

Read these before making any design suggestions:

- `design/foundations.md` — Guiding beliefs, glossary, external boundaries, content model (3-layer type system), viewer-first design, editor augmentation model, edit protocol, undo/history architecture
- `design/decisions.md` — 17 tiered decisions with tradeoffs, implementation readiness table, dependency chains between decisions
- `design/decision-map.mermaid` — Visual dependency graph of all decisions
- `design/architecture.mermaid` — System architecture diagram (process layers, IPC, memory mapping)
- `design/journal.md` — Open threads, discussion backlog, insights log, research spikes. The "pick up where you left off" document.
- `system/DESIGN.md` — Userspace architecture: libraries, services, drivers. Component status (foundational vs scaffolding), constraints, gaps, dependency map. Companion to `system/kernel/DESIGN.md`.
- `design/concept.md` — The core idea: OS → Document → Tool, mimetype evolution, layered rendering, compound documents

## Settled Decisions

1. **Audience & Goals (Tier 0):** Personal design project. Primary artifact is a coherent OS design. Build selectively to validate. Success = coherent design > working prototype > deep learning. Not a daily driver. Target: personal workstation (text, images, audio, video, email, calendar, messaging, videoconferencing, web, coding).

## Initial Leanings (Not Yet Committed)

2. **Data model (SETTLED):** Document-centric. The main axiom. OS → Document → Tool.
3. **Compatibility (SETTLED):** Rethink everything. No POSIX. Build on established standard interfaces (mimetypes, URIs, HTTP, Unicode, arm64), not implementations. Own native APIs. Development on host OS (macOS). Self-hosting is not a goal.
4. **Complexity (SETTLED):** Simple everywhere. Complexity is a design smell. Essential complexity pushed into leaf nodes behind simple interfaces. Connective tissue (protocols, APIs, inter-component relationships) must be simple. User simplicity > developer simplicity when in conflict, but conflicts signal unfinished design.
5. **File understanding (SETTLED):** OS natively understands content types. Mimetype is fundamental OS-managed metadata, not a userspace convention. Declaration at creation, content detection as fallback. Standard formats ensure interop.
6. **View vs edit (SETTLED):** View is default, edit is deliberate. Applies to all content including live/streaming. Editors bind to content types, not use cases (same text editor for documents, chat, email). OS interfaces (GUI/CLI) are not documents.
7. **File organization (SETTLED):** Rich queryable metadata (automatic, extracted, user-applied). Simple query API (equality, comparison, AND/OR) backed by embedded DB. SQL escape hatch for power users. Users navigate by query, not path.
8. **Editor model (SETTLED):** Editors are modal plugins (one active per document). No pending changes — edits write immediately (COW filesystem). No "save." OS always renders (pure function of file state). OS provides content-type interaction primitives (cursor, selection, playhead). Editor overlays for tool UI chrome only. One path, no alternative rendering or save paths.
9. **Edit protocol (SETTLED):** Modal tools, immediate writes, thin protocol. Editors call beginOperation/endOperation; OS snapshots at boundaries. OS is semantically ignorant — tracks ordering and attribution only. Content-type rebase handlers (leaf nodes) optionally enable selective undo and collaboration. Cross-type interactions handled by layout engine.
10. **Rendering technology (SETTLED):** Existing web engine integrated via adaptation layer. Webpage is a compound document (HTML=manifest, CSS=layout, media=referenced content) — same translator pattern as .docx. Rendering direction leaning B (native renderer, web translates inward to compound doc format) over A (web engine renders everything). B preserves "OS renders everything" and lets the renderer do things CSS can't express. Engine choice and rendering direction deferred to prototype phase; prototype on macOS.
11. **Undo (SETTLED):** COW snapshots at operation boundaries for sequential undo. Global undo regardless of which editor. Selective undo requires content-type rebase handlers (same investment as collaboration). Cross-session history via filesystem snapshot retention.
12. **Collaboration (SETTLED):** Designed for, build later. Same content-type rebase handlers needed for selective undo unlock collaboration. Architecture supports it; implementation deferred.
13. **Compound documents (SETTLED):** Uniform manifest model — every document is a manifest referencing content files. Three composable relationship axes: spatial (flow, canvas, grid, freeform), temporal (simultaneous, sequential, timed), logical (flat, sequential, hierarchical, graph). Simple/compound is internal property, not user-facing. Layout engine mediates cross-type interactions. Translators handle import/export. Content-type registration via editor metadata.

## Key Architectural Principles (Settled)

- Everything-is-files is architectural, not UX. Users see abstractions (documents, conversations, meetings), not files.
- File paths are metadata, not the organizing principle.
- GUI and CLI are equally fundamental OS interfaces, not applications.
- How view/edit translates to CLI is an open question (tools-as-subshells? read-commands-always-safe?).
- Prototype success = demonstrating the concept works and scales, even with only 1-2 content types fully implemented. Breadth is not required; depth on the interesting parts is.
- If the design has value, it could be open-sourced for community build-out. But no expectation of that — design coherence is the goal regardless.

## Decision Dependencies (Critical Chains)

1. Data model → File understanding → Editor model → Edit protocol → Undo/Collaboration
2. Editor model → Rendering technology → Compound documents → Layout
3. Compatibility stance → Technical foundation
4. Data model + File organization → Interaction model

**Most influential decision:** #2 (Data Model). If document-centric is confirmed, most other decisions are constrained in useful ways.

## Where We Left Off

**Session 2026-03-11 (latest):** Input driver + event loop implementation. Keyboard input end-to-end. Wait timeout fix. **Kernel crash under rapid typing — FIXED.** Root causes: aliasing UB in syscall dispatch, `nomem` on DAIF/system register asm, deferred thread drop use-after-free, idle thread park bug. 11 fixes total (see `design/journal.md`). Headless stress test + property-based scheduler tests added. 20 scheduler tests pass. Pragmatic resolution: opt-level 1 (opt 2-3 have residual issue, deferred).

**Session 2026-03-10:** Structured IPC design, TrueType font rasterizer, alpha blending, overlapping surface compositing, userspace architecture audit.

1. **Input driver + event loops implemented (2026-03-11).** virtio-input keyboard driver reading evdev events, forwarding to compositor via cross-process IPC channel. Compositor runs event loop: wait for input → update text buffer → re-render → signal GPU. GPU driver runs present loop: wait for compositor → transfer+flush. Init creates cross-process channels (input→compositor, compositor→GPU), starts all processes, idles. Interactive text demo: typed characters appear on screen. QEMU `-device virtio-keyboard-device` added. **Known:** QMP `input-send-event` doesn't route to virtio-keyboard (QEMU limitation) — must type into display window. **Known:** kernel `wait` syscall doesn't implement finite timeouts (only poll or infinite block).

2. **Structured IPC designed.** Four sub-decisions settled: (a) one mechanism — ring buffers for everything, config = first message (Singularity pattern), no separate config path; (b) separate pages per direction — each channel has two 4 KiB pages, each a SPSC ring buffer; (c) fixed 64-byte messages — one AArch64 cache line, 4-byte type + 60-byte payload, 62 slots per ring; (d) split architecture — shared `ipc` library for ring mechanics, per-protocol payload definitions. Ring buffer layout designed in `system/DESIGN.md` §1.5. Kernel change: `channel::create()` allocates 2 pages. Pressure point documented: messages >60 bytes use shared-memory reference pattern. Prior art: io_uring, LMAX Disruptor, Singularity contracts. Implementation next.

3. **TrueType font rasterizer built and running on bare metal.** Zero-copy TTF parser (7 tables). Scanline rasterizer with 4× oversampling. ProggyClean.ttf embedded. 21 new tests (83 total).

4. **Alpha blending + compositor rewrite.** Porter-Duff source-over compositing. Three panels with per-pixel alpha, composited back-to-front. TrueType text demo.

5. **Userspace architecture audit and `system/DESIGN.md` created.** Systematic classification of every component. Five constraints documented, dependency map and roadmap.

**Previous session highlights (still relevant):** Display pipeline complete end-to-end. Init is proto-OS-service. Kernel Phase 7 (memory sharing) done. 27 syscalls. Alignment bug fixed (DFSC check + ISS diagnostics).

**Still open from previous sessions:** Trust/complexity orthogonality (solid), blue-wraps-all-sides (solid), shell is blue-layer (leaning), one-document-at-a-time (leaning), compound document editing (unresolved tension — connected to compositor tree model).

**Decision #14 sub-decisions open:** Referenced vs owned parts, mimetype of the whole document, manifest format, COW atomicity for multi-part documents, filesystem organization of manifests + content files.

**Decision #16 sub-decisions open:** Filesystem COW on-disk design (research complete, placement settled). New constraint: metadata DB must be on COW filesystem for uniform rewind. Favors time-correlated snapshots.

**Two tracks forward:** GUI (more interesting, closer to the project's soul) and filesystem (important infrastructure, but doesn't feel like anything without a visual layer). GUI track: input + event loops done → text layout next → editor process separation. Longer-term: Decisions #15 (layout engine API), #17 (interaction model), #10 (view state). FS track: COW on-disk design (Decision #16, independent).

**System code:** `system/kernel/` (35 source files), `system/services/{init,compositor,drivers/{virtio-blk,virtio-console,virtio-gpu,virtio-input}}/`, `system/libraries/{sys,virtio,drawing,ipc}/`, `system/user/echo/`, `system/test/` (83 drawing + 221 kernel = 304 tests across 16 files). Boots on QEMU `virt` with 4 SMP cores, EEVDF scheduler, interactive display pipeline with keyboard input + event loops. 27 syscalls. Userspace architecture documented in `system/DESIGN.md`.

## Design Discussion Rules

- Decisions should favor clarity and interestingness over market viability
- All decisions in the register are unsettled until explicitly committed
- When discussing tradeoffs, be honest about downsides — don't sell options
- Reference the decision register tiers and dependency chains
- New decisions should be recorded in the appropriate reference documents

## Reference Influences

- **Mercury OS:** Fluid, focused, familiar. Module/Flow/Space hierarchy. Intent-driven. Locus (command bar combining CLI + NLP + GUI). No apps, no folders. Artificial Collaborators. Mirrors (same module in multiple spaces).
- **Ideal OS:** Document database replaces filesystem for apps. Message bus as sole IPC. Compositor controlled by messages. Apps become small modules. Structured object streams instead of text pipes.
- **OpenDoc / Xerox Star / Plan 9 / BeOS:** Historical attempts at document-centric or radically simplified OS design.
