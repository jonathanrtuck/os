# Rendering Pipeline — Capabilities and Limitations

An honest audit of what this OS's rendering pipeline can and cannot do, compared against real systems. Updated 2026-03-20.

---

## Pipeline summary

Event-driven 2D document rendering with pluggable render services:

```text
Core (layout + scene build) → Scene Graph (shared memory) → Render Service → Display
```

The scene graph is the interface. Render services are thick GPU drivers that read the scene graph, perform the full tree walk, and produce pixels. Two render services:

- **`cpu-render`**: software rasterization via `CpuBackend` + virtio-gpu 2D presentation. Used for headless testing and non-virgl QEMU.
- **`virgil-render`**: GPU-accelerated rendering via Gallium3D command streams (virtio-gpu 3D / Virgl). Glyph atlas, image textures, stencil-then-cover path rendering. Same scene graph interface, hardware-accelerated compositing and blending.

Both live as sibling directories under `services/drivers/`. Init probes GPU capabilities at boot and selects the appropriate render service.

The pipeline uses a **configurable-cadence frame scheduler** (60/30/120fps) with event coalescing, frame budgeting, and idle optimization. Updates are driven by state changes (keystroke, clock tick, pointer move), coalesced within frame boundaries.

---

## What it does well

### Text rendering

The strongest part of the pipeline. Comparable to macOS Core Text for Latin text at common sizes.

- **TrueType rasterizer** with LCD subpixel rendering (6× horizontal, 4× vertical oversampling)
- **Stem darkening** for heavier strokes at small sizes
- **Variable font axes** (weight, optical size, MONO) with content-type-aware defaults
- **HarfBuzz-level shaping** via read-fonts (glyph lookup, horizontal metrics, kerning)
- **Glyph cache** (LRU, keyed on glyph ID + size + axis hash) — cache hits are memory copies
- **Gamma-correct blending** of glyph coverage maps (sRGB↔linear LUTs)

### Compositing

- **Porter-Duff source-over** with gamma-correct sRGB blending (integer math, lookup tables)
- **Per-pixel alpha** on all surfaces
- **Z-ordered back-to-front compositing** with insertion-sort by z-order (up to 16 surfaces)

Most desktop compositors skip gamma-correct blending. This pipeline gets it right.

### Damage tracking

- **512-bit dirty bitmap** in the scene header (one bit per node slot, 8 × u64 words) — no overflow, O(1) mark/test
- **Triple-buffered scene graph** with mailbox semantics — writer never blocks, reader always gets latest frame
- **Copy-forward** for selective mutation (acquire previous frame, modify only changed nodes, publish)
- **Four incremental update paths**: clock (in-place overwrite), cursor (position only), selection (truncate + rebuild), document content (re-layout visible lines)
- **Dirty rect derivation** from changed node positions → partial GPU transfer
- **Full-rebuild fallback** when data buffer exceeds 75%

Clock and cursor updates are near-zero-cost (no heap allocations, no layout). Document edits are O(visible_lines).

### Other 2D primitives

