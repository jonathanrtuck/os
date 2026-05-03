# Project Status

**Last updated:** 2026-05-03

## Current State

**Kernel rewrite: STARTING.** Clean restart from first principles.

The previous implementation (v0.1-v0.6) is preserved at tag `v0.6-pre-rewrite`. The design documents carry forward; all code starts fresh.

## The Rewrite

The kernel-userspace interface has been redesigned from scratch:

- **Spec:** `design/research/kernel-userspace-interface.md`
- **Hardware companion:** `design/research/m4-pro-kernel-design.md`
- **5 kernel objects:** VMO, Channel, Event, Thread, Address Space
- **25 syscalls** (down from 46 in the previous implementation)
- **Data/control plane split:** shared memory for bulk data, channels for control messages, events for synchronization

The userspace (services, libraries, UI) will be rebuilt to this interface, preserving the same UI/UX and rough service structure from the previous prototype.

## What Exists

- `design/` — OS design documents (philosophy, foundations, 17 settled decisions, architecture, research)
- `.cargo/`, `rust-toolchain.toml`, `rustfmt.toml` — Rust build infrastructure
- `.claude/` — Claude Code hooks and settings

## What's Next

Build the kernel to the spec.

## Previous Milestones (v0.1-v0.6)

All preserved in git history. Key achievements from the prototype:

- **v0.3:** Rendering foundation (Metal GPU, scene graph, text rendering, animation, visual polish)
- **v0.4:** Document store (filesystem, COW snapshots, metadata queries, undo/redo)
- **v0.5:** Rich text (piece table, style palette, a11y roles)
- **v0.6:** Kernel (arch abstraction, capabilities, VMOs, pager, signals, SMP/EEVDF, ASLR, PAC/BTI)
