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
- Currently: `SourceCodePro-Regular.ttf` (9,436 bytes)
- New assets for this mission: proportional font, PNG test image, SVG icons

## QEMU Notes

- DTB loaded at 0x40000000 via `-device loader` (HVF on macOS doesn't pass DTB in x0)
- `run-qemu.sh` auto-generates `virt.dtb` if missing
- `sendkey` via monitor socket works for basic ASCII input to virtio-keyboard
- 8 second boot wait is reliable for the healthcheck
