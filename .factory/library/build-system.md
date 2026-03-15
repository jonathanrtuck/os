# Build System Notes

## rerun-if-changed gap for service submodules

`system/build.rs` compiles each service (init, compositor, core, drivers) via direct `rustc` invocation. The PROGRAMS loop only emits `cargo:rerun-if-changed` for each program's `main.rs`.

Services that use `#[path = "..."]` to include submodules (e.g., Core includes `scene_state.rs`, `fallback.rs`, `typography.rs`) are **not tracked** for incremental rebuilds. If you edit a submodule without touching `main.rs`, Cargo won't trigger a rebuild of that service's ELF.

**Workaround:** After editing a service submodule, touch its `main.rs` or run `cargo build --release` with a clean build (`cargo clean` first).

**Scope:** Affects all services with submodules. Currently known:
- `services/core/main.rs` includes: `scene_state.rs`, `fallback.rs`, `typography.rs`
- `services/compositor/main.rs` includes: `scene_render.rs`, `damage.rs`, `compositing.rs`, `cursor.rs`, `svg.rs`
