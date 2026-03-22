# os

A personal exploration of a document-centric operating system — one where documents are first-class citizens and applications are interchangeable tools that attach to content.

This is a design project, not a product. The primary artifact is a coherent OS design. Code is written selectively — to prove out areas of the design, research potential solutions, and validate uncertain assumptions. Some of the implementation may be independently useful (the kernel, in particular, is a self-contained bare-metal aarch64 microkernel in Rust); other parts exist purely to serve the design exploration.

## The Idea

Modern operating systems are app-centric: **OS → App → File.** You open an app, create or find a file inside it, and work within that app's world.

This project explores inverting that: **OS → Document → Tool.** Documents have independent identity. The OS understands what they are (via mimetypes) and can view any of them natively. Editing means attaching a tool to content — and tools are interchangeable.

- View is the default; editing is a deliberate step
- Editors bind to content types, not use cases (same text editor for documents, chat, email)
- No "save" — edits write immediately on a copy-on-write filesystem
- Files are organized by queryable metadata, not folder paths
- The GUI and CLI are equally fundamental OS interfaces

## Status

The project has a working interactive demo running on a bare-metal aarch64 microkernel. The display pipeline renders text with anti-aliasing, stem darkening, and variable font axes, composites z-ordered surfaces with rounded corners, Gaussian-blurred box shadows, and layer opacity over a radial gradient background, decodes PNG images, renders cubic bezier paths, and supports a text editor with selection, scrolling, and mouse click-to-position plus an image viewer — switchable at runtime with context-aware glyph icons. A hardware RTC clock ticks in the title bar. Three render services are available: native Metal GPU rendering (`metal-render`) via the [hypervisor](https://github.com/jonathanrtuck/hypervisor), GPU-accelerated Virgil3D rendering (`virgil-render`) via QEMU, and CPU software rendering (`cpu-render`) — auto-selected at boot. The rendering pipeline uses a configurable-cadence frame scheduler (60/30/120fps) with event coalescing and idle optimization, triple-buffered incremental scene graph updates with change-list-driven damage tracking, 2D affine transforms, fractional DPI scaling, and bilinear image resampling — only dirty screen regions are re-rendered and transferred to the GPU.

For the full design landscape, see the [decision register](design/decisions.md) and the [exploration journal](design/journal.md).

## What's Implemented

**Kernel** — Bare-metal aarch64 microkernel. 28 syscalls, EEVDF scheduler with scheduling contexts, 4 SMP cores (GICv3 interrupt controller, tickless idle with IPI wakeup), demand-paged memory, channel-based IPC with shared memory.

**Display pipeline** — Core builds a scene graph in shared memory; a render service reads it, rasterizes, and presents to the display. Init auto-detects the GPU device at boot and selects between `metal-render` (native Metal via the [hypervisor](https://github.com/jonathanrtuck/hypervisor)), `virgil-render` (Virgil3D/Gallium3D via QEMU), and `cpu-render` (software rendering via virtio-gpu 2D). Triple-buffered scene graph with mailbox semantics (writer never blocks, reader always gets latest frame). Configurable-cadence frame scheduler (60/30/120fps) with event coalescing, frame budgeting, and idle optimization. Incremental scene graph updates — clock ticks and cursor moves are zero-allocation mutations; only changed nodes are recorded in a change list. Change-list-driven damage tracking with subtree clip skipping. Dirty-rectangle GPU transfers (only changed regions sent to the host).

**Core (OS service)** — Sole writer to document state. Builds a scene graph describing the visual structure of the document. Routes input to the active editor. Editors are read-only consumers that send write requests via IPC.

**Render services** — Three interchangeable render backends behind the same scene graph interface. `metal-render`: native Metal GPU rendering via serialized Metal commands over a custom virtio device — used with the [hypervisor](https://github.com/jonathanrtuck/hypervisor) for zero-translation-layer GPU passthrough with 4x MSAA. `virgil-render`: GPU-accelerated rendering via Gallium3D command streams (virtio-gpu 3D mode) — used with QEMU's virgl path. `cpu-render`: CpuBackend software rasterizer + virtio-gpu 2D presentation. All three are single-process thick drivers that handle the full pipeline: tree walk, rasterization/GPU commands, compositing, and present. Shared capabilities: Z-ordered surface compositing with translucent chrome, Gaussian-blurred box shadows (configurable blur radius, offset, spread), per-subtree layer opacity via offscreen compositing, rounded corners with SDF-based anti-aliased fill and corner-radius-aware child clipping, 2D affine transforms (3x3 matrix per node, composition through tree, transform-aware clipping), fractional DPI scaling (f32 scale factors) with pixel-snapped borders and fractional font sizing, bilinear image resampling, radial gradient background with noise texture, title bar with glyph icons and hardware RTC wall-clock (PL031, UTC), pure monochrome palette, procedural arrow cursor, damage tracking for incremental re-rendering.

**Render library** — Shared rendering infrastructure used by `cpu-render`: scene graph tree walk, CpuBackend (full scene rasterization), incremental rendering with per-node state tracking, frame scheduler, surface pool, damage rect computation.

**Drawing library** — Surfaces, colors, Porter-Duff compositing, gamma-correct sRGB blending. NEON SIMD acceleration for fill, blend, rounded-corner, and blur operations. Anti-aliased lines (Wu's algorithm). Path rendering (MoveTo/LineTo/CurveTo/Close, fill and stroke, cubic beziers). Separable Gaussian blur (two-pass horizontal/vertical, configurable radius/sigma). PNG decoder (DEFLATE, all filter types). Bilinear image resampling. Monochrome palette system.

**Font library** — TrueType/OpenType rasterizer with grayscale anti-aliasing, stem darkening for heavier strokes, variable font axis support (weight, optical size, MONO), HarfBuzz-level shaping, and glyph cache. Three variable fonts: Source Code Pro (monospace, editor), Nunito Sans (proportional, chrome), Recursive (proportional, variable).

**Layout library** — Unified text layout engine. Single `layout_paragraph()` function for both monospace and proportional text, parameterized by a `FontMetrics` trait. `CharBreaker` for character-level wrapping (monospace), `WordBreaker` for word-boundary wrapping (proportional). Alignment (left, center, right). Standalone `byte_to_line_col()` for cursor positioning.

**Scene graph library** — Typed visual node tree in shared memory. Triple-buffered with mailbox semantics for lock-free producer/consumer across processes. Four geometric content types: `None` (containers/solid fills), `Image` (pixel buffers), `Path` (cubic bezier contours), `Glyphs` (shaped glyph runs). Change list for incremental damage tracking. Copy-forward with selective mutation for zero-allocation updates. Per-node 2D affine transforms, corner radius, layer opacity, box shadows.

**Text editor** — Cursor movement, text selection (shift+arrow), scrolling, mouse click-to-position, insert and delete. Communicates with core via IPC write requests.

**Image viewer** — Decodes and displays a PNG image. Toggle between editor and viewer with Ctrl+Tab (title bar icon updates to reflect current mode).

**Mouse support** — virtio-tablet driver (absolute pointer events) with procedural arrow cursor. Left-click positions the text cursor in the editor content area.

**Assets via 9P** — Fonts, images, and icons loaded at boot from the host filesystem via virtio-9p passthrough.

**Tests** — 2,099 tests (2,078 system + 21 prototype).

## Running the Demo

### Prerequisites

- **Rust nightly** with `aarch64-unknown-none` target (`rustup target add aarch64-unknown-none`)
- **QEMU** (`qemu-system-aarch64`)
- **Python 3 with Pillow** (optional, for screenshot conversion only)

### Build

```sh
cd system
cargo build -r
```

### Run

```sh
cd system
cargo run -r
```

This builds the kernel and launches it in the [native hypervisor](https://github.com/jonathanrtuck/hypervisor) with Metal GPU rendering. Close the window or Cmd+Q to exit. Use `QEMU=1 cargo run -r` for QEMU instead.

### Interaction

- **Type** to insert text in the editor
- **Arrow keys** to move the cursor
- **Shift+arrow** to select text
- **Backspace** to delete
- **Left-click** to position the text cursor
- **Ctrl+Tab** to toggle between text editor and image viewer

## Project Structure

```text
os/
├── design/                          # Design documentation
│   ├── philosophy.md                # Two root principles and their consequences
│   ├── foundations.md               # The core idea, glossary, guiding beliefs, content model
│   ├── decisions.md                 # 17 tiered design decisions with tradeoffs
│   ├── architecture.md              # Architectural narrative and decision checklist
│   ├── journal.md                   # Open threads, insights, research spikes
│   ├── research/                    # COW filesystems, OS landscape, font rendering
│   ├── architecture.mermaid         # System architecture diagram
│   ├── decision-map.mermaid         # Visual dependency graph
│   └── rendering-pipeline.mermaid   # Rendering pipeline diagram
├── system/                          # OS implementation (Rust, no_std)
│   ├── kernel/                      # Microkernel (28 syscalls, EEVDF, GICv3, SMP)
│   ├── services/
│   │   ├── init/                    # Root task — spawns everything, wires IPC
│   │   ├── core/                    # OS service — sole writer, scene graph builder, input router
│   │   └── drivers/
│   │       ├── metal-render/        # Metal render service (native Metal via hypervisor)
│   │       ├── cpu-render/          # CPU render service (CpuBackend + virtio-gpu 2D)
│   │       ├── virgil-render/       # GPU render service (Virgil3D/Gallium3D)
│   │       ├── virtio-input/        # Keyboard + tablet input (evdev translation)
│   │       ├── virtio-blk/          # Block device (sector reads)
│   │       ├── virtio-9p/           # Host filesystem passthrough
│   │       └── virtio-console/      # Serial console (minimal)
│   ├── libraries/
│   │   ├── drawing/                 # Surfaces, colors, PNG, compositing, palette
│   │   ├── fonts/                   # TrueType rasterizer, subpixel rendering, glyph cache
│   │   ├── render/                  # Render backend (CpuBackend, damage, incremental, frame scheduler)
│   │   ├── layout/                  # Unified text layout engine (mono + proportional)
│   │   ├── scene/                   # Scene graph nodes, shared memory layout
│   │   ├── ipc/                     # Lock-free SPSC ring buffers
│   │   ├── protocol/                # IPC message types + payload structs (all protocols)
│   │   ├── sys/                     # Syscall wrappers + userspace allocator
│   │   ├── virtio/                  # MMIO transport + split virtqueue
│   │   └── link.ld                  # Shared userspace linker script
│   ├── user/
│   │   ├── text-editor/             # Editor process (input → write requests)
│   │   ├── echo/                    # IPC test program
│   │   ├── stress/                  # IPC stress test program
│   │   ├── fuzz/                    # Fuzzing harness
│   │   └── fuzz-helper/             # Fuzzing helper
│   ├── test/                        # Host-side unit + integration tests (62 files)
│   └── share/                       # Runtime assets (fonts, images, icons)
├── prototype/
│   └── files/                       # Files interface prototype (macOS-backed)
├── CLAUDE.md                        # AI collaboration context
├── README.md
└── UNLICENSE
```

## Design Documents

If you're curious about the design, read in this order:

1. **[Philosophy](design/philosophy.md)** — Two root principles and their consequences. The thinking framework behind every design decision.
2. **[Foundations](design/foundations.md)** — The core idea, glossary of terms, guiding beliefs, external boundaries, content model, editing model
3. **[Decisions](design/decisions.md)** — All 17 design decisions: settled positions with reasoning, open questions with tradeoffs, considered-and-rejected alternatives
4. **[Architecture](design/architecture.md)** — The system's architectural narrative: pipeline, responsibilities, decision checklist
5. **[Journal](design/journal.md)** — Where the design exploration is right now: open threads, discussion backlog, insights

## Influences

- [Mercury OS](https://uxdesign.cc/introducing-mercury-os-f4de45a04289) (Jason Yuan) — Intent-driven, no apps or folders, Modules/Flows/Spaces
- [Ideal OS](https://joshondesign.com/2017/08/18/idealos_essay) (Josh Marinacci) — Document database, message bus as IPC, structured object streams
- [OpenDoc](https://en.wikipedia.org/wiki/OpenDoc) (Apple/IBM, 1990s) — Component-based document editing
- [Xerox Star](https://en.wikipedia.org/wiki/Xerox_Star) (1981) — Document-centric desktop
- [Plan 9](https://en.wikipedia.org/wiki/Plan_9_from_Bell_Labs) (Bell Labs) — Everything-is-a-file taken to its logical conclusion
- [BeOS](https://en.wikipedia.org/wiki/Be_File_System) — Queryable metadata built into the filesystem

## AI Collaboration

This project uses [Claude](https://claude.ai) as a thinking partner for design exploration. The [CLAUDE.md](CLAUDE.md) file provides context for that collaboration — settled decisions, working mode, and where things left off. The design process is visible in the commit history and [journal](design/journal.md).

## License

[Unlicense](UNLICENSE) — public domain.
