# user/text-editor

Content-type-specific editor process. Translates editing key events into document write requests via IPC to core (the sole writer).

Handles only content mutation: character insertion, single-character deletion (Backspace/Delete), and Tab/Shift+Tab (indent/dedent). Navigation and selection are owned by core, not the editor.

## IPC Protocol

- Receives: `MSG_KEY_EVENT`, `MSG_SET_CURSOR`, `MSG_EDITOR_CONFIG` from core
- Sends: `MSG_WRITE_INSERT`, `MSG_WRITE_DELETE`, `MSG_WRITE_DELETE_RANGE` to core

The editor has a read-only shared memory mapping of the document buffer (hardware-enforced via page table attributes). All writes go through IPC.
