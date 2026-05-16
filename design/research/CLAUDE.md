# design/research

Research documents informing OS design decisions.

## Files

- `kernel-userspace-interface.md` -- **The original kernel spec.** Ideal
  kernel-userspace interface derived from first principles. 5 kernel objects, 25
  syscalls, data/control plane split, open design tensions. The kernel rewrite
  is complete and grew beyond this spec (6 objects, 35 syscalls). This document
  captures the initial design reasoning; see `STATUS.md` for current state.
- `m4-pro-kernel-design.md` -- Hardware-specific kernel design opportunities for
  M4 Pro (companion to interface doc).
- `cow-filesystems.md` -- COW filesystem designs (Btrfs, ZFS, APFS, HAMMER2)
  evaluated against undo/snapshot requirements.
- `os-landscape.md` -- Non-Rust OSes with relevant ideas: Phantom OS,
  BeOS/Haiku, Singularity, Midori, Oberon.
- `font-rendering.md` -- Font rendering pipeline research and implementation
  status.
- `control-plane-ipc.md` -- Control plane IPC research.
- `manifest-model.md` -- Compound document manifest data model. Axes ×
  positioning model, URI content references, one-level-deep rule, edge data.
  Design complete, not yet in code.
- `smp-concurrency.md` -- SMP concurrency model evaluation. Five approaches
  (BKL, per-object, multikernel, per-cluster, QNX preemptible) ranked against M4
  Pro hardware and project design principles. Decision: per-object locking
  (Zircon model) with per-CPU scheduler, RW+PI handle table locks, per-object
  SpinLocks, and IPI for cross-core wake.

These documents capture findings at a point in time. Some decisions they
informed are now settled; the research remains as rationale.
