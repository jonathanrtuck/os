# Environment

Environment variables, external dependencies, and setup notes.

**What belongs here:** Required env vars, external API keys/services, dependency quirks, platform-specific notes.
**What does NOT belong here:** Service ports/commands (use `.factory/services.yaml`).

---

## Build Environment

- **Host:** macOS aarch64 (Apple Silicon)
- **Rust target:** `aarch64-unknown-none` (bare-metal) for kernel, `aarch64-apple-darwin` for tests
- **NEON SIMD:** Available natively on host (aarch64 macOS) — NEON tests run on host without cross-compilation
- **Python3 PIL:** Available for PPM→PNG screenshot conversion
- **QEMU:** v10.2.1 at `/opt/homebrew/bin/qemu-system-aarch64`

## Project Structure

- `system/` — all OS source (kernel, services, libraries, test crate)
- `system/test/` — host-side test crate with own .cargo/config.toml overriding target to darwin
- `system/share/` — fonts and assets loaded via 9P passthrough
- `prototype/` — macOS-hosted prototypes (Files interface)
- `design/` — design documents and journal
