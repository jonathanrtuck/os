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

- `design/philosophy.md` — **Read first.** Two root principles and their consequences. The thinking framework behind every design decision.
- `design/foundations.md` — The core idea, guiding beliefs, glossary, external boundaries, content model (3-layer type system), viewer-first design, editor augmentation model, edit protocol, undo/history architecture
- `design/decisions.md` — 17 tiered decisions with tradeoffs, implementation readiness table, dependency chains between decisions
- `design/decision-map.mermaid` — Visual dependency graph of all decisions
- `design/architecture.md` — The system's architectural narrative: one-way pipeline, what each component understands, where responsibilities live, decision checklist
- `design/architecture.mermaid` — System architecture diagram (process layers, IPC, memory mapping)
- `design/journal.md` — Open threads, discussion backlog, insights log, research spikes. The "pick up where you left off" document.
- `system/DESIGN.md` — Userspace architecture: libraries, services, drivers. Component status (foundational vs scaffolding), constraints, gaps, dependency map. Companion to `system/kernel/DESIGN.md`.

## Settled Decisions

1. **Audience & Goals (Tier 0):** Personal design project. Primary artifact is a coherent OS design. Build selectively to validate. Success = coherent design > working prototype > deep learning. Not a daily driver. Target: personal workstation (text, images, audio, video, email, calendar, messaging, videoconferencing, web, coding).

## Settled Decisions (Continued)

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

**Session 2026-03-21 (latest):** Fixed metal-render transform+rounded-corner bug: SDF rounded-rect path was checked before transform path, so rotation/scale/skew were silently ignored on nodes with corner_radius. Fix: `emit_transformed_rounded_rect_quad()` — vertex NDC positions are transformed through the affine while texCoords stay in local pixel space (no shader changes needed). Removed all 21 demo nodes (composition, audit, animation) and test content — clean editor scene. N_POINTER renumbered 14→8, WELL_KNOWN_COUNT 29→9. "Rendering sample compound document" idea noted in journal for future visual test mode. Hypervisor multi-frame capture: `--capture 30,60,90 /tmp/prefix.png` produces numbered PNGs in a single boot cycle (backward compatible with single-frame). 2,046 tests pass. Next: v0.3 Phase 3 (Text & Interaction). Spec: `design/v0.3-spec.md`.

**Session 2026-03-21 (earlier):** Hypervisor extracted to standalone repo (`~/Sites/hypervisor/`) for other hobby OS developers. Metal render pipeline FEATURE-COMPLETE — all content types (None, Glyphs, Image, Path), 4x MSAA, scissor + stencil clipping, backdrop blur (linear-light 3-pass box blur via compute shaders), shadows, built-in screenshot capture (`--capture N PATH` / SIGUSR1). All 13 audit findings resolved (see `~/Sites/hypervisor/AUDIT.md`). Scene node truncation bug fixed (dangling `first_child` pointers on `set_count`). Stack overflow in metal-render fixed + RAM size now read from DTB. Default development path: `cd system && cargo run -r` launches hypervisor with Metal GPU. QEMU still available via `QEMU=1`. ~44 GB of ANGLE/virglrenderer/QEMU source trees deleted. 2,046 tests pass.

**Session 2026-03-20:** Backdrop blur rewrite COMPLETE. Replaced broken 9-tap Gaussian with mathematically correct three-pass box blur (CLT convergence to Gaussian). 5 commits across 7 tasks: (1) `drawing::box_blur_widths()` — W3C standard formula, integer-only arithmetic via 8.8 fixed-point + `isqrt_fp`, (2) `drawing::box_blur_3pass()` — O(1)-per-pixel running sums, tiled V-pass (TILE_COLS=8) for cache friendliness, rounded division prevents systematic darkening, (3) CpuBackend `apply_backdrop_blur()` — padded capture region (pad = sum of 3 half-widths) eliminates edge banding, writes back center portion only, (4) CpuBackend `render_shadow_blurred()` migrated to same algorithm, (5) virgil-render: `BlurRequest` expanded with bg color/corner_radius, bg skipped during scene walk (drawn post-blur), TGSI shaders replaced with BGNLOOP/ENDLOOP loop-based box blur (any radius), 6-pass ping-pong pipeline with padded capture, post-blur bg quad drawn on top. Constant buffer: single 8-dword upload (CONST[0]+CONST[1] in binding 0). 10 new box blur tests (width computation, 3-pass convergence, symmetry, no-darkening, reference Gaussian comparison ≤ ±3 levels). 2,013+ tests pass. QEMU visual verified: both CPU and virgl renderers show correct frosted glass with no edge artifacts. Plan: `design/plan-v0.3-blur-rewrite.md`. Next: v0.3 Phase 3 (Text & Interaction). Spec: `design/v0.3-spec.md`.

