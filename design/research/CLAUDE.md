# design/research

Research documents informing OS design decisions.

## Files

- `kernel-userspace-interface.md` -- **The kernel spec.** Ideal kernel-userspace interface derived from first principles. 5 kernel objects, 25 syscalls, data/control plane split, open design tensions. This is the target for the kernel rewrite.
- `m4-pro-kernel-design.md` -- Hardware-specific kernel design opportunities for M4 Pro (companion to interface doc).
- `cow-filesystems.md` -- COW filesystem designs (Btrfs, ZFS, APFS, HAMMER2) evaluated against undo/snapshot requirements.
- `os-landscape.md` -- Non-Rust OSes with relevant ideas: Phantom OS, BeOS/Haiku, Singularity, Midori, Oberon.
- `font-rendering.md` -- Font rendering pipeline research and implementation status.

These documents capture findings at a point in time. Some decisions they informed are now settled; the research remains as rationale.
