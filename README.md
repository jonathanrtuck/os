# os

A personal exploration of a document-centric operating system — one where documents are first-class citizens and applications are interchangeable tools that attach to content.

This is a design project, not a product. The primary artifact is a coherent OS design; code is written selectively to validate design decisions.

## The Idea

Modern operating systems are app-centric: **OS → App → Document.** You open an app, create or find a document inside it, and work within that app's world.

This project explores inverting that: **OS → Document → Tool.** Documents have independent identity. The OS understands what they are (via mimetypes) and can view any of them natively. Editing means attaching a tool to content — and tools are interchangeable.

- View is the default; editing is a deliberate step
- Editors bind to content types, not use cases (same text editor for documents, chat, email)
- No "save" — edits write immediately on a copy-on-write filesystem
- Files are organized by queryable metadata, not folder paths
- The GUI and CLI are equally fundamental OS interfaces

## Status

**Design phase with research spikes.** 12 of 17 design decisions are settled. A bare-metal aarch64 kernel boots in QEMU as a research spike to inform the technical foundation decision.

See the [decision register](design/decisions.md) for the full decision landscape and the [exploration journal](design/journal.md) for active threads and open questions.

## Project Structure

```
os/
├── design/                      # Design documentation
│   ├── concept.md               # The core idea: OS → Document → Tool
│   ├── foundations.md            # Glossary, guiding beliefs, external boundaries
│   ├── decisions.md             # 17 tiered design decisions with tradeoffs
│   ├── decision-map.mermaid     # Visual dependency graph
│   └── journal.md               # Open threads, insights, research spikes
├── system/                      # OS implementation
│   └── kernel/                  # Bare-metal aarch64 research spike (Rust)
├── CLAUDE.md                    # AI collaboration context
├── README.md
└── LICENSE
```

## Design Documents

If you're curious about the design, read in this order:

1. **[Concept](design/concept.md)** — The document-centric model, mimetype evolution, layered rendering
2. **[Foundations](design/foundations.md)** — Glossary of terms, guiding beliefs, external boundaries, content model, editing model
3. **[Decisions](design/decisions.md)** — All 17 design decisions: settled positions with reasoning, open questions with tradeoffs, considered-and-rejected alternatives
4. **[Journal](design/journal.md)** — Where the design exploration is right now: open threads, discussion backlog, insights

## Building the Kernel

The kernel is a research spike — a bare-metal aarch64 binary that boots in QEMU and prints to serial console.

**Prerequisites:** Rust toolchain, QEMU

```bash
# Install the cross-compilation target
rustup target add aarch64-unknown-none

# Build
cd system/kernel
./build.sh

# Run in QEMU (Ctrl-A X to quit)
./run.sh
```

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

[Unlicense](LICENSE) — public domain.
