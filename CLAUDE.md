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
13. **Compound documents (SETTLED):** Manifests with references + layout model. Five fundamental layouts: flow, fixed canvas, timeline, grid, freeform canvas. Layout engine mediates cross-type interactions. Translators (leaf nodes) handle import/export to external formats (docx, pptx, etc.).

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

**Session 2026-03-08 (latest):** Settled driver model (userspace drivers with kernel-mediated MMIO mapping + interrupt forwarding) and filesystem placement (userspace service; kernel owns COW/VM mechanics, filesystem manages on-disk layout). Recognized the kernel has converged to a microkernel — not by ideology, but because each sub-decision independently pushed complexity outward. Identified the kernel's identity: "handle multiplexer with one event-driven wait primitive." Documented new kernel primitives needed: `wait_any` (event multiplexing), device handles (MMIO + interrupts), timers, futex, DTB parser. All recorded in decisions.md (Decision #16), journal.md, and kernel DESIGN.md (§8.1–8.6).

**Decision #16 sub-decisions settled:** Soft RT, no hypervisor (EL1), preemptive + cooperative, traditional privilege (EL0), split TTBR, OS-mediated handles, ELF, ring buffer IPC, three-layer process arch, EEVDF + scheduling contexts, SMP (4 cores), from-scratch kernel (Rust), **userspace drivers** (MMIO mapping + interrupt forwarding), **userspace filesystem** (kernel owns COW/VM, filesystem manages on-disk layout). Kernel is a microkernel by convergence.

**Decision #16 sub-decisions open:** Filesystem COW on-disk design (research complete, placement settled).

**Kernel implementation done (this session):** DTB parser (`device_tree.rs`, 260 lines, 14 host tests, FDT magic scan for QEMU/macOS compatibility). Futex (`futex.rs`, 64-bucket PA-keyed wait table, two syscalls #10/#11, lost-wakeup prevention via pending flag). L3 page table fix (pre-kernel area now mapped for DTB access). 12 syscalls total. File naming convention: `dtb.rs` → `device_tree.rs`, `elf.rs` → `executable.rs`, `FutexTable` → `WaitTable`.

**Syscall API leaning:** 12 syscalls in three families. Handle family: `wait(handles[])` (multiplexer, subsumes old `channel_wait`), `close(handle)`, `signal(handle)`. Synchronization: `futex_wait`, `futex_wake`. Scheduling: `sched_create/bind/borrow/return`. Plus `exit`, `yield`, `write` (debug). Generic verbs on typed handles. OS service uses reactive/stream composition on top of `wait`. File naming convention: purpose-driven names, not acronyms or format names.

**Kernel implementation next:** `wait` syscall (handle-based event multiplexer). Then: timer handles, interrupt forwarding, wire DTB into device init, migrate virtio drivers to userspace.

**Kernel code:** `system/kernel/` (32 source files) + `system/user/{init,echo,libsys}/` + `system/host-tests/` (154 tests across 11 files). Boots on QEMU `virt` with 4 SMP cores, EEVDF scheduler with scheduling contexts, two user processes with IPC. Three-tier memory (buddy + slab + linked-list). Full process cleanup on exit.

**Design side next:** Layout engine (#15, highest-leverage unsettled design decision). Interaction model (#17, also unblocked). Filesystem COW on-disk design (research complete, ready for design discussion).

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
