# virtio-9p

Host filesystem passthrough driver using the 9P2000.L protocol over virtio transport. QEMU serves a host directory; this driver reads files on behalf of init for loading fonts, images, and other assets during boot.

## Key Files

- `main.rs` — Entry point, P9Client (9P message exchange), MsgReader/MsgWriter (9P wire format), file read IPC loop. Implements Tversion, Tattach, Twalk, Tlopen, Tread, Tclunk message types.

## IPC Protocol

**Receives:**

- `MSG_DEVICE_CONFIG` — MMIO address and IRQ from init (handle 0)
- `MSG_FS_READ_REQUEST` — File path and target VA from init (handle 0)

**Sends:**

- `MSG_FS_READ_RESPONSE` — File size and status to init (handle 0)

## Dependencies

- `sys` — Syscalls, DMA allocation
- `ipc` — Channel communication
- `protocol` — Wire format (device, fs)
- `virtio` — Virtio device/virtqueue management
