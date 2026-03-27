# virtio-input

Keyboard and tablet input driver. Reads Linux evdev events from the virtio-input event queue and translates them into IPC messages for core. Handles three event types: EV_KEY (keyboard keys and mouse buttons), EV_ABS (absolute pointer coordinates from virtio-tablet). Maintains modifier key state (shift, ctrl, alt, cmd, caps lock) and performs keycode-to-ASCII translation.

## Key Files

- `main.rs` — Entry point, event queue setup, keycode-to-ASCII tables (US layout), modifier tracking, event dispatch loop

## IPC Protocol

**Receives:**
- `MSG_DEVICE_CONFIG` — MMIO address and IRQ from init (handle 0)

**Sends:**
- `MSG_KEY_EVENT` — Keyboard key press/release with ASCII translation and modifier state (to core, handle 1)
- `MSG_POINTER_BUTTON` — Mouse button press/release (to core, handle 1)
- Pointer position written to shared atomic `PointerState` register (u64, no IPC ring)

## Dependencies

- `sys` — Syscalls, DMA allocation
- `ipc` — Channel communication
- `protocol` — Wire format (device, input)
- `virtio` — Virtio device/virtqueue management
