# png

Sandboxed PNG decoder service. Decodes PNG images into BGRA8888 pixels via the generic decoder harness. Format-specific code is limited to header parsing and pixel decoding; all IPC plumbing is handled by `harness.rs`.

## Key Files

- `main.rs` — Entry point, adapter functions mapping `png_header`/`png_decode` to the harness signature
- `png.rs` — Full PNG decoder: no_std, no_alloc, supports all color types (0/2/3/4/6), all bit depths (1/2/4/8/16), Adam7 interlacing, PLTE palettes, tRNS transparency, CRC32 validation on every chunk. 162/162 PngSuite conformance.

## IPC Protocol

- Inherits the generic decode protocol from `harness.rs`
- Receives `DecodeRequest` from core on handle 1
- Sends `DecodeResponse` on handle 1

## Dependencies

- `sys` — Syscalls
- `ipc` — Channel communication
- `protocol` — Decode wire format (`protocol/decode.rs`)
