# design

Design documents for the document-centric OS. This is the primary artifact of the project -- the design matters more than the implementation.

## Key Files

- `philosophy.md` — **Read first.** Two root principles and their consequences. If you internalize these, you can predict why any component is structured the way it is.
- `foundations.md` — The core idea, guiding beliefs, glossary, content model (3-layer type system), viewer-first design, editor augmentation model, edit protocol, undo/history architecture
- `decisions.md` — 17 tiered decisions with tradeoffs, implementation readiness, dependency chains
- `architecture.md` — The system's architectural narrative: pipeline, responsibilities, decision checklist
- `journal.md` — Open threads, discussion backlog, insights log, research spikes. The "pick up where you left off" document
- `research/` — COW filesystems, OS landscape, font rendering, kernel hardening gap analysis

## Diagrams

- `architecture.mermaid` — System architecture (process layers, IPC, memory mapping)
- `decision-map.mermaid` — Visual dependency graph of all decisions
- `dependency-graph.mermaid` — Component dependency graph
- `rendering-pipeline.mermaid` — Rendering pipeline data shapes and translators
- `userspace-graph.mermaid` — Userspace component relationships

## Conventions

- Decisions are numbered and tiered (Tier 0 = foundational, higher = more derived)
- "Settled" means committed; "leaning" means current direction but not locked
- The journal is append-only -- new sessions add to the top, old context stays for reference
- Mermaid files are visual companions to the text documents, not standalone
