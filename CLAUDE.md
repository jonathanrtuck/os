# Project: Document-Centric OS

## Working Protocol (MANDATORY)

These rules govern how you work on this project. They are not preferences — they are requirements. Violating them wastes the user's time and erodes trust.

### 1. Understand before acting

- Read every file you will modify, AND every file that depends on it
- Trace all downstream effects of the change before writing code
- If the problem has known algorithms or prior art, research them from authoritative sources (specs, papers, reference implementations) — never improvise when a solution exists
- Never guess an API, syscall, instruction encoding, or wire format — look it up in the actual source or documentation. Wrong assumptions cascade silently.

### 2. Build bottom-up

- Complete the current architectural layer before starting the next
- No scaffolding, no "good enough for now," no "fix later" — production-grade from the first line
- Each component should work as a standalone, world-class library behind a clean interface

### 3. Verify everything yourself

- Write or identify tests BEFORE implementing. Watch them fail. Implement. Watch them pass.
- Run the FULL test suite, not just tests you think are relevant
- For display changes: capture screenshots, run imgdiff.py, report numbers — never eyeball
- Trace every affected code path. Finding A bug is not the same as finding THE bug.
- Never declare "done" without evidence.
- If verification tooling doesn't exist for a change, STOP. Building the tooling becomes the immediate priority. Push the original task onto the stack, build what's needed to verify, then resume. Unverifiable work does not ship — no exceptions.

**Visual verification means correctness, not existence.** "Something rendered" is not verification. You must verify it looks _correct_:

- For ANY visual change: capture a BEFORE screenshot as baseline. Make changes. Capture AFTER. Diff with imgdiff.py — verify only intended changes appear and nothing else regressed.
- For NEW visual elements (no "before" exists): generate a reference image with Python/PIL showing what correct output looks like (e.g., draw an arc, a rectangle, expected text layout). Compare the actual render against it.
- BEFORE capturing: state what "correct" looks like numerically (bounding box, aspect ratio, position, symmetry). AFTER capturing: compare measured values against stated expectations. If they don't match, the implementation is wrong — don't rationalize the discrepancy.

### 4. Fix root causes, not symptoms

- When something breaks, diagnose the actual cause — don't patch the surface
- When fixing a bug, check for the same class of bug in related code
- If an interface is confusing enough to cause a bug, STOP and flag it — interfaces are architectural decisions in this project. Propose the fix, don't silently apply it.

Read `STATUS.md` at session start for current project state and session resume context.

## What This Is

A personal project exploring an alternative operating system design where documents (files) are first-class citizens and applications are interchangeable tools that attach to content. This is a learning/exploration project, not a product. Currently in the design phase with research spikes — code is written selectively to validate assumptions or flesh out settled decisions.

## Working Mode

This is a long-running exploration project with no deadline. Sessions may be days or months apart. The designer wants a **thinking partner**, not a project manager:

- **Explore, don't push.** Help think through ideas, poke holes, surface tradeoffs. Don't rush toward decisions or implementation.
- **Hold context across sessions.** Use MEMORY.md, the exploration journal, and STATUS.md to resume seamlessly.
- **Connect the dots.** Flag similarities, inconsistencies, or connections to previous discussions. Remind when something was already explored or rejected.
- **Guide gently.** Suggest topics that would address gaps in the emerging design. Ask for clarity when needed. Flag dead ends or common traps.
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
- GUI, CLI, and assistive interfaces are equally fundamental OS interfaces, not applications. Accessibility is first-class — semantic structure in the data model, not annotations after the fact.
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

- `cargo test -- --test-threads=1` in `system/test/` MUST pass (all ~2,236 tests).
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

### Render backends and testing methods

**metal-render (hypervisor, DEFAULT):** The primary development path. The hypervisor has built-in screenshot capture and scripted input injection — no window focus, no macOS utilities, no fragility. Reads directly from the Metal drawable via GPU blit.

