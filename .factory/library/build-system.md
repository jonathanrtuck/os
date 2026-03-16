# Build System Notes

## rlib compilation chain

`system/build.rs` compiles userspace into ELFs embedded in the kernel. Libraries are hand-compiled rlibs via `rustc --crate-type=rlib`. The chain is:

1. `sys` (no deps)
2. `protocol` (no deps)
3. `virtio` (depends on sys)
4. `scene` (no deps)
5. `ipc` (no deps)
6. `fonts` (Cargo-managed — has harfrust dependency tree)
7. `drawing` (depends on protocol, fonts search paths)
8. `render` (depends on drawing, scene, protocol, fonts search paths) — **Added in Phase 1 extract**
9. Programs (compositor, core, drivers, etc.) link against required rlibs

To add a new library: add a `rustc_rlib` call in build.rs after its dependencies, then add `--extern` flags to downstream consumers.

## rerun-if-changed gap for service submodules

`system/build.rs` compiles each service via direct `rustc` invocation. The PROGRAMS loop only emits `cargo:rerun-if-changed` for each program's `main.rs`.

Services that use `#[path = "..."]` to include submodules are **not tracked** for incremental rebuilds. If you edit a submodule without touching `main.rs`, Cargo won't trigger a rebuild.

**Workaround:** After editing a service submodule, touch its `main.rs` or run `cargo clean` first.

**Scope:** Affects all services with submodules. Currently known:
- `services/core/main.rs` includes: `scene_state.rs`, `fallback.rs`, `typography.rs`
- `services/compositor/main.rs` includes: `frame_scheduler.rs` (rendering modules moved to libraries/render/)
