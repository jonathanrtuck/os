# Project Status

**Last updated:** 2026-04-01

## Current State

**v0.6 Kernel: IN PROGRESS.** Phases 1-2 complete. Phase 3 (core primitives) next.

- **Phase 1 (Arch Abstraction): COMPLETE.** 14 files under `kernel/arch/aarch64/`, zero asm outside arch, clean `#[cfg(target_arch)]` boundary. Settled interface: MMU, Context, interrupts, timer, serial, power.
- **Phase 2 (Capability Model): COMPLETE.** Rights attenuation (8 named rights, monotonic AND on transfer, per-syscall enforcement). Dynamic handle table (two-level: 256 base + overflow pages, 4096 cap, Handle u16). Badges (u64 per-handle, set/get syscalls, preserved through transfer). 30 syscalls. 2,425 tests pass.
- **Phase 3 (Core Primitives): IN PROGRESS.** Phase 3a (VMOs) design settled; implementation next. Phases 3b–3e (pager, signals, thread inspection, clock) need design discussions.
  - **Phase 3a (VMOs): DESIGNED.** Settled 2026-04-01. Novel design: versioned (COW generation snapshots), sealed (immutable freeze), content-typed (u64 tag), lazy-backed (demand-paged). 10 new syscalls (30-39). Subsumes `dma_alloc`/`dma_free`/`memory_share`. VMAR extension point documented. 8-step implementation plan ready.

**v0.5 Rich Text: COMPLETE** (2026-03-30). Piece table library (512 pieces, 32 styles, operation coalescing). Style palette with semantic a11y roles. Rich-editor process for text/rich documents. Content-type-aware edit protocol — document, layout engine, and presenter all dispatch on text/rich vs text/plain. Style shortcuts (Cmd+B/I, Cmd+1/2). Underline and strikethrough decorations.

**v0.4 Document Store: COMPLETE** (2026-03-25-26). Every document has identity (FileId), media type, queryable metadata, and version history (COW snapshots). Document service replaces filesystem service. Undo/redo (Cmd+Z / Cmd+Shift+Z) wired to COW snapshots — 64-entry undo ring, character-level granularity.

## Architecture (Decomposed 2026-03-29)

```text
Input Driver → Presenter → Scene Graph → Render Service → Display
                  ↕         ↕
               Editor    Layout Engine
                  ↕         ↕
             Document Model
                  ↕
           Document Service → Disk
```

The monolithic `core` has been decomposed into three processes: document, layout engine, and presenter (view engine). Protocol library consolidated to 10 modules (init, device, input, edit, layout, view, document, decode, content, metal). 2,313 tests pass.

Content types: `None`, `InlineImage` (per-frame scene data), `Image` (Content Region via content_id), `Path`, `Glyphs`. Sole render backend: `metal-render` (native Metal GPU via hypervisor). cpu-render and virgil-render removed (2026-03-30).

**Service pack (2026-03-30):** Service ELFs packed into flat archive linked as `.services` section in the kernel binary. Init reads ELFs from a memory-mapped pack at `SERVICE_PACK_BASE` (512 MiB) instead of `include_bytes!` statics. Breaks the 19 MB relinking cascade — changing a service now requires only repack + relink. Init shrank from ~5 MB to 904 KB. Pack format: `pack_format.rs` (SSOT), 14 services, page-aligned ELF data. Build: llvm-objcopy converts pack to `.o`, linked via `cargo:rustc-link-arg`.

**IPC:** Two mechanisms, matched to data semantics. Event rings (64-byte SPSC messages over shared memory) for discrete events where order/count matter (keys, clicks, config). State registers (atomic shared memory) for continuous data where only the latest value matters (pointer position). Both signaled via `channel_signal` syscall. **Content Region** (4 MiB shared memory with registry) for persistent decoded content (font TTF data, decoded image pixels) — init allocates, core writes, render services read. **Document IPC** (`protocol::document`): 13 message types. Core sends `MSG_DOC_COMMIT` at operation boundaries; document service reads doc buffer from shared memory. `MSG_DOC_SNAPSHOT`/`MSG_DOC_RESTORE` for undo/redo. `MSG_DOC_QUERY` for media-type/attribute queries. See `system/DESIGN.md` §0 for full details.

**Content Pipeline (2026-03-25):** Three memory regions: File Store (1 MiB, shared with decoder services), Content Region (4 MiB, shared decoded content with registry + free-list allocator + generation-based GC), Scene Graph (per-frame visual primitives). Init allocates both, loads fonts into Content Region + PNG into File Store. Core sends decode requests to sandboxed decoder services via generic IPC protocol (`protocol/decode.rs`). Decoder services read File Store (RO), write decoded BGRA pixels into Content Region (RW). Core manages Content Region registry and allocator. Render services find fonts and images via `protocol::content` registry lookup. Compositor never sees encoded files. Generic decoder harness (`services/decoders/harness.rs`) handles all IPC plumbing; format-specific code is just header + decode functions.

