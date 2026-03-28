# Rendering Pipeline — Capabilities and Limitations

An honest audit of what this OS's rendering pipeline can and cannot do, compared against real systems. Updated 2026-03-24.

---

## Pipeline summary

Event-driven 2D document rendering with pluggable render services:

```text
Core (layout + scene build) → Scene Graph (shared memory) → Render Service → Display
```

The scene graph is the interface. Render services are thick drivers that read the scene graph, perform the full tree walk, and produce pixels. Three render services:

- **`metal-render`** (default): Native Metal GPU rendering via serialized Metal commands over a custom virtio device. Used with the [hypervisor](https://github.com/jonathanrtuck/hypervisor) on Apple Silicon. 4x MSAA, sRGB render targets, analytical Gaussian shadows, on-demand glyph atlas, hardware cursor plane. Auto-detects native display resolution (e.g., 4112×2658@120Hz on ProMotion).
- **`virgil-render`** _(deprecated)_: GPU-accelerated rendering via Gallium3D command streams (virtio-gpu 3D / Virgl). v0.3 research spike; no longer maintained. Will be removed in a future milestone.
- **`cpu-render`**: Software rasterization via `CpuBackend` + virtio-gpu 2D presentation. Used for QEMU integration testing. Not actively developed; will be retired once test infrastructure migrates to hypervisor.

All three live as sibling directories under `services/drivers/`. Init probes GPU capabilities at boot and selects the appropriate render service.

The pipeline uses a **configurable-cadence frame scheduler** with actual display refresh rate (120 Hz on ProMotion, 60 Hz on QEMU), event coalescing, frame budgeting, and idle optimization. Updates are driven by state changes (keystroke, clock tick, pointer move), coalesced within frame boundaries.

---

## What it does well

### Text rendering

Comparable to macOS Core Text for Latin text at common sizes. The result of a dedicated quality sprint matching the macOS reference pixel-for-pixel.

- **Analytic area coverage rasterizer** — exact signed-area trapezoids (not quantized), matching macOS precision
- **Outline dilation** for stem darkening — symmetric miter-join (macOS formula, Pathfinder coefficients × 1.3 boost)
- **Device-pixel rasterization** — atlas rendered at `font_size_pt × scale_factor` for crisp output at native resolution
- **Subpixel glyph positioning** — 16.16 fixed-point advances (ShapedGlyph is 16 bytes), eliminates cursor drift from float truncation
- **Variable font axes** (weight, optical size) with three-font stack: JetBrains Mono (mono), Inter (sans), Source Serif 4 (serif)
- **HarfBuzz-level shaping** via HarfRust (pure Rust port, no_std) — glyph lookup, metrics, kerning
- **Glyph cache** (LRU, keyed on glyph ID + size + axis hash) — cache hits are memory copies
- **Gamma-correct blending** of glyph coverage maps (sRGB↔linear LUTs)
- **On-demand glyph atlas** in metal-render — fixes ligature drops from fixed-size atlases

### Text layout

Unified layout engine in `libraries/layout/`:

- **Single `layout_paragraph()` function** for both monospace and proportional text, parameterized by a `FontMetrics` trait
- **`CharBreaker`** for character-level wrapping (monospace)
- **`WordBreaker`** for word-boundary wrapping (proportional)
- **Alignment** (left, center, right)
- **Standalone `byte_to_line_col()`** for cursor positioning

### Compositing

- **Porter-Duff source-over** with gamma-correct sRGB blending (integer math, lookup tables)
- **NEON SIMD acceleration** for fill, blend, rounded-corner, and blur operations
- **Per-pixel alpha** on all surfaces
- **Z-ordered back-to-front compositing** with insertion-sort by z-order
- **Backdrop blur** — 3-pass box blur approximating Gaussian, used for frosted-glass chrome overlays

Most desktop compositors skip gamma-correct blending. This pipeline gets it right.

### Animation

Full animation library (`libraries/animation/`):

- **Easing functions** — standard CSS-style easing curves (ease-in, ease-out, ease-in-out, etc.)
- **Spring physics** — semi-implicit Euler with 4ms fixed substeps for stability. Used for smooth scroll and document slide transitions
- **Timeline sequencing** — composable animation timelines with start/end/duration
- **Cursor blink** — phase-based blink with smooth transitions
- **Smooth scroll** — spring-based scroll with momentum and settle detection
- **Document strip** — horizontal strip of N document spaces with spring-based Ctrl+Tab slide transition (both documents always in scene — no teardown/rebuild on switch)

### Damage tracking

- **512-bit dirty bitmap** in the scene header (one bit per node slot, 8 × u64 words) — no overflow, O(1) mark/test
- **Triple-buffered scene graph** with mailbox semantics — writer never blocks, reader always gets latest frame
- **Copy-forward** for selective mutation (acquire previous frame, modify only changed nodes, publish)
- **Four incremental update paths**: clock (in-place overwrite), cursor (position only), selection (truncate + rebuild), document content (re-layout visible lines)
- **Dirty rect derivation** from changed node positions → partial GPU transfer
- **Full-rebuild fallback** when data buffer exceeds 75%

Clock and cursor updates are near-zero-cost (no heap allocations, no layout). Document edits are O(visible_lines).

### Cursor plane

Hardware cursor decoupled from the content rendering pipeline:

- **Shared pointer state register** — atomic u64 in init-allocated shared memory replaces MSG_POINTER_ABS ring messages. Eliminates input ring overflow for pointer events
- **Cursor-only frames** — metal-render detects `!scene_changed && cursor_moved` and sends a lightweight cursor-plane-only command (no scene walk)
- **State vs event distinction** at the IPC level: pointer position is continuous state (latest wins), button clicks are discrete events (every one matters)

### Coordinate system

- **Millipoint coordinates** — i32 at 1/1024 pt (Mpt/Umpt type aliases). Sub-pixel precision at any density. Range: ±2,097,151 pt (±2,489 A4 pages)
- **Fractional DPI scaling** — f32 scale factors (1.0, 1.25, 1.5, 2.0, etc.) with pixel-snapped borders
- **AffineTransform stays f32** — dimensionless multipliers, GPU-bound

### Other 2D primitives

- **Gradients**: radial and vertical linear, dithered with Bayer 4×4 ordered dithering (band-free, deterministic)
- **Path rendering**: Content::Path with MoveTo/LineTo/CurveTo/Close, fill and stroke, cubic beziers
- **SVG→path pipeline**: build-time SVG path parser with stroke expansion engine, arc-to-cubic conversion. Tabler Icons (MIT) compiled into const path data
- **Path rasterization**: cubic bezier flattening (De Casteljau), scanline fill, non-zero winding, antialiased
- **Anti-aliased lines** (Wu's algorithm), filled/outlined rectangles, horizontal/vertical lines
- **Rounded corners**: SDF-based fill with anti-aliased edges, NEON SIMD, corner-radius-aware child clipping
- **Box shadows**: analytical Gaussian in metal-render (exact erf integral per-pixel, no offscreen textures); separable blur in cpu-render/virgil-render. Configurable radius, offset, spread. Declarative on Node, damage-tracking-aware
- **Layer opacity**: per-subtree opacity via offscreen compositing (group opacity), pool-based buffer management
- **Clip masks**: per-subtree clip regions for non-rectangular clipping
- **2D affine transforms**: 3×3 matrix per node, transform composition through tree, transform-aware clipping
- **Bilinear image resampling**: for scaled/rotated content, ResamplingMethod enum for future Lanczos
- **PNG decoding** (DEFLATE, all filter types) from byte slices
- **Page surface**: white A4-proportioned page centered on dark desk background

---

## What it cannot do

These are genuine architectural gaps. Each would require structural additions.

### No 3D rendering

No 3D scene graph, no depth buffer, no projection matrix, no GPU compute. The GPU backends use hardware for 2D rendering (textured quads, glyph atlas, stencil-then-cover paths) but the pipeline is fundamentally 2D.

**Implication:** 3D games, CAD, WebGL-style content not possible. Consistent with the document-centric focus — documents are 2D.

### No rich inline text

One font/style per Glyphs node. No inline style switching within a text run. Additionally:

- No bidirectional text (RTL, Arabic, Hebrew)
- No complex script shaping beyond what HarfRust provides (no full Unicode text stack)
- No text decoration (underline, strikethrough) as a text primitive

**What it would take:** Run-level font/style switching with shaping across run boundaries, the Unicode BiDi algorithm (UAX #9), text decoration geometry.

**Implication:** Document rendering handles single-style text (monospace code, proportional prose) but not rich text within a paragraph. Non-Latin scripts untested.

### No video or animated media

No codec integration, no frame decode pipeline, no audio subsystem, no A/V synchronization.

**What it would take:** Codec library (AV1/H.264/VP9 decoder), frame buffer ring, audio output driver, A/V sync clock, timeline playback control.

### No multi-display

Single framebuffer, single resolution. No display hotplug, no per-display scale/refresh.

**What it would take:** Display enumeration, per-display scanout, independent resolution/scale/refresh per output.

### No arbitrary path clipping

Clip masks handle rectangular and rounded-rect regions. No clip-to-arbitrary-bezier-path. Adequate for UI panels and cards; insufficient for complex vector artwork masking.

### Remaining text gaps

- No rotated text rendering (text is always axis-aligned)
- No Lanczos or bicubic image filtering (bilinear only)
- No italic rendering (deferred — see journal)

---

## Performance envelope

| Metric                 | Current state                                              | Bottleneck                                                        | Practical ceiling                                                                          |
| ---------------------- | ---------------------------------------------------------- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Resolution             | Native display (e.g., 4112×2658 on Retina, configurable)   | GPU command throughput (metal-render); CPU bandwidth (cpu-render) | Retina resolutions at 120fps via metal-render. cpu-render limited to ~1080p at 60fps       |
| Compositing throughput | NEON SIMD per-pixel blend (cpu-render); GPU (metal/virgil) | Full-screen recomposite at high res is CPU-bound in cpu-render    | metal-render bypasses this entirely via GPU compositing                                    |
| Text rendering         | Cache hit = memcpy. Miss = outline + scanline + coverage   | Cache misses are expensive. LRU eviction under font-size variety  | Adequate for document editing. Would struggle with many font sizes or rapid font switching |
| Scene graph            | 512 nodes max, 64 KB data buffer, triple-buffered          | Fixed. Selection rects and glyph runs consume capacity            | Sufficient for single-document editing. Complex compound documents would hit limits        |
| Frame cadence          | Actual display refresh (120/60 Hz) with event coalescing   | Frame budget enforcement. Heavy layout can miss deadline          | 8.3ms budget at 120Hz. Event coalescing prevents redundant frames. Idle optimization       |
| Damage tracking        | 512-bit dirty bitmap + dirty rects                         | Full repaint on data buffer exhaustion                            | Effective for all editor interactions. Bitmap covers all 512 node slots without overflow   |

---

## Comparison to real systems

| System                            | Model                                                               | Similarity                                                           | Key difference                                                              |
| --------------------------------- | ------------------------------------------------------------------- | -------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| **macOS Quartz / Core Animation** | GPU-composited layer tree, hardware blur/shadow, implicit animation | Same scene-graph→compositor split. Font quality comparable for Latin | macOS has implicit property animation, per-layer transforms, view embedding |
| **Fuchsia Scenic**                | Scene graph in shared memory, GPU-composited                        | Closest architectural prior art                                      | Scenic has 3D node types, view embedding, Vulkan backend                    |
| **Wayland compositors**           | Client renders to buffer, compositor composites via EGL/Vulkan      | Different: this pipeline renders everything — editors don't render   | Wayland clients own their rendering; here editors have no pixel access      |
| **Plan 9 rio**                    | CPU-rasterized, rectangular windows, no compositing                 | Similar simplicity                                                   | This pipeline adds alpha, subpixel text, scene graph, damage, animation     |
| **Game engines (2D)**             | Frame-driven render loop, GPU batched draws, animation system       | Animation library + frame scheduler provide similar cadence          | Game engines have GPU batching, sprite systems, continuous rendering        |

---

## Summary

This is a **high-quality 2D document renderer with animation, GPU acceleration, and macOS-grade text**. The architecture (one-way pipeline, scene graph interface, three pluggable render backends, damage tracking) is clean and well-separated.

The gap between this and a modern desktop compositor is approximately: rich inline text (multi-style runs, BiDi) + multi-display + video/media + arbitrary path clipping. These are leaf-node complexity behind the existing scene graph interface. The architecture does not prevent any of these additions — it accommodates them.

The design is coherent for its stated purpose: a document-centric OS where documents are first-class and tools attach to content. The rendering pipeline renders documents. It does not try to be a general-purpose graphics engine, and it should not.
