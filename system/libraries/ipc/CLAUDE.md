# ipc

Lock-free SPSC ring buffer for 64-byte IPC messages over shared memory. Each channel has two pages (one per direction), each containing a ring of fixed-size message slots. `no_std`, depends on `sys` only on bare-metal target.

## Key Files

- `lib.rs` -- `Channel` (bidirectional IPC), `RingBuf` (single-direction SPSC ring), `send`/`try_recv`/`recv_blocking`. Constants: `SLOT_SIZE` (64 bytes), `PAYLOAD_SIZE` (60 bytes), `SLOT_COUNT` (254 per page)
- `build.rs` -- Sets `SYSTEM_CONFIG` env var pointing to `system_config.rs` for PAGE_SIZE

## Dependencies

- `sys` (bare-metal only, cfg-gated) -- for `channel_signal` and `wait` syscalls in `recv_blocking`

## Conventions

- Ring buffer page layout: producer header (64B) + consumer header (64B) + 254 message slots (64B each) = 16 KiB
- Head and tail on separate cache lines to avoid false sharing
- Memory ordering: producer writes payload (relaxed) then increments head (release); consumer reads head (acquire) then payload then increments tail (release)
- Endpoint index (0 or 1) determines which page is send vs recv
- `recv_blocking()` handles spurious wakeups with a retry loop -- safe for synchronous RPC patterns
