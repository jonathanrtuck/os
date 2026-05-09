# layout

Layout service — pure function from document content to positioned text runs.
Reads the document buffer and viewport state, computes line breaks, writes
positioned lines to a shared VMO.

## Responsibilities

- Reads document buffer VMO (RO, from document service via SETUP)
- Reads viewport state via seqlock register (from presenter)
- Computes layout using the `layout` library (layout_paragraph + WordBreaker)
- Writes results to seqlock-protected layout results VMO
- Replies to RECOMPUTE with updated layout stats

## Key Files

- `src/lib.rs` -- protocol definitions (SETUP, RECOMPUTE, GET_INFO), wire
  formats (ViewportState, LayoutHeader, LineInfo, SetupReply, InfoReply)
- `src/main.rs` -- service implementation, seqlock write, font metrics adapter

## IPC Protocol

### Serves (on own endpoint, registered as "layout")

- `SETUP` -- presenter sends viewport state VMO handle, receives layout results
  VMO handle (RO). Establishes the shared memory data plane.
- `RECOMPUTE` -- trigger immediate relayout. Replies with InfoReply when done.
- `GET_INFO` -- returns current layout statistics without recomputing.

### Calls (outbound to document service)

- `SETUP` -- receives document buffer VMO handle (RO)

## Shared Memory Layout

### Viewport State VMO (input, read by layout)

Seqlock register (ipc::register) with ViewportState value (20 bytes): scroll_y
(i32), viewport_width (u32), viewport_height (u32), char_width_fp (u32,
fixed-point 16.16), line_height (u32).

### Layout Results VMO (output, written by layout)

Seqlock protocol (manual odd/even generation bumps):

| Offset | Size     | Field      | Notes                      |
| ------ | -------- | ---------- | -------------------------- |
| 0      | 8        | generation | AtomicU64, seqlock counter |
| 8      | 16       | header     | LayoutHeader               |
| 24     | 20×lines | lines      | LineInfo array (max 512)   |

LayoutHeader: line_count (u32), total_height (i32), content_len (u32), reserved.
LineInfo: byte_offset (u32), byte_length (u32), x (f32), y (i32), width (f32).

## Dependencies

Services: document (via name service watch) Libraries: abi, ipc, layout, name,
console, document-service (protocol only)

## Testing

```sh
# Protocol round-trip tests (host target, no service deps)
cd user/servers/layout
cargo test --lib --no-default-features --target aarch64-apple-darwin

# Integration test: runs as test-layout service under hypervisor with disk
mkdisk /tmp/test.img 512
hypervisor --no-gpu --timeout 45 --drive /tmp/test.img target/aarch64-unknown-none/release/kernel
```
