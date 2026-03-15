# Rendering Pipeline — Capabilities and Limitations

An honest audit of what this OS's rendering pipeline can and cannot do, compared against real systems. Written against the architecture as of 2026-03-15 (incremental scene graph mission in progress).

---

## Pipeline summary

CPU-rasterized, event-driven, 2D document rendering:

```text
Core (layout + scene build) → Scene Graph (shared memory) → Compositor (pixel pump) → GPU Driver (transfer) → Display
```

All rendering is software. The GPU is a dumb transport (virtio-gpu copies guest→host framebuffer). There is no GPU-accelerated compositing, no shaders, no hardware-accelerated blending.

The pipeline is **demand-driven**, not frame-driven. Updates happen in response to state changes (keystroke, clock tick, pointer move), not on a fixed cadence. There is no vsync loop, no frame budget, no animation clock.

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

### Damage tracking (post-mission)

The incremental scene graph mission adds:

- **Per-node change list** in the scene header (up to 24 changed node IDs per frame)
- **Copy-front-to-back** for selective mutation (only touch changed nodes)
- **Four incremental update paths**: clock (in-place overwrite), cursor (position only), selection (truncate + rebuild), document content (re-layout visible lines)
- **Dirty rect derivation** from changed node positions → partial GPU transfer
- **Full-rebuild fallback** when data buffer exceeds 75% or change list overflows

Clock and cursor updates are near-zero-cost (no heap allocations, no layout). Document edits are O(visible_lines).

### Other 2D primitives

- **Gradients**: radial and vertical linear, dithered with Bayer 4×4 ordered dithering (band-free, deterministic)
- **SVG path rasterization**: cubic bezier flattening (De Casteljau), scanline fill, non-zero winding, antialiased. Adequate for icons
- **Bresenham lines**, filled/outlined rectangles, horizontal/vertical lines
- **PNG decoding** (DEFLATE, all filter types) from byte slices
- **Integer display scaling** (1×, 2×). Glyph cache rasterized at physical pixel size

---

## What it cannot do

These are architectural constraints. Each would require structural additions to the pipeline.

### No 3D rendering

No geometry pipeline, no vertex/fragment shaders, no depth buffer, no projection matrix, no GPU compute. All rendering is 2D scanline rasterization on CPU.

**What it would take:** A GPU abstraction layer (Vulkan/Metal-style), a 3D scene representation, shader compilation. Essentially a second rendering pipeline alongside the existing one.

**Who has this:** Every modern desktop OS (via OpenGL/Vulkan/Metal/DirectX). Game engines (Unity, Unreal, Godot).

**Implication for the OS:** 3D games, CAD software, WebGL-style content, and GPU-accelerated video decode are not possible. This is consistent with the project's document-centric focus — documents are 2D.

### No smooth animation

The pipeline is event-driven, not frame-driven. There is no render loop, no animation clock, no easing functions, no interpolation between states.

When state changes, the scene graph is rebuilt and rendered immediately. There is no concept of "animate property X from value A to value B over 300ms." State transitions are instantaneous.

**What it would take:** A vsync-driven render loop (or timer-driven frame scheduler), an animation timeline with easing curves, property interpolation, and frame budgeting (skip frames if overdue).

**Who has this:** macOS Core Animation (implicit animation on every CALayer property), Android Choreographer, CSS transitions/animations, every game engine.

**Implication for the OS:** UI transitions (window open/close, panel slide, fade) would be jarring step functions. Cursor blink requires a timer hack. No momentum scrolling, no spring physics, no kinetic gestures.

### No continuous motion

Scroll is discrete — integer line jumps with no sub-pixel offset, no momentum, no inertia. Pointer movement updates the cursor position but there is no velocity tracking or physics simulation.

**What it would take:** Sub-pixel scroll positions (fractional pixel offsets in layout), momentum/decay physics for scroll flinging, frame-rate-independent integration.

**Who has this:** macOS (NSScrollView momentum), iOS (UIKit spring animations), every web browser (smooth-scroll), Wayland compositors.

**Implication for the OS:** Scrolling feels like a 1990s text editor — functional but not fluid. Trackpad gestures would feel broken without momentum.

### No 2D transforms

No rotation, skew, or non-axis-aligned scaling. Nodes are axis-aligned rectangles with (x, y, width, height) in `i16` coordinates. The coordinate system limits positions to ±32,767 logical pixels.

