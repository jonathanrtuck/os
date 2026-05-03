# Project: Document-Centric OS

## Working Protocol (MANDATORY)

These rules govern how you work on this project. They are not preferences — they
are requirements. Violating them wastes the user's time and erodes trust.

### 1. Understand before acting

- Read every file you will modify, AND every file that depends on it
- Trace all downstream effects of the change before writing code
- If the problem has known algorithms or prior art, research them from
  authoritative sources (specs, papers, reference implementations) — never
  improvise when a solution exists
- Never guess an API, syscall, instruction encoding, or wire format — look it up
  in the actual source or documentation. Wrong assumptions cascade silently.

### 2. Build bottom-up

- Complete the current architectural layer before starting the next
- No scaffolding, no "good enough for now," no "fix later" — production-grade
  from the first line
- Each component should work as a standalone, world-class library behind a clean
  interface

### 3. Verify everything yourself

- Write or identify tests BEFORE implementing. Watch them fail. Implement. Watch
  them pass.
- Run the FULL test suite, not just tests you think are relevant
- Trace every affected code path. Finding A bug is not the same as finding THE
  bug.
- Never declare "done" without evidence.
- If verification tooling doesn't exist for a change, STOP. Building the tooling
  becomes the immediate priority. Push the original task onto the stack, build
  what's needed to verify, then resume. Unverifiable work does not ship — no
  exceptions.

### 4. Fix root causes, not symptoms

- When something breaks, diagnose the actual cause — don't patch the surface
- When fixing a bug, check for the same class of bug in related code
- If an interface is confusing enough to cause a bug, STOP and flag it —
  interfaces are architectural decisions in this project. Propose the fix, don't
  silently apply it.

### 5. Present the design space, not a default answer

- When suggesting kernel or OS design approaches, never default to a single
  system's patterns (especially Zircon/Fuchsia). Present the design space across
  multiple systems.
- Name which systems you're drawing from and why. "Zircon does it this way" is
  not a justification — "This approach is correct because [reason], and [system]
  chose it for [their reason]" is.
- Give extra weight to systems aligned with this project's unique goals
  (document-centric, content-type aware, personal workstation). Zircon was
  designed for phones; seL4 for safety-critical; Plan 9 for distributed
  computing. Different goals produce different correct answers.
- Reference landscape: seL4, L4 family, EROS/Coyotos, Genode, QNX, Plan 9,
  Barrelfish, Redox, Minix 3 — not just Zircon.

### 6. Update reference docs at milestone boundaries

When completing a milestone (version tag), verify these files reflect the
current architecture:

- `design/glossary.md` — new terms, changed definitions, removed components
- `design/landscape.md` — if any comparison axis changed materially
- `STATUS.md` — current state and milestone completion

Read `STATUS.md` at session start for current project state and session resume
context.

## What This Is

A personal project exploring an alternative operating system design where
documents (files) are first-class citizens and applications are interchangeable
tools that attach to content. This is a learning/exploration project, not a
product.

**Current phase: kernel rewrite.** The previous implementation (v0.1–v0.6,
tagged `v0.6-pre-rewrite`) validated the design through a working prototype.
This restart rewrites the kernel from first principles, guided by
`design/research/kernel-userspace-interface.md`, then rebuilds the userspace to
match.

## Working Mode

This is a long-running exploration project with no deadline. Sessions may be
days or months apart. The designer wants a **thinking partner**, not a project
manager:

- **Explore, don't push.** Help think through ideas, poke holes, surface
  tradeoffs. Don't rush toward decisions or implementation.
- **Hold context across sessions.** Use MEMORY.md and STATUS.md to resume
  seamlessly.
- **Connect the dots.** Flag similarities, inconsistencies, or connections to
  previous discussions. Remind when something was already explored or rejected.
- **Guide gently.** Suggest topics that would address gaps in the emerging
  design. Ask for clarity when needed. Flag dead ends or common traps.
- **Respect the pace.** The designer may want to deep-dive a topic, switch to
  coding, or just chat loosely. Follow their energy.

## Key Design Documents

Read these before making any design suggestions:

- `design/philosophy.md` — **Read first.** Two root principles and their
  consequences.
