# text-editor

Content-type leaf node: translates key events into document edits. The first
editor in the system — thin translation layer between the presenter (input
routing) and the document service (buffer management).

## Architecture

The presenter owns cursor movement (it has layout knowledge). The editor handles
only content mutations:

- Printable character insertion
- Backspace (delete before cursor)
- Delete (delete after cursor)
- Return (insert newline)
- Tab (insert 4 spaces)
- Shift+Tab (dedent — remove up to 4 leading spaces from current line)

## IPC Protocol

### Serves (on own endpoint, registered as "editor.text")

- `DISPATCH_KEY` — presenter sends `KeyDispatch` (key_code, modifiers,
  character), editor translates to document ops and replies with `KeyReply`
  (action, content_len, cursor_pos)

### Calls (outbound to document service)

- `SETUP` — get doc buffer VMO (read-only shared memory)
- `INSERT` — insert bytes at cursor
- `DELETE` — delete byte range

## Shared Memory

Document buffer (read-only mapping from document service):

| Offset | Size | Field       | Notes                      |
| ------ | ---- | ----------- | -------------------------- |
| 0      | 8    | content_len | u64 LE                     |
| 8      | 8    | cursor_pos  | u64 LE                     |
| 16     | 4    | generation  | AtomicU32, Release/Acquire |
| 64     | ...  | content     | raw bytes                  |

## Bootstrap

Handle 2: name service endpoint. Discovers console and document via
`name::watch`.

## Key Files

- `src/lib.rs` — protocol definitions (DISPATCH_KEY, KeyDispatch, KeyReply, HID
  key codes)
- `src/main.rs` — service implementation, key-to-edit translation

## Testing

```sh
# Protocol tests (host target)
cargo test --lib --no-default-features --target aarch64-apple-darwin

# Integration test: runs as test-editor service under hypervisor
# Verifies: insert, backspace, multi-char, return, tab, shift+tab, delete
```
