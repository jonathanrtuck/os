# init

Root userspace task. The kernel spawns only init; init reads the device manifest from kernel shared memory, then spawns and orchestrates all other processes. Implements the microkernel pattern: kernel provides mechanism, init provides policy.

## Key Files

- `main.rs` -- Entry point, device manifest parsing, driver spawning, render pipeline setup (10-phase handshake), font loading, IPC topology creation, process lifecycle management. Reads service ELFs from the memory-mapped service pack at `SERVICE_PACK_BASE`.

## IPC Protocol

**Creates and manages all IPC channels:**

- Config channels (init to each child) -- device config, compositor config, editor config
- Input channels (input driver to presenter) -- keyboard/tablet events
- Scene update channel (presenter to render service) -- scene graph change signals
- Store channel (document to store service) -- document operations

**Sends:**

- `MSG_DEVICE_CONFIG` -- MMIO PA and IRQ to all hardware drivers
- `MSG_GPU_CONFIG` -- Framebuffer dimensions to render service
- `MSG_COMPOSITOR_CONFIG` -- Scene graph VA, font data, scale factor to render service
- `MSG_CORE_CONFIG` -- Configuration to presenter (doc buffer, Content Region, font data)
- `MSG_FRAME_RATE` -- Display refresh rate to presenter
- `MSG_EDITOR_CONFIG` -- Doc buffer VA and capacity to text editor
- `MSG_IMAGE_CONFIG`, `MSG_RTC_CONFIG` -- Image and RTC config to presenter
- `MSG_STORE_CONFIG`, `MSG_STORE_QUERY`, `MSG_STORE_READ`, `MSG_STORE_BOOT_DONE` -- Store service boot sequence
- `MSG_DOC_CONFIG` -- Document process configuration

**Receives:**

- `MSG_DISPLAY_INFO` -- Display resolution from render service
- `MSG_GPU_READY` -- Ready signal from render service
- `MSG_FS_READ_RESPONSE` -- File data from 9P driver
- `MSG_STORE_READY`, `MSG_STORE_QUERY_RESULT`, `MSG_STORE_READ_DONE` -- Store service replies

## Dependencies

- `sys` -- Syscalls, process creation, memory mapping
- `ipc` -- Channel communication
- `protocol` -- Wire format (init, store, device, edit, view, layout)
- `scene` -- Scene graph constants (TRIPLE_SCENE_SIZE)