**Session 2026-03-20 (earlier, Phase 2):** v0.3 Phase 2 (Composition) CpuBackend COMPLETE. 7 commits: Node struct growth (120→136 bytes: `clip_path: DataRef`, `backdrop_blur_radius: u8`, `_reserved: [u8; 8]`), clip mask infrastructure (8bpp alpha rasterizer + 16-slot LRU cache in `render/clip_mask.rs`), CpuBackend clip path integration (offscreen render + per-pixel mask multiplication), backdrop blur (extract-blur-composite using existing Gaussian blur), pointer cursor (N_POINTER=14 as Content::Path, arrow shape, 3s auto-hide with 300ms fade), document switching fix (Ctrl+Tab shows centered test image, "Image" title bar), Phase 2 demo scenes (star clip + image, circle clip + text, frosted glass panel). WELL_KNOWN_COUNT bumped to 15. Phase 1 demo animation code removed. 2,013 total tests pass. QEMU visual verified: star clip, circle clip, frosted glass all render correctly. virgil-render also complete: Task 4 (stencil-based clip path — clip fan → stencil write → stencil test for content) and Task 6 (two-pass separable Gaussian blur — 9-tap TGSI shaders, render-to-texture via cmd_blit_region, 1024×1024 intermediate textures). All 9 Phase 2 tasks done. 2,013 tests pass.

**Session 2026-03-20 (earlier, Phase 1):** v0.3 Phase 1 (Motion) COMPLETE. New `libraries/animation/` library (475+ lines): 24 easing functions, spring physics (4 presets), Lerp trait with gamma-correct sRGB color interpolation, Transform2D, 32-slot Timeline. Integrated into core: smooth scroll (spring physics, pixel-space f32 model), animated cursor blink (4-phase state machine with smooth fade), selection fade-in, document switch fade transition, demo scenes (bouncing ball, easing sampler). 11 commits, 2,004 total tests pass.

**Session 2026-03-20 (earlier):** Design folder cleanup (8 files deleted, concept.md merged into foundations.md, research/ subfolder, rendering-capabilities.md moved to system/). README and 4 CLAUDE.md files updated. v0.3 spec written and approved (`design/v0.3-spec.md`). Phase 1 plan written (`design/plan-v0.3-phase1.md`).

**Session 2026-03-18:** Virgl implementation plan COMPLETE (all 8 tasks) + Phase 5 COMPLETE (cpu-render merge). Both render pipelines are now single-process: `virgil-render` (GPU-accelerated) and `cpu-render` (CPU software). Init auto-detects at boot. 1,816+ total tests pass.

**Phase 5: cpu-render merge (2026-03-18):** Merged `compositor/` + `virtio-gpu/` into single `cpu-render/` process. Key insight: cpu-render self-allocates framebuffers via `dma_alloc`, making its init handshake identical to virgil-render's. Eliminated compositor→GPU IPC channel, MSG_PRESENT/MSG_PRESENT_DONE protocol, one process boundary. Old compositor/ and virtio-gpu/ deleted (no parallel implementations). Init's two pipeline functions (`setup_virgl_pipeline()` / `setup_display_pipeline()`) unified into a single `setup_render_pipeline(name, ...)` — the `name` parameter (`b"virgl"` or `b"cpu-render"`) drives diagnostic output only.