- `design/microkernel-principles.md` — First-principles reasoning about what a
  microkernel must do and why. Three irreducible responsibilities, CPU/memory
  multiplexing models, memory object abstraction, isolation rules.
- `design/foundations.md` — The core idea, guiding beliefs, content model
  (3-layer type system), viewer-first design, editor augmentation model, edit
  protocol, undo/history architecture.
- `design/decisions.md` — 17 tiered decisions with tradeoffs, dependency chains.
- `design/landscape.md` — Comparison against 9 other operating systems.
- `design/architecture.md` — The system's architectural narrative: one-way
  pipeline, component responsibilities, decision checklist.
- `design/glossary.md` — System-specific terminology.

### Kernel Spec (the rewrite target)

- `design/research/kernel-userspace-interface.md` — **The kernel spec.** 5
  kernel objects (VMO, Channel, Event, Thread, Address Space), 25 syscalls,
  data/control plane split. Derived from first principles, ignoring the previous
  implementation.
- `design/research/m4-pro-kernel-design.md` — Hardware-specific kernel design
  opportunities for M4 Pro (ARM64).

### Research

- `design/research/cow-filesystems.md` — COW filesystem designs evaluated
  against undo/snapshot requirements.
- `design/research/os-landscape.md` — Non-Rust OSes with relevant ideas (Phantom
  OS, BeOS/Haiku, Singularity, Midori, Oberon).
- `design/research/font-rendering.md` — Font rendering pipeline research.

## Settled Decisions

These are the OS design decisions, not implementation choices. They carry
forward across any rewrite.

1. **Audience & Goals (Tier 0):** Personal design project. Primary artifact is a
   coherent OS design. Build selectively to validate. Success = coherent
   design > working prototype > deep learning. Not a daily driver. Target:
   personal workstation (text, images, audio, video, email, calendar, messaging,
   videoconferencing, web, coding).
2. **Data model (SETTLED):** Document-centric. The main axiom. OS -> Document ->
   Tool.
3. **Compatibility (SETTLED):** Rethink everything. No POSIX. Build on
   established standard interfaces (mimetypes, URIs, HTTP, Unicode, arm64), not
   implementations. Own native APIs. Development on host OS (macOS).
   Self-hosting is not a goal.
4. **Complexity (SETTLED):** Simple everywhere. Complexity is a design smell.
   Essential complexity pushed into leaf nodes behind simple interfaces.
   Connective tissue (protocols, APIs, inter-component relationships) must be
   simple.
5. **File understanding (SETTLED):** OS natively understands content types.
   Mimetype is fundamental OS-managed metadata, not a userspace convention.
6. **View vs edit (SETTLED):** View is default, edit is deliberate. Editors bind
   to content types, not use cases.
7. **File organization (SETTLED):** Rich queryable metadata. Users navigate by
   query, not path.
8. **Editor model (SETTLED):** Editors are modal plugins (one active per
   document). No pending changes — edits write immediately (COW filesystem). No
   "save." OS always renders (pure function of file state).
9. **Edit protocol (SETTLED):** Modal tools, immediate writes, thin protocol.
   Editors call beginOperation/endOperation; OS snapshots at boundaries.
10. **Rendering technology (SETTLED):** Native renderer, web translates inward
    to compound doc format.
11. **Undo (SETTLED):** COW snapshots at operation boundaries for sequential
    undo. Global undo regardless of which editor.
12. **Collaboration (SETTLED):** Designed for, build later.
13. **Compound documents (SETTLED):** Uniform manifest model. Three composable
    relationship axes: spatial, temporal, logical.
14. **Iconography (SETTLED):** Vector path icons, outline style, runtime stroke
    rendering.

## Key Architectural Principles (Settled)

- Everything-is-files is architectural, not UX. Users see abstractions
  (documents, conversations, meetings), not files.
- File paths are metadata, not the organizing principle.
- GUI, CLI, and assistive interfaces are equally fundamental OS interfaces, not
  applications. Accessibility is first-class.
- Prototype success = demonstrating the concept works and scales, even with only
  1-2 content types fully implemented. Depth on the interesting parts is.

## Decision Dependencies (Critical Chains)

1. Data model -> File understanding -> Editor model -> Edit protocol ->
   Undo/Collaboration
