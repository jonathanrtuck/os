# decoders

Parent directory for sandboxed content decoder services. Each decoder runs as an isolated process behind the generic decode protocol (`protocol/decode.rs`). Format-specific code is minimal; all IPC plumbing lives in the shared harness.

## Key Files

- `harness.rs` — Generic decoder service harness: config reception, IPC loop, bounds checking, heap-allocated decode buffer, Content Region copy. Format decoders supply only `header()` and `decode()` functions.

## IPC Protocol

- Receives `DecodeRequest` from core (file offset, length, flags, output location)
- Sends `DecodeResponse` to core (status, dimensions, bytes written)
- `DECODE_FLAG_HEADER_ONLY` — Header-only query returns dimensions without decoding

## Architecture

Each decoder service:

1. Receives config from init (File Store VA/size, Content Region VA/size)
2. Reads encoded file data from File Store (read-only shared memory)
3. Decodes into a private heap buffer (BGRA pixels + decompression scratch)
4. Copies final BGRA pixels into Content Region (read-write shared memory)
5. Content Region holds only clean pixel data; scratch stays private

## Subdirectories

- `png/` — PNG decoder service (see `png/CLAUDE.md`)