**Task 8: Init integration (2026-03-18):** Added `probe_virgl()` to init — maps GPU MMIO region, reads virtio feature bits, checks `VIRTIO_GPU_F_VIRGL` (bit 0). Selects `VIRGIL_RENDER_ELF` or `CPU_RENDER_ELF` accordingly, then calls `setup_render_pipeline()` for either backend. No new IPC messages needed — simpler than planned.

**Virgl Tasks 1-7 (2026-03-17/18):** Virgil3D GPU driver (`virgil-render`) built from scratch. All four content types render via GPU: backgrounds (color quads), text (glyph atlas), images (BGRA textures), paths (stencil-then-cover). See `project_virgl_progress.md` memory for details.

**Triple buffering + flow control (2026-03-17, earlier):** Replaced double-buffered scene graph with triple buffering (mailbox semantics). `TripleWriter`/`TripleReader` replace `DoubleWriter`/`DoubleReader` — `acquire()` always succeeds (writer never blocks), `publish()` atomically makes buffer latest, reader always gets most recent (intermediate frames silently skipped). `copy_front_to_back()` eliminated entirely. Core scene dispatch simplified: all update paths use acquire/publish, no retry logic. `MSG_PRESENT_DONE` (ID 21) added for GPU→compositor completion signaling — compositor tracks in-flight framebuffers, waits for GPU before reuse. Compositor always renders to non-displayed buffer (fixes tearing on partial updates). GPU driver dirty rect coalescing now unions all rects instead of keeping only the last. Damage tracking `update_bounds_for_skip()` keeps `prev_bounds` consistent across skipped frames. Init allocates `TRIPLE_SCENE_SIZE` shared memory. 23 new tests, 1,791 total pass. QEMU visual verified + 68s stress test on 4 SMP cores.

**Session 2026-03-16:** Tickless idle + IPI wakeup mission COMPLETE — GICv3 migration, cross-core IPI wakeup, tickless idle scheduling. 70 new tests, 1,768 total pass.

**GICv3 migration + tickless idle (2026-03-16):** Full interrupt controller migration from GICv2 to GICv3. `InterruptController` trait with `GicV3` implementation using system register CPU interface (ICC\_\*) and MMIO distributor/redistributor. GICv2 code deleted entirely — no parallel implementations. boot.S updated for ICC_SRE_EL2 during EL2→EL1 transition. All QEMU scripts updated to `gic-version=3`. IPI-driven cross-core wakeup: `try_wake` sends SGI 0 via ICC_SGI1R_EL1 to idle cores blocked in WFI. Per-core idle tracking (`is_idle` in `PerCoreState`). Tickless idle: `reprogram_next_deadline` replaces fixed 250Hz tick — deadline computed from timer objects, quantum expiry, and context replenishment. `TICKS_PER_SEC` removed. Lock-free deadline cache (AtomicU64) avoids STATE→TIMERS lock ordering. Fuzz test updated (syscall 27 now valid). 70 new tests (33 GICv3 + 7 idle tracking + 13 IPI + 17 tickless), 1,768 total pass.

**Session 2026-03-16:** Rendering architecture redesign COMPLETE — all three phases shipped.

**Rendering architecture Phases 1–3 (2026-03-16):** The full rendering pipeline redesign is done. Three phases delivered in sequence:

- **Phase 1: Extract Render Backend.** Created `libraries/render/` with `RenderBackend` trait and `CpuBackend` implementation. Moved ~3,100 lines of rendering code from compositor into the standalone library: scene_render.rs (tree walk, content rendering, transforms), compositing.rs, surface_pool.rs, damage.rs, cursor.rs. CpuBackend encapsulates all rendering state (glyph caches, damage tracker, surface pool, PREV_BOUNDS).
- **Phase 2: Geometric Content Types.** Replaced semantic scene graph content types (`Text`, `Path`) with geometric primitives (`Glyphs`). `Image` unchanged. `FillRect` was initially added but subsequently removed — solid fills now use `Content::None` with `node.background` color. Core emits one `Glyphs` node per visible text line, background-colored containers for cursor and selection highlights. SVG parser and all path rendering code eliminated — icons use the glyph cache. Render backend updated for new content types. QEMU visual output pixel-identical before and after.
- **Phase 3: Architecture Cleanup.** Compositor minimized to 174 lines — content-agnostic pixel pump with zero font knowledge, zero content dispatch, no SVG. Layout helpers (`layout_mono_lines`, `byte_to_line_col`, `scroll_runs`) confirmed in core (not scene library). Font handling boundary clean: core owns shaping + metrics, render backend owns rasterization + glyph caching. Scene library is purely geometric (no content-aware code).

