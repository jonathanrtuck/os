# Roadmap

Milestone plan for the document-centric OS. Foundation-up: generic infrastructure first, UX iteration last.

## Completed

| Version | Theme                                                                         | Completed  |
| ------- | ----------------------------------------------------------------------------- | ---------- |
| v0.1    | Microkernel (memory, scheduling, IPC, syscalls, handle table, virtio drivers) | 2026-03-10 |
| v0.2    | Kernel audit, display pipeline, rendering architecture (3 backends, GICv3)    | 2026-03-19 |
| v0.3    | Rendering + UI foundation (animation, composition, text, visual polish)       | 2026-03-25 |
| v0.4    | Filesystem, Document Store (COW, snapshots, undo)                             | 2026-03-26 |
| v0.5    | Rich text (piece table, multi-style runs, content-type dispatch, decorations) | 2026-03-30 |

## Planned

| Version    | Theme                           | Character      | Key Deliverables                                                                                                                                                                                                                                                 |
| ---------- | ------------------------------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **v0.6**   | **Kernel**                      | Foundation     | General-purpose microkernel. Arch abstraction (aarch64 as reference impl), capability model (rights attenuation, dynamic handles, badges), core primitives (VMOs, pager, signals), security hardening (ASLR), standalone packaging. See `design/kernel-v0.6.md`. |
| **v0.7**   | **Media**                       | Foundation     | JPEG decoder (mimetype routing exists). Audio subsystem. Video playback (frame ring in Content Region). Larger milestone. _(Swappable with v0.8.)_                                                                                                               |
| **v0.8**   | **Design Decisions**            | Design sprint  | Settle #10 (view state), #15 (layout engine), #17 (interaction model) _as interfaces_. System clipboard. Prototyping to validate, not to ship.                                                                                                                   |
| **v0.9**   | **Compound Documents & Layout** | Foundation     | Layout engine (#15). Spatial composition. Manifest format. Translators. Generic -- not encoding a specific UX.                                                                                                                                                   |
| **v0.10**  | **Realtime & Streaming**        | Foundation     | Conversations, presence, streaming media as document types. Local prototype with mock transport. Exercises the temporal axis of compound docs (#14).                                                                                                             |
| **v0.11**  | **CLI / TUI**                   | Foundation     | The other native OS interface (GUI and CLI are equally fundamental -- not an app). Shell model, tools-as-subshells, structured pipes. Depends on #17 settled in v0.8.                                                                                            |
| **v0.12**  | **Network**                     | Infrastructure | Network stack, TCP/IP, DNS, TLS. Unlocks real transport for v0.10's realtime content types.                                                                                                                                                                      |
| **v0.13**  | **Web**                         | Foundation     | Web content as compound document, browser-as-translator. Depends on network + compound docs + layout engine.                                                                                                                                                     |
| **v0.14**  | **Real Hardware**               | Infrastructure | Apple Silicon or other bare-metal target. Driver work behind existing interfaces. Second architecture enabled by v0.6's arch abstraction.                                                                                                                        |
| **v0.15+** | **UX Iteration**                | Polish         | GUI + CLI together. Document browse/search interface. Look, feel, interaction, animation. Multiple passes expected -- this is where the document-centric thesis gets tested as an _experience_, not just an architecture.                                        |
| **v1.0**   | **Ship**                        |                | Whatever "done" means.                                                                                                                                                                                                                                           |

## Sequencing Rationale

**Foundation-up, UX-last.** v0.1--v0.14 build generic infrastructure behind clean interfaces. UX iteration comes at the end (v0.15+) when all the pieces are on the table and can be iterated freely. This maximizes reusability -- if the UX needs to change, the damage is contained to the leaf layer.

**Kernel before media (v0.6 before v0.7).** The kernel is the strongest component and the most generally useful. v0.6 focuses exclusively on making it standalone: architecture abstraction, capability model, core primitives (VMOs, pager, signals), security hardening. This work benefits the OS (pager interface unlocks memory-mapped documents, capabilities unlock compound doc security) AND makes the kernel independently valuable. No userspace changes.

**Design decisions (v0.8) separate from UX iteration (v0.15+).** v0.8 settles _interfaces_: "what are the interaction primitives?", "how does view state work?", "what's the layout engine's API?" v0.15 iterates on _implementations_ behind those interfaces: visual language, animation, spatial relationships, the actual feel.

**Realtime before network (v0.10 before v0.12).** Forces the realtime content model to be designed without assuming a specific transport. Conversations and streams become document types with temporal semantics, not "network features." When the network stack arrives, it plugs in as transport beneath content types that already work locally.

**CLI/TUI as its own milestone (v0.11).** The CLI is a fundamental OS interface, not an afterthought. Placed after design decisions (#17 settled) and compound docs (rich enough to be interesting) but before network/web (complexity) and UX iteration (so both GUI and CLI can be iterated together).

**Flexibility points:** v0.7 (media) and v0.8 (design decisions) are swappable -- media has no dependency on the interaction model. Everything else chains naturally.

## Descoped

- Multi-display (single display only -- interfaces are clean enough for others to add)
- Self-hosting (development stays on macOS)

## Decision Dependencies

Unsettled decisions and when they get resolved:

| Decision              | Status                            | Resolved in |
| --------------------- | --------------------------------- | ----------- |
| #10 View State        | Unsettled (leaning: opaque blobs) | v0.8        |
| #15 Layout Engine     | Unsettled                         | v0.8        |
| #17 Interaction Model | Exploring                         | v0.8        |

Settled decisions: #1--9, #11--14, #16, #18. See `decisions.md` for full details.