**What it would take:** A 2D affine transform matrix per node (or 3×3 for perspective), transform-aware clipping, rotated text rendering, matrix composition through the tree.

**Who has this:** macOS Core Animation (CATransform3D on every layer), CSS transform, Fuchsia Scenic, Skia, every game engine's 2D mode.

**Implication for the OS:** No rotated images, no skewed panels, no page-turn effects. Image viewers cannot rotate photos. PDF rendering with rotated pages would need workarounds.

### No layer opacity

The `opacity` field exists on Node but is not used by the renderer. All compositing is per-pixel alpha, not per-subtree opacity.

Layer opacity requires rendering a subtree to an offscreen buffer, then compositing that buffer at the specified opacity. This needs temporary surface allocation (render-to-texture).

**Who has this:** Every modern compositor (Core Animation, Wayland, CSS opacity). It's a basic expectation.

**Implication for the OS:** Fade effects, disabled UI dimming, and translucent overlays would need per-pixel alpha baked into every leaf node's color — fragile and non-composable.

### No rounded corners

The `corner_radius` field exists on Node but the renderer ignores it. All clipping and filling is rectangular.

**What it would take:** Rounded-rect fill (SDF-based or geometry-based), corner-radius-aware clipping (alpha mask or per-pixel SDF test).

**Who has this:** macOS (CALayer.cornerRadius), CSS border-radius, Android (RoundedCornerShape). Universal in modern UI.

**Implication for the OS:** UI will look angular. Buttons, cards, dialogs, avatar thumbnails — anything with rounded corners — would appear flat and dated.

### No blur or real shadows

Shadows are currently faked with semi-transparent solid rectangles. There is no Gaussian blur, no box blur, no convolution kernel of any kind.

**What it would take:** Separable Gaussian blur kernel (two-pass horizontal/vertical), offscreen render target for the shadow source, tunable radius and spread. CPU-feasible at small radii; expensive at large radii without GPU.

**Who has this:** macOS (NSShadow, CALayer.shadowRadius, vibrancy), CSS box-shadow/filter:blur, Android elevation/ambient shadows.

**Implication for the OS:** No frosted-glass effects, no depth cues via shadow softness, no blurred backgrounds under dialogs. Depth hierarchy is communicated only through z-order and color contrast.

### No non-rectangular clipping

All clipping is axis-aligned rectangles (the `ClipRect` in scene_render.rs).

**What it would take:** Clip masks (alpha mask or stencil buffer), per-pixel clip testing. Or clipping to the intersection of arbitrary paths.

**Who has this:** Every 2D renderer (Skia, Direct2D, Cairo, Core Graphics).

### No rich inline text

One font/style per TextRun. To have a bold word in a paragraph, you need separate TextRun entries manually positioned. No inline style changes within a run.

Additionally:

- No bidirectional text (RTL, Arabic, Hebrew)
- No complex script shaping beyond what read-fonts provides (no full Unicode text stack)
- No text decoration (underline, strikethrough) as a text primitive
- Monospace layout only (no proportional word-wrap / line-break algorithm)