**Post-cleanup architecture (updated 2026-03-18):**

```text
Core (shaping, layout, scene building) → Scene Graph (shared memory) → Render Service (tree walk, rasterization/GPU, compositing, present) → Display
```

Content types: `None`, `Path`, `Glyphs`, `Image`. Each render service (`metal-render`, `cpu-render`, or `virgil-render`) is a single process that reads the scene graph and produces display output. The render library (`libraries/render/`) provides `CpuBackend` used by cpu-render. See `design/rendering-pipeline.mermaid` and journal entry.

**Rendering architecture design (2026-03-16, earlier):** Top-down audit of the rendering stack committed to path-centric rendering: the pipeline is a series of data shape transformations (Hardware Events → Key Events → Write Requests → Scene Tree → Pixel Buffer → Display Signal) with four translators (Input Driver, Editor, Core, Render Backend). Key decisions: (1) glyph rasterization in the render backend, (2) tree-structured scene graph with geometric content types (Container, Glyphs, Image, Path), (3) explicit `RenderBackend` trait with `fn render(scene, surface)` — backend owns tree walk, rasterization, compositing, (4) multi-core rasterization internal to backend, (5) Glyphs type serves both text and monochrome icons (eliminates SVG parser), (6) render service is a single process (tree walk + render + present). See `design/rendering-pipeline.mermaid` and full journal entry.

**Session 2026-03-14:** Scene scroll fix + kernel TPIDR race fix (EC=0x21 crash resolved).

**Scene scroll fix (2026-03-14):** Text runs were positioned at absolute y coords without scroll adjustment — content overflowed viewport, cursor misaligned. Extracted layout helpers (`layout_mono_lines`, `byte_to_line_col`, `scroll_runs`) from core into scene library. Core pre-applies scroll via `scroll_runs`, positions cursor/selection viewport-relative. 11 new tests, 943 total pass.

**Kernel TPIDR race fix (2026-03-14, Fix 17):** Root cause of intermittent EC=0x21 crash under SMP. `schedule_inner` returned the new thread's context, but `TPIDR_EL1` was updated by exception.S _after_ the scheduler lock dropped (re-enabling IRQs). A timer IRQ in that window caused `save_context` to overwrite the old thread's Context with kernel-mode state. Fix: set `TPIDR_EL1` inside `schedule_inner` while the lock is held. Added `validate_context_before_eret` for defense-in-depth. 3000-key stress test passes, 943 tests pass.

**Session 2026-03-13:** Compositor split + scene graph design. Protocol crate refactor.

**Protocol crate (2026-03-13):** Created `libraries/protocol/` as single source of truth for all IPC message types and payload structs. 8 modules by protocol boundary. Zero duplicated constants or structs remain. Libraries now have proper Cargo.toml files; test crate uses normal Cargo dependencies instead of `#[path]` source includes.

**Compositor split design (2026-03-13, in progress):** The compositor (2260 lines) splits into OS service (document semantics) and compositor (pixels). Interface between them: a **scene graph in shared memory** — the OS service compiles document structure into a tree of typed visual nodes, the compositor renders them. Key insight: the screen is the root compound document. Layout and compositing are the same pipeline: document → scene graph → pixels. Prior art surveyed: Fuchsia Scenic, Core Animation, Wayland, game engines (Unity/Godot/Bevy). **Next:** scene graph node type design.

**Session 2026-03-11:** Filesystem design session. Major edit protocol revision + Files interface designed. Kernel bug audit mission running in parallel.

