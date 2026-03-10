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

**Session 2026-03-10 (latest):** Display engine build plan complete — all three steps done. Init became a proto-OS-service. Full display pipeline working end-to-end on QEMU.

1. **Display pipeline complete (steps a→b→c all done).** Init allocates DMA framebuffer → shares with compositor via `memory_share` → compositor draws demo scene → signals init → GPU driver presents to display. Boots cleanly, all processes spawn, communicate, and exit.

2. **Init is now a proto-OS-service** (`system/platform/init/`). Kernel spawns only init. Init reads a device manifest from kernel channel shared memory, spawns all other processes (drivers + compositor), orchestrates the display pipeline. Embeds all other ELFs via build.rs `include_bytes!`. Matches Fuchsia component_manager / seL4 root task / QNX procnto pattern.

3. **Kernel Phase 7 (memory sharing) done.** Syscall #24 (`memory_share`) maps physical pages from caller into target process's shared memory region. Per-process channel SHM bump allocator for correct addressing. 25 syscalls total.

4. **Alignment bug found and fixed.** Device manifest had u64 field at non-8-byte-aligned offset — undefined behavior in Rust's `read_volatile`. Silent process death (user fault handler diagnostic message never printed — known kernel bug to investigate).

**Still open from previous sessions:** Trust/complexity orthogonality (solid), blue-wraps-all-sides (solid), shell is blue-layer (leaning), one-document-at-a-time (leaning), compound document editing (unresolved tension — connected to compositor tree model).

**Decision #14 sub-decisions open:** Referenced vs owned parts, mimetype of the whole document, manifest format, COW atomicity for multi-part documents, filesystem organization of manifests + content files.

**Decision #16 sub-decisions open:** Filesystem COW on-disk design (research complete, placement settled). New constraint: metadata DB must be on COW filesystem for uniform rewind. Favors time-correlated snapshots.

**Two tracks forward:** GUI (more interesting, closer to the project's soul) and filesystem (important infrastructure, but doesn't feel like anything without a visual layer). GUI track: display engine complete, next is design exploration — Decisions #15 (layout engine API), #17 (interaction model: shell/editor boundary, compound editing), #10 (view state). FS track: COW on-disk design (Decision #16, independent).

**System code:** `system/kernel/` (35 source files), `system/platform/{init,compositor,drivers/{virtio-blk,virtio-console,virtio-gpu}}/`, `system/library/{sys,virtio,drawing}/`, `system/user/echo/`, `system/test/` (257 tests across 16 files). Boots on QEMU `virt` with 4 SMP cores, EEVDF scheduler, full display pipeline (init → drivers → compositor → GPU → pixels on screen). 25 syscalls. Three-tier memory (buddy + slab + linked-list) with address-based dealloc routing. Full process cleanup on exit.

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