2. Editor model -> Rendering technology -> Compound documents -> Layout
3. Compatibility stance -> Technical foundation
4. Data model + File organization -> Interaction model
5. File understanding + Rendering technology + Complexity -> Iconography

## Kernel Change Protocol (MANDATORY)

The kernel is the foundation; a bug here corrupts everything above.

### Unsafe code and inline assembly

- Every `unsafe` block MUST have a `// SAFETY:` comment explaining the invariant
  it relies on and what would break if violated.
- Inline asm `options()`: **never use `nomem` by default.** Only add `nomem`
  with explicit justification citing the instruction's side effects from the ARM
  architecture manual. `nomem` tells LLVM the instruction doesn't access memory
  — if that's a lie, LLVM will reorder memory accesses past it, creating races
  that only manifest at higher optimization levels or under SMP load.
  - **Safe to use `nomem`:** `mrs` of truly immutable registers (MPIDR_EL1,
    CNTFRQ_EL0), `wfe`/`wfi` hints.
  - **Never use `nomem`:** `msr` to any system register (DAIF, TTBR, TPIDR,
    timer registers), `dsb`/`isb` barriers, `hvc`/`smc` calls, `tlbi`
    instructions, any `ldr`/`str` (obviously reads/writes memory).
- When editing existing `unsafe` blocks, re-verify the SAFETY comment still
  holds with the change.

### Testing requirements

- All kernel tests MUST pass before committing.
- Any change touching syscall handlers, scheduling, IPC, or thread lifecycle
  MUST be stress tested under concurrent load.
- Property-based tests for state machine invariants (e.g., scheduler) — run
  after relevant changes.

### Anomaly tracking

- Any unexplained kernel behavior (spurious wakeups, unexpected fault codes,
  timing anomalies) MUST be documented with an open-bug status.
- Workarounds are acceptable as defense-in-depth but do NOT close the bug. The
  root cause investigation continues.

## Rust Formatting Convention (MANDATORY)

All `.rs` files follow standard Rust community conventions. Mechanical
formatting is handled by `rustfmt` (config in `rustfmt.toml`); file layout is
enforced by convention.

### Mechanical formatting (rustfmt)

A PostToolUse hook (`.claude/hooks/rustfmt-post-edit.sh`) runs
`rustfmt --edition 2021` on every `.rs` file after Edit or Write. Manual runs:
`rustfmt --edition 2021 <file>` or `cargo +nightly fmt` from the repo root.

`rustfmt.toml` enables two nightly features:

- `group_imports = "StdExternalCrate"` — separates std, external, and local
  imports with blank lines
- `imports_granularity = "Crate"` — merges imports from the same crate into one
  `use` statement

### File layout convention

Every `.rs` file follows this order:

1. **Module doc comment** (`//!`)
2. **Imports** (`use` statements, grouped by rustfmt)
3. **Constants and statics**
4. **Types in dependency order, each co-located with its `impl` blocks** —
   define a type, then immediately its `impl` block(s), before the next type.
   Within `impl` blocks: constructors first (`new`, `from_*`), then public
   methods, then private methods.
5. **Free functions**
6. **Tests** (`#[cfg(test)]` module)

**Co-located, not types-first.** Do NOT group all type definitions at the top
with all `impl` blocks below. Each type lives next to its implementation. Types
appear in dependency order.

## Design Discussion Rules

- Decisions should favor clarity and interestingness over market viability
- All decisions in the register are unsettled until explicitly committed
- When discussing tradeoffs, be honest about downsides — don't sell options
- Reference the decision register tiers and dependency chains
- New decisions should be recorded in the appropriate reference documents

## Reference Influences

- **Mercury OS:** Fluid, focused, familiar. Module/Flow/Space hierarchy.
  Intent-driven. No apps, no folders.
- **Ideal OS:** Document database replaces filesystem. Message bus as sole IPC.
  Apps become small modules.
- **OpenDoc / Xerox Star / Plan 9 / BeOS:** Historical attempts at
  document-centric or radically simplified OS design.

## Previous Implementation

The v0.1–v0.6 prototype is preserved in git history at tag `v0.6-pre-rewrite`.
Use `git show v0.6-pre-rewrite:<path>` to reference old code. The prototype
validated the design and surfaced insights captured in the design docs and
memory.
