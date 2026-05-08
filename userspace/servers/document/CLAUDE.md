# document

The document service owns the document buffer. It is the sole writer to the
buffer and applies all edits received from editors via sync IPC. Pure data
service -- no knowledge of layout, display, input, or animation.

## Responsibilities

- Owns document buffer VMO (sole writer). Editors and other services read via RO
  shared memory mapping.
- Applies edits (insert, delete) from editors via sync IPC.
- Manages undo ring: COW snapshots via store service at edit boundaries.
- Tracks cursor position and selection.
- Shares the buffer with clients via SETUP (returns RO VMO handle).

## Key Files

- `src/lib.rs` -- protocol definitions (SETUP, INSERT, DELETE, CURSOR_MOVE,
  SELECT, UNDO, REDO, GET_INFO) and document buffer header format
- `src/main.rs` -- service implementation, store interaction, undo/redo

## IPC Protocol

### Serves (on own endpoint, registered as "document")

- `SETUP` -- returns doc buffer VMO handle (RO) + metadata
- `INSERT` -- insert bytes at offset (inline in payload)
- `DELETE` -- delete byte range
- `CURSOR_MOVE` -- update cursor position
- `SELECT` -- update selection (anchor + cursor)
- `UNDO` / `REDO` -- snapshot-based undo/redo
- `GET_INFO` -- returns document metadata

### Calls (outbound to store service)

- `SETUP` -- establish shared VMO for bulk I/O
- `CREATE` -- create document file
- `WRITE_DOC` / `READ_DOC` -- bulk data via shared VMO
- `TRUNCATE` -- trim file to exact length
- `SNAPSHOT` / `RESTORE` / `DELETE_SNAPSHOT` -- COW undo

## Document Buffer Layout

64-byte header + content bytes in a shared VMO:

| Offset | Size | Field       | Notes                          |
| ------ | ---- | ----------- | ------------------------------ |
| 0      | 8    | content_len | u64 LE                         |
| 8      | 8    | cursor_pos  | u64 LE                         |
| 16     | 4    | generation  | AtomicU32, Release/Acquire     |
| 20     | 4    | format      | 0=plain, 1=rich                |
| 24     | 40   | reserved    |                                |
| 64     | ...  | content     | raw bytes (plain text for now) |

## Dependencies

Services: store (via name service lookup) Libraries: abi, ipc, name, console,
store-service (protocol only)

## Testing

```sh
# Bare-metal build
cargo build --release

# Integration test: runs as test-document service under hypervisor
# Verifies: insert, delete, undo, redo, cursor move, GET_INFO, shared memory reads
```
