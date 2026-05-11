# Roadmap

Milestone plan for the document-centric OS. Foundation-up: generic
infrastructure first, UX iteration last.

## Completed

| Version        | Theme                                                                   | Completed  |
| -------------- | ----------------------------------------------------------------------- | ---------- |
| v0.1           | Microkernel (memory, scheduling, IPC, syscalls, handle table, virtio)   | 2026-03-10 |
| v0.2           | Kernel audit, display pipeline, rendering architecture (3 backends)     | 2026-03-19 |
| v0.3           | Rendering + UI foundation (animation, composition, text, visual polish) | 2026-03-25 |
| v0.4           | Filesystem, Document Store (COW, snapshots, undo)                       | 2026-03-26 |
| v0.5           | Rich text (piece table, multi-style runs, content-type dispatch)        | 2026-03-30 |
| v0.6 (rewrite) | Kernel rewrite from first principles                                    | 2026-04-19 |
| v0.7           | Userspace rebuild — services, drivers, integration, rendering           | 2026-05-11 |

### v0.6 — Kernel Rewrite

Complete kernel rewrite guided by
`design/research/kernel-userspace-interface.md`. 35 syscalls, 6 object types
(VMO, Endpoint, Event, Thread, Address Space, Resource). Framekernel discipline
(all `unsafe` in `frame/`). SMP up to 8 cores. Synchronous call/recv/reply IPC
with priority inheritance, direct switch (−13.4% IPC latency). 12-phase
verification campaign: 557 tests, 4 fuzz targets, 33 proptests, mutation
testing, Miri, sanitizers. 26 bugs found and fixed. Benchmark baselines gated.

### v0.7 — Userspace Rebuild

Rebuilt the full userspace stack on the verified kernel. Five phases:

1. **Protocol + service infrastructure** — protocol crate (17 message types),
   service pack tool (SVPK), init, name service with handle transfer
2. **Drivers** — console (PL011), virtio-input, virtio-blk, Metal render,
   virtio-9p, virtio-rng, virtio-snd. Kernel extended with device VMOs, DMA VMOs
   (capability-gated via Resource type)
3. **Core libraries** — 13 libraries (scene, drawing, animation, fs, piecetable,
   layout, icons, fonts, render, store, png, jpeg, wav), 1,100+ tests
4. **Core services** — store (COW filesystem over blk), document (shared VMO
   buffer, undo ring), layout (word-breaking, seqlock results), presenter (scene
   graph builder), text editor (key dispatch, multi-hop RPC), filesystem (VFS
   over 9p), PNG decoder, JPEG decoder, audio mixer
5. **Integration + visual chrome** — full boot (15 services), scene-graph
   compositor, analytical shadows, Content::Path rasterizer, title bar + clock,
   page geometry, hardware cursor, pointer interaction (click/double/triple +
   drag selection), content-type typography (Inter + JetBrains Mono + Source
   Serif 4, font fallback), document switching (Ctrl+Tab spring animation,
   text + image spaces), rich text rendering (proportional layout, italic axis),
   120Hz frame loop, play button with audio playback

## Planned

