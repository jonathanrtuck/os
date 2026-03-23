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
14. **Iconography (SETTLED):** Icons are `Content::Path` vector data — same primitive as pointer cursors and shapes. Outline style (stroke-based) with runtime stroke rendering. Build-time SVG→path converter. Source: Tabler Icons (MIT). Compiled into `libraries/icons/` as `const` arrays. Mimetype → icon lookup with three-level fallback (specific → category → universal). Baseline-aligned with text via font metrics. Operational cursors remain hand-built.

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
5. File understanding + Rendering technology + Complexity → Iconography

**Most influential decision:** #2 (Data Model). If document-centric is confirmed, most other decisions are constrained in useful ways.

## Where We Left Off

**Current state (2026-03-23):** v0.3 Phase 4 (Visual Polish) IN PROGRESS. 2,134 tests pass.

**Phase 4 progress so far:**

- **Blank slate + three-font stack:** Pure black/white palette. JetBrains Mono (mono), Inter (sans), Source Serif 4 (serif) loaded via 9p. On-demand glyph atlas in metal-render (fixes ligature drops).
- **Font rendering quality sprint (5 changes to match macOS Core Text):** (1) Outline dilation via symmetric miter-join (macOS formula, Pathfinder coefficients × 1.3 boost). (2) Analytic area coverage rasterizer (exact signed-area trapezoids, not quantized). (3) Device-pixel rasterization (atlas at `font_size_pt × scale_factor`). (4) Subpixel glyph positioning (ShapedGlyph widened 8→16 bytes, 16.16 fixed-point advances). (5) Single `char_w_fx` source of truth (eliminates cursor drift from truncation).
- **Icon pipeline (2026-03-23):** SVG path parser, stroke expansion engine, arc-to-cubic conversion, build-time SVG→path compilation. Tabler file-text/photo icons in title bar. Pointer cursor redesigned to Tabler proportions.
- **Page surface + document strip (2026-03-23):** White A4-proportioned page centered on dark desk. Dark text/cursor. Horizontal strip of N document spaces with spring-based slide transition (Ctrl+Tab). Both documents always in scene — no teardown/rebuild on switch.
- **Shared pointer state register (2026-03-23):** Replaced MSG_POINTER_ABS IPC ring messages with atomic u64 in init-allocated shared memory. Eliminates input ring overflow for pointer events. State vs event distinction at the IPC level (see journal).
- **Deferred:** AA transition softness tuning, italic rendering (in journal).
- **Next:** Continue visual polish (spacing, colors, effects), or declare v0.3 complete.

**Completed phases (see git log for details):**

- Phase 3 (Text & Interaction, 2026-03-22): Unified layout library (`FontMetrics` trait, CharBreaker/WordBreaker). All navigation/selection in core (not editor). Full macOS key combos. Editor slimmed to ~195 lines. Hypervisor event scripts + fixed resolution for visual regression testing.
- Phase 2 (Composition, 2026-03-20): Clip masks, backdrop blur (3-pass box blur), pointer cursor. All three render backends.
- Phase 1 (Motion, 2026-03-20): Animation library (easing, springs, timeline). Smooth scroll, cursor blink, transitions.
- Rendering architecture redesign (2026-03-16): `RenderBackend` trait, geometric content types, compositor minimized to 174 lines.
- Virgl driver + cpu-render merge (2026-03-17-18): Three single-process render backends. Init auto-detects GPU.
- GICv3 + tickless idle (2026-03-16): Full GICv2→GICv3 migration, IPI wakeup, tickless scheduling.
- Rendering correctness (2026-03-21): Analytical shadows, sRGB render targets, alpha compositing fix. Hypervisor extracted to `~/Sites/hypervisor/`.

**Architecture (settled 2026-03-18):**

```text
Core (shaping, layout, scene building) → Scene Graph (shared memory) → Render Service → Display
```

Content types: `None`, `Path`, `Glyphs`, `Image`. Three render services: `metal-render` (default), `cpu-render`, `virgil-render`.

