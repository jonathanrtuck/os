# document-model (A)

The document-model process owns the document buffer. It is the sole writer to
the buffer and applies all edits received from editors via IPC. Pure data
service -- no knowledge of layout, display, input, or animation.

## Responsibilities

- Owns document buffer (sole writer). Editors and other processes read via RO shared memory.
- Applies edits (insert, delete, style) from editors via IPC.
- Manages undo ring: COW snapshots via document service at operation boundaries.
- Communicates with document service (persistence, queries, snapshots).
- Communicates with decoder services (decode requests/responses).
- Notifies view-engine (C) on document changes and image decode completion.

## Key Files

- `main.rs` -- entry point, IPC loop, edit application, undo/redo, decoder communication

## IPC Protocol

### Receives
- `MSG_DOC_MODEL_CONFIG` -- Init config (doc buffer VA, Content Region, handles)
- `MSG_WRITE_INSERT`, `MSG_WRITE_DELETE`, `MSG_WRITE_DELETE_RANGE` -- Edits from editor
- `MSG_STYLE_APPLY`, `MSG_STYLE_SET_CURRENT` -- Style changes from editor
- `MSG_UNDO_REQUEST`, `MSG_REDO_REQUEST` -- Undo/redo from view-engine (C)

### Sends
- `MSG_DOC_LOADED` -- Initial document loaded (to C)
- `MSG_DOC_CHANGED` -- Buffer changed after edit or undo/redo (to C)
- `MSG_IMAGE_DECODED` -- Image decoded and registered in Content Region (to C)
- `MSG_DOC_COMMIT`, `MSG_DOC_SNAPSHOT`, `MSG_DOC_RESTORE` -- Document service ops

## Dependencies

Libraries: sys, ipc, protocol, piecetable, content (allocator)
