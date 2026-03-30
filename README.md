# os

A personal exploration of a document-centric operating system — one where documents are first-class citizens and applications are interchangeable tools that attach to content.

This is a design project, not a product. The primary artifact is a coherent OS design. Code is written selectively — to prove out areas of the design, research potential solutions, and validate uncertain assumptions. Some of the implementation may be independently useful (the kernel, in particular, is a self-contained bare-metal aarch64 microkernel in Rust); other parts exist purely to serve the design exploration.

## the idea

Modern operating systems are app-centric: **OS → App → File.** You open an app, create or find a file inside it, and work within that app’s world.

This project explores inverting that: **OS → Document → Tool.** Documents have independent identity. The OS understands what they are (via mimetypes) and can view any of them natively. Editing means attaching a tool to content — and tools are interchangeable.

- View is the default; editing is a deliberate step
- Editors bind to content types, not use cases (same text editor for documents, chat, email)
- No “save” — edits write immediately on a copy-on-write filesystem
- Files are organized by queryable metadata, not folder paths
- The GUI and CLI are equally fundamental OS interfaces

## status

The project has a working interactive demo running on a bare-metal aarch64 microkernel. The display pipeline renders rich text with multi-style runs (per-span font, weight, size, color, underline, strikethrough), macOS-grade anti-aliasing (analytic coverage, outline dilation, subpixel glyph positioning), composites z-ordered surfaces with rounded corners, analytical Gaussian shadows, backdrop blur, and layer opacity, decodes PNG images, renders cubic bezier paths and Tabler vector icons, and supports both a plain text editor and a rich text editor with styled text, selection (click, double-click word select, triple-click line select, click-and-drag), smooth scrolling, and mouse click-to-position, plus an image viewer — switchable at runtime with context-aware glyph icons via a spring-animated document strip (Ctrl+Tab). Documents persist to a COW filesystem with undo/redo (Cmd+Z / Cmd+Shift+Z). A hardware RTC clock ticks in the title bar. Rendering uses native Metal GPU passthrough (`metal-render`) via the [hypervisor](https://github.com/jonathanrtuck/hypervisor). The rendering pipeline uses a display-refresh-rate frame scheduler (120 Hz on ProMotion, 60 Hz default) with event coalescing and idle optimization, triple-buffered incremental scene graph updates with change-list-driven damage tracking, millipoint coordinates (1/1024 pt), 2D affine transforms, fractional DPI scaling, and bilinear image resampling — only dirty screen regions are re-rendered and transferred to the GPU.

For the full design landscape, see the [decision register](design/decisions.md) and the [exploration journal](design/journal.md).

## what’s implemented

**Kernel** — Bare-metal aarch64 microkernel. 28 syscalls, EEVDF scheduler with scheduling contexts, 4 SMP cores (GICv3 interrupt controller, tickless idle with IPI wakeup), demand-paged memory, channel-based IPC with shared memory.

**Display pipeline** — Presenter builds a scene graph in shared memory; metal-render reads it, submits Metal GPU commands, and presents to the display via the [hypervisor](https://github.com/jonathanrtuck/hypervisor). Triple-buffered scene graph with mailbox semantics (writer never blocks, reader always gets latest frame). Configurable-cadence frame scheduler (60/30/120fps) with event coalescing, frame budgeting, and idle optimization. Incremental scene graph updates — clock ticks and cursor moves are zero-allocation mutations; only changed nodes are recorded in a change list. Change-list-driven damage tracking with subtree clip skipping. Dirty-rectangle GPU transfers (only changed regions sent to the host).

**Document pipeline** — Three cooperating processes: `document` (sole writer to document state — applies edits, manages piece table or flat buffer), `layout` (text layout with styled runs, word/character breaking, font metrics), and `presenter` (scene graph builder, input router, style shortcuts). Content-type dispatch throughout: text/rich documents use the piece table; text/plain uses a flat UTF-8 buffer. Editors are read-only consumers that send write requests via IPC.

**Render service (metal-render)** — Native Metal GPU rendering via serialized Metal commands over a custom virtio device — used with the [hypervisor](https://github.com/jonathanrtuck/hypervisor) for zero-translation-layer GPU passthrough with 4x MSAA. Single-process thick driver: tree walk, GPU command submission, compositing, and present. Capabilities: Z-ordered surface compositing with translucent chrome, Gaussian-blurred box shadows (configurable blur radius, offset, spread), per-subtree layer opacity via offscreen compositing, rounded corners with SDF-based anti-aliased fill and corner-radius-aware child clipping, 2D affine transforms (3x3 matrix per node, composition through tree, transform-aware clipping), fractional DPI scaling (f32 scale factors) with pixel-snapped borders and fractional font sizing, bilinear image resampling, radial gradient background with noise texture, title bar with glyph icons and hardware RTC wall-clock (PL031, UTC), pure monochrome palette, procedural arrow cursor, damage tracking for incremental re-rendering.

**Render library** — Shared rendering infrastructure: frame scheduler (used by metal-render), path rasterizer (used by presenter for loading screen), coordinate scaling helpers, scene graph tree walk and compositing.

**Drawing library** — Surfaces, colors, Porter-Duff compositing, gamma-correct sRGB blending. NEON SIMD acceleration for fill, blend, rounded-corner, and blur operations. Anti-aliased lines (Wu’s algorithm). Path rendering (MoveTo/LineTo/CurveTo/Close, fill and stroke, cubic beziers). Separable Gaussian blur (two-pass horizontal/vertical, configurable radius/sigma). PNG decoder (DEFLATE, all filter types). Bilinear image resampling. Monochrome palette system.

**Font library** — TrueType/OpenType rasterizer with grayscale anti-aliasing, stem darkening for heavier strokes, variable font axis support (weight, optical size), HarfBuzz-level shaping, and glyph cache. Three fonts selected by content type: JetBrains Mono (monospace, editor/code), Inter (sans-serif, chrome/UI), Source Serif 4 (serif, prose/body).

**Layout library** — Unified text layout engine. Single `layout_paragraph()` function for both monospace and proportional text, parameterized by a `FontMetrics` trait. `CharBreaker` for character-level wrapping (monospace), `WordBreaker` for word-boundary wrapping (proportional). Alignment (left, center, right). Standalone `byte_to_line_col()` for cursor positioning.

**Piece table library** — Fixed-size, arena-allocated rich text data structure. 512 pieces, 32-entry style palette (font family, weight, size, color, flags, semantic a11y role), append-only add buffer. Sequential same-style inserts coalesce into a single piece. Zero-copy shared memory access — the buffer IS the piece table (pointer cast with validation). Used for text/rich documents.

**Filesystem and store libraries** — COW filesystem (`fs`): block device trait, superblock ring, free-extent allocator, inodes, two-flush commit, per-file snapshots. Document store (`store`): metadata catalog (media types, queryable attributes), wraps `Box<dyn Files>`.

**Icon library** — Named vector icons: `get(name, mimetype)` lookup, pre-compiled Tabler SVGs (MIT), layer annotations for fill+stroke. Build-time SVG→path converter with stroke expansion and arc-to-cubic conversion.

**Animation library** — Easing functions (standard CSS curves), spring physics (semi-implicit Euler with 4ms fixed substeps), and timeline sequencing. Used for smooth scroll, cursor blink, and document slide transitions (Ctrl+Tab).

**Scene graph library** — Typed visual node tree in shared memory. Triple-buffered with mailbox semantics for lock-free producer/consumer across processes. Four geometric content types: `None` (containers/solid fills), `Image` (pixel buffers), `Path` (cubic bezier contours), `Glyphs` (shaped glyph runs). Change list for incremental damage tracking. Copy-forward with selective mutation for zero-allocation updates. Per-node 2D affine transforms, corner radius, layer opacity, box shadows.

**Text editor** — Editor for text/plain documents. Cursor movement, text selection (shift+arrow, click-and-drag, double-click word, triple-click line), scrolling, mouse click-to-position, insert and delete. Communicates with document via IPC write requests.

**Rich text editor** — Editor for text/rich documents backed by the piece table library. Same editing operations as text editor, with content-type detection at startup. Style shortcuts (Cmd+B bold, Cmd+I italic, Cmd+1/2 headings) handled by the presenter.

**Document service** — Persistent document storage over COW filesystem on virtio-blk. Undo/redo via COW snapshots (Cmd+Z / Cmd+Shift+Z, 64-entry ring, character-level granularity). Receives commit messages from document, reads document buffer from shared memory, writes to disk with two-flush crash consistency.

**Image viewer** — Decodes and displays a PNG image. Toggle between editor and viewer with Ctrl+Tab (title bar icon updates to reflect current mode).

**Mouse support** — virtio-tablet driver (absolute pointer events) with procedural arrow cursor. Left-click positions the text cursor in the editor content area.

**Assets** — Fonts loaded at boot from the native COW filesystem (disk.img). Images and icons loaded via virtio-9p host filesystem passthrough.

**Tests** — ~2,313 host-side unit tests, 15 visual regression tests (verify.py assertions + hypervisor capture).

## running the demo

### Prerequisites

- **Rust nightly** with `aarch64-unknown-none` target (handled automatically by `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- **[Hypervisor](https://github.com/jonathanrtuck/hypervisor)** (`make install` from that repo) — native Metal GPU rendering on macOS
- For QEMU path: **QEMU** with `qemu-system-aarch64` (`brew install qemu`)

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
- **Arrow keys** to move the cursor (Cmd+Left/Right for line start/end, Cmd+Up/Down for document start/end)
- **Shift+arrow** to select text
- **Click** to position cursor, **double-click** to select word, **triple-click** to select line
- **Click-and-drag** to select a range
- **Backspace/Delete** to delete
- **Cmd+Z** to undo, **Cmd+Shift+Z** to redo
- **Cmd+B** to toggle bold, **Cmd+I** to toggle italic (rich text documents)
- **Cmd+1/2** to toggle heading styles (rich text documents)
- **Ctrl+Tab** to toggle between text editor and image viewer

## project structure

```text
os/
├── design/                          # Design documentation
│   ├── philosophy.md                # Two root principles and their consequences
│   ├── foundations.md               # The core idea, glossary, guiding beliefs, content model
│   ├── decisions.md                 # 17 tiered design decisions with tradeoffs
│   ├── architecture.md              # Architectural narrative and decision checklist
│   ├── roadmap.md                   # Milestone plan (v0.5–v1.0), sequencing rationale
│   ├── journal.md                   # Open threads, insights, research spikes
│   ├── research/                    # COW filesystems, OS landscape, font rendering
│   └── *.mermaid                    # Architecture, dependency, pipeline diagrams
├── system/                          # OS implementation (Rust, no_std)
│   ├── kernel/                      # Microkernel (28 syscalls, EEVDF, GICv3, SMP)
│   ├── services/
│   │   ├── init/                    # Root task — spawns everything, wires IPC
│   │   ├── presenter/               # View engine (C) — scene graph builder, input router
│   │   ├── layout/                  # Layout engine (B) — text layout with styled runs
│   │   ├── document/                # Document service — COW persistence, undo/redo
│   │   ├── store/                   # Store service — metadata catalog
│   │   ├── decoders/png/            # Sandboxed PNG decoder
│   │   └── drivers/
│   │       ├── metal-render/        # Metal render service (sole backend, via hypervisor)
│   │       ├── virtio-input/        # Keyboard + tablet input (evdev translation)
│   │       ├── virtio-9p/           # Host filesystem passthrough (9P2000.L)
│   │       ├── virtio-blk/          # Block device (sector reads)
│   │       └── virtio-console/      # Serial console (minimal)
│   ├── libraries/
│   │   ├── sys/                     # Syscall wrappers + userspace allocator
│   │   ├── virtio/                  # MMIO transport + split virtqueue
│   │   ├── drawing/                 # Surfaces, colors, compositing, palette
│   │   ├── fonts/                   # TrueType rasterizer, stem darkening, glyph cache
│   │   ├── piecetable/              # Piece table for text/rich documents
│   │   ├── animation/               # Easing functions, spring physics, timelines
│   │   ├── layout/                  # Unified text layout engine (mono + proportional)
│   │   ├── render/                  # Frame scheduler, path rasterizer, coordinate helpers
│   │   ├── scene/                   # Scene graph nodes, triple-buffered shared memory
│   │   ├── icons/                   # Named vector icons (Tabler SVGs, mimetype lookup)
│   │   ├── ipc/                     # Lock-free SPSC ring buffers
│   │   ├── protocol/                # IPC message types + payload structs (10 boundaries)
│   │   ├── fs/                      # COW filesystem (block device, snapshots, inodes)
│   │   └── store/                   # Document store metadata layer
│   ├── user/
│   │   ├── text-editor/             # Editor for text/plain (input → write requests)
│   │   ├── rich-editor/             # Editor for text/rich (piece table documents)
│   │   ├── echo/                    # IPC test program
│   │   ├── stress/                  # IPC stress test program
│   │   ├── fuzz/                    # Fuzzing harness
│   │   └── fuzz-helper/             # Fuzzing helper
│   ├── test/                        # Host-side unit + visual regression tests
│   └── tools/mkdisk/               # Factory disk image builder
├── prototype/
│   └── files/                       # Files interface prototype (macOS-backed)
├── CLAUDE.md                        # AI collaboration context
├── README.md
└── UNLICENSE
```

## design documents

If you’re curious about the design, read in this order:

1. **[Philosophy](design/philosophy.md)** — Two root principles and their consequences. The thinking framework behind every design decision.
2. **[Foundations](design/foundations.md)** — The core idea, glossary of terms, guiding beliefs, external boundaries, content model, editing model
3. **[Decisions](design/decisions.md)** — All 17 design decisions: settled positions with reasoning, open questions with tradeoffs, considered-and-rejected alternatives
4. **[Landscape](design/landscape.md)** — How this OS compares technically to Linux, macOS, Fuchsia, and 6 other systems
5. **[Architecture](design/architecture.md)** — The system’s architectural narrative: pipeline, responsibilities, decision checklist
6. **[Journal](design/journal.md)** — Where the design exploration is right now: open threads, discussion backlog, insights

If you're curious about the implementation, start here:

1. **[System Design](system/DESIGN.md)** — Userspace architecture: libraries, services, drivers, what's foundational vs scaffolding
2. **[Kernel Design](system/kernel/DESIGN.md)** — Rationale for every kernel subsystem (boot, memory, scheduling, IPC, devices)
3. **[Kernel README](system/kernel/README.md)** — Feature list, build/test commands, source file guide
4. **[Rendering Capabilities](system/rendering-capabilities.md)** — What the rendering pipeline can and cannot do, compared to real systems

## influences

- [Mercury OS](https://uxdesign.cc/introducing-mercury-os-f4de45a04289) (Jason Yuan) — Intent-driven, no apps or folders, Modules/Flows/Spaces
- [Ideal OS](https://joshondesign.com/2017/08/18/idealos_essay) (Josh Marinacci) — Document database, message bus as IPC, structured object streams
- [OpenDoc](https://en.wikipedia.org/wiki/OpenDoc) (Apple/IBM, 1990s) — Component-based document editing
- [Xerox Star](https://en.wikipedia.org/wiki/Xerox_Star) (1981) — Document-centric desktop
- [Plan 9](https://en.wikipedia.org/wiki/Plan_9_from_Bell_Labs) (Bell Labs) — Everything-is-a-file taken to its logical conclusion
- [BeOS](https://en.wikipedia.org/wiki/Be_File_System) — Queryable metadata built into the filesystem

## AI collaboration

This project uses [Claude](https://claude.ai) as a thinking partner for design exploration. The [CLAUDE.md](CLAUDE.md) file provides context for that collaboration — settled decisions, working mode, and where things left off. The design process is visible in the commit history and [journal](design/journal.md).

## license

[Unlicense](UNLICENSE) — public domain.
