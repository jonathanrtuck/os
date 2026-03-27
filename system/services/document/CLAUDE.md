# document

Document service: metadata-aware document store over virtio-blk. Replaces the filesystem service. Owns the block device directly (MMIO mapping, DMA I/O) and layers the COW filesystem (`fs` library) and document store (`store` library) on top. This service is a thin IPC translator -- all document/metadata logic lives in the `store` library.

## Key Files

- `main.rs` — Entry point, VirtioBlockDevice (BlockDevice trait impl), boot-query phase, main IPC loop

## IPC Protocol

**Receives (from core, handle 1):**

- `MSG_DOC_COMMIT` — Read doc buffer from shared memory, write to file, commit
- `MSG_DOC_QUERY` — Run store query (media type, type, attribute), return matching FileIds
- `MSG_DOC_READ` — Read file content into shared memory (doc buffer or Content Region)
- `MSG_DOC_SNAPSHOT` — Create COW snapshot of specified files
- `MSG_DOC_RESTORE` — Restore a previous snapshot
- `MSG_DOC_CREATE` — Create a new document with a media type
- `MSG_DOC_DELETE_SNAPSHOT` — Fire-and-forget snapshot cleanup

**Receives (from init, handle 0, boot phase only):**

- `MSG_DOC_CONFIG` — MMIO address, IRQ, doc buffer VA, Content Region VA
- `MSG_DOC_QUERY` / `MSG_DOC_READ` — Font loading queries during boot
- `MSG_DOC_BOOT_DONE` — End of boot-query phase

**Sends:**

- `MSG_DOC_READY` — Ready signal to init
- `MSG_DOC_QUERY_RESULT`, `MSG_DOC_READ_DONE`, `MSG_DOC_SNAPSHOT_RESULT`, `MSG_DOC_RESTORE_RESULT`, `MSG_DOC_CREATE_RESULT` — Replies to core

## Dependencies

- `sys` — Syscalls, DMA allocation, counter/timer
- `ipc` — Channel communication
- `protocol` — Document wire format (`protocol/document.rs`)
- `virtio` — Virtio device/virtqueue management
- `fs` — COW filesystem (BlockDevice trait, Filesystem, Files)
- `store` — Document store metadata layer (catalog, queries)