| Version    | Theme                           | Character      | Key Deliverables                                                                                                                                                 |
| ---------- | ------------------------------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **v0.8**   | **Media Pipeline**              | Foundation     | Video playback (frame ring in content region). Additional audio codecs. Mimetype-driven decode routing. Media transport abstraction.                             |
| **v0.9**   | **Design Decisions**            | Design sprint  | Settle #10 (view state), #15 (layout engine), #17 (interaction model) _as interfaces_. System clipboard. Prototyping to validate, not to ship.                   |
| **v0.10**  | **Compound Documents & Layout** | Foundation     | Layout engine (#15). Spatial composition. Manifest format. Translators. Generic — not encoding a specific UX.                                                    |
| **v0.11**  | **Realtime & Streaming**        | Foundation     | Conversations, presence, streaming media as document types. Local prototype with mock transport. Exercises the temporal axis of compound docs (#14).             |
| **v0.12**  | **CLI / TUI**                   | Foundation     | The other native OS interface (GUI and CLI are equally fundamental). Shell model, tools-as-subshells, structured pipes. Depends on #17 settled in v0.9.          |
| **v0.13**  | **Network**                     | Infrastructure | Network stack, TCP/IP, DNS, TLS. Unlocks real transport for v0.11's realtime content types.                                                                      |
| **v0.14**  | **Web**                         | Foundation     | Web content as compound document, browser-as-translator. Depends on network + compound docs + layout engine.                                                     |
| **v0.15**  | **Real Hardware**               | Infrastructure | Apple Silicon bare-metal target. Driver work behind existing interfaces. Second architecture enabled by v0.6's arch abstraction.                                 |
| **v0.16+** | **UX Iteration**                | Polish         | GUI + CLI together. Document browse/search interface. Look, feel, interaction, animation. Multiple passes — this is where the document-centric thesis is tested. |
| **v1.0**   | **Ship**                        |                | Whatever "done" means.                                                                                                                                           |

## What's Already In Place

The v0.7 userspace built significant media and rendering infrastructure that was
originally planned for later milestones:

- **Image:** PNG decoder (all color types, Adam7 interlacing), JPEG decoder
  (EXIF orientation), texture rendering pipeline, mimetype-routed decode via
  decoder services
- **Audio:** WAV decoder, virtio-snd driver, audio mixer service, play button UI
- **Typography:** Three font families (JetBrains Mono, Inter, Source Serif 4),
  proportional layout, italic axis, font fallback chain
- **Rendering:** Analytical shadows, path rasterizer (fill + stroke), gradient
  paths, hardware cursor, 120Hz frame loop, batched GPU submit, atlas eviction
- **Interaction:** Click/double/triple-click, drag selection
  (character/word/line granularity), scene-graph hit testing, semantic cursor
  shapes
- **Document switching:** Multi-space scene graph, spring animation,
  content-type aware key dispatch

This means v0.8 (media) is partially complete — audio playback and image
decoding are done. Video is the remaining gap.

## Sequencing Rationale

**Foundation-up, UX-last.** v0.1–v0.15 build generic infrastructure behind clean
interfaces. UX iteration comes at the end (v0.16+) when all the pieces are on
the table and can be iterated freely. This maximizes reusability — if the UX
needs to change, the damage is contained to the leaf layer.

**Design decisions (v0.9) separate from UX iteration (v0.16+).** v0.9 settles
_interfaces_: "what are the interaction primitives?", "how does view state
work?", "what's the layout engine's API?" v0.16 iterates on _implementations_
behind those interfaces.

**Realtime before network (v0.11 before v0.13).** Forces the realtime content
model to be designed without assuming a specific transport. Conversations and
streams become document types with temporal semantics, not "network features."
When the network stack arrives, it plugs in as transport beneath content types
that already work locally.

**CLI/TUI as its own milestone (v0.12).** The CLI is a fundamental OS interface,
not an afterthought. Placed after design decisions (#17 settled) and compound
docs (rich enough to be interesting) but before network/web.

**Flexibility points:** v0.8 (media) and v0.9 (design decisions) are swappable —
media has no dependency on the interaction model. Everything else chains
naturally.

## Descoped

- Multi-display (single display only — interfaces are clean enough for others to
  add)
- Self-hosting (development stays on macOS)

## Decision Dependencies

Unsettled decisions and when they get resolved:

| Decision              | Status                            | Resolved in |
| --------------------- | --------------------------------- | ----------- |
| #10 View State        | Unsettled (leaning: opaque blobs) | v0.9        |
| #15 Layout Engine     | Unsettled                         | v0.9        |
| #17 Interaction Model | Exploring                         | v0.9        |

Settled decisions: #1–9, #11–14, #16, #18. See `decisions.md`.
