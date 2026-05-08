# protocol

IPC message type definitions for every service boundary in the userspace. Single
source of truth for message layouts.

## Structure

Remaining module (for services that don't exist yet):

- `decode` -- Content decoding requests/replies (e.g., PNG)

Protocols that have moved to their server crate's lib.rs:

- `name_service` -> `servers/name`
- `bootstrap` -> `servers/init`
- `blk` -> `servers/drivers/blk`
- `input` -> `servers/drivers/input`
- `metal` -> `servers/drivers/render`
- `store` -> `servers/store`
- `edit`, `view` -> `servers/document`

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
