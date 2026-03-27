# virtio-blk

Standalone block device driver and self-test. Maps a virtio-blk device, negotiates features (including VIRTIO_BLK_F_FLUSH), and runs a round-trip verification: format filesystem, create file, write data, commit, read back, verify. In production, the document service owns the block device directly; this driver serves as a validation tool.

## Key Files

- `main.rs` — Entry point, VirtioBlockDevice (BlockDevice trait impl), self-test, capacity reporting

## IPC Protocol

**Receives:**

- `MSG_DEVICE_CONFIG` — MMIO address and IRQ from init (handle 0)

**Sends:**

- Signals handle 0 on completion

## Dependencies

- `sys` — Syscalls, DMA allocation
- `ipc` — Channel communication
- `protocol` — Wire format (device)
- `virtio` — Virtio device/virtqueue management
- `fs` — COW filesystem (BlockDevice trait, Filesystem, Files)