**Filesystem design (2026-03-11):** Comprehensive filesystem discussion settling several open questions. Key decisions: (1) **Editors are read-only consumers** — all writes go through the OS service via IPC. "Never make the wrong path the happy path": undo is automatic and non-circumventable, no editor cooperation required. (2) **Compound documents use copy semantics** — embedding creates an independent copy, COW shares physical blocks, provenance metadata enables "update to latest." (3) **Files interface designed** — 12 operations, files by opaque ID, no paths/permissions/locking/links. A dumb file store; all semantics live above. (4) **Prototype-on-host strategy** — implement Files against macOS during prototyping, build real COW FS later. (5) **Compound atomicity solved** — OS service as sole writer sequences multi-file writes, no FS transactions needed. (6) **Snapshot scope punted** — per-document vs global vs time-correlated still open, doesn't block interface.

**Earlier in session 2026-03-11:** Input driver + event loop implementation. Keyboard input end-to-end. Wait timeout fix. **Kernel crash under rapid typing — FIXED.** Root causes: aliasing UB in syscall dispatch, `nomem` on DAIF/system register asm, deferred thread drop use-after-free, idle thread park bug. 11 fixes total (see `design/journal.md`). Headless stress test + property-based scheduler tests added. 20 scheduler tests pass. Opt-level 3 verified crash-free (50M iterations, 137s headless stress test). Follow-up audit fixed break-before-make in guard page setup and added AddressSpace Drop for leak prevention.

**Session 2026-03-10:** Structured IPC design, TrueType font rasterizer, alpha blending, overlapping surface compositing, userspace architecture audit.

1. **Input driver + event loops implemented (2026-03-11).** virtio-input keyboard driver reading evdev events, forwarding to compositor via cross-process IPC channel. Compositor runs event loop: wait for input → update text buffer → re-render → signal GPU. GPU driver runs present loop: wait for compositor → transfer+flush. Init creates cross-process channels (input→compositor, compositor→GPU), starts all processes, idles. Interactive text demo: typed characters appear on screen. QEMU `-device virtio-keyboard-device` added. **Known:** QMP `input-send-event` doesn't route to virtio-keyboard (QEMU limitation) — must type into display window. **Known:** kernel `wait` syscall doesn't implement finite timeouts (only poll or infinite block).

2. **Structured IPC designed.** Four sub-decisions settled: (a) one mechanism — ring buffers for everything, config = first message (Singularity pattern), no separate config path; (b) separate pages per direction — each channel has two 4 KiB pages, each a SPSC ring buffer; (c) fixed 64-byte messages — one AArch64 cache line, 4-byte type + 60-byte payload, 62 slots per ring; (d) split architecture — shared `ipc` library for ring mechanics, per-protocol payload definitions. Ring buffer layout designed in `system/DESIGN.md` §1.5. Kernel change: `channel::create()` allocates 2 pages. Pressure point documented: messages >60 bytes use shared-memory reference pattern. Prior art: io_uring, LMAX Disruptor, Singularity contracts. Implementation next.

3. **TrueType font rasterizer built and running on bare metal.** Zero-copy TTF parser (7 tables). Scanline rasterizer with 4× vertical and 6× horizontal (subpixel) oversampling. GPOS kerning. Fonts: Source Code Pro (mono) and Nunito Sans (proportional), loaded from host via 9p. 21 new tests (83 total).

4. **Alpha blending + compositor rewrite.** Porter-Duff source-over compositing. Three panels with per-pixel alpha, composited back-to-front. TrueType text demo.

5. **Userspace architecture audit and `system/DESIGN.md` created.** Systematic classification of every component. Five constraints documented, dependency map and roadmap.

**Previous session highlights (still relevant):** Display pipeline complete end-to-end. Init is proto-OS-service. Kernel Phase 7 (memory sharing) done. 27 syscalls. Alignment bug fixed (DFSC check + ISS diagnostics).

**Still open from previous sessions:** Trust/complexity orthogonality (solid), blue-wraps-all-sides (solid), shell is blue-layer (leaning), one-document-at-a-time (leaning), compound document editing (unresolved tension — connected to compositor tree model).