**What it would take:** Run-level font/style switching with shaping across run boundaries, the Unicode BiDi algorithm (UAX #9), a proportional line-breaking algorithm (UAX #14 or Knuth-Plass), text decoration geometry.

**Who has this:** macOS Core Text, Pango + HarfBuzz + FriBidi, DirectWrite, web browsers.

**Implication for the OS:** Document rendering is limited to monospace plaintext. Rich text documents (.docx, .rtf, markdown) cannot be rendered faithfully. Non-Latin scripts are unsupported for layout (even if glyphs exist in the font).

### No image resampling

`blit_blend` copies pixels 1:1. No bilinear, bicubic, or Lanczos filtering. Images display at their native resolution. Scaling an image means showing it at the wrong size or not at all.

**What it would take:** A resampling filter (bilinear at minimum, Lanczos for quality), mipmap generation for downscaling.

**Who has this:** Every image viewer, web browser, Skia, Core Graphics, Direct2D.

### No video or animated media

No codec integration, no frame decode pipeline, no audio subsystem, no A/V synchronization.

**What it would take:** Codec library (AV1/H.264/VP9 decoder), frame buffer ring, audio output driver, A/V sync clock, timeline playback control.

**Who has this:** Every modern OS has a media framework (AVFoundation, GStreamer, MediaFoundation).

### No fractional DPI scaling

Scale factor is an integer (`u32`). Supports 1× and 2×. No 1.25×, 1.5×, 175%.

**What it would take:** Fractional scale factor in layout, sub-pixel text positioning at fractional ppem, scaled coordinate system throughout.

**Who has this:** macOS (continuous scaling), Windows (125%, 150%, 175% via DPI virtualization), Wayland (wp_fractional_scale_v1).

### No multi-display

Single framebuffer, single resolution, configured at init. No display hotplug, no per-display scale/refresh.

**What it would take:** Display enumeration, per-display scanout, independent resolution/scale/refresh per output.

**Who has this:** Every desktop OS, Wayland (wl_output), X11 (XRandR).

---

## Performance envelope

| Metric                 | Current state                                              | Bottleneck                                                       | Practical ceiling                                                                           |
| ---------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| Resolution             | 1024×768 (hardcoded)                                       | CPU compositing bandwidth; virtio-gpu copy cost                  | ~4K at low refresh with NEON SIMD. CPU-bound without GPU compositing                        |
| Compositing throughput | Scalar per-pixel blend_over                                | 1280×800 @ 60fps full recomposite = ~245 MB/s                    | NEON could do ~4× (4 pixels/cycle). Still CPU-bound for full-screen recomposite at high res |
| Text rendering         | Cache hit = memcpy. Miss = bezier flatten + scanline sweep | Cache misses are expensive. LRU eviction under font-size variety | Adequate for document editing. Would struggle with many font sizes or rapid font switching  |
| Scene graph            | 512 nodes max, 64 KB data buffer                           | Fixed. Selection rects and text runs consume capacity            | Sufficient for single-document text editing. Complex compound documents would hit limits    |
| Frame cadence          | Event-driven, no fixed rate                                | No frame budget. Heavy layout blocks the next frame              | Untested under load. 60fps requires <16ms per frame                                         |
| Damage tracking        | Per-node change list (24 entries) + dirty rects            | Falls back to full repaint on overflow                           | Effective for typical editor interactions. Large multi-region updates may overflow          |

---

## Comparison to real systems

| System                                    | Model                                                               | Similarity                                                                          | Key difference                                                                 |
| ----------------------------------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| **macOS Quartz / Core Animation**         | GPU-composited layer tree, hardware blur/shadow, implicit animation | Same scene-graph→compositor split                                                   | macOS has GPU compositing, per-layer transforms, implicit animation, blur      |
| **Fuchsia Scenic**                        | Scene graph in shared memory, GPU-composited                        | Closest architectural prior art                                                     | Scenic has GPU rendering, 3D node types, view embedding                        |
| **Wayland compositors**                   | Client renders to buffer, compositor composites via EGL/Vulkan      | Different: your compositor is the sole renderer, not a compositor of client buffers | Wayland clients own their rendering; your editors don't render at all          |
| **Plan 9 rio**                            | CPU-rasterized, rectangular windows, no compositing                 | Similar simplicity                                                                  | Your pipeline has alpha blending, subpixel text, scene graph, damage tracking  |
| **Game engines (2D)**                     | Frame-driven render loop, GPU batched draws, animation system       | Very different model                                                                | Game engines assume continuous 60fps rendering; your pipeline is demand-driven |
| **Terminal emulators (kitty, alacritty)** | Fixed-width glyph grid, GPU-accelerated text, damage tracking       | Closest functional analog today                                                     | Your compositor is a very good terminal renderer with chrome and SVG icons     |

---

## Summary

This is a **high-quality 2D document renderer for static and semi-static content**. Text rendering is genuinely excellent. The architecture (one-way pipeline, scene graph interface, damage tracking) is clean and well-separated.

The gap between this and a modern desktop compositor is approximately: GPU compositing + animation timeline + blur/shadow + transforms + rounded corners + fractional scaling + multi-display + proportional text layout + image resampling. That's substantial, but all of it is leaf-node complexity behind the existing scene graph interface. The architecture does not prevent any of these additions — it accommodates them.

The design is coherent for its stated purpose: a document-centric OS where documents are first-class and tools attach to content. The rendering pipeline renders documents. It does not try to be a general-purpose graphics engine, and it should not.
