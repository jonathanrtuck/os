# Hypervisor & Metal-Render Audit Findings

Audit date: 2026-03-21. Both the Swift hypervisor host and the Rust metal-render guest driver were reviewed for correctness, robustness, security, performance, and feature completeness.

All findings resolved 2026-03-21.

---

## Hypervisor (host, Swift)

### Critical

- [x] **9P path traversal** — Two-layer defense: (1) reject `..`, `.`, `/`, `\0` in walk components, (2) resolve symlinks and verify path stays within rootPath. Returns EPERM on violation.

### High

- [x] **Texture size DoS** — Capped at 8192x8192. Rejects zero-dimension or oversized textures with a log message.

### Medium

- [x] **Event queue overflow** — Capped `pendingEvents` at 256 entries. Drops oldest when full.
- [x] **Unknown command logging** — Logs each unknown command ID exactly once (deduplicated via `Set<UInt16>`), regardless of verbose mode.

### Low

- [x] **DTB hardcodes 4 CPUs** — `cpuCount` parameter added to `DTB.minimal()`. `main.swift` uses a shared `cpuCount` constant for both DTB and VM boot.
- [x] **Vendor ID string** — Changed from `0x554D4551` ("QEMU") to `0x4143_4F53` ("ACOS" — Arts & Crafts OS hypervisor).

---

## Metal-Render Driver (guest, Rust)

### High

- [x] **`node.transform` not applied** — Full 2D affine transform now applied during scene walk. Identity and pure-translation take the fast path. Non-trivial transforms (rotation, scale, skew) generate per-vertex transformed positions via `emit_transformed_quad()`. AABB computed for clip/scissor purposes. 8 demo scenes showcase each transform type.
- [x] **`corner_radius` not rendered** — Implemented via SDF (signed distance field) fragment shader `fragment_rounded_rect`. Evaluates `sd_rounded_rect()` per pixel for subpixel-accurate anti-aliased corners at any radius. New pipeline `PIPE_ROUNDED_RECT` with uniform buffer for rect params. The frosted glass demo now renders with correct rounded corners.

### Medium

- [x] **`border` not rendered** — Handled by the same SDF rounded-rect shader. The `RoundedRectParams` uniform includes border width and color. Border region computed as the SDF annulus between the outer edge and `dist + border_width`. Composited as border-over-fill in a single fragment shader pass. Demo scenes include bordered rects, border+corner_radius, and border-only (no fill) nodes.
- [x] **`FillRule` ignored** — Added `DSS_STENCIL_INVERT` depth-stencil state with `STENCIL_INVERT` operation (added to metal protocol). Winding rule uses `STENCIL_REPLACE` (nonzero everywhere), even-odd uses `STENCIL_INVERT` (XOR flips on overlap, odd count = inside). `draw_path_stencil_cover()` selects based on `fill_rule` parameter.
- [x] **Blur bounds could overflow** — All `px + pw + pad` calculations now use `saturating_add()` / `saturating_sub()`.

### Low

- [x] **Atlas overflow is silent** — One-time warning printed via serial when `pack()` returns false.
- [x] **Path flattening truncation** — `MAX_PATH_POINTS` increased from 256 to 512. One-time warning printed when the limit is reached.

---

## Not Issues (auditor false positives, recorded for context)

- **`fatalError` on shader compilation** — Intentional. Shader source is our own hardcoded MSL, not user input. A compilation failure means our code is wrong.
- **scratch_ptr "memory leak"** — Process-lifetime allocation in `no_std`. The driver runs until system shutdown; this is just memory, not a leak.
- **Image pixel overflow** — Already bounded by `IMG_TEX_DIM = 1024` check before multiplication.
- **`Layout::unwrap()` panic** — Can't fail for fixed-size types with known alignment.
- **Redundant `nextDrawable()`** — Already guarded by `if currentDrawable == nil`.
- **AppWindow texture copy per frame** — Unused code path (old cpu-render display, not Metal passthrough).
- **Sync image upload performance** — 1-2 images per frame, sub-millisecond. Not a bottleneck.
