# virtio-console

Console output driver (placeholder). Maps a virtio-console device, negotiates features, writes a single test string ("virtio console ok") to the TX virtqueue to validate the userspace driver model, then exits. Not a long-lived service.

## Key Files

- `main.rs` — Entry point, device setup, single TX submission, completion wait

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
