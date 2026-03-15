# Environment

## System

- macOS (darwin 25.3.0), 48GB RAM, 14 CPU cores
- Rust toolchain with aarch64-unknown-none target
- QEMU for visual verification (optional per-feature, required at milestone boundaries)

## Project Structure

```
system/
  build.rs              — Custom build orchestration (rlibs + ELFs)
  Cargo.toml            — Kernel crate (builds via cargo, triggers build.rs)
  libraries/
    sys/                — Syscall wrappers (rustc-compiled rlib)
    protocol/           — IPC message types (rustc-compiled rlib)
    virtio/             — Virtio MMIO transport (rustc-compiled rlib)
    scene/              — Scene graph data types (rustc-compiled rlib)
    ipc/                — Ring buffer IPC (rustc-compiled rlib)
    drawing/            — Pixel primitives (rustc-compiled rlib, depends on protocol+fonts)
    shaping/            — Font pipeline (cargo-managed, has external deps)
    link.ld             — Userspace linker script
  services/
    init/               — Proto-OS-service, process spawner
    core/               — OS service: documents, layout, scene graph building
    compositor/         — Pixel pump: scene rendering, damage tracking
    drivers/            — virtio-{blk,gpu,input,9p,console}
  test/                 — Host-side test crate (1,462+ tests)
  user/                 — User programs (text-editor, echo, stress, fuzz)
```

## Build Commands

- Full build: `cd system && cargo build --release`
- Tests: `cd system/test && cargo test -- --test-threads=1`
- Fonts library standalone: `cd system/libraries/shaping && cargo build --target=aarch64-unknown-none --release`

## Key Files for This Mission

- `system/build.rs` — Must be updated for library renames/moves
- `system/test/Cargo.toml` — Must be updated for library renames
- `system/libraries/drawing/lib.rs` — The 2000+ line file being slimmed down (uses include!() pattern)
- `system/libraries/drawing/*.rs` — Sub-files included into lib.rs
- `system/libraries/shaping/src/*.rs` — Being renamed to fonts
- `system/services/core/{main.rs,scene_state.rs}` — Absorbing layout logic
- `system/services/compositor/{main.rs,scene_render.rs,scene_state.rs}` — Absorbing compositor concerns
