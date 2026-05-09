# render

Scene graph rendering library: compositing, damage tracking, clip masks,
incremental repaint, and offscreen buffer management.

## Pure rendering library

No dependency on `abi` or `ipc` -- this is a pure computation library.
Dependencies: `drawing`, `scene`, `fonts`. Anything that touches syscalls or IPC
belongs in the compositor (`user/servers/drivers/render/`), not here.

## Testing

Tests run on the **host target**, not bare-metal:

```sh
cargo test --manifest-path user/libraries/render/Cargo.toml \
    --lib --no-default-features --target aarch64-apple-darwin
```

This is also wired into `make test`. The `--no-default-features` flag is
required because the default build is `no_std` for bare-metal.

## Key modules

- `scene_render/` -- walks the scene graph and emits pixels (walk, coords,
  content, path_raster)
- `incremental.rs` -- per-node dirty tracking from scene graph diffs
- `damage.rs` -- dirty rectangle tracking and coalescing
- `clip_mask.rs` -- per-node clip mask cache
- `surface_pool.rs` -- offscreen buffer allocation and reuse
- `geometry.rs` -- coordinate math (points, rects, transforms)
- `cache.rs` -- render cache (glyph atlases, rasterized paths)