```sh
# IMPORTANT: --drive disk.img is REQUIRED for all direct hypervisor invocations.
# Without it, the document store has no content → nothing renders → no captures.
# (cargo run -r handles this automatically via run.sh)

# Automated: capture frame 30 as PNG, then exit
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --background --capture 30 /tmp/screenshot.png
# Then Read /tmp/screenshot.png

# Multi-frame: capture frames 30, 60, 90 in a single boot
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --background --capture 30,60,90 /tmp/test.png
# Produces /tmp/test-030.png, /tmp/test-060.png, /tmp/test-090.png

# Event script: type text, edit, capture result (deterministic visual test)
cat > /tmp/test.events << 'SCRIPT'
type hello world
key left left left
key backspace
wait 5
capture /tmp/after-edit.png
SCRIPT
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --background --events /tmp/test.events
# Then Read /tmp/after-edit.png

# Fixed resolution for wrap testing (e.g., ~37 chars/line at 400px)
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --background --resolution 400x300 --events /tmp/test.events

# Ad-hoc: send SIGUSR1 to running hypervisor
kill -USR1 $(pgrep hypervisor)
# Saves to /tmp/hypervisor-capture.png — then Read it
```

**Event script format** (evdev key names from `linux/input-event-codes.h`):

- `type hello` — type each character (handles shift for uppercase)
- `key backspace` — single key press (also: `left`, `right`, `up`, `down`, `return`, `tab`, `delete`, `home`, `end`, `pageup`, `pagedown`, `escape`, `f1`-`f12`)
- `key shift+left` — modified key (modifiers: `shift`, `ctrl`, `alt`, `cmd`)
- `move 100 200` — move pointer to (x, y) without clicking
- `click 100 200` — left click at (x, y) in framebuffer pixels (matches `--resolution`)
- `dblclick 100 200` — double click at (x, y) in framebuffer pixels
- `wait 10` — wait 10 extra frames
- `capture /tmp/out.png` — screenshot at this point

**Background mode:** Always use `--background` for automated invocations (captures, event scripts, CI). It sets `.accessory` activation policy: no Dock icon, no window activation, no focus stealing. Metal rendering still works (window exists in compositing tree but ordered behind). Previously background mode was implicit with `--events`; now it's an explicit flag.

### Numerical image verification (MANDATORY)

**Never eyeball screenshots to judge correctness.** Downscaled images in the conversation are unreliable — you WILL hallucinate pixel differences. Always use `system/test/imgdiff.py` for hard numbers.

```sh
# Measure a single screenshot: page position, colored region
python3 system/test/imgdiff.py /tmp/screenshot.png

# Compare two screenshots: positions + pixel diff count
python3 system/test/imgdiff.py /tmp/before.png /tmp/after.png
```

Output includes:

- **Page edges** — left/right x of the white page (first/last bright pixel in the middle row)
- **Page center** — midpoint of left/right edges
- **Colored region** — bounding box of the test image (non-black, non-white)
- **Pixel diff** — count of differing pixels between two images

Use this to verify: "page left=1164 in both images" is proof of no shift. "colored region: not found" is proof the image is offscreen. Never claim a visual result without a number backing it.

**Always capture a baseline BEFORE making display changes.** Compare before/after with imgdiff.py to verify only intended pixels changed. For new visual elements, generate a reference with Python/PIL (draw the expected shape, text, layout) and compare the actual render against it. "Something rendered" is NOT verification — you must verify it is geometrically and visually correct.

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

**virgil-render (DEPRECATED):** No longer maintained. Do not use for visual testing. Use metal-render instead.

### When to use this

- Any change to: drawing library, render backends, scene walk, text editor, core, init (display pipeline setup)
- Any bug fix where the symptom was visual (wrong rendering, missing content, latency)
- Before committing display-related changes
- Serial output alone is NOT sufficient — it only proves messages flow, not that pixels update
- **Prefer metal-render** for visual verification — built-in capture is deterministic and doesn't require window focus

## Rendering Pipeline Changes (MANDATORY)

Changes to the rendering pipeline must follow this process. These rules exist because a session was lost to shipping unverified rendering changes that broke the display.

Two active render backends — changes may affect one or both:

- **metal-render** (default): scene graph → metal command buffer → virtio → hypervisor → Metal API → CAMetalLayer
- **cpu-render**: scene graph → CpuBackend (libraries/render/) → virtio-gpu 2D → QEMU → display

(virgil-render is deprecated and no longer maintained.)

Relevant code: `protocol/metal.rs`, `libraries/render/`, `services/drivers/{metal-render,cpu-render}/`, `services/core/` (scene building).

### Before implementing

1. **Validate assumptions against source code, not general knowledge.** Every layer in the pipeline has source code or documentation. If you're uncertain whether a GPU feature is supported, READ the source — do not guess from general knowledge. For metal-render, check the hypervisor's Swift Metal code (`~/Sites/hypervisor/`).
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
