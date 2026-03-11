# design

Design documents for the document-centric OS. This is the primary artifact of the project -- the design matters more than the implementation.

## Key Files

- `foundations.md` — Guiding beliefs, glossary, content model (3-layer type system), viewer-first design, editor augmentation model, edit protocol, undo/history architecture
- `decisions.md` — 17 tiered decisions with tradeoffs, implementation readiness, dependency chains
- `decision-map.mermaid` — Visual dependency graph of all decisions
- `architecture.mermaid` — System architecture diagram (process layers, IPC, memory mapping)
- `concept.md` — The core idea: OS -> Document -> Tool, mimetype evolution, layered rendering, compound documents
- `journal.md` — Open threads, discussion backlog, insights log, research spikes. The "pick up where you left off" document
- `research-cow-filesystems.md` — Research on COW filesystem designs (ZFS, Btrfs, WAFL, etc.)
- `research-os-landscape.md` — Survey of prior art (BeOS, Plan 9, Mercury OS, Ideal OS, OpenDoc, Xerox Star)

## Conventions

- Decisions are numbered and tiered (Tier 0 = foundational, higher = more derived)
- "Settled" means committed; "leaning" means current direction but not locked
- The journal is append-only -- new sessions add to the top, old context stays for reference
- Mermaid files are visual companions to the text documents, not standalone
