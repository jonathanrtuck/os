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
10. **Rendering technology:** Web engine as substrate, programs not constrained to JS/HTML
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

**Session 2026-03-07:** Completed all 7 kernel spike steps plus a 5-step post-spike plan (handle table, multiple processes + user-side Rust, IPC channels, process lifecycle, better allocator). The spike's original question — is a from-scratch kernel tractable? — is answered. The kernel now has full resource lifecycle: processes are created, run, communicate via shared-memory IPC, exit, and all resources are properly reclaimed.

**Kernel code exists:** `system/kernel/` + `system/user/{init,echo}/` — ~2,500 lines across 21 source files. Boots on QEMU `virt`, drops EL2→EL1, sets up MMU with split TTBR, preemptive round-robin scheduler, two user processes at EL0 with per-process address spaces communicating via IPC channels. Linked-list heap allocator with coalescing (replaced original bump allocator). Full process cleanup on exit: handle table drain, address space teardown (TLB invalidation, page/table freeing, ASID recycling), kernel stack deallocation, channel endpoint cleanup. Scheduler reaps exited threads on next schedule tick. Modules: boot.S, exception.S, main.rs, memory.rs, heap.rs (linked-list allocator), page_alloc.rs, asid.rs (with recycling), addr_space.rs (owned/shared page tracking), channel.rs (shared-memory IPC + signal/wait), handle.rs (per-process handle table), elf.rs, build.rs, scheduler.rs (+ reaper), thread.rs (+ Drop for stack cleanup), syscall.rs (exit, write, yield, handle_close, channel_signal, channel_wait), timer.rs, gic.rs, uart.rs, mmio.rs. User programs: system/user/init/ and system/user/echo/ (Rust, compiled by build.rs). Single-core only.

**Decision #16 sub-decisions settled:** Soft RT (not hard), no hypervisor (EL1 not EL2), preemptive + cooperative multitasking, traditional privilege model (all non-kernel code at EL0), split TTBR (TTBR1 for kernel, TTBR0 per-process), OS-mediated handles for access control (per-process handle table, read/write rights, kernel-enforced), ELF as binary format, IPC via shared memory ring buffers with handle-based access control, three-layer process architecture (kernel EL1 + OS service EL0 trusted + editors EL0 untrusted).

**Decision #16 sub-decisions tentative:** From-scratch kernel, Rust as kernel language. Spike completed without hitting blocking obstacles — evidence favors tractability, but final decision deferred.

**Decision #16 sub-decisions open:** Driver model, filesystem (COW required), scheduling algorithm, multi-core.

**IPC summary:** Channels are shared memory ring buffers accessed via handles. Kernel creates channels, maps shared memory, validates messages at trust boundaries (control plane). Data flows directly through shared memory (not through kernel). Documents are memory-mapped separately — ring buffers carry only control messages (edit protocol, input events, overlays, queries). One mechanism for all IPC.

**Process architecture:** One OS service process (EL0, trusted) handles rendering, metadata, input routing, compositing. Editors are separate EL0 processes (untrusted). Kernel (EL1) handles hardware, memory, scheduling, IPC setup, handle management, message validation. Primary IPC is editor ↔ OS service. See `design/architecture.mermaid`.

**Previous sessions:** Established working mode (thinking partner, not project manager), exploration journal, implementation readiness table. Settled decisions #9 (edit protocol) and #14 (compound documents). Formalized glossary, external boundaries, adaptation layer principle.

**Risk tracking:** Reversibility & Risk table added to decisions.md. Tracks confidence level, revisit triggers, fallbacks, and blast radius for every non-axiomatic settled decision.

**What to explore next:** Rendering technology (#11) is the highest-leverage unsettled _design_ decision. Follow the designer's interest. On the kernel side, the 5-step post-spike plan is complete. Natural next implementation steps if continuing: driver model, filesystem (COW required by undo design), multi-core support, or more sophisticated scheduling.

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
