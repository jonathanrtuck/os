# store

Store service: metadata-aware persistence layer over virtio-blk. Owns the block device directly (MMIO mapping, DMA I/O) and layers the COW filesystem (`fs` library) and document store (`store` library) on top. This service is a thin IPC translator -- all document/metadata logic lives in the `store` library.

## Key Files

- `main.rs` -- Entry point, VirtioBlockDevice (BlockDevice trait impl), boot-query phase, main IPC loop

## IPC Protocol

**Receives (from document, handle 1):**

- `MSG_STORE_COMMIT` -- Read doc buffer from shared memory, write to file, commit
- `MSG_STORE_QUERY` -- Run store query (media type, type, attribute), return matching FileIds
- `MSG_STORE_READ` -- Read file content into shared memory (doc buffer or Content Region)
- `MSG_STORE_SNAPSHOT` -- Create COW snapshot of specified files
- `MSG_STORE_RESTORE` -- Restore a previous snapshot
- `MSG_STORE_CREATE` -- Create a new document with a media type
- `MSG_STORE_DELETE_SNAPSHOT` -- Fire-and-forget snapshot cleanup

**Receives (from init, handle 0, boot phase only):**

- `MSG_STORE_CONFIG` -- MMIO address, IRQ, doc buffer VA, Content Region VA
- `MSG_STORE_QUERY` / `MSG_STORE_READ` -- Font loading queries during boot
- `MSG_STORE_BOOT_DONE` -- End of boot-query phase

**Sends:**

- `MSG_STORE_READY` -- Ready signal to init
- `MSG_STORE_QUERY_RESULT`, `MSG_STORE_READ_DONE`, `MSG_STORE_SNAPSHOT_RESULT`, `MSG_STORE_RESTORE_RESULT`, `MSG_STORE_CREATE_RESULT` -- Replies to document

## Dependencies

- `sys` -- Syscalls, DMA allocation, counter/timer
- `ipc` -- Channel communication
- `protocol` -- Store wire format (`protocol/store.rs`)
- `virtio` -- Virtio device/virtqueue management
- `fs` -- COW filesystem (BlockDevice trait, Filesystem, Files)
- `store` -- Document store metadata layer (catalog, queries)
