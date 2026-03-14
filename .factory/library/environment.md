# Environment

Environment variables, external dependencies, and setup notes.

**What belongs here:** Required env vars, external dependencies, dependency quirks, platform-specific notes.
**What does NOT belong here:** Service ports/commands (use `.factory/services.yaml`).

---

## Toolchain

- **Rust nightly** (pinned in `rust-toolchain.toml`): `channel = "nightly"`, target `aarch64-unknown-none`
- **QEMU 10.2.1**: `/opt/homebrew/bin/qemu-system-aarch64`
- **Python 3.9.6** with Pillow: for PPM→PNG screenshot conversion
- **netcat (nc)**: macOS BSD variant, for QEMU monitor socket communication

## Build Notes

- Single Cargo workspace at `system/Cargo.toml`
- `build.rs` compiles the entire userspace as a sub-build (libraries as rlibs, programs as standalone ELFs)
- Init embeds all userspace ELFs via `include_bytes!()`; kernel embeds only init
- Profile: `opt-level = 3`, `panic = "abort"` for both dev and release
- Linker script: `kernel/link.ld`

## Assets

- Fonts and other runtime assets go in `system/share/` (mounted via virtio-9p at `hostshare`)
- Currently: `source-code-pro.ttf` (9,436 bytes)
- New assets for this mission: proportional font, PNG test image, SVG icons

## Testing Notes

- **Single-threaded tests required**: `cargo test -- --test-threads=1` — kernel logic uses global statics that aren't thread-safe across concurrent test functions.
- **page_allocator global state**: `PageAllocator::init()` ADDS pages to existing state (it does not reset). All buddy allocator stress tests must either run in a single test function or carefully drain state between tests. Multiple test functions calling `init()` will accumulate pages and produce incorrect free counts.
- **Slab allocator global state**: Similar to page_allocator — slab caches persist across test functions within the same binary. Tests that exercise slab exhaustion should be isolated or share a single test function.
- **xorshift64 PRNG**: Stress tests use a custom xorshift64 Rng struct (no external `rand` dependency). The pattern is defined inline in each test file. See existing `stress_*.rs` files for the implementation.

## QEMU Notes

- DTB loaded at 0x40000000 via `-device loader` (HVF on macOS doesn't pass DTB in x0)
- `run-qemu.sh` auto-generates `virt.dtb` if missing
- `sendkey` via monitor socket works for basic ASCII input to virtio-keyboard
- 8 second boot wait is reliable for the healthcheck