**Decision #14 sub-decisions open:** Mimetype of the whole document, manifest format, filesystem organization of manifests + content files. **Settled this session:** referenced vs owned (copy semantics), COW atomicity (sole-writer solves it).

**Decision #16 sub-decisions open:** Filesystem COW on-disk design (deferred via prototype-on-host). Files interface designed (12 operations). Snapshot scope (per-document vs global vs time-correlated) punted. New constraint: metadata DB must be on COW filesystem for uniform rewind.

**Editor process separation implemented (2026-03-11, commit 827bcc8).** Text editor process (`system/user/text-editor/`) demonstrates the settled architecture: editor receives input events from compositor, translates to write requests (MSG_WRITE_INSERT, MSG_WRITE_DELETE), sends back via IPC. Compositor is sole writer to document buffer. Four processes in the display pipeline: GPU driver → input driver → text editor → compositor. Init creates compositor↔editor bidirectional channel (handle 3 in compositor, handle 1 in editor). Build and smoke tests pass.

**Files macOS prototype completed (2026-03-11).** `prototype/files/` — trait definition (12 operations: create, clone, delete, size, resize, map_read, map_write, snapshot, restore, map_snapshot, snapshots, delete_snapshot, flush), HostFiles implementation backed by regular macOS files, 21 tests all passing. Validates the interface design before building the real COW filesystem.

