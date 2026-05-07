# protocol

IPC message type definitions for every service boundary in the userspace. Single
source of truth for message layouts.

## Structure

One module per protocol boundary:

- `name_service` -- Register/Lookup/Unregister for service discovery
- `bootstrap` -- One-shot config delivery from init to services
- `input` -- Keyboard and pointer events (16-byte ring buffer slots)
- `edit` -- Editor-to-document editing operations
- `store` -- Document-to-store persistence operations
- `view` -- Document-to-presenter notifications
- `decode` -- Content decoding requests/replies (e.g., PNG)

## Conventions

- `no_std`, zero dependencies -- pure data definitions
- Manual little-endian serialization via `write_to`/`read_from`
- Every type has a `SIZE` constant matching its encoded byte length
- All payloads fit within 120 bytes (`MAX_PAYLOAD`)
- Handle transfers are out-of-band via IPC handle slots, not payload
- Method IDs are per-service (each protocol starts from 1)

## Testing

```sh
cargo test --target aarch64-apple-darwin
```

Host target required -- `aarch64-unknown-none` has no test harness.
