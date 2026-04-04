# virtio

Virtio MMIO transport and split virtqueue implementation. Pure library -- no syscalls, no hardware access. Drivers allocate DMA memory and map MMIO via `sys`, then hand addresses to this library. `no_std`, no dependencies.

## Key Files

- `lib.rs` -- `Device` (MMIO register access, feature negotiation, queue setup, status management), `Virtqueue` (split virtqueue with descriptor ring, available ring, used ring), `Descriptor` (16-byte descriptor struct), `UsedElem`. Constants: `DEFAULT_QUEUE_SIZE` (128), `DESC_F_NEXT`, `DESC_F_WRITE`

## Dependencies

- None (reads `system_config.rs` via `include!` for PAGE_SIZE)

## Conventions

- MMIO registers accessed via volatile reads/writes at known offsets from device base address
- Virtqueue uses the split virtqueue layout (descriptors + available ring + used ring in contiguous DMA memory)
- Feature negotiation follows the virtio spec: acknowledge, driver, features_ok, driver_ok
- Device config space starts at MMIO offset 0x100
- Queue size default of 128 entries fits all three regions in one page