**Two tracks forward:** GUI (more interesting, closer to the project's soul) and filesystem (important infrastructure, unblocked by prototype-on-host strategy). GUI track: input + event loops done → editor process separation done → **read-only document mapping next** (give editor zero-copy read access) → text layout. Longer-term: Decisions #15 (layout engine API), #17 (interaction model), #10 (view state). FS track: Files prototype complete → integrate with OS service when document pipeline reaches that point.

**System code:** `system/kernel/` (33 .rs files + 2 .S + link.ld), `system/services/{init,core,drivers/{cpu-render,virgil-render,metal-render,virtio-blk,virtio-console,virtio-input,virtio-9p}}/`, `system/libraries/{sys,virtio,drawing,fonts,animation,scene,ipc,protocol,render}/`, `system/user/{echo,text-editor,stress,fuzz,fuzz-helper}/`, `system/test/`. `prototype/files/` (21 tests). Boots via native hypervisor (Metal GPU, default) or QEMU `virt` (virgl or software) with 4 SMP cores, EEVDF scheduler, interactive display pipeline with scene graph + render services. 28 syscalls. Userspace architecture documented in `system/DESIGN.md`.

## Design Discussion Rules

- Decisions should favor clarity and interestingness over market viability
- All decisions in the register are unsettled until explicitly committed
- When discussing tradeoffs, be honest about downsides — don't sell options
- Reference the decision register tiers and dependency chains
- New decisions should be recorded in the appropriate reference documents

## Kernel Change Protocol (MANDATORY)

**Every change to the kernel MUST follow this protocol.** These rules exist because 14 kernel bugs were found in a single investigation — most were latent bugs that only manifested under concurrent load. The kernel is the foundation; a bug here corrupts everything above.

### Unsafe code and inline assembly

- Every `unsafe` block MUST have a `// SAFETY:` comment explaining the invariant it relies on and what would break if violated.
- Inline asm `options()`: **never use `nomem` by default.** Only add `nomem` with explicit justification citing the instruction's side effects from the ARM architecture manual. `nomem` tells LLVM the instruction doesn't access memory — if that's a lie, LLVM will reorder memory accesses past it, creating races that only manifest at higher optimization levels or under SMP load.
  - **Safe to use `nomem`:** `mrs` of truly immutable registers (MPIDR_EL1, CNTFRQ_EL0), `wfe`/`wfi` hints.
  - **Never use `nomem`:** `msr` to any system register (DAIF, TTBR, TPIDR, timer registers), `dsb`/`isb` barriers, `hvc`/`smc` calls, `tlbi` instructions, any `ldr`/`str` (obviously reads/writes memory).
- When editing existing `unsafe` blocks, re-verify the SAFETY comment still holds with the change.

### Testing requirements

- `cargo test -- --test-threads=1` in `system/test/` MUST pass (all ~2,046 tests).
- Any change touching syscall handlers, scheduling, IPC (channel/timer/interrupt/futex), or thread lifecycle MUST be stress tested:
  ```sh
  # Boot QEMU with full display pipeline and send sustained input for 60+ seconds
  # Verify no crash (💥) or panic in serial output
  ```
- Property-based scheduler tests (`cargo test scheduler_state`) cover state machine invariants — run after scheduler changes.

### Anomaly tracking

- Any unexplained kernel behavior (spurious wakeups, unexpected fault codes, timing anomalies) MUST be documented in `design/journal.md` with `Status: open-bug`.
- Workarounds (retry loops, defensive checks) are acceptable as defense-in-depth but do NOT close the bug. The root cause investigation continues.
- Check for `Status: open-bug` entries in the journal at session start.

## Rust Formatting Convention (MANDATORY)

All `.rs` files follow standard Rust community conventions. Mechanical formatting is handled by `rustfmt` (config in `system/rustfmt.toml`); file layout is enforced by convention.

### Mechanical formatting (rustfmt)

A PostToolUse hook (`.claude/hooks/rustfmt-post-edit.sh`) runs `rustfmt --edition 2021` on every `.rs` file after Edit or Write. Manual runs: `rustfmt --edition 2021 <file>` or `cargo +nightly fmt` from `system/`.

`system/rustfmt.toml` enables two nightly features:

- `group_imports = "StdExternalCrate"` — separates std, external, and local imports with blank lines
- `imports_granularity = "Crate"` — merges imports from the same crate into one `use` statement

### File layout convention

Every `.rs` file follows this order:

1. **Module doc comment** (`//!`)
2. **Imports** (`use` statements, grouped by rustfmt)
3. **Constants and statics**
4. **Types in dependency order, each co-located with its `impl` blocks** — define a type, then immediately its `impl` block(s), before the next type. Within `impl` blocks: constructors first (`new`, `from_*`), then public methods, then private methods.
5. **Free functions**
6. **Tests** (`#[cfg(test)]` module)

**Co-located, not types-first.** Do NOT group all type definitions at the top with all `impl` blocks below (C header style). Each type lives next to its implementation. Types appear in dependency order: if type B uses type A, define A first.

### What this means in practice

- When creating a new file, follow the layout above.
- When editing an existing file, match its current layout. If the file uses the old types-first pattern, re-lay it out to co-located style while you're there.
- `rustfmt` handles all whitespace, indentation, line wrapping, trailing commas, and brace placement. Do not fight it.

## Visual Testing (MANDATORY)

**Every change that affects the display pipeline MUST be visually verified before declaring it done.** The user is not a tester. Do not ask them to check if something works. Do not declare a fix without seeing the result yourself. If you cannot close the verification loop, say so explicitly — do not declare success.

### Three render backends, three testing methods

**metal-render (hypervisor, DEFAULT):** The primary development path. The hypervisor has built-in screenshot capture — no window focus, no macOS utilities, no fragility. Reads directly from the Metal drawable via GPU blit.

```sh
# Automated: capture frame 30 as PNG, then exit
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --capture 30 /tmp/screenshot.png
# Then Read /tmp/screenshot.png

# Multi-frame: capture frames 30, 60, 90 in a single boot
hypervisor target/aarch64-unknown-none/release/kernel --capture 30,60,90 /tmp/test.png
# Produces /tmp/test-030.png, /tmp/test-060.png, /tmp/test-090.png

# Ad-hoc: send SIGUSR1 to running hypervisor
kill -USR1 $(pgrep hypervisor)
# Saves to /tmp/hypervisor-capture.png — then Read it
```

Launch: `cd system && cargo run -r` (default, no env vars needed).

**cpu-render (QEMU software):** Uses `screendump` via the QEMU monitor socket. This captures the guest framebuffer directly and works reliably.

```sh
cd system && QEMU=1 ./test-qemu.sh --keys "h e l l o" --boot-wait 8 --wait 3
# screendump works for cpu-render:
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"
# Then Read /tmp/qemu-screen.png
```

Launch: `cd system && QEMU=1 VIRGL=0 cargo run -r`.

**virgil-render (QEMU virgl): `screendump` DOES NOT WORK.** It produces stale/cached images because the virgl cocoa display renders via the host GPU, not the guest framebuffer. Instead:

1. Launch with `cd system && QEMU=1 cargo run --release` (opens a cocoa window)
2. Focus the QEMU window
3. Use macOS screenshot: `screencapture -l $(osascript -e 'tell app "QEMU" to id of window 1') /tmp/qemu-virgl.png`
4. Read the PNG with the Read tool

**Sanity check:** If two screenshots show identical clock times, the capture is stale. The clock updates every second — identical timestamps mean you're reading cached data.

### When to use this

- Any change to: drawing library, render backends, scene walk, text editor, core, init (display pipeline setup)
- Any bug fix where the symptom was visual (wrong rendering, missing content, latency)
- Before committing display-related changes
- Serial output alone is NOT sufficient — it only proves messages flow, not that pixels update
- **Prefer metal-render** for visual verification — built-in capture is deterministic and doesn't require window focus

## Rendering Pipeline Changes (MANDATORY)

Changes to the rendering pipeline must follow this process. These rules exist because a session was lost to shipping unverified rendering changes that broke the display.

Three render backends exist — changes may affect one or all:

- **metal-render** (default): scene graph → metal command buffer → virtio → hypervisor → Metal API → CAMetalLayer
- **virgil-render**: scene graph → virgl command encoding → virtio-gpu wire → QEMU → virglrenderer → ANGLE → Metal
- **cpu-render**: scene graph → CpuBackend (libraries/render/) → virtio-gpu 2D → QEMU → display

Relevant code: `protocol/metal.rs`, `protocol/virgl.rs`, `libraries/render/`, `services/drivers/{metal-render,virgil-render,cpu-render}/`, `services/core/` (scene building).

### Before implementing

1. **Validate assumptions against source code, not general knowledge.** Every layer in the pipeline has source code or documentation. If you're uncertain whether a GPU feature is supported, READ the source — do not guess from general knowledge. For metal-render, check the hypervisor's Swift Metal code (`~/Sites/hypervisor/`). For virgil-render, check virglrenderer source or ANGLE docs.
2. **If an agreed approach turns out to be infeasible, STOP and discuss.** Do not silently switch to a different approach. Come back to the user with: what we agreed, what you found, why it doesn't work, what the alternatives are.

### During implementation

3. **One subsystem at a time, verified at each step.** Do not change the render target, stencil, viewport, scissor, blur pipeline, and resolve path simultaneously. Change one, verify, then change the next.
4. **Points and pixels are the only two coordinate units.** Points (1/72 inch) are used everywhere above the render boundary. Pixels are physical framebuffer coordinates used only by render backends. Never conflate them. Never introduce a third unit. Variable names must make the unit clear.

### Timing instrumentation

The sys library provides `sys::counter()` (reads CNTVCT_EL0) and `sys::counter_freq()` (reads CNTFRQ_EL0). Use these for sub-millisecond timing of hot paths. Frequency varies by platform (typically 24 MHz on Apple Silicon via hypervisor, 62.5 MHz on QEMU). Enabled by kernel setting CNTKCTL_EL1.EL0VCTEN=1 in timer::init().

## Reference Influences

- **Mercury OS:** Fluid, focused, familiar. Module/Flow/Space hierarchy. Intent-driven. Locus (command bar combining CLI + NLP + GUI). No apps, no folders. Artificial Collaborators. Mirrors (same module in multiple spaces).
- **Ideal OS:** Document database replaces filesystem for apps. Message bus as sole IPC. Compositor controlled by messages. Apps become small modules. Structured object streams instead of text pipes.
- **OpenDoc / Xerox Star / Plan 9 / BeOS:** Historical attempts at document-centric or radically simplified OS design.
