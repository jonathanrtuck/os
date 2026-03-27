# filesystem

**DEPRECATED: Replaced by `document/`.** Legacy COW filesystem service. Owned the virtio-blk device directly and provided basic file create/write/commit operations. The document service supersedes this with full metadata-aware document storage, queries, snapshots, and undo support.

## Key Files

- `main.rs` — Entry point, VirtioBlockDevice, format/mount, basic IPC commit loop

## IPC Protocol

**Receives:**

- `MSG_DEVICE_CONFIG` — MMIO address and IRQ from init (handle 0)
- `MSG_FS_CONFIG` — Doc buffer VA and capacity from init (handle 0)
- `MSG_FS_COMMIT` — Commit request from core (handle 1)

## Dependencies

- `sys` — Syscalls, DMA allocation
- `ipc` — Channel communication
- `protocol` — Wire format (device, blkfs)
- `virtio` — Virtio device/virtqueue management
- `fs` — COW filesystem (BlockDevice trait, Filesystem, Files)
