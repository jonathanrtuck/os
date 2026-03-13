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

The project has a working interactive demo running on a bare-metal aarch64 microkernel in QEMU. The display pipeline renders text with LCD subpixel anti-aliasing and stem darkening, composites z-ordered surfaces with translucent chrome and drop shadows over a radial gradient background, decodes PNG images, rasterizes SVG paths, and supports a text editor with selection, scrolling, and mouse click-to-position plus an image viewer — switchable at runtime with context-aware icons. A hardware RTC clock ticks in the title bar. Only dirty screen regions are transferred to the GPU.

For the full design landscape, see the [decision register](design/decisions.md) and the [exploration journal](design/journal.md).

## What's Implemented

**Kernel** — Bare-metal aarch64 microkernel. 27 syscalls, EEVDF scheduler, 4 SMP cores, demand-paged memory, channel-based IPC with shared memory.

**Display pipeline** — Four-process architecture: virtio-input driver → compositor → text editor → virtio-gpu driver. Dirty-rectangle GPU transfers (only changed regions sent, not the full framebuffer) and incremental content rendering (only changed text lines re-rendered).

**Compositor** — Z-ordered surface compositing with translucent chrome (alpha ~170) and drop shadows (12px depth). Radial gradient background with noise texture. Title bar with context-aware document/image icons and hardware RTC wall-clock (PL031, UTC). Pure monochrome palette. Sole writer to document state — editors are read-only consumers. Procedural arrow cursor rendered at top z-order.

**Drawing library** — TrueType font rasterizer with LCD subpixel rendering (per-channel RGB coverage, 6× horizontal oversampling), stem darkening for heavier strokes, GPOS kerning, and proper hhea baseline metrics. PNG decoder (DEFLATE, all filter types). SVG path parser and rasterizer. Porter-Duff compositing. Two fonts: Source Code Pro (monospace, editor) and Nunito Sans (proportional, chrome) at 20px.

**Text editor** — Cursor movement, text selection (shift+arrow), scrolling, mouse click-to-position, insert and delete. Communicates with compositor via IPC write requests.

**Image viewer** — Decodes and displays a PNG image. Toggle between editor and viewer with Ctrl+Tab (title bar icon updates to reflect current mode).

**Mouse support** — virtio-tablet driver (absolute pointer events) with procedural arrow cursor. Left-click positions the text cursor in the editor content area.

**Assets via 9P** — Fonts, images, and icons loaded at boot from the host filesystem via virtio-9p passthrough.

**Tests** — 921 tests (900 system + 21 prototype).

## Running the Demo

### Prerequisites

- **Rust nightly** with `aarch64-unknown-none` target (`rustup target add aarch64-unknown-none`)
- **QEMU** (`qemu-system-aarch64`)
- **Python 3 with Pillow** (optional, for screenshot conversion only)

### Build

```bash
cd system
cargo build --release
```

### Run

```bash
cd system
cargo run --release
```

This builds the kernel and launches QEMU with the correct device configuration (see `run-qemu.sh` for details). `Ctrl-A X` to exit QEMU.

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
│   ├── concept.md                   # The core idea: OS → Document → Tool
│   ├── foundations.md               # Glossary, guiding beliefs, content model
│   ├── decisions.md                 # 17 tiered design decisions with tradeoffs
│   ├── decision-map.mermaid         # Visual dependency graph
│   ├── journal.md                   # Open threads, insights, research spikes
│   └── architecture.mermaid         # System architecture diagram
├── system/                          # OS implementation (Rust, no_std)
│   ├── kernel/                      # Microkernel (27 syscalls, EEVDF, SMP)
│   ├── services/
│   │   ├── init/                    # Root task — spawns everything, wires IPC
│   │   ├── compositor/              # Sole writer, renderer, input router
│   │   └── drivers/
│   │       ├── virtio-gpu/          # Display output (2D commands, present loop)
│   │       ├── virtio-input/        # Keyboard + tablet input (evdev translation)
│   │       ├── virtio-blk/          # Block device (sector reads)
│   │       ├── virtio-9p/           # Host filesystem passthrough
│   │       └── virtio-console/      # Serial console (minimal)
│   ├── libraries/
│   │   ├── drawing/                 # Surfaces, fonts, PNG, SVG, compositing
│   │   ├── ipc/                     # Lock-free SPSC ring buffers
│   │   ├── sys/                     # Syscall wrappers + userspace allocator
│   │   ├── virtio/                  # MMIO transport + split virtqueue
│   │   └── link.ld                  # Shared userspace linker script
│   ├── user/
│   │   ├── text-editor/             # Editor process (input → write requests)
│   │   └── echo/                    # IPC test program
│   ├── test/                        # Integration + stress tests
│   └── share/                       # Runtime assets (fonts, images, icons)
├── prototype/
│   └── files/                       # Files interface prototype (macOS-backed)
├── CLAUDE.md                        # AI collaboration context
├── README.md
└── UNLICENSE
```

## Design Documents

If you're curious about the design, read in this order:

1. **[Concept](design/concept.md)** — The document-centric model, mimetype evolution, layered rendering
2. **[Foundations](design/foundations.md)** — Glossary of terms, guiding beliefs, external boundaries, content model, editing model
3. **[Decisions](design/decisions.md)** — All 17 design decisions: settled positions with reasoning, open questions with tradeoffs, considered-and-rejected alternatives
4. **[Journal](design/journal.md)** — Where the design exploration is right now: open threads, discussion backlog, insights

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
