# Hypervisor & Metal-Render Audit Findings

Audit date: 2026-03-21. Both the Swift hypervisor host and the Rust metal-render guest driver were reviewed for correctness, robustness, security, performance, and feature completeness.

Findings are prioritized. Fix the top ones first; the rest are improvements for when the relevant code is next touched.

---

## Hypervisor (host, Swift)

### Critical

- [ ] **9P path traversal** — `Virtio9P.swift` builds file paths with `appendingPathComponent()` without sanitizing `..` segments. A guest can walk to `../../../etc/passwd` and read arbitrary host files. Fix: reject any path component that is `..` or contains path separators.

### High

- [ ] **Texture size DoS** — `VirtioMetal.swift` `createTexture` accepts guest-supplied width/height without upper bounds. A malicious guest can request 65535x65535 textures, allocating gigabytes of VRAM. Fix: cap max texture dimensions (e.g., 4096x4096).

### Medium

- [ ] **Event queue overflow** — `VirtioInput.swift` `pendingEvents` grows unbounded if the guest never posts receive buffers. Fix: cap at ~256 entries, drop oldest.
- [ ] **Unknown command logging** — `VirtioMetal.swift` silently skips unknown command IDs without logging (even in non-verbose mode). Fix: always log unknown commands at least once.

### Low

- [ ] **DTB hardcodes 4 CPUs** — `DTB.swift` `minimal()` loops `0..<4` regardless of the actual `cpuCount` parameter. Fix: pass and use `cpuCount`.
- [ ] **Vendor ID string** — `VirtioMMIO.swift` reports vendor ID `0x554D4551` ("QEMU"). Should use a custom identifier since this isn't QEMU.

---

## Metal-Render Driver (guest, Rust)

### High

- [ ] **`node.transform` not applied** — The full 2D affine transform (`node.transform`) is completely ignored during scene walk. Only `content_transform.tx/ty` (scroll offset) is used. Any node with rotation, scale, or skew will render at wrong position/size. Not currently used by the core service, but the field exists and will break when transforms are introduced.
- [ ] **`corner_radius` not rendered** — `node.corner_radius` is present in the scene graph but never applied. All rectangles render with sharp corners. Visibly wrong: the frosted glass demo panel has `corner_radius: 8` but renders as a sharp rectangle. Options: SDF fragment shader, or reuse stencil clip infrastructure with a rounded-rect path.

### Medium

- [ ] **`border` not rendered** — `node.border` (color, width, padding) is ignored. No border quads are emitted. Will be needed for text input frames and UI chrome.
- [ ] **`FillRule` ignored** — `Content::Path` match arm uses `fill_rule: _` wildcard. Always assumes winding rule. Paths with holes or self-intersections render incorrectly under even-odd rule. Fix: use XOR stencil operation for even-odd, current replace for winding.
- [ ] **Blur bounds could overflow** — `px + pw + pad` in blur capture region calculation uses plain u32 arithmetic. While unlikely to overflow in practice (max framebuffer 1024x768), saturating arithmetic (`saturating_add`) would be defensive. Same for `py + ph + pad`.

### Low

- [ ] **Atlas overflow is silent** — When the glyph atlas fills up (512x512, ~95 ASCII glyphs), `pack()` returns false and the glyph is silently skipped. No warning is printed. Fix: log a one-time warning when the atlas is full.
- [ ] **Path flattening truncation** — Cubic Bezier flattening stops at MAX_PATH_POINTS (256). Complex paths silently lose segments. Fix: increase constant or log a warning.

---

## Not Issues (auditor false positives, recorded for context)

- **`fatalError` on shader compilation** — Intentional. Shader source is our own hardcoded MSL, not user input. A compilation failure means our code is wrong.
- **scratch_ptr "memory leak"** — Process-lifetime allocation in `no_std`. The driver runs until system shutdown; this is just memory, not a leak.
- **Image pixel overflow** — Already bounded by `IMG_TEX_DIM = 1024` check before multiplication.
- **`Layout::unwrap()` panic** — Can't fail for fixed-size types with known alignment.
- **Redundant `nextDrawable()`** — Already guarded by `if currentDrawable == nil`.
- **AppWindow texture copy per frame** — Unused code path (old cpu-render display, not Metal passthrough).
- **Sync image upload performance** — 1-2 images per frame, sub-millisecond. Not a bottleneck.
