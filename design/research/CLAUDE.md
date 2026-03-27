# design/research

Research documents informing OS design decisions. Each file investigates a domain relevant to the project's settled or open decisions.

## Files

- `cow-filesystems.md` -- COW filesystem designs (Btrfs, ZFS, APFS, HAMMER2) evaluated against our undo/snapshot requirements (Decision #16)
- `os-landscape.md` -- Non-Rust OSes with relevant ideas: Phantom OS, BeOS/Haiku, Singularity, Midori, Oberon
- `font-rendering.md` -- Font rendering pipeline research and implementation status (rasterizer, shaping, layout, atlas)
- `kernel-hardening.md` -- Gap analysis comparing this kernel against seL4, Zircon, Redox, Asterinas, Theseus, Linux

These documents capture findings at a point in time. Some decisions they informed are now settled; the research remains as rationale.