**Crash reporting:** Kernel panic → diagnostic output via UART → `pvpanic_signal()` (MMIO write to 0x0902_0000) → hypervisor captures vCPU registers + serial log → crash report at `/tmp/hypervisor-crash-<ts>.log` → `exit(1)`. Fallback: `system_off()` (PSCI SYSTEM_OFF). pvpanic device discovered from DTB at boot, address stored in `PVPANIC_ADDR` AtomicUsize.

## v0.4 Document Store Details

Seven-layer fs stack: `BlockDevice` trait → superblock ring → free-extent allocator → inodes → COW write path → snapshots → `Files` trait. Store library adds metadata layer: catalog (media types, attributes), queries, wraps `Box<dyn Files>`. Document service (`services/document/`) replaces filesystem service — thin IPC translator over store library. Factory disk image builder (`tools/mkdisk/`) pre-populates fonts. Boot loads fonts from native filesystem (no 9p dependency). Multi-document persistence (text + image spaces). Undo/redo via COW snapshots: `UndoState` in core, Cmd+Z/Cmd+Shift+Z, 64-entry ring, character-level granularity. Protocol: `protocol::document` (13 message types). IPC: `MSG_DOC_COMMIT` at operation boundaries, `MSG_DOC_SNAPSHOT`/`MSG_DOC_RESTORE` for undo.

**16 KiB page migration (2026-03-25): DONE.** Kernel page granule changed from 4K to 16K. 2-level page tables (L2+L3, T0SZ/T1SZ=28, 64 GiB VA). KERNEL_VA_OFFSET changed to 0xFFFF_FFF0_0000_0000 (T1SZ=28 consequence). Boot tables: 2 L2 roots with 32 MiB block entries. Address space: simplified 4-level→2-level walk. Userspace: 16K section alignment in link.ld, PAGE_SIZE updated in ipc/sys/protocol/virtio libraries.

## Completed Milestones

### v0.3 Rendering Foundation (2026-03-16–25)

- **Phase 4 (Visual Polish):** Dark desk (#202020) / white page palette. JetBrains Mono, Inter, Source Serif 4 fonts. Font rendering quality sprint (5 changes to match macOS Core Text): outline dilation, analytic area coverage, device-pixel rasterization, subpixel glyph positioning, single char_w_fx SSOT. Icon pipeline (SVG→path, stroke expansion). Page surface + document strip with spring-based slide. Shared pointer state register. Cursor-only frames. Float16 rendering pipeline with Bayer dither. Content Region allocator + GC. PNG decoder factored to sandboxed service (162/162 PngSuite, CRC32).
- **Phase 3 (Text & Interaction):** Unified layout library (`FontMetrics` trait, CharBreaker/WordBreaker). All navigation/selection in core (not editor). Full macOS key combos. Editor slimmed to ~195 lines. Hypervisor event scripts + fixed resolution for visual regression testing.
- **Phase 2 (Composition):** Clip masks, backdrop blur (3-pass box blur), pointer cursor. All three render backends.
- **Phase 1 (Motion):** Animation library (easing, springs, timeline). Smooth scroll, cursor blink, transitions.
- **Earlier:** Rendering architecture redesign, virgl driver + cpu-render merge, GICv3 + tickless idle, rendering correctness, hypervisor extraction.

### System Code

`system/kernel/` (33 .rs + 2 .S), `system/services/{init,document,layout,presenter,store,filesystem,drivers/{metal-render,virtio-blk,virtio-console,virtio-input,virtio-9p},decoders/{png}}/`, `system/libraries/{sys,virtio,drawing,fonts,animation,layout,scene,ipc,protocol,render,piecetable,icons,fs,store}/`, `system/user/{echo,text-editor,rich-editor,stress,fuzz,fuzz-helper}/`, `system/test/`, `tools/mkdisk/`. 28 syscalls. 4 SMP cores, EEVDF scheduler.

## Milestone Roadmap

- **v0.5:** Rich text (multi-style runs, operation coalescing)
- **v0.6:** Kernel (arch abstraction, capabilities, VMOs, pager, signals, SMP scalability, ASLR, standalone packaging)
- **v0.7:** Media (JPEG, audio, video) — swappable with v0.8
- **v0.8:** Design decisions (settle #10, #15, #17 as interfaces, clipboard)
- **v0.9:** Compound documents & layout engine
- **v0.10:** Realtime & streaming (conversations/presence as document types)
- **v0.11:** CLI / TUI (fundamental OS interface, not an app)
- **v0.12:** Network (TCP/IP, DNS, TLS)
- **v0.13:** Web (browser-as-translator)
- **v0.14:** Real hardware (bare-metal target)
- **v0.15+:** UX iteration (GUI + CLI, document browse/search, look & feel — multiple passes)
- **v1.0:** Ship

See `design/roadmap.md` for full details and rationale.

## Open Questions

- Trust/complexity orthogonality (solid), blue-wraps-all-sides (solid), shell is blue-layer (leaning), one-document-at-a-time (leaning), compound document editing (unresolved)
- Decision #14: Mimetype of whole document, manifest format, FS organization of manifests + content files
- Decision #16: COW on-disk design (deferred via prototype-on-host), snapshot scope (punted)

## Known Issues

- JPEG decoder blocked on mimetype-based decoder routing (requires filesystem/metadata layer)
- Deferred: AA transition softness tuning, italic rendering (in journal)
