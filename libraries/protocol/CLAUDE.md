# protocol

IPC message types and payload structs for all inter-process boundaries. Single source of truth -- every component that sends or receives IPC messages imports from here. Also defines the Content Region shared memory layout, decode protocol, and Metal/Virgl wire formats. `no_std`.

## Key Files

- `lib.rs` -- `DirtyRect`, `decode_payload` helper, `channel_shm_va()`, shared memory base address constants. Module declarations for all protocol boundaries (device, input, edit, init, view, store, layout, decode, content, metal)
- `content.rs` -- Content Region shared memory layout: `ContentRegionHeader`, `ContentEntry`, `ContentClass` (Font/Pixels), `ContentAllocator` (free-list with first-fit, coalescing, 16-byte alignment, generation-based GC). Well-known content IDs for fonts
- `decode.rs` -- Generic decode protocol for sandboxed decoder services: `DecodeRequest`, `DecodeResponse`, `DecoderConfig`. Format-agnostic (same types for PNG, JPEG, etc.)
- `store.rs` -- Store service protocol: 15 message types (`MSG_STORE_CONFIG` through `MSG_STORE_DELETE_SNAPSHOT`), payload structs for config, commit, query, read, snapshot, restore, create
- `metal.rs` -- Metal command wire format for Metal-over-virtio: command headers, setup commands, render commands, compute commands. Guest pre-assigns u32 handle IDs
- `build.rs` -- Sets `SYSTEM_CONFIG` env var for PAGE_SIZE

## Dependencies

- None (uses `alloc` only in `metal.rs` for command buffer Vec)

## Conventions

- All payload structs are `#[repr(C)]` and must fit within 60 bytes (enforced by const assertions)
- One module per protocol boundary
- `DirtyRect` is defined here and re-exported by the drawing library
- Content Region allocator uses deferred free with generation-based GC for safe triple-buffer reclamation