**IPC:** Two mechanisms, matched to data semantics. Event rings (64-byte SPSC messages over shared memory) for discrete events where order/count matter (keys, clicks, config). State registers (atomic shared memory) for continuous data where only the latest value matters (pointer position). Both signaled via `channel_signal` syscall. See `system/DESIGN.md` §0 for full details.

**Open design questions (from earlier sessions):**

- Trust/complexity orthogonality (solid), blue-wraps-all-sides (solid), shell is blue-layer (leaning), one-document-at-a-time (leaning), compound document editing (unresolved)
- Decision #14: Mimetype of whole document, manifest format, FS organization of manifests + content files
- Decision #16: COW on-disk design (deferred via prototype-on-host), snapshot scope (punted)

**Future milestones (from v0.3 spec, now deleted):**

- v0.4: Undo/redo (needs COW filesystem), system clipboard
- v0.5: Rich inline text / multi-style runs
- v0.6: Video / animated media
- Later: BiDi / complex scripts, multi-display

**System code:** `system/kernel/` (33 .rs + 2 .S), `system/services/{init,core,drivers/{cpu-render,virgil-render,metal-render,virtio-blk,virtio-console,virtio-input,virtio-9p}}/`, `system/libraries/{sys,virtio,drawing,fonts,animation,layout,scene,ipc,protocol,render}/`, `system/user/{echo,text-editor,stress,fuzz,fuzz-helper}/`, `system/test/`, `prototype/files/`. 28 syscalls. 4 SMP cores, EEVDF scheduler.

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

- `cargo test -- --test-threads=1` in `system/test/` MUST pass (all ~2,134 tests).
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

**metal-render (hypervisor, DEFAULT):** The primary development path. The hypervisor has built-in screenshot capture and scripted input injection — no window focus, no macOS utilities, no fragility. Reads directly from the Metal drawable via GPU blit.

```sh
# Automated: capture frame 30 as PNG, then exit
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --capture 30 /tmp/screenshot.png
# Then Read /tmp/screenshot.png

# Multi-frame: capture frames 30, 60, 90 in a single boot
hypervisor target/aarch64-unknown-none/release/kernel --capture 30,60,90 /tmp/test.png
# Produces /tmp/test-030.png, /tmp/test-060.png, /tmp/test-090.png

# Event script: type text, edit, capture result (deterministic visual test)
cat > /tmp/test.events << 'SCRIPT'
type hello world
key left left left
key backspace
wait 5
capture /tmp/after-edit.png
SCRIPT
hypervisor target/aarch64-unknown-none/release/kernel --events /tmp/test.events
# Then Read /tmp/after-edit.png

# Fixed resolution for wrap testing (e.g., ~37 chars/line at 400px)
hypervisor target/aarch64-unknown-none/release/kernel --resolution 400x300 --events /tmp/test.events

# Ad-hoc: send SIGUSR1 to running hypervisor
kill -USR1 $(pgrep hypervisor)
# Saves to /tmp/hypervisor-capture.png — then Read it
```

**Event script format** (evdev key names from `linux/input-event-codes.h`):

- `type hello` — type each character (handles shift for uppercase)
- `key backspace` — single key press (also: `left`, `right`, `up`, `down`, `return`, `tab`, `delete`, `home`, `end`, `pageup`, `pagedown`, `escape`, `f1`-`f12`)
- `key shift+left` — modified key (modifiers: `shift`, `ctrl`, `alt`, `cmd`)
- `click 100 200` — left click at (x, y) in framebuffer pixels (matches `--resolution`)
- `dblclick 100 200` — double click at (x, y) in framebuffer pixels
- `wait 10` — wait 10 extra frames
- `capture /tmp/out.png` — screenshot at this point

**When you cannot verify a change with available tools, that is a BLOCKING problem.** Fix the tooling gap before shipping the change. Do not ship unverifiable work.

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
