# design

Design documents for the document-centric OS. This is the primary artifact of
the project -- the design matters more than the implementation.

## Key Files

- `philosophy.md` -- **Read first.** Two root principles and their consequences.
- `microkernel-principles.md` -- First-principles reasoning about what a
  microkernel must do and why.
- `foundations.md` -- The core idea, guiding beliefs, content model (3-layer
  type system), viewer-first design, editor augmentation model, edit protocol,
  undo/history architecture.
- `decisions.md` -- 18 tiered decisions with tradeoffs, implementation
  readiness, dependency chains.
- `landscape.md` -- Technical comparison against 9 other operating systems.
- `glossary.md` -- System-specific terminology.
- `architecture.md` -- The system's architectural narrative: pipeline,
  responsibilities, decision checklist.
- `research/` -- Kernel interface spec, hardware design, COW filesystems, OS
  landscape, font rendering.

## Conventions

- Decisions are numbered and tiered (Tier 0 = foundational, higher = more
  derived)
- "Settled" means committed; "leaning" means current direction but not locked