- **Gradients**: radial and vertical linear, dithered with Bayer 4×4 ordered dithering (band-free, deterministic)
- **Path rendering**: Content::Path with MoveTo/LineTo/CurveTo/Close, fill and stroke, cubic beziers
- **Path rasterization**: cubic bezier flattening (De Casteljau), scanline fill, non-zero winding, antialiased
- **Anti-aliased lines** (Wu's algorithm), filled/outlined rectangles, horizontal/vertical lines
- **Rounded corners**: SDF-based fill with anti-aliased edges, NEON SIMD, corner-radius-aware child clipping
- **Gaussian blur**: separable two-pass (horizontal/vertical), NEON SIMD, configurable radius/sigma, GPU-ready trait interface
- **Box shadows**: blur + offset + spread, declarative on Node, damage-tracking-aware
- **Layer opacity**: per-subtree opacity via offscreen compositing (group opacity), pool-based buffer management
- **2D affine transforms**: 3×3 matrix per node, transform composition through tree, transform-aware clipping
- **Bilinear image resampling**: for scaled/rotated content, ResamplingMethod enum for future Lanczos
- **PNG decoding** (DEFLATE, all filter types) from byte slices
- **Fractional DPI scaling** (f32 scale factors: 1.0, 1.25, 1.5, 2.0, etc.) with pixel-snapped borders and fractional font sizing

---

## What it cannot do

These are architectural constraints. Each would require structural additions to the pipeline.

### No 3D rendering

No 3D scene graph, no depth buffer, no projection matrix, no GPU compute. The `virgil-render` backend uses GPU hardware for 2D rendering (textured quads, glyph atlas, stencil-then-cover paths) but the pipeline is fundamentally 2D — no 3D geometry or scene representation.

**What it would take:** A 3D scene representation, vertex/mesh pipeline, camera/projection, lighting. Essentially a second rendering pipeline alongside the existing one.

**Who has this:** Every modern desktop OS (via OpenGL/Vulkan/Metal/DirectX). Game engines (Unity, Unreal, Godot).

**Implication for the OS:** 3D games, CAD software, WebGL-style content, and GPU-accelerated video decode are not possible. This is consistent with the project's document-centric focus — documents are 2D.

### No smooth animation _(partially addressed)_

The frame scheduler now provides configurable-cadence rendering (60/30/120fps) with event coalescing and frame budgeting. However, there is no animation timeline, no easing functions, no property interpolation between states.

When state changes, the scene graph is updated and rendered at the next frame boundary. There is no concept of "animate property X from value A to value B over 300ms." State transitions are instantaneous within a single frame.

**What remains:** An animation timeline with easing curves, property interpolation, and spring physics. The frame scheduler provides the cadence; what's missing is the interpolation layer on top.

**Who has this:** macOS Core Animation (implicit animation on every CALayer property), Android Choreographer, CSS transitions/animations, every game engine.

**Implication for the OS:** UI transitions (window open/close, panel slide, fade) are still step functions. No momentum scrolling, no spring physics, no kinetic gestures. But the frame scheduler ensures consistent frame pacing when animations are eventually added.

### No continuous motion _(infrastructure partially ready)_

The scene graph now supports sub-pixel scroll via `content_transform: AffineTransform` (f32 translation), replacing the old `scroll_y: i32`. The infrastructure for smooth scrolling exists, but the current implementation still uses integer point offsets. No momentum, no inertia, no velocity tracking.

**What it would take:** Fractional scroll offsets in core (the scene graph already supports them), momentum/decay physics for scroll flinging, frame-rate-independent integration.

**Who has this:** macOS (NSScrollView momentum), iOS (UIKit spring animations), every web browser (smooth-scroll), Wayland compositors.

**Implication for the OS:** Scrolling is functional but not fluid. The content_transform field means smooth scroll is a core-service change, not a pipeline change.

### ~~No 2D transforms~~ ✅ Implemented

2D affine transforms are now supported: 3×3 matrix per node, transform composition through the tree, transform-aware clipping. Supports rotation, scaling, skew, and translation. Bilinear image resampling for transformed content.

**Remaining gaps:** No rotated text rendering (text is always axis-aligned). No perspective transforms (3D). Coordinate system still uses `i16` (±32,767 points).

### ~~No layer opacity~~ ✅ Implemented

Per-subtree opacity is now supported via offscreen compositing (group opacity). A subtree is rendered to a temporary buffer, then composited onto the parent at the specified opacity. Pool-based buffer management avoids per-frame allocation.

Fade effects, disabled UI dimming, and translucent overlays work correctly with composed opacity.

### ~~No rounded corners~~ ✅ Implemented

`corner_radius` on Node is now fully functional. SDF-based rounded-rect fill with anti-aliased edges, NEON SIMD acceleration, and corner-radius-aware child clipping. Buttons, cards, dialogs, and avatar thumbnails render with smooth corners.

### ~~No blur or real shadows~~ ✅ Implemented

Separable Gaussian blur (two-pass horizontal/vertical) with NEON SIMD acceleration, configurable radius and sigma, and a GPU-ready trait interface. Box shadows with blur + offset + spread, declarative on Node, integrated with damage tracking.

**Remaining gaps:** No frosted-glass / backdrop blur (blurring content behind a surface). No drop shadows on arbitrary shapes (shadows are rectangular). Large blur radii are CPU-expensive without GPU compute.

### Non-rectangular clipping _(partially implemented)_

Corner-radius-aware clipping is now supported — child content is clipped to the parent's rounded rectangle. This covers the most common UI case (content inside rounded cards/panels).

**Remaining gaps:** No arbitrary path clipping (clip to circle, polygon, bezier outline). No clip masks or stencil buffer. Clipping is still fundamentally rectangular, with rounded-corner SDF as a special case.

**Who has this (full path clipping):** Every 2D renderer (Skia, Direct2D, Cairo, Core Graphics).

### No rich inline text

One font/style per Glyphs node. To have a bold word in a paragraph, you need separate Glyphs nodes manually positioned. No inline style changes within a run.

Additionally:

- No bidirectional text (RTL, Arabic, Hebrew)
- No complex script shaping beyond what read-fonts provides (no full Unicode text stack)
- No text decoration (underline, strikethrough) as a text primitive
- Monospace layout only (no proportional word-wrap / line-break algorithm)

**What it would take:** Run-level font/style switching with shaping across run boundaries, the Unicode BiDi algorithm (UAX #9), a proportional line-breaking algorithm (UAX #14 or Knuth-Plass), text decoration geometry.

**Who has this:** macOS Core Text, Pango + HarfBuzz + FriBidi, DirectWrite, web browsers.

**Implication for the OS:** Document rendering is limited to monospace plaintext. Rich text documents (.docx, .rtf, markdown) cannot be rendered faithfully. Non-Latin scripts are unsupported for layout (even if glyphs exist in the font).

### ~~No image resampling~~ ✅ Partially implemented

Bilinear resampling is now supported for scaled and rotated content (non-1:1 display). A `ResamplingMethod` enum exists for future Lanczos support.

**Remaining gaps:** No Lanczos or bicubic filtering for higher-quality downscaling. No mipmap generation. Bilinear is adequate for moderate scale changes but visibly soft for large downscales.

### No video or animated media

No codec integration, no frame decode pipeline, no audio subsystem, no A/V synchronization.

**What it would take:** Codec library (AV1/H.264/VP9 decoder), frame buffer ring, audio output driver, A/V sync clock, timeline playback control.

**Who has this:** Every modern OS has a media framework (AVFoundation, GStreamer, MediaFoundation).

### ~~No fractional DPI scaling~~ ✅ Implemented

Scale factor is now `f32`, supporting any fractional value (1.0, 1.25, 1.5, 2.0, etc.). Pixel-snapped borders prevent sub-pixel artifacts. Fractional font sizing with glyph cache keyed on physical pixel size.

### No multi-display

Single framebuffer, single resolution, configured at init. No display hotplug, no per-display scale/refresh.

**What it would take:** Display enumeration, per-display scanout, independent resolution/scale/refresh per output.

**Who has this:** Every desktop OS, Wayland (wl_output), X11 (XRandR).

---

## Performance envelope

| Metric                 | Current state                                              | Bottleneck                                                       | Practical ceiling                                                                            |
| ---------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Resolution             | 1024×768 (hardcoded)                                       | CPU compositing bandwidth; virtio-gpu copy cost                  | ~4K at low refresh with NEON SIMD. CPU-bound without GPU compositing                         |
| Compositing throughput | Scalar per-pixel blend_over                                | 1280×800 @ 60fps full recomposite = ~245 MB/s                    | NEON could do ~4× (4 pixels/cycle). Still CPU-bound for full-screen recomposite at high res  |
| Text rendering         | Cache hit = memcpy. Miss = bezier flatten + scanline sweep | Cache misses are expensive. LRU eviction under font-size variety | Adequate for document editing. Would struggle with many font sizes or rapid font switching   |
| Scene graph            | 512 nodes max, 64 KB data buffer, triple-buffered          | Fixed. Selection rects and glyph runs consume capacity           | Sufficient for single-document text editing. Complex compound documents would hit limits     |
| Frame cadence          | Configurable (60/30/120fps) with event coalescing          | Frame budget enforcement. Heavy layout can miss deadline         | 60fps = 16ms budget. Event coalescing prevents redundant frames. Idle optimization saves CPU |
| Damage tracking        | 512-bit dirty bitmap + dirty rects                         | Full repaint on data buffer exhaustion                           | Effective for all editor interactions. Bitmap covers all 512 node slots without overflow     |

---

## Comparison to real systems

| System                                    | Model                                                               | Similarity                                                                          | Key difference                                                                   |
| ----------------------------------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| **macOS Quartz / Core Animation**         | GPU-composited layer tree, hardware blur/shadow, implicit animation | Same scene-graph→compositor split                                                   | macOS has GPU compositing, per-layer transforms, implicit animation, blur        |
| **Fuchsia Scenic**                        | Scene graph in shared memory, GPU-composited                        | Closest architectural prior art                                                     | Scenic has GPU rendering, 3D node types, view embedding                          |
| **Wayland compositors**                   | Client renders to buffer, compositor composites via EGL/Vulkan      | Different: your compositor is the sole renderer, not a compositor of client buffers | Wayland clients own their rendering; your editors don't render at all            |
| **Plan 9 rio**                            | CPU-rasterized, rectangular windows, no compositing                 | Similar simplicity                                                                  | Your pipeline has alpha blending, subpixel text, scene graph, damage tracking    |
| **Game engines (2D)**                     | Frame-driven render loop, GPU batched draws, animation system       | Frame scheduler now provides similar cadence                                        | Game engines have GPU batching, animation timelines, and continuous rendering    |
| **Terminal emulators (kitty, alacritty)** | Fixed-width glyph grid, GPU-accelerated text, damage tracking       | Closest functional analog today                                                     | The render pipeline is a very good terminal renderer with chrome and glyph icons |

---

## Summary

This is a **high-quality 2D document renderer for static and semi-static content**. Text rendering is genuinely excellent. The architecture (one-way pipeline, scene graph interface, damage tracking) is clean and well-separated.

The gap between this and a modern desktop compositor is approximately: animation timeline + multi-display + proportional text layout + backdrop blur + arbitrary path clipping + smooth scroll/momentum. The pipeline has closed several major gaps (GPU rendering, rounded corners, blur/shadows, transforms, fractional scaling, layer opacity, image resampling, frame scheduling, incremental damage tracking). The remaining items are leaf-node complexity behind the existing scene graph interface. The architecture does not prevent any of these additions — it accommodates them.

The design is coherent for its stated purpose: a document-centric OS where documents are first-class and tools attach to content. The rendering pipeline renders documents. It does not try to be a general-purpose graphics engine, and it should not.
